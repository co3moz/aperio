use axum::extract::ws::Message;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast, mpsc, oneshot, watch};
use tracing::error;

use crate::oidc;
use crate::store::audit::AuditLog;
use crate::store::stats::{self, StatsStore};
use crate::store::tokens::TokenStore;
use crate::store::webhooks::{self, WebhookStore};

use crate::protocol::TunnelMessage;
use crate::settings::{ServerConfig, SettingsOverrides};

/// In-memory server-wide traffic statistics.
#[derive(Serialize, Clone)]
pub(crate) struct ServerStats {
  /// Total count of incoming proxied requests.
  pub(crate) total_requests: u64,
  /// Count of successful request forwards.
  pub(crate) successful_requests: u64,
  /// Count of failed request forwards.
  pub(crate) failed_requests: u64,
  /// Total bytes of payloads transferred through the server.
  pub(crate) total_bytes_transferred: u64,
}

/// Details of an active tunnel client connection.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct ClientDetail {
  /// Unique client UUID.
  pub(crate) id: String,
  /// Remote socket IP address of the client connection.
  pub(crate) ip: String,
  /// Number of seconds elapsed since connection establishment.
  pub(crate) connected_for_seconds: u64,
  /// Total request count processed by this client connection.
  pub(crate) request_count: u64,
  /// Path bind in effect (declared by the client or granted by its token).
  pub(crate) path_bind: Option<String>,
  /// Hostnames in effect (declared, token-granted, and random-subdomain).
  pub(crate) hostname_binds: Vec<String>,
  /// The subset of `hostname_binds` the client asked for itself, in the order
  /// it declared them. The dashboard shows the first of these as the client's
  /// primary hostname, since a name the operator chose identifies the service
  /// better than one the server handed out.
  pub(crate) declared_hostnames: Vec<String>,
  /// The random subdomain the server assigned this client, if any. Listed
  /// apart from the declared names because it is the server's name, not the
  /// client's, and it changes whenever the subdomain pattern does.
  pub(crate) random_hostname: Option<String>,
  /// Name of the dynamic token this client authenticated with (None = master).
  pub(crate) token_name: Option<String>,
  /// Organization this client belongs to, from its token (None = master).
  pub(crate) org_id: Option<String>,
  /// Temporary server-side path bind override (dashboard overrule).
  pub(crate) override_path_bind: Option<String>,
  /// Temporary server-side hostname binds (dashboard overrule). Empty = none;
  /// otherwise this list is what the connection is routed on, in place of
  /// every declared and assigned name.
  pub(crate) override_hostname_binds: Vec<String>,
  /// Seconds elapsed since the last heartbeat Ping was received.
  pub(crate) last_ping_seconds_ago: Option<u64>,
  /// Concurrency limit announced by the client (None = unlimited).
  pub(crate) max_concurrent: Option<u32>,
  /// Client build version announced via Ping (None until the first Ping).
  pub(crate) version: Option<String>,
  /// Service name announced via Ping (multi-service clients).
  pub(crate) service: Option<String>,
  /// What that service is called on screen, when the client's file said so.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) service_custom_name: Option<String>,
  /// True when this client serves its traffic without the visitor auth gate.
  pub(crate) public: bool,
  /// True when this client gates its service behind a client-set visitor
  /// login (the credentials themselves are never exposed to the dashboard).
  pub(crate) visitor_auth: bool,
  /// Visitor IPs/CIDRs allowed to reach this client's service (empty = everyone).
  pub(crate) allowed_ips: Vec<String>,
  /// Tunnel protocol version announced via Ping.
  pub(crate) protocol: Option<u32>,
  /// True when the announced protocol version differs from the server's.
  pub(crate) protocol_mismatch: bool,
  /// Latest backend health verdict reported by the client's own probe.
  pub(crate) backend_healthy: bool,
  /// False only while a configured health check has not completed its first
  /// probe (dashboard shows "checking" instead of "backend down").
  pub(crate) backend_probed: bool,
  /// What the client reports about itself (planned_features #37): CPU as a
  /// percentage of one core and resident memory of the client process, then
  /// round-trip time, jitter and reconnects of this tunnel connection. All
  /// `None` from a client that does not report them, and the process figures
  /// are `None` where they cannot be read without guessing.
  pub(crate) cpu_percent: Option<f64>,
  pub(crate) rss_bytes: Option<u64>,
  pub(crate) rtt_ms: Option<u64>,
  pub(crate) jitter_ms: Option<u64>,
  pub(crate) reconnects: Option<u32>,
  /// Announced load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: u32,
  /// Announced downstream link capacity in bytes/second (None = unlimited).
  pub(crate) bandwidth_bps: Option<u64>,
  /// False when the client missed its heartbeat window and is out of the pool.
  pub(crate) healthy: bool,
  /// True while the client is gracefully draining before shutdown.
  pub(crate) draining: bool,
  /// True while the client is passively ejected from routing after crossing the
  /// outlier failure threshold (5xx / timeout / connection loss). The tunnel
  /// connection stays up; it is re-admitted automatically when the window ends.
  pub(crate) ejected: bool,
  /// Dashboard kill switch state (false = excluded from routing).
  pub(crate) enabled: bool,
  /// True when the service opted into caching (`cache: true`) but the server's
  /// response cache is disabled (APERIO_CACHE off), so the opt-in does nothing.
  pub(crate) cache_ignored: bool,
  /// False when this service asked not to be recorded for the request
  /// inspector (`capture: false`).
  pub(crate) capture: bool,
  /// True when the service is willing to be recorded but the server has the
  /// inspector off altogether, so nothing about it can be inspected however
  /// the client is configured.
  pub(crate) capture_disabled_by_server: bool,
  /// Client-process instance id self-reported via Ping (`--client-id`).
  pub(crate) instance_id: Option<String>,
  /// True when another live connection reports the same instance id, a
  /// misconfiguration warning surfaced in the dashboard (`--bind-tunnels`
  /// and failover `wait` lookups become ambiguous).
  pub(crate) instance_id_shared: bool,
  /// Process-wide instance group id (the client's raw `client_id` base, shared
  /// by every service and every parallel connection of one client process).
  /// `None` for clients that predate the `x-aperio-instance` handshake header.
  /// The dashboard groups connections by this so a multi-connection client
  /// shows as one entity.
  pub(crate) instance_group: Option<String>,
}

/// Enhanced metrics stats combined with active client details.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct EnhancedServerStats {
  /// Total incoming request count.
  pub(crate) total_requests: u64,
  /// Successful requests count.
  pub(crate) successful_requests: u64,
  /// Failed requests count.
  pub(crate) failed_requests: u64,
  /// Total bytes transferred.
  pub(crate) total_bytes_transferred: u64,
  /// Current count of connected tunnel clients.
  pub(crate) connected_clients_count: usize,
  /// Uptime in seconds.
  pub(crate) uptime_seconds: u64,
  /// Request count waiting in the reconnection buffer.
  pub(crate) pending_requests_count: usize,
  /// List of client connection details.
  pub(crate) active_clients: Vec<ClientDetail>,
  /// Restart-surviving counters and period buckets.
  pub(crate) persistent: stats::PersistentStats,
  /// All-time average response time in milliseconds.
  pub(crate) avg_response_ms: f64,
  /// Today.s traffic bucket.
  pub(crate) today: stats::PeriodStats,
}

/// Structure representing a logged HTTP transaction.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct RequestLog {
  /// Request UUID.
  pub(crate) id: String,
  /// Timestamp formatted as string.
  pub(crate) timestamp: String,
  /// HTTP method (GET, POST, etc.).
  pub(crate) method: String,
  /// Request URI path.
  pub(crate) uri: String,
  /// Status code returned.
  pub(crate) status: Option<u16>,
  /// Duration of processing in milliseconds.
  pub(crate) duration_ms: u128,
  /// Reason string if request failed.
  pub(crate) error: Option<String>,
  /// Request hostname (None for failures resolved before routing). Also the
  /// selector the right-to-erasure purge matches log entries on.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) host: Option<String>,
  /// Organization of the client that served the request (None = master, or a
  /// server-level failure with no client). The dashboard traffic log and live
  /// stream are filtered to the caller's effective org on this field.
  #[serde(skip)]
  pub(crate) org_id: Option<String>,
}

/// A fully captured HTTP transaction for the dashboard inspector. Bodies are
/// capped at [`CAPTURE_BODY_LIMIT`] bytes; larger bodies are truncated for
/// display and cannot be replayed.
#[derive(Serialize, Clone)]
pub(crate) struct CapturedRequest {
  /// Request UUID (matches the RequestLog id).
  pub(crate) id: String,
  /// Timestamp formatted as string.
  pub(crate) timestamp: String,
  pub(crate) method: String,
  /// Full request URI including query string.
  pub(crate) uri: String,
  /// Request headers as forwarded to the tunnel client.
  pub(crate) req_headers: Vec<(String, String)>,
  /// Base64 request body (possibly truncated).
  pub(crate) req_body: Option<String>,
  /// True when the request body exceeded the capture limit.
  pub(crate) req_body_truncated: bool,
  pub(crate) status: u16,
  pub(crate) resp_headers: Vec<(String, String)>,
  /// Base64 response body (buffered responses only, possibly truncated).
  pub(crate) resp_body: Option<String>,
  pub(crate) resp_body_truncated: bool,
  /// True when the response body was streamed (not captured).
  pub(crate) resp_streamed: bool,
  pub(crate) duration_ms: u128,
  /// High-resolution stage timeline (buffered responses of v2+ clients).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) timeline: Option<RequestTimeline>,
  /// Connection id of the client that served it. Unique and unreadable, so
  /// it is what an action addresses rather than what the screen shows.
  pub(crate) client_id: String,
  /// What that client calls itself, if anything: the operator's
  /// `custom_name`, else the `name` of its `services:` entry. `None` for a
  /// client that declared neither, where the id is all there is.
  pub(crate) client_name: Option<String>,
  /// Organization of the client that served the request (None = master). The
  /// inspector and replay are gated to the caller's effective org on this.
  #[serde(skip)]
  pub(crate) org_id: Option<String>,
}

/// Maximum number of captured requests kept in memory.
pub(crate) const CAPTURE_MAX_ENTRIES: usize = 50;

/// Makes room for one capture, evicting from whichever organization is
/// holding the most (planned_features #69).
///
/// The ring used to evict from the front, which is fair only if every
/// organization is equally busy. It is not: one org serving a thousand
/// requests a minute walked a quiet org's captures out of the buffer within
/// seconds, so a tenant investigating one request an hour could never find it.
/// Multi-tenant hygiene the byte and client quotas already had, and this one
/// did not.
///
/// A fair share rather than a fixed per-org ceiling, and that is the design
/// decision worth naming. A ceiling has to be chosen, and it interacts badly
/// with the total: five orgs capped at twenty each is a hundred in a buffer
/// that holds fifty, so the total cap evicts across tenants again and the
/// ceiling bought nothing. Evicting from the largest holder needs no number,
/// converges on an even split by itself, and lets one org use the whole buffer
/// while it is the only one there, which is what an operator would want.
pub(crate) fn evict_for_fairness(captured: &mut VecDeque<CapturedRequest>) {
  // Whoever holds the most. Ties go to the org whose oldest capture is oldest,
  // which is the front-eviction rule applied *within* the tie rather than
  // across tenants, and it is why the winner is found by walking the deque
  // rather than by taking a maximum out of the map: a `HashMap` iterates in an
  // arbitrary order, so a tie would otherwise be broken differently on every
  // call and two equally busy tenants would take turns at random.
  let mut counts: HashMap<Option<&str>, usize> = HashMap::new();
  for entry in captured.iter() {
    *counts.entry(entry.org_id.as_deref()).or_insert(0) += 1;
  }
  let Some(most) = counts.values().copied().max() else {
    return;
  };
  if let Some(at) = captured
    .iter()
    .position(|e| counts.get(&e.org_id.as_deref()) == Some(&most))
  {
    captured.remove(at);
  }
}
/// Maximum captured body size per direction (decoded bytes).
pub(crate) const CAPTURE_BODY_LIMIT: usize = 64 * 1024;
/// Request bodies above this size are streamed to v2 clients as
/// RequestStart/Chunk/End frames instead of being buffered in memory.
pub(crate) const REQUEST_STREAM_THRESHOLD: u64 = 256 * 1024;

/// Names of the per-request stages tracked for latency statistics, in
/// timeline order. `queue` and `serve` come from server measurements alone;
/// the middle stages exist only for timing-aware clients.
pub(crate) const STAGE_KEYS: [&str; 7] = [
  "queue",
  "transit_out",
  "client_processing",
  "backend_wait",
  "backend_body",
  "transit_back",
  "serve",
];

/// Rolling per-stage latency window for one route (hostname), feeding the
/// stage-statistics API: mean and standard deviation per stage plus an
/// anomaly verdict for the most recent sample. In-memory only.
pub(crate) struct StageWindow {
  /// Recent samples, one array of per-stage µs durations each (None =
  /// stage not measured for that request).
  samples: std::collections::VecDeque<[Option<u64>; 7]>,
  /// Organization serving this route (`None` = master); the dashboard filters
  /// the per-stage view to the caller's org.
  pub(crate) org_id: Option<String>,
  /// When a sample was last recorded for this route, used to evict the
  /// least-recently-used route when the route cap is hit (bounds memory
  /// under hostname churn, e.g. random preview subdomains).
  last_recorded: Instant,
}

/// Samples kept per route.
const STAGE_WINDOW_CAP: usize = 500;
/// Minimum samples before anomaly verdicts are emitted.
const STAGE_MIN_SAMPLES: usize = 20;
/// Distinct routes tracked; the least-recently-used route is evicted past
/// this so a churn of hostnames cannot grow the map without bound.
pub(crate) const STAGE_ROUTE_CAP: usize = 256;

impl StageWindow {
  fn new(org_id: Option<String>) -> Self {
    StageWindow {
      samples: std::collections::VecDeque::new(),
      org_id,
      last_recorded: Instant::now(),
    }
  }

  /// Extracts per-stage durations from a timeline and records them.
  pub(crate) fn record(&mut self, tl: &RequestTimeline) {
    let diff = |a: Option<u64>, b: Option<u64>| -> Option<u64> {
      match (a, b) {
        (Some(a), Some(b)) => Some(b.saturating_sub(a)),
        _ => None,
      }
    };
    let sample: [Option<u64>; 7] = [
      Some(tl.dispatched_us),
      diff(Some(tl.dispatched_us), tl.client_received_us),
      diff(tl.client_received_us, tl.backend_sent_us),
      diff(tl.backend_sent_us, tl.backend_first_byte_us),
      diff(tl.backend_first_byte_us, tl.backend_done_us),
      diff(tl.client_responded_us, Some(tl.response_received_us)),
      Some(tl.finished_us.saturating_sub(tl.response_received_us)),
    ];
    if self.samples.len() >= STAGE_WINDOW_CAP {
      self.samples.pop_front();
    }
    self.samples.push_back(sample);
    self.last_recorded = Instant::now();
  }

  /// Per-stage statistics of the window. A stage's latest sample is
  /// anomalous when it sits more than three standard deviations above the
  /// mean of a big-enough window.
  pub(crate) fn stats(&self) -> Vec<StageRow> {
    (0..STAGE_KEYS.len())
      .map(|i| {
        let values: Vec<u64> = self.samples.iter().filter_map(|s| s[i]).collect();
        let count = values.len();
        if count == 0 {
          return StageRow {
            stage: STAGE_KEYS[i],
            count: 0,
            mean: 0.0,
            stddev: 0.0,
            last: None,
            anomalous: false,
          };
        }
        let mean = values.iter().sum::<u64>() as f64 / count as f64;
        let var = values
          .iter()
          .map(|v| {
            let d = *v as f64 - mean;
            d * d
          })
          .sum::<f64>()
          / count as f64;
        let stddev = var.sqrt();
        let last = self.samples.back().and_then(|s| s[i]);
        let anomalous = count >= STAGE_MIN_SAMPLES
          && last.is_some_and(|l| l as f64 > mean + 3.0 * stddev && l as f64 > mean * 1.5);
        StageRow {
          stage: STAGE_KEYS[i],
          count,
          mean,
          stddev,
          last,
          anomalous,
        }
      })
      .collect()
  }
}

/// One stage's statistics over the rolling window.
pub(crate) struct StageRow {
  pub(crate) stage: &'static str,
  pub(crate) count: usize,
  pub(crate) mean: f64,
  pub(crate) stddev: f64,
  pub(crate) last: Option<u64>,
  pub(crate) anomalous: bool,
}

/// All routes' stage windows, keyed by request hostname (or `*`).
#[derive(Default)]
pub(crate) struct StageStats {
  pub(crate) routes: std::collections::HashMap<String, StageWindow>,
}

impl StageStats {
  pub(crate) fn record(&mut self, host: Option<&str>, org: Option<&str>, tl: &RequestTimeline) {
    let key = host.unwrap_or("*").to_string();
    // Bound the number of tracked routes: evict the least-recently-used one
    // before admitting a new route past the cap, so a churn of distinct
    // hostnames (wildcard/preview subdomains) cannot grow the map without
    // bound. Only runs when at capacity and a genuinely new route arrives.
    if !self.routes.contains_key(&key)
      && self.routes.len() >= STAGE_ROUTE_CAP
      && let Some(lru) = self
        .routes
        .iter()
        .min_by_key(|(_, w)| w.last_recorded)
        .map(|(k, _)| k.clone())
    {
      self.routes.remove(&lru);
    }
    let window = self
      .routes
      .entry(key)
      .or_insert_with(|| StageWindow::new(org.map(str::to_string)));
    // A route is served by one org; keep its label current.
    window.org_id = org.map(str::to_string);
    window.record(tl);
  }
}

/// Rolling latency window for one endpoint (`host|path`), feeding the
/// slowest-endpoints report. In-memory only, like the stage windows.
pub(crate) struct EndpointWindow {
  /// Durations (ms) of the most recent requests, insertion order.
  durations: std::collections::VecDeque<u64>,
  /// Lifetime request count for this endpoint since server start.
  pub(crate) count: u64,
  /// Lifetime 5xx/error count since server start.
  pub(crate) errors: u64,
  /// Organization serving this endpoint (`None` = master).
  pub(crate) org_id: Option<String>,
}

/// Samples kept per endpoint.
const ENDPOINT_WINDOW_CAP: usize = 200;
/// Distinct endpoints tracked; overflow folds into `__other`.
const ENDPOINT_KEY_CAP: usize = 300;
/// Endpoints with fewer recent samples than this are left out of the report.
pub(crate) const ENDPOINT_MIN_SAMPLES: usize = 5;

impl EndpointWindow {
  /// Latency summary over the recent window: (avg, p50, p95, max) in ms.
  pub(crate) fn summary(&self) -> (f64, u64, u64, u64) {
    if self.durations.is_empty() {
      return (0.0, 0, 0, 0);
    }
    let mut sorted: Vec<u64> = self.durations.iter().copied().collect();
    sorted.sort_unstable();
    let avg = sorted.iter().sum::<u64>() as f64 / sorted.len() as f64;
    let pick =
      |p: f64| sorted[((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1)];
    (avg, pick(0.50), pick(0.95), *sorted.last().unwrap_or(&0))
  }

  /// Recent samples in the window.
  pub(crate) fn samples(&self) -> usize {
    self.durations.len()
  }
}

/// All endpoints' latency windows, keyed `host|path` (path without query).
#[derive(Default)]
pub(crate) struct EndpointStats {
  pub(crate) endpoints: std::collections::HashMap<String, EndpointWindow>,
}

impl EndpointStats {
  /// Records one served request for the slowest-endpoints report.
  pub(crate) fn record(
    &mut self,
    host: Option<&str>,
    path: &str,
    status: u16,
    duration_ms: u64,
    org: Option<&str>,
  ) {
    let key = format!("{}|{}", host.unwrap_or("*"), path);
    let key = if self.endpoints.contains_key(&key) || self.endpoints.len() < ENDPOINT_KEY_CAP {
      key
    } else {
      "__other|__other".to_string()
    };
    let w = self.endpoints.entry(key).or_insert_with(|| EndpointWindow {
      durations: std::collections::VecDeque::new(),
      count: 0,
      errors: 0,
      org_id: org.map(str::to_string),
    });
    // A route is served by one org; keep its label current.
    w.org_id = org.map(str::to_string);
    if w.durations.len() >= ENDPOINT_WINDOW_CAP {
      w.durations.pop_front();
    }
    w.durations.push_back(duration_ms);
    w.count += 1;
    if status >= 500 {
      w.errors += 1;
    }
  }
}

/// One one-minute status-class bucket of a route's traffic.
#[derive(Clone, Copy, Default, Serialize)]
pub(crate) struct RouteTrendBucket {
  /// Minute index (unix seconds / 60) this bucket covers.
  pub(crate) minute: u64,
  pub(crate) total: u32,
  pub(crate) s2xx: u32,
  pub(crate) s3xx: u32,
  pub(crate) s4xx: u32,
  pub(crate) s5xx: u32,
}

/// Rolling minute-bucketed status trend for one route (hostname).
pub(crate) struct RouteTrend {
  buckets: VecDeque<RouteTrendBucket>,
  /// Organization serving this route (`None` = master).
  pub(crate) org_id: Option<String>,
}

/// Minute buckets kept per route.
const ROUTE_TREND_MINUTES: usize = 60;
/// Distinct routes tracked (overflow is simply not trended).
const ROUTE_TREND_CAP: usize = 100;

impl RouteTrend {
  /// The last `minutes` buckets ending at `now_minute`, gaps zero-filled,
  /// chronological. Feeds the dashboard sparklines directly.
  pub(crate) fn series(&self, minutes: usize, now_minute: u64) -> Vec<RouteTrendBucket> {
    // Defensive: never let a large `minutes` underflow the start minute (a
    // debug panic / release wrap). The window can't extend before minute 0.
    let start = (now_minute + 1).saturating_sub(minutes as u64);
    (0..minutes)
      .map(|i| {
        let minute = start + i as u64;
        self
          .buckets
          .iter()
          .find(|b| b.minute == minute)
          .copied()
          .unwrap_or(RouteTrendBucket {
            minute,
            ..Default::default()
          })
      })
      .collect()
  }
}

/// Bucket width of the activity ring, in seconds.
///
/// Five rather than one: the dashboard's live chart derives per-second rates
/// from its own polls and covers a minute, which is the right tool for "is it
/// moving right now". This series answers the other question, "what did the
/// last quarter of an hour look like", and a second's resolution there is
/// noise the eye cannot use, at five times the memory.
pub(crate) const ACTIVITY_BUCKET_SECS: u64 = 5;

/// How many buckets are kept: 180 × 5 s = 15 minutes.
pub(crate) const ACTIVITY_BUCKETS: usize = 180;

/// The two coarse rings, and why the width grows with the span: a series is
/// readable at roughly sixty cells whatever it covers, and it costs what it
/// stores. A day at five-second resolution would be seventeen thousand points
/// drawn into a few hundred pixels, which is a wall, not a chart, and seventeen
/// thousand points per organization to hold it.
///
/// Two hours in two-minute slices: 60 buckets.
pub(crate) const ACTIVITY_COARSE_SECS: u64 = 120;
pub(crate) const ACTIVITY_COARSE_BUCKETS: usize = 60;
/// A day in quarter-hour slices: 96 buckets.
pub(crate) const ACTIVITY_DAILY_SECS: u64 = 900;
pub(crate) const ACTIVITY_DAILY_BUCKETS: usize = 96;

/// Which resolution of the activity series a caller wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityRange {
  /// 15 minutes in 5-second slices, in memory only.
  Quarter,
  /// 2 hours in 2-minute slices.
  TwoHours,
  /// 24 hours in 15-minute slices.
  Day,
}

impl ActivityRange {
  /// Parses the `range` query value. Anything unrecognized, including an
  /// absent one, is the quarter hour: that is what this endpoint returned
  /// before the parameter existed, so an old caller keeps its answer.
  pub(crate) fn parse(raw: Option<&str>) -> ActivityRange {
    match raw {
      Some("2h") => ActivityRange::TwoHours,
      Some("1d") => ActivityRange::Day,
      _ => ActivityRange::Quarter,
    }
  }

  pub(crate) fn width_secs(self) -> u64 {
    match self {
      ActivityRange::Quarter => ACTIVITY_BUCKET_SECS,
      ActivityRange::TwoHours => ACTIVITY_COARSE_SECS,
      ActivityRange::Day => ACTIVITY_DAILY_SECS,
    }
  }

  pub(crate) fn buckets(self) -> usize {
    match self {
      ActivityRange::Quarter => ACTIVITY_BUCKETS,
      ActivityRange::TwoHours => ACTIVITY_COARSE_BUCKETS,
      ActivityRange::Day => ACTIVITY_DAILY_BUCKETS,
    }
  }
}

/// Organizations tracked before new ones stop being admitted, the same guard
/// the route trends carry: this is in-memory per-request state, and a bound is
/// what keeps a tenant from growing it.
const ACTIVITY_ORG_CAP: usize = 100;

/// One five-second slice of served traffic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivityBucket {
  /// Unix seconds of the bucket's start, so a gap reads as a gap rather than
  /// as a shift.
  pub(crate) at: u64,
  /// Requests served in this slice, and how many of them failed.
  pub(crate) total: u32,
  pub(crate) failed: u32,
}

/// Recent request volume in fixed slices, per organization.
///
/// The dashboard's minute-long chart is built in the browser from successive
/// polls, so it starts empty on every reload and cannot look back further than
/// the tab has been open. This is the same shape kept by the server: it
/// survives a reload, it is the same for two people looking at once, and it
/// costs one increment per request on a lock the request already takes.
/// One resolution of the series: a fixed bucket width, a bounded ring per
/// organization, and nothing else. Three of these make up `Activity`.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ActivityRing {
  width_secs: u64,
  capacity: usize,
  /// Keyed by org id; `None` (master) is the empty string, so one map holds
  /// both without an Option key.
  by_org: HashMap<String, VecDeque<ActivityBucket>>,
}

impl ActivityRing {
  fn new(width_secs: u64, capacity: usize) -> ActivityRing {
    ActivityRing {
      width_secs,
      capacity,
      by_org: HashMap::new(),
    }
  }

  fn record(&mut self, key: &str, failed: bool, now: u64) {
    if !self.by_org.contains_key(key) && self.by_org.len() >= ACTIVITY_ORG_CAP {
      return;
    }
    let width = self.width_secs;
    let capacity = self.capacity;
    let buckets = self.by_org.entry(key.to_string()).or_default();
    let at = now - now % width;
    if buckets.back().map(|b| b.at) != Some(at) {
      if buckets.len() >= capacity {
        buckets.pop_front();
      }
      buckets.push_back(ActivityBucket {
        at,
        ..Default::default()
      });
    }
    let bucket = buckets.back_mut().expect("bucket just ensured");
    bucket.total += 1;
    if failed {
      bucket.failed += 1;
    }
  }

  fn series(&self, key: &str, count: usize, now: u64) -> Vec<ActivityBucket> {
    let latest = now - now % self.width_secs;
    let empty = VecDeque::new();
    let buckets = self.by_org.get(key).unwrap_or(&empty);
    (0..count)
      .rev()
      .map(|back| {
        let at = latest.saturating_sub(back as u64 * self.width_secs);
        buckets
          .iter()
          .find(|b| b.at == at)
          .copied()
          .unwrap_or(ActivityBucket {
            at,
            ..Default::default()
          })
      })
      .collect()
  }

  /// Drops what has aged out of the ring since it was written. Restoring a
  /// day-old file without this would draw yesterday's traffic as today's.
  fn forget_before(&mut self, cutoff: u64) {
    for buckets in self.by_org.values_mut() {
      buckets.retain(|b| b.at >= cutoff);
    }
    self.by_org.retain(|_, buckets| !buckets.is_empty());
  }

  /// Repairs a ring read back from disk: the file carries the widths it was
  /// written with, and a build that changed them must not fold the old
  /// buckets into the new geometry.
  fn matches(&self, width_secs: u64, capacity: usize) -> bool {
    self.width_secs == width_secs && self.capacity == capacity
  }
}

/// What is written to the store: the two coarse rings only.
///
/// The fine one is deliberately absent. Fifteen minutes of five-second
/// buckets is the view of "right now", and a restart is exactly the moment
/// when "right now" changed; restoring it would redraw the minutes before a
/// deploy as if the new process had served them.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct PersistedActivity {
  coarse: ActivityRing,
  daily: ActivityRing,
}

impl PersistedActivity {
  /// Drops every organization but master, for a dump that does not carry the
  /// organizations themselves: a ring keyed by an org the target does not
  /// have is an orphan, exactly as the other sections treat their rows.
  pub(crate) fn retain_master_only(&mut self) {
    for ring in [&mut self.coarse, &mut self.daily] {
      ring.by_org.retain(|org, _| org.is_empty());
    }
  }

  /// Organizations represented, for the dump's per-section count.
  pub(crate) fn len(&self) -> usize {
    self
      .coarse
      .by_org
      .keys()
      .chain(self.daily.by_org.keys())
      .collect::<std::collections::HashSet<_>>()
      .len()
  }
}

/// Recent request volume in fixed slices, per organization, at three
/// resolutions.
///
/// The dashboard's minute-long chart is built in the browser from successive
/// polls, so it starts empty on every reload and cannot look back further than
/// the tab has been open. This is the same shape kept by the server: it
/// survives a reload, it is the same for two people looking at once, and it
/// costs one increment per request on a lock the request already takes.
///
/// The two long ranges also survive a restart, which is not a nicety: a view
/// covering a day that empties on every deploy answers "what happened
/// overnight" with a shrug, and would be worse than not offering the range at
/// all. They are small enough to write whole (about 40 KB at the organization
/// cap) on the same flush the persistent stats already use.
pub(crate) struct Activity {
  fine: ActivityRing,
  coarse: ActivityRing,
  daily: ActivityRing,
  /// Absent in tests and wherever no data directory exists; then the long
  /// ranges simply behave like the fine one and start empty.
  conn: Option<rusqlite::Connection>,
  dirty: bool,
}

impl Default for Activity {
  fn default() -> Activity {
    Activity {
      fine: ActivityRing::new(ACTIVITY_BUCKET_SECS, ACTIVITY_BUCKETS),
      coarse: ActivityRing::new(ACTIVITY_COARSE_SECS, ACTIVITY_COARSE_BUCKETS),
      daily: ActivityRing::new(ACTIVITY_DAILY_SECS, ACTIVITY_DAILY_BUCKETS),
      conn: None,
      dirty: false,
    }
  }
}

impl Activity {
  fn key(org: Option<&str>) -> String {
    org.unwrap_or("").to_string()
  }

  /// Reads the coarse rings back from the store and keeps the connection for
  /// later flushes.
  pub(crate) fn load(data_dir: &str, now: u64) -> Activity {
    let conn = crate::store::open_db(data_dir);
    let persisted = conn
      .query_row("SELECT data FROM stats WHERE key = 'activity'", [], |row| {
        row.get::<_, String>(0)
      })
      .ok()
      .and_then(|raw| serde_json::from_str::<PersistedActivity>(&raw).ok())
      .unwrap_or_default();
    let mut activity = Activity {
      conn: Some(conn),
      ..Default::default()
    };
    if persisted
      .coarse
      .matches(ACTIVITY_COARSE_SECS, ACTIVITY_COARSE_BUCKETS)
    {
      activity.coarse = persisted.coarse;
      activity
        .coarse
        .forget_before(now.saturating_sub(ACTIVITY_COARSE_SECS * ACTIVITY_COARSE_BUCKETS as u64));
    }
    if persisted
      .daily
      .matches(ACTIVITY_DAILY_SECS, ACTIVITY_DAILY_BUCKETS)
    {
      activity.daily = persisted.daily;
      activity
        .daily
        .forget_before(now.saturating_sub(ACTIVITY_DAILY_SECS * ACTIVITY_DAILY_BUCKETS as u64));
    }
    activity
  }

  /// Records one served request into the current slice of every resolution.
  pub(crate) fn record(&mut self, org: Option<&str>, failed: bool, now: u64) {
    let key = Self::key(org);
    self.fine.record(&key, failed, now);
    self.coarse.record(&key, failed, now);
    self.daily.record(&key, failed, now);
    self.dirty = true;
  }

  /// The last `count` slices of one resolution for one organization, oldest
  /// first, with the silent ones filled in. A quiet minute is a real answer,
  /// and a series that simply omits it draws the traffic on either side as if
  /// it were adjacent.
  pub(crate) fn series(
    &self,
    org: Option<&str>,
    range: ActivityRange,
    now: u64,
  ) -> Vec<ActivityBucket> {
    let key = Self::key(org);
    let ring = match range {
      ActivityRange::Quarter => &self.fine,
      ActivityRange::TwoHours => &self.coarse,
      ActivityRange::Day => &self.daily,
    };
    ring.series(&key, range.buckets(), now)
  }

  /// The coarse rings, for a dump. The fine one is not included for the same
  /// reason it is not persisted: fifteen minutes of five-second slices is the
  /// view of *right now*, and "now" is not a thing a dump can carry.
  pub(crate) fn export(&self) -> PersistedActivity {
    PersistedActivity {
      coarse: ActivityRing {
        width_secs: self.coarse.width_secs,
        capacity: self.coarse.capacity,
        by_org: self.coarse.by_org.clone(),
      },
      daily: ActivityRing {
        width_secs: self.daily.width_secs,
        capacity: self.daily.capacity,
        by_org: self.daily.by_org.clone(),
      },
    }
  }

  /// Replaces the coarse rings from a dump, dropping what has aged out and
  /// refusing a ring whose geometry this build no longer uses, exactly as
  /// reading them back from the store does. Returns the organizations taken.
  pub(crate) fn import(&mut self, dump: PersistedActivity, now: u64) -> usize {
    let mut taken = 0;
    if dump
      .coarse
      .matches(ACTIVITY_COARSE_SECS, ACTIVITY_COARSE_BUCKETS)
    {
      self.coarse = dump.coarse;
      self
        .coarse
        .forget_before(now.saturating_sub(ACTIVITY_COARSE_SECS * ACTIVITY_COARSE_BUCKETS as u64));
      taken += self.coarse.by_org.len();
    }
    if dump
      .daily
      .matches(ACTIVITY_DAILY_SECS, ACTIVITY_DAILY_BUCKETS)
    {
      self.daily = dump.daily;
      self
        .daily
        .forget_before(now.saturating_sub(ACTIVITY_DAILY_SECS * ACTIVITY_DAILY_BUCKETS as u64));
      taken = taken.max(self.daily.by_org.len());
    }
    self.dirty = true;
    taken
  }

  /// Writes the coarse rings to the store, if anything was recorded since the
  /// last write. Called on the same periodic flush and shutdown path as the
  /// persistent stats.
  pub(crate) fn save_if_dirty(&mut self) {
    if !self.dirty {
      return;
    }
    let Some(conn) = self.conn.as_ref() else {
      self.dirty = false;
      return;
    };
    let persisted = PersistedActivity {
      coarse: std::mem::take(&mut self.coarse),
      daily: std::mem::take(&mut self.daily),
    };
    match serde_json::to_string(&persisted) {
      Ok(json) => {
        match conn.execute(
          "INSERT INTO stats (key, data) VALUES ('activity', ?1)
           ON CONFLICT(key) DO UPDATE SET data = excluded.data",
          rusqlite::params![json],
        ) {
          Ok(_) => self.dirty = false,
          Err(e) => error!("Failed to persist the activity rings to the store: {}", e),
        }
      }
      Err(e) => error!("Failed to serialize the activity rings: {}", e),
    }
    self.coarse = persisted.coarse;
    self.daily = persisted.daily;
  }
}

/// All routes' status trends, keyed by request hostname (or `*`).
#[derive(Default)]
pub(crate) struct RouteTrends {
  pub(crate) routes: HashMap<String, RouteTrend>,
}

impl RouteTrends {
  /// Records one served request into its route's current minute bucket.
  pub(crate) fn record(&mut self, host: Option<&str>, status: u16, org: Option<&str>, now: u64) {
    let key = host.unwrap_or("*").to_string();
    if !self.routes.contains_key(&key) && self.routes.len() >= ROUTE_TREND_CAP {
      return;
    }
    let trend = self.routes.entry(key).or_insert_with(|| RouteTrend {
      buckets: VecDeque::new(),
      org_id: org.map(str::to_string),
    });
    trend.org_id = org.map(str::to_string);
    let minute = now / 60;
    if trend.buckets.back().map(|b| b.minute) != Some(minute) {
      if trend.buckets.len() >= ROUTE_TREND_MINUTES {
        trend.buckets.pop_front();
      }
      trend.buckets.push_back(RouteTrendBucket {
        minute,
        ..Default::default()
      });
    }
    let b = trend.buckets.back_mut().expect("bucket just ensured");
    b.total += 1;
    match status {
      200..=299 => b.s2xx += 1,
      300..=399 => b.s3xx += 1,
      400..=499 => b.s4xx += 1,
      _ => b.s5xx += 1,
    }
  }
}

/// Handle tracking active WebSocket sender channel and metadata.
pub(crate) struct ClientHandle {
  /// Sender channel to push messages to the client.
  pub(crate) tx: mpsc::Sender<Message>,
  /// Notified to force this connection's read loop to end (e.g. when the token
  /// it connected with is revoked), so the client leaves the routing pool at
  /// once instead of serving until it next reconnects.
  pub(crate) disconnect: Arc<Notify>,
  /// Instant when client connection was established.
  pub(crate) connected_at: Instant,
  /// Client remote IP address.
  pub(crate) client_ip: String,
  /// Total request count processed by this specific client connection.
  pub(crate) request_count: Arc<AtomicU64>,
  /// Path prefix the client declared via Ping (from APERIO_PATH),
  /// validated against the token permissions.
  pub(crate) declared_path: Option<String>,
  /// Path bind granted by the token permissions when the client declared none.
  pub(crate) assigned_path: Option<String>,
  /// Hostname the client declared via Ping (from APERIO_HOSTNAME),
  /// validated against the token permissions.
  pub(crate) declared_hostname: Option<String>,
  /// Additional hostnames the client declared beyond `declared_hostname`
  /// (multi-hostname services), each already validated against the token.
  pub(crate) declared_hostnames: Vec<String>,
  /// Hostnames granted automatically: token-bound hostnames and/or the
  /// randomly assigned subdomain.
  pub(crate) assigned_hostnames: Vec<String>,
  /// The randomly assigned hostname within `assigned_hostnames`, tracked
  /// separately so a runtime pattern change can swap it in place.
  pub(crate) random_hostname: Option<String>,
  /// Temporary path bind override set from the dashboard. Not persisted:
  /// lost when the client reconnects or the server restarts.
  pub(crate) override_path_bind: Option<String>,
  /// Temporary hostname binds set from the dashboard, replacing every declared
  /// and assigned name while set. A list rather than a single name so an
  /// operator can retarget the hostname the client declared without dropping
  /// the random subdomain the server handed it (or the other way round). Not
  /// persisted: lost when the client reconnects or the server restarts.
  pub(crate) override_hostname_binds: Vec<String>,
  /// Instant of the last heartbeat Ping received from this client.
  pub(crate) last_ping_at: Option<Instant>,
  /// Permissions attached to the token this client authenticated with.
  pub(crate) perms: ClientPerms,
  /// Announced concurrency limit of the client (from Ping), for display.
  pub(crate) max_concurrent: Option<u32>,
  /// Semaphore enforcing the client's announced concurrency limit. Requests
  /// beyond the limit wait here (bounded by the gateway timeout) instead of
  /// being dispatched, so the server never floods the client's backend.
  pub(crate) inflight_limiter: Option<Arc<Semaphore>>,
  /// True after the client announced a graceful shutdown: no new requests
  /// are routed to it while in-flight ones finish.
  pub(crate) draining: bool,
  /// Dashboard kill switch: false = temporarily excluded from routing even
  /// though the connection and heartbeats remain healthy.
  pub(crate) admin_enabled: bool,
  /// False when the service asked not to be recorded for the request
  /// inspector (`capture: false` in its aperio.yaml), announced via Ping.
  pub(crate) capture: bool,
  /// Parallel tunnel connections the client runs for this service
  /// (`connections:`), announced via Ping. Display-only: the server treats
  /// each connection as its own client regardless.
  pub(crate) connections: Option<u32>,
  /// The pool's floor and ceiling when the client runs an elastic one
  /// (`connections: {min, max}`); both absent for a fixed `connections: N`.
  /// Without them a pool sitting at its floor is indistinguishable from a
  /// fixed pool of the same size, so the dashboard cannot say whether the
  /// count beside it is expected to move.
  pub(crate) connections_min: Option<u32>,
  pub(crate) connections_max: Option<u32>,
  /// The id the client calls this connection, `<base>-<service>` for the first
  /// of a service and `<base>-<service>-c<N>` for the rest. Not trusted for
  /// state changes (the server's own connection id is), but it is what names
  /// the *service* a connection belongs to, which is the unit the
  /// per-service connection ceiling is about.
  pub(crate) declared_client_id: Option<String>,
  /// Settings the client resolved to something other than its config asked
  /// for (a bandwidth budget divided across connections, a clamped connection
  /// count, …), announced via Ping. Display-only, surfaced in the dashboard's
  /// per-connection config view.
  pub(crate) config_notes: Vec<crate::protocol::ConfigNote>,
  /// Static Prometheus labels this client announced, already validated and
  /// capped (planned_features #53). Attached to its own metric series only.
  pub(crate) metrics_labels: Vec<(String, String)>,
  /// Seconds this client says it gives its own in-flight requests when asked
  /// to stand down. Advisory: it sizes `shutdown_drain: auto`, under the
  /// operator's cap, and is never trusted on its own.
  pub(crate) drain_secs: Option<u64>,
  /// True when the client announced a TCP target (experimental TCP tunneling).
  pub(crate) tcp_enabled: bool,
  /// Client build version announced via Ping (None until the first Ping,
  /// or for clients predating version reporting).
  pub(crate) client_version: Option<String>,
  /// Tunnel protocol version announced via Ping.
  pub(crate) client_protocol: Option<u32>,
  /// Latest backend health verdict reported by the client's own probe
  /// (APERIO_TARGET_HEALTH). False = excluded from routing while the
  /// tunnel connection itself stays up.
  pub(crate) backend_healthy: bool,
  /// False only while a configured health check has not completed its first
  /// probe (dashboard shows "checking" instead of "backend down").
  pub(crate) backend_probed: bool,
  /// What the client reports about itself (planned_features #37): CPU as a
  /// percentage of one core and resident memory of the client process, then
  /// round-trip time, jitter and reconnects of this tunnel connection. All
  /// `None` from a client that does not report them, and the process figures
  /// are `None` where they cannot be read without guessing.
  pub(crate) cpu_percent: Option<f64>,
  pub(crate) rss_bytes: Option<u64>,
  pub(crate) rtt_ms: Option<u64>,
  pub(crate) jitter_ms: Option<u64>,
  pub(crate) reconnects: Option<u32>,
  /// Announced load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: u32,
  /// Client-process instance ID self-reported via Ping. Unlike the
  /// server-assigned connection ID it survives reconnects of the same
  /// process, letting the failover `wait` mode recognize a returning client.
  pub(crate) reported_instance_id: Option<String>,
  /// Process-wide instance group id from the `x-aperio-instance` handshake
  /// header (the client's raw `client_id` base). Shared by every service and
  /// parallel connection of one client process; used to group connections in
  /// the dashboard and to share one random hostname across them. `None` for
  /// clients that do not send the header.
  pub(crate) instance_group: Option<String>,
  /// Topic filters this connection has subscribed to. Held per connection
  /// because that is what the client re-sends after a reconnect, and reduced
  /// to one delivery per client *process* at publish time.
  pub(crate) subscriptions: Vec<String>,
  /// Announced downstream link capacity in bytes/second (0 = unlimited).
  /// Shared with the connection's writer task, which paces outgoing frames.
  pub(crate) bandwidth_bps: Arc<AtomicU64>,
  /// Display name of the service this connection exposes (announced via
  /// Ping by multi-service clients).
  pub(crate) service_name: Option<String>,
  /// What that service is called on screen, when the client named one.
  pub(crate) service_custom_name: Option<String>,
  /// True when the client declared its service public AND its token permits
  /// publishing public services: the visitor auth gate is skipped for
  /// routes served exclusively by public clients.
  pub(crate) public: bool,
  /// Ensures the "public requested but not permitted" warning logs once.
  pub(crate) public_denied_warned: bool,
  /// Client-declared visitor login (`user:password`) for this service, honored
  /// only when the token may control the visitor gate. `None` = no override.
  pub(crate) visitor_auth: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach this client's service, declared via
  /// Ping (empty = everyone). Enforced against every proxied request routed
  /// here; invalid entries are dropped when the heartbeat is applied.
  pub(crate) allowed_ips: Vec<String>,
  /// Ensures the "visitor_auth requested but not permitted/invalid" warning
  /// logs once per connection.
  pub(crate) visitor_auth_denied_warned: bool,
  /// Ensures the "allowed_ips entry invalid" warning fires once per client
  /// connection, not on every heartbeat.
  pub(crate) allowed_ips_invalid_warned: bool,
  /// True once this connection was warned about a malformed `scaling:` block,
  /// so a heartbeat every few seconds cannot flood the log.
  pub(crate) scaling_invalid_warned: bool,
  /// Tunnels declared by the client via Ping (`tunnels:` list): normally
  /// unexposed local services a peer client may bind with `--bind-tunnels`
  /// (same token, explicit client id required).
  pub(crate) tunnels: Vec<crate::protocol::TunnelDecl>,
  /// The client opted its service into the server-side response cache
  /// (`cache: true` via Ping). Effective only with APERIO_CACHE on.
  pub(crate) cache: bool,
  /// Ensures the "cache requested but the server cache is disabled" warning
  /// logs once per connection, not on every heartbeat.
  pub(crate) cache_ignored_warned: bool,
  /// The client asked for serve-stale resilience: cached responses for its
  /// routes stay servable (marked) while no healthy client is connected.
  pub(crate) resilience: bool,
  /// Client-declared request body cap for this service, in bytes (via Ping).
  /// Enforced before dispatch with an early 413; never loosens the global
  /// APERIO_MAX_BODY_SIZE limit.
  pub(crate) max_request_body: Option<u64>,
  /// Client-declared per-service response timeout, in seconds (via Ping).
  /// Overrides the global gateway response timeout for this service's
  /// dispatches (None = use the global value).
  pub(crate) response_timeout: Option<u64>,
  /// The client asked to persist inbound POSTs to this service into the
  /// webhook inbox (`webhook_inbox: true` via Ping).
  pub(crate) webhook_inbox: bool,
  /// Redirect URL for visitors this candidate's `allowed_ips` rejects
  /// (`denied:` via Ping). Used only when every candidate of a route rejects
  /// the visitor; without one anywhere, the answer is stealth (identical to
  /// an unclaimed route).
  pub(crate) denied: Option<String>,
  /// Passive outlier ejection: timestamps of recent dispatch failures
  /// (5xx / response timeout / connection loss) still inside the outlier
  /// window. Independent of the active `/health` probe (`backend_healthy`).
  pub(crate) recent_failures: VecDeque<Instant>,
  /// Instant until which this client is ejected from routing after crossing
  /// the failure threshold (None = not ejected). Re-admitted automatically.
  pub(crate) ejected_until: Option<Instant>,
}

/// Permissions resolved at connection time from the presented token.
#[derive(Clone)]
pub(crate) struct ClientPerms {
  /// True for the master `APERIO_SERVER_TOKEN`: no restrictions.
  pub(crate) master: bool,
  /// Allowed hostname binds. Empty or containing "*" = unrestricted.
  pub(crate) hostnames: Vec<String>,
  /// Allowed path binds. Empty or containing "*" = unrestricted.
  pub(crate) paths: Vec<String>,
  /// Name of the dynamic token used (None for the master token).
  pub(crate) token_name: Option<String>,
  /// Record ID of the dynamic token used (None for the master token);
  /// rate limits and quotas key on this.
  pub(crate) token_id: Option<String>,
  /// May this token publish services as public (visitor auth gate skipped)?
  pub(crate) allow_public: bool,
  /// May this token bind another client's tunnels within its organization?
  pub(crate) allow_bind: bool,
  /// May this token send OpenTelemetry exports through the OTel bridge?
  pub(crate) allow_otel: bool,
  /// Topic filters this token may publish to and subscribe to. Empty = no
  /// messaging at all, which is the default for a token that never asked for
  /// it: a capability that switches itself on for everything predating it is
  /// how a permission model stops meaning anything.
  pub(crate) topics: Vec<String>,
  /// Organization this token (and therefore this client) belongs to
  /// (None = master).
  pub(crate) org_id: Option<String>,
  /// Hostname allowlist of that organization, resolved once at connection
  /// time (empty = unrestricted). It fences every bind this client can claim,
  /// *outside* of what its token permits: a token minted before the org was
  /// fenced, or one carrying `*`, still cannot bind beyond the org's own
  /// hostnames.
  pub(crate) org_hostnames: Vec<String>,
  /// Parallel connections per service this token permits. `None` = whatever
  /// the server allows. It can only lower the server's ceiling: the effective
  /// number is the smaller of the two, resolved by `connection_ceiling`.
  pub(crate) max_connections: Option<u32>,
}

impl ClientPerms {
  pub(crate) fn master() -> Self {
    ClientPerms {
      master: true,
      hostnames: Vec::new(),
      paths: Vec::new(),
      token_name: None,
      token_id: None,
      allow_public: true,
      allow_bind: true,
      allow_otel: true,
      topics: vec!["#".to_string()],
      org_id: None,
      org_hostnames: Vec::new(),
      max_connections: None,
    }
  }

  /// The ceiling in force for this connection: the server's setting, lowered
  /// by the token's own if it asked for less. A token asking for more is not
  /// an error, it simply does not get it, which is what makes the server's
  /// number a policy rather than a suggestion.
  pub(crate) fn connection_ceiling(&self, server_max: u32) -> u32 {
    match self.max_connections {
      Some(token_max) => token_max.min(server_max).max(1),
      None => server_max.max(1),
    }
  }

  /// True when the organization's allowlist admits `host`. The master token
  /// and the master organization are never fenced.
  pub(crate) fn org_hostname_allowed(&self, host: &str) -> bool {
    self.master || crate::store::orgs::hostname_in_org_allowlist(host, &self.org_hostnames)
  }

  pub(crate) fn hostname_allowed(&self, host: &str) -> bool {
    // Both fences must admit the bind: the organization's allowlist first
    // (which a token can never widen), then the token's own permissions.
    self.org_hostname_allowed(host)
      && (self.master
        || self.hostnames.is_empty()
        || self.hostnames.iter().any(|h| h == "*" || h == host))
  }

  pub(crate) fn path_allowed(&self, path: &str) -> bool {
    self.master || self.paths.is_empty() || self.paths.iter().any(|p| p == "*" || p == path)
  }

  /// Specific (non-wildcard) hostnames granted by the token; these are
  /// auto-bound to the client on connect. Filtered by the organization's
  /// allowlist, so a token minted before the org was fenced cannot auto-bind
  /// a hostname the org may no longer claim.
  pub(crate) fn granted_hostnames(&self) -> Vec<String> {
    self
      .hostnames
      .iter()
      .filter(|h| *h != "*" && self.org_hostname_allowed(h))
      .cloned()
      .collect()
  }

  /// First specific path granted by the token, used as the automatic path
  /// bind when the client did not declare one.
  pub(crate) fn granted_path(&self) -> Option<String> {
    self.paths.iter().find(|p| *p != "*").cloned()
  }
}

impl ClientHandle {
  /// True while this client is passively ejected from routing.
  pub(crate) fn is_ejected(&self, now: Instant) -> bool {
    self.ejected_until.is_some_and(|t| now < t)
  }

  /// Records one dispatch failure (5xx / timeout / connection loss). Prunes
  /// the failure window, then ejects the client for `eject_for` once
  /// `threshold` failures land inside `window`. Returns true when this call
  /// caused the ejection.
  pub(crate) fn record_failure(
    &mut self,
    now: Instant,
    window: Duration,
    threshold: u32,
    eject_for: Duration,
  ) -> bool {
    while self
      .recent_failures
      .front()
      .is_some_and(|t| now.duration_since(*t) > window)
    {
      self.recent_failures.pop_front();
    }
    self.recent_failures.push_back(now);
    if !self.is_ejected(now) && self.recent_failures.len() as u32 >= threshold {
      self.ejected_until = Some(now + eject_for);
      self.recent_failures.clear();
      return true;
    }
    false
  }

  /// Path bind used for routing: dashboard override wins over the declared
  /// value, which wins over the token-granted value.
  pub(crate) fn effective_path_bind(&self) -> Option<&String> {
    self
      .override_path_bind
      .as_ref()
      .or(self.declared_path.as_ref())
      .or(self.assigned_path.as_ref())
  }

  /// Hostnames used for routing. A dashboard override replaces the whole
  /// set; otherwise the union of assigned and declared hostnames applies.
  pub(crate) fn effective_hostnames(&self) -> Vec<&String> {
    if !self.override_hostname_binds.is_empty() {
      return self.override_hostname_binds.iter().collect();
    }
    let mut set: Vec<&String> = self.assigned_hostnames.iter().collect();
    if let Some(d) = &self.declared_hostname
      && !set.contains(&d)
    {
      set.push(d);
    }
    for d in &self.declared_hostnames {
      if !set.contains(&d) {
        set.push(d);
      }
    }
    set
  }

  /// What this client calls itself, if it says anything.
  ///
  /// The order the clients table uses, and every other place a client is
  /// shown to a person: the `custom_name` an operator gave the service, else
  /// the `name` of its `services:` entry. `None` leaves only the id.
  pub(crate) fn display_name(&self) -> Option<String> {
    self
      .service_custom_name
      .clone()
      .or_else(|| self.service_name.clone())
  }

  pub(crate) fn matches_host(&self, host: &str) -> bool {
    self
      .effective_hostnames()
      .iter()
      .any(|h| h.as_str() == host)
  }

  pub(crate) fn has_hostname_bind(&self) -> bool {
    !self.effective_hostnames().is_empty()
  }

  /// A client is healthy while its last heartbeat (or, before the first
  /// Ping, its connection time) is within the down threshold.
  pub(crate) fn is_healthy(&self, down_threshold: Duration) -> bool {
    let reference = self.last_ping_at.unwrap_or(self.connected_at);
    reference.elapsed() < down_threshold
  }
}

/// Round-robin group key: (hostname group, path group) of the selected pool.
pub(crate) type RouteGroupKey = (Option<String>, Option<String>);

/// A pending OIDC login: (post-login redirect, bound org id, the callback URL
/// sent to the provider, expiry).
///
/// The callback URL is remembered rather than re-derived: the token exchange
/// has to present the *same* `redirect_uri` the authorization request did, and
/// re-deriving it from the callback request's `Host` would let that header
/// decide what the exchange claims.
pub(crate) type OidcStateEntry = (String, Option<String>, String, Instant);

/// One frame of a streamed response body relayed from the tunnel: data
/// chunks, then optionally one trailer block (e.g. gRPC's `grpc-status`).
/// `Bytes` rather than `Vec<u8>` for the same reason as
/// `TunnelResponse::body_raw`: the WebSocket message the chunk arrived in is
/// refcounted, so the frame is a slice of it rather than a copy of it.
pub(crate) enum BodyFrame {
  Data(axum::body::Bytes),
  Trailers(Vec<(String, String)>),
}

/// Standard response payload returned by tunnel client.
pub(crate) struct TunnelResponse {
  /// HTTP status code.
  pub(crate) status: u16,
  /// List of response headers (preserves duplicates like Set-Cookie).
  pub(crate) headers: Vec<(String, String)>,
  /// Base64 encoded payload body (buffered responses only, peers before v5).
  pub(crate) body: Option<String>,
  /// The same body as bytes, from a v5 full-response frame. When this is set
  /// `body` is not: the point of the frame is that the body never becomes a
  /// base64 string on either side. `Bytes` rather than `Vec<u8>` because the
  /// WebSocket message already owns them refcounted, so this is a slice of
  /// what arrived rather than a copy of it.
  pub(crate) body_raw: Option<axum::body::Bytes>,
  /// HTTP trailers of a buffered response (e.g. `grpc-status` for gRPC).
  pub(crate) trailers: Option<Vec<(String, String)>>,
  /// For streamed responses: receiver of decoded body frames. The proxy
  /// handler turns this into a streaming HTTP body.
  pub(crate) stream_rx: Option<mpsc::Receiver<Result<BodyFrame, std::io::Error>>>,
  /// Client-side stage durations, from a buffered response or from the head
  /// of a streamed one (timing-aware clients; the streamed head reports every
  /// stage except the end of the backend body, which has not happened yet).
  pub(crate) timings: Option<crate::protocol::ClientTimings>,
}

/// High-resolution timeline of one proxied request: microsecond offsets from
/// t0 = the server first receiving the request. Client-side stages are
/// measured on the client's own monotonic clock and anchored here by
/// splitting the unaccounted tunnel transit evenly between the two
/// directions, clocks are never mixed, and the estimate is flagged.
#[derive(Serialize, Clone, Copy)]
pub(crate) struct RequestTimeline {
  /// A connected client was available (end of any wait-for-client). Measured;
  /// `None` when not captured. Sub-boundary of the pre-dispatch phase.
  pub(crate) client_ready_us: Option<u64>,
  /// Admitted past the server-wide concurrency limit. Measured.
  pub(crate) admitted_us: Option<u64>,
  /// A serving client was selected (routing done). Measured.
  pub(crate) selected_us: Option<u64>,
  /// The request left the server into the tunnel (queueing, routing, and
  /// admission all happen before this).
  pub(crate) dispatched_us: u64,
  /// Estimated: the client received the request.
  pub(crate) client_received_us: Option<u64>,
  /// Estimated anchor + measured client offset: backend request sent.
  pub(crate) backend_sent_us: Option<u64>,
  /// ... backend response headers arrived at the client.
  pub(crate) backend_first_byte_us: Option<u64>,
  /// ... backend body fully read by the client.
  pub(crate) backend_done_us: Option<u64>,
  /// ... the client handed the response to the tunnel.
  pub(crate) client_responded_us: Option<u64>,
  /// The server received the response from the tunnel (measured).
  pub(crate) response_received_us: u64,
  /// The response was handed to the visitor connection (measured).
  pub(crate) finished_us: u64,
  /// True when the client stages above are anchored estimates.
  pub(crate) estimated_anchor: bool,
}

impl RequestTimeline {
  /// Assembles the timeline from the server's own measurements and the
  /// client-reported stage durations (when present).
  pub(crate) fn assemble(
    dispatched_us: u64,
    response_received_us: u64,
    finished_us: u64,
    client: Option<crate::protocol::ClientTimings>,
  ) -> RequestTimeline {
    let anchored = client.map(|c| {
      // Whatever part of dispatch->response the client did not spend
      // processing is tunnel transit; split it evenly per direction.
      let round_trip = response_received_us.saturating_sub(dispatched_us);
      let transit = round_trip.saturating_sub(c.respond_us);
      let anchor = dispatched_us + transit / 2;
      (
        anchor,
        anchor + c.backend_sent_us,
        anchor + c.backend_first_byte_us,
        // Absent on the head of a streamed response, where the body has not
        // finished arriving at the client. Every other stage is measured the
        // same way it is for a buffered one, so the hole is exactly one row
        // wide rather than the whole waterfall.
        c.backend_done_us.map(|us| anchor + us),
        anchor + c.respond_us,
      )
    });
    RequestTimeline {
      client_ready_us: None,
      admitted_us: None,
      selected_us: None,
      dispatched_us,
      client_received_us: anchored.map(|a| a.0),
      backend_sent_us: anchored.map(|a| a.1),
      backend_first_byte_us: anchored.map(|a| a.2),
      backend_done_us: anchored.and_then(|a| a.3),
      client_responded_us: anchored.map(|a| a.4),
      response_received_us,
      finished_us,
      estimated_anchor: anchored.is_some(),
    }
  }
}

/// Default backlog at which the producer of a pumped stream is asked to
/// pause (`StreamPause`, `APERIO_STREAM_PAUSE_BYTES`): enough to ride out
/// short consumer hiccups without ever involving the client, small enough
/// that a slow visitor costs little memory.
pub(crate) const STREAM_PAUSE_BYTES: usize = 2 * 1024 * 1024;
/// Default backlog below which a paused producer is asked to resume
/// (`APERIO_STREAM_RESUME_BYTES`). Well under the pause mark so the pair
/// does not flap on every forwarded chunk.
pub(crate) const STREAM_RESUME_BYTES: usize = 512 * 1024;
/// Default hard per-stream backlog cap (`APERIO_STREAM_BACKLOG_LIMIT`): the
/// stream is dropped beyond it. Only reachable when the producer cannot be
/// paused (a pre-v3 client) or ignores the pause; a pausing client stops
/// with at most its frames already on the wire outstanding, far below the
/// gap between this and the pause mark.
pub(crate) const STREAM_BACKLOG_LIMIT: usize = 16 * 1024 * 1024;

/// The flow-control watermarks one pumped stream runs with, snapshotted from
/// the live config when the stream starts (a later settings change applies
/// to new streams only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StreamLimits {
  /// Backlog bytes at which the producer is asked to pause.
  pub(crate) pause_bytes: usize,
  /// Backlog bytes under which a paused producer is asked to resume.
  pub(crate) resume_bytes: usize,
  /// Hard per-stream backlog cap in bytes.
  pub(crate) backlog_limit: usize,
  /// Bytes per second a consumer must take **while data is waiting for it**,
  /// or the stream is ended (`0` = no floor). See [`MIN_THROUGHPUT_WINDOW`].
  pub(crate) min_throughput: u64,
}

impl Default for StreamLimits {
  fn default() -> Self {
    StreamLimits {
      pause_bytes: STREAM_PAUSE_BYTES,
      resume_bytes: STREAM_RESUME_BYTES,
      backlog_limit: STREAM_BACKLOG_LIMIT,
      min_throughput: 0,
    }
  }
}

impl StreamLimits {
  /// Repairs an inconsistent trio so the mechanism cannot be configured into
  /// nonsense: the resume mark must sit below the pause mark (else pausing
  /// and resuming race on the same chunk) and the hard cap must sit above it
  /// (else every stream is cut before it can be paused). Out-of-order values
  /// are pulled back relative to `pause_bytes`, which is taken as the
  /// operator's intent.
  pub(crate) fn sanitized(
    pause_bytes: usize,
    resume_bytes: usize,
    backlog_limit: usize,
    min_throughput: u64,
  ) -> Self {
    let pause_bytes = pause_bytes.max(64 * 1024);
    let resume_bytes = if resume_bytes >= pause_bytes {
      pause_bytes / 4
    } else {
      resume_bytes
    };
    let backlog_limit = backlog_limit.max(pause_bytes.saturating_mul(2));
    StreamLimits {
      pause_bytes,
      resume_bytes,
      backlog_limit,
      min_throughput,
    }
  }
}

/// What one queued item costs against a pumped stream's byte backlog.
pub(crate) trait PumpCost {
  fn cost(&self) -> usize;
}
impl PumpCost for Result<BodyFrame, std::io::Error> {
  fn cost(&self) -> usize {
    match self {
      Ok(BodyFrame::Data(bytes)) => bytes.len(),
      _ => 0,
    }
  }
}
impl PumpCost for TcpConsumerMsg {
  fn cost(&self) -> usize {
    match self {
      TcpConsumerMsg::Data(bytes) => bytes.len(),
      TcpConsumerMsg::Close => 0,
    }
  }
}
impl PumpCost for WsStreamMessage {
  fn cost(&self) -> usize {
    match self {
      WsStreamMessage::Data(Message::Text(t)) => t.len(),
      WsStreamMessage::Data(Message::Binary(b)) => b.len(),
      _ => 0,
    }
  }
}

/// Producer-side flow control of one pumped stream: tracks the bytes queued
/// between the tunnel read loop and the consumer, and asks the producing
/// client to pause/resume around the watermarks (protocol v3). For older
/// clients only the hard backlog cap applies.
pub(crate) struct StreamFlow {
  /// The stream's id on the wire (request id or stream id).
  stream_id: String,
  /// The producing client's tunnel writer, for pause/resume messages.
  client_tx: mpsc::Sender<Message>,
  /// Whether the client announced protocol v3+ when the stream started.
  supports_pause: bool,
  /// The watermarks this stream runs with.
  limits: StreamLimits,
  /// Bytes enqueued but not yet forwarded to the consumer.
  backlog: std::sync::atomic::AtomicUsize,
  /// True while a `StreamPause` is outstanding.
  paused: AtomicBool,
}

impl StreamFlow {
  /// The floor this stream's consumer is held to, in bytes per second.
  pub(crate) fn min_throughput(&self) -> u64 {
    self.limits.min_throughput
  }

  pub(crate) fn stream_id(&self) -> &str {
    &self.stream_id
  }

  pub(crate) fn new(
    stream_id: String,
    client_tx: mpsc::Sender<Message>,
    supports_pause: bool,
    limits: StreamLimits,
  ) -> Self {
    StreamFlow {
      stream_id,
      client_tx,
      supports_pause,
      limits,
      backlog: std::sync::atomic::AtomicUsize::new(0),
      paused: AtomicBool::new(false),
    }
  }

  /// Sends `msg` on the client's tunnel without blocking; used from contexts
  /// (read loop, Drop) that must never wait on the writer.
  fn try_notify(&self, msg: &TunnelMessage) -> bool {
    match serde_json::to_string(msg) {
      Ok(json) => self.client_tx.try_send(Message::Text(json.into())).is_ok(),
      Err(_) => false,
    }
  }

  /// Accounts newly enqueued bytes and pauses the producer past the high
  /// watermark. Called on the tunnel read loop, so it never blocks.
  fn on_enqueued(&self, cost: usize) {
    let backlog = self.backlog.fetch_add(cost, Ordering::SeqCst) + cost;
    if self.supports_pause
      && backlog >= self.limits.pause_bytes
      && !self.paused.swap(true, Ordering::SeqCst)
    {
      // A full writer channel just means the pause is retried on the next
      // chunk; the flag is put back so that retry happens.
      let sent = self.try_notify(&TunnelMessage::StreamPause {
        id: self.stream_id.clone(),
      });
      if !sent {
        self.paused.store(false, Ordering::SeqCst);
      }
    }
  }

  /// Accounts bytes handed to the consumer and resumes a paused producer
  /// once the backlog has drained below the low watermark.
  fn on_forwarded(&self, cost: usize) {
    let backlog = self.backlog.fetch_sub(cost, Ordering::SeqCst) - cost;
    if backlog <= self.limits.resume_bytes
      && self.paused.load(Ordering::SeqCst)
      && self.try_notify(&TunnelMessage::StreamResume {
        id: self.stream_id.clone(),
      })
    {
      self.paused.store(false, Ordering::SeqCst);
    }
  }

  fn over_limit(&self, cost: usize) -> bool {
    self.backlog.load(Ordering::SeqCst) + cost > self.limits.backlog_limit
  }
}

impl Drop for StreamFlow {
  fn drop(&mut self) {
    // A stream torn down while its producer is paused must not leave that
    // producer waiting forever (the client also has its own resume-timeout
    // safety net for the case where this best-effort send is lost).
    if self.paused.load(Ordering::SeqCst) {
      let _ = self.try_notify(&TunnelMessage::StreamResume {
        id: self.stream_id.clone(),
      });
    }
  }
}

/// Why a pumped stream refused a chunk; both end the stream.
#[derive(Debug, PartialEq)]
pub(crate) enum PumpPushError {
  /// The pump ended: the consumer vanished or stalled beyond the timeout.
  ConsumerGone,
  /// The backlog cap was hit: the producer cannot be paused (or ignored it).
  BacklogFull,
}

/// The read loop's handle to a pumped stream: a non-blocking enqueue with
/// byte accounting and flow control.
pub(crate) struct PumpedSender<T> {
  feed: mpsc::UnboundedSender<T>,
  flow: Arc<StreamFlow>,
}

impl<T> Clone for PumpedSender<T> {
  fn clone(&self) -> Self {
    PumpedSender {
      feed: self.feed.clone(),
      flow: self.flow.clone(),
    }
  }
}

impl<T: PumpCost> PumpedSender<T> {
  /// Enqueues one item without ever blocking the tunnel read loop.
  pub(crate) fn push(&self, item: T) -> Result<(), PumpPushError> {
    let cost = item.cost();
    if self.flow.over_limit(cost) {
      return Err(PumpPushError::BacklogFull);
    }
    self
      .feed
      .send(item)
      .map_err(|_| PumpPushError::ConsumerGone)?;
    self.flow.on_enqueued(cost);
    Ok(())
  }
}

#[cfg(test)]
impl StreamFlow {
  /// A flow handle detached from any tunnel client, for tests: the dummy
  /// writer channel means a pause can never be delivered, so only the
  /// backlog cap and the stall timeout apply.
  pub(crate) fn detached(stream_id: &str) -> Self {
    Self::new(
      stream_id.to_string(),
      mpsc::channel(1).0,
      false,
      StreamLimits::default(),
    )
  }
}

/// Test convenience: a pumped stream with a detached flow and a long stall
/// timeout, for tests that only need the sender type.
#[cfg(test)]
pub(crate) fn test_pump<T: PumpCost + Send + 'static>(out: mpsc::Sender<T>) -> PumpedSender<T> {
  spawn_consumer_pump(out, Duration::from_secs(30), StreamFlow::detached("test"))
}

/// Puts a forwarding task between the tunnel read loop and one public
/// consumer, and returns the sender the read loop should hold.
///
/// A tunnel connection has a single read loop, shared by every request,
/// upgrade and raw stream on it, so it must never wait on one consumer. A
/// visitor that stops reading its download fills that stream's channel, and
/// waiting there stalls the other visitors' responses, the TCP data and even
/// the Ping handling of that whole tunnel; a stall outlasting
/// `client_down_threshold` drops the client from routing and 504s visitors
/// with nothing to do with it. The read loop therefore only ever pushes
/// into the returned queue, and this task does the waiting on its behalf,
/// giving up on a consumer stalled beyond `stall_timeout`.
///
/// Backpressure reaches the producer instead of piling up here: past
/// `STREAM_PAUSE_BYTES` of backlog the producing client is asked to stop
/// reading the stream's source until the backlog drains (protocol v3). A
/// producer that cannot be paused is cut off at `STREAM_BACKLOG_LIMIT`,
/// and a consumer that accepts nothing for `stall_timeout` ends the stream
/// exactly as the old blocking send's timeout did.
/// How long a consumer is measured over before the throughput floor applies.
///
/// Long enough that a browser pausing a video, or a phone changing network,
/// is not read as an attack; short enough that a stream held open on purpose
/// does not survive a shift.
pub(crate) const MIN_THROUGHPUT_WINDOW: Duration = Duration::from_secs(30);

/// Tracks whether a consumer is taking data fast enough to keep its stream.
///
/// ## What the existing defense already covers, and what it does not
///
/// The pump gives up when a single item cannot be handed over within the
/// gateway timeout, so a consumer that reads *nothing* is already ended. The
/// hole is the one in between: a reader that accepts one chunk every twenty
/// nine seconds resets that timeout forever and holds the stream, the
/// client-side `max_concurrent` slot behind it, and megabytes of server-side
/// buffer, for as long as it likes. Flow control is what made this possible,
/// by making the server well-behaved and therefore patient.
///
/// ## Why it only counts while data is waiting
///
/// A stream can be idle because the *backend* has nothing to send, which is
/// ordinary for server-sent events and long polling and is not the consumer's
/// fault. Measuring wall-clock throughput would end those. So the window only
/// accumulates while the pump has something to hand over: what is measured is
/// "data was ready and you did not take it", which is the actual accusation.
struct ThroughputGuard {
  floor: u64,
  window_started: Instant,
  bytes_this_window: u64,
  /// Time in this window during which the pump had an item to deliver.
  waiting: Duration,
}

impl ThroughputGuard {
  fn new(floor: u64) -> Self {
    ThroughputGuard {
      floor,
      window_started: Instant::now(),
      bytes_this_window: 0,
      waiting: Duration::ZERO,
    }
  }

  /// Records one delivery and says whether the stream should be ended.
  fn record(&mut self, bytes: u64, waited: Duration, now: Instant) -> bool {
    if self.floor == 0 {
      return false;
    }
    self.bytes_this_window += bytes;
    self.waiting += waited;
    if now.duration_since(self.window_started) < MIN_THROUGHPUT_WINDOW {
      return false;
    }
    // Judged against the time the consumer actually kept data waiting, not
    // against the window: a stream that was mostly idle because the backend
    // was quiet has a small denominator and passes on a small numerator.
    let owed = (self.floor as f64 * self.waiting.as_secs_f64()) as u64;
    let starved = self.bytes_this_window < owed;
    self.window_started = now;
    self.bytes_this_window = 0;
    self.waiting = Duration::ZERO;
    starved
  }
}

pub(crate) fn spawn_consumer_pump<T: PumpCost + Send + 'static>(
  out: mpsc::Sender<T>,
  stall_timeout: Duration,
  flow: StreamFlow,
) -> PumpedSender<T> {
  let flow = Arc::new(flow);
  let (feed_tx, mut feed_rx) = mpsc::unbounded_channel::<T>();
  let pump_flow = flow.clone();
  let floor = flow.min_throughput();
  let stream_id = flow.stream_id().to_string();
  tokio::spawn(async move {
    let mut guard = ThroughputGuard::new(floor);
    while let Some(item) = feed_rx.recv().await {
      let cost = item.cost();
      // A stalled or vanished consumer ends the pump; dropping `feed_rx`
      // closes the queue, so the read loop's next push reports the stream
      // as gone and removes it.
      let handing_over = Instant::now();
      if !matches!(
        tokio::time::timeout(stall_timeout, out.send(item)).await,
        Ok(Ok(()))
      ) {
        break;
      }
      pump_flow.on_forwarded(cost);
      // The time spent inside `send` above is time this item was ready and
      // the consumer had not taken it, which is exactly the denominator the
      // floor is judged against.
      let now = Instant::now();
      if guard.record(cost as u64, now.duration_since(handing_over), now) {
        tracing::warn!(
          "Stream {stream_id} ended: the consumer took less than {floor} bytes/second while data \
           was waiting for it"
        );
        break;
      }
    }
  });
  PumpedSender {
    feed: feed_tx,
    flow,
  }
}

/// Sender half of an in-flight streamed response body, kept so the tunnel
/// read loop can push chunks and so disconnect cleanup can drop it.
pub(crate) struct ResponseStreamHandle {
  pub(crate) tx: PumpedSender<Result<BodyFrame, std::io::Error>>,
  pub(crate) client_id: String,
}

/// Message relayed from the tunnel to a public TCP consumer WebSocket.
/// `Bytes` for the same zero-copy reason as `BodyFrame::Data`.
pub(crate) enum TcpConsumerMsg {
  Data(axum::body::Bytes),
  Close,
}

/// Handle to an active TCP tunnel stream (consumer side).
pub(crate) struct TcpStreamHandle {
  pub(crate) tx: PumpedSender<TcpConsumerMsg>,
  pub(crate) client_id: String,
}

/// Handle to an active UDP relay stream (consumer side). Unlike the pumped
/// TCP/WS/response streams, UDP is best-effort by contract: a congested
/// consumer drops datagrams instead of buffering or pausing the producer,
/// so the handle keeps a plain bounded sender.
pub(crate) struct UdpStreamHandle {
  pub(crate) tx: mpsc::Sender<TcpConsumerMsg>,
  pub(crate) client_id: String,
}

/// Structure tracking requests waiting for client execution.
pub(crate) struct PendingRequest {
  /// Oneshot channel sender to return client response to proxy handler thread.
  pub(crate) tx: oneshot::Sender<TunnelResponse>,
  /// Target client UUID.
  pub(crate) client_id: String,
}

/// Which of the two waiting-for-a-client maps a [`PendingGuard`] belongs to.
#[derive(Clone, Copy)]
pub(crate) enum PendingMap {
  Requests,
  Upgrades,
}

/// Removes a pending entry when the handler that registered it goes away.
///
/// A proxied request registers itself, dispatches, and awaits the answer. If
/// the visitor's connection drops, axum drops the handler future mid-await:
/// the timeout is dropped with it and nothing removes the entry. The only
/// sweep that ever reached it ran when the *serving client* disconnected, so
/// under a long-lived client the map grew with every visitor that hung up,
/// which is also an alert metric (`pending_requests`), so the leak reported
/// itself as load.
///
/// Drop cannot await, so it takes the fast path when it can: `try_lock`
/// succeeds in the ordinary case, and removing an id that the response path
/// already took is a lookup that finds nothing. Only genuine contention pays
/// for a task, and if there is no runtime left to spawn on the process is
/// ending anyway.
pub(crate) struct PendingGuard {
  state: Option<Arc<AppState>>,
  map: PendingMap,
  id: String,
}

impl PendingGuard {
  pub(crate) fn new(state: Arc<AppState>, map: PendingMap, id: String) -> PendingGuard {
    PendingGuard {
      state: Some(state),
      map,
      id,
    }
  }
}

impl Drop for PendingGuard {
  fn drop(&mut self) {
    let Some(state) = self.state.take() else {
      return;
    };
    let map = self.map;
    let id = std::mem::take(&mut self.id);
    fn slot(s: &AppState, map: PendingMap) -> &Mutex<HashMap<String, PendingRequest>> {
      match map {
        PendingMap::Requests => &s.pending_requests,
        PendingMap::Upgrades => &s.pending_upgrades,
      }
    }
    if let Ok(mut pending) = slot(&state, map).try_lock() {
      pending.remove(&id);
      return;
    }
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      handle.spawn(async move {
        slot(&state, map).lock().await.remove(&id);
      });
    }
  }
}

/// Registered relay for a proxied public WebSocket: the sender that pushes
/// tunnel frames to the public side, tagged with the serving client's id so a
/// `WsData`/`WsClose` frame can be verified to come from the owning client.
pub(crate) struct WsStreamHandle {
  pub(crate) tx: PumpedSender<WsStreamMessage>,
  pub(crate) client_id: String,
}

/// A WebSocket frame relayed from the tunnel client, to be forwarded to the public WS.
pub(crate) enum WsStreamMessage {
  /// A data frame (text or binary) to forward to the public WebSocket.
  Data(Message),
  /// Close the public WebSocket stream.
  Close,
}

/// Bucket tracking current tokens and refill state for rate limiting.
pub(crate) struct RateLimitState {
  /// Current token balance.
  pub(crate) tokens: f64,
  /// Last instant when tokens were updated.
  pub(crate) last_updated: Instant,
}

/// Size past which the per-token rate/quota maps are garbage-collected of
/// stale entries (idle or revoked tokens), so a churn of dynamic tokens
/// cannot grow them without bound. Mirrors the per-IP rate limiter's
/// failsafe threshold.
const TOKEN_MAP_GC_THRESHOLD: usize = 1000;

/// Token-rate map GC: once the map is large, drop buckets for tokens idle
/// past the refill window (revoked or unused) so churned dynamic tokens do
/// not accumulate forever. A fully-refilled idle bucket carries no state
/// worth keeping.
pub(crate) fn gc_token_rate(map: &mut HashMap<String, RateLimitState>, now: Instant) {
  if map.len() > TOKEN_MAP_GC_THRESHOLD {
    map.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
  }
}

/// Token daily-byte map GC: once the map is large, drop entries from a past
/// day (only the current day's usage feeds any quota).
pub(crate) fn gc_token_daily_bytes(map: &mut HashMap<String, (String, u64)>, today: &str) {
  if map.len() > TOKEN_MAP_GC_THRESHOLD {
    map.retain(|_, (day, _)| day == today);
  }
}

/// Upper bounds (seconds) of the request duration histogram buckets exposed
/// on `/aperio/metrics`; a `+Inf` bucket is added implicitly.
const DURATION_BUCKETS: [f64; 12] = [
  0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Lock-free in-memory histogram of proxied request durations, rendered as a
/// Prometheus `histogram` (cumulative buckets + sum + count). In-memory only:
/// counters reset on restart, which Prometheus handles natively.
#[derive(Default)]
pub(crate) struct DurationHistogram {
  pub(crate) buckets: [AtomicU64; DURATION_BUCKETS.len()],
  pub(crate) sum_micros: AtomicU64,
  pub(crate) count: AtomicU64,
}

impl DurationHistogram {
  pub(crate) fn observe(&self, duration: Duration) {
    let secs = duration.as_secs_f64();
    for (i, bound) in DURATION_BUCKETS.iter().enumerate() {
      if secs <= *bound {
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
      }
    }
    self
      .sum_micros
      .fetch_add(duration.as_micros() as u64, Ordering::Relaxed);
    self.count.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn render(&self, out: &mut String) {
    out.push_str(
      "# HELP aperio_request_duration_seconds Proxied request duration (dispatch to response).\n",
    );
    out.push_str("# TYPE aperio_request_duration_seconds histogram\n");
    for (i, bound) in DURATION_BUCKETS.iter().enumerate() {
      out.push_str(&format!(
        "aperio_request_duration_seconds_bucket{{le=\"{}\"}} {}\n",
        bound,
        self.buckets[i].load(Ordering::Relaxed)
      ));
    }
    let count = self.count.load(Ordering::Relaxed);
    out.push_str(&format!(
      "aperio_request_duration_seconds_bucket{{le=\"+Inf\"}} {}\n",
      count
    ));
    out.push_str(&format!(
      "aperio_request_duration_seconds_sum {}\n",
      self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
    ));
    out.push_str(&format!(
      "aperio_request_duration_seconds_count {}\n",
      count
    ));
  }
}

pub(crate) use crate::store::sessions::SessionInfo;

/// Connection liveness state, kept under a single lock for consistent snapshots.
pub(crate) struct ConnectionState {
  pub(crate) connected: bool,
  pub(crate) last_disconnect: Option<Instant>,
}

/// One maintenance flag: who owns it, why it is up, and when it ends.
///
/// It started as just the owning organization. The rest is what someone
/// asking "why is this site 503ing" needs and had to find in a chat log: a
/// reason, and an expiry, because the flag that causes an outage is the one
/// switched on for twenty minutes of work and left up.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct MaintenanceFlag {
  /// The organization that enabled it (`None` = master).
  pub(crate) org: Option<String>,
  /// Free text shown on the 503 page and in the dashboard. Empty = none.
  pub(crate) reason: Option<String>,
  /// Unix seconds after which the flag stops applying and is swept away.
  /// `None` = until someone turns it off.
  pub(crate) until: Option<u64>,
  /// Unix seconds when it was set, and who set it.
  pub(crate) since: u64,
  pub(crate) actor: String,
}

impl MaintenanceFlag {
  /// True once `until` has passed. An expired flag serves no request, and is
  /// removed on the next write rather than on a timer: a sweep that has to be
  /// scheduled is a sweep that can stop running.
  pub(crate) fn expired(&self, now: u64) -> bool {
    self.until.is_some_and(|end| now >= end)
  }

  /// Seconds until it lifts, for `Retry-After`. `None` when open-ended, where
  /// the response keeps its fixed fallback rather than promising a time.
  pub(crate) fn retry_after(&self, now: u64) -> Option<u64> {
    self.until.map(|end| end.saturating_sub(now).max(1))
  }
}

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
/// One served (or refused) request's bookkeeping, handed to the telemetry
/// collector task instead of being written under four global locks on the
/// request's own back.
pub(crate) struct TelemetryEvent {
  /// The dashboard log entry; its fields (host, uri, org) also feed the
  /// per-endpoint and per-route records.
  pub(crate) log: RequestLog,
  pub(crate) status: u16,
  pub(crate) duration_ms: u64,
  /// True for a served request (feeds the endpoint/trend records); false for
  /// a refusal, which only the activity ring counts, as a failure.
  pub(crate) success: bool,
  pub(crate) now_secs: u64,
}

pub(crate) struct AppState {
  /// Every live tunnel connection. An RwLock rather than a Mutex: routing,
  /// stats, pubsub fan-out and the maps read it on every request, and only
  /// connect/disconnect and the heartbeat's announcement processing mutate
  /// it. Under a Mutex those readers serialized on one queue with each other
  /// and with every heartbeat.
  pub(crate) clients: tokio::sync::RwLock<HashMap<String, ClientHandle>>,
  /// Client-to-client dependencies observed on the tunnel endpoints: who is
  /// dialing whose tunnel (planned_features #56). Behind its own Mutex rather
  /// than folded into `clients`: a consumer is not a registered client, and
  /// this is written on connect/disconnect and read only when somebody opens
  /// the topology view.
  pub(crate) consumers: tokio::sync::Mutex<crate::consumers::Consumers>,
  /// Queue into the telemetry collector task, which owns the writes to
  /// `endpoint_stats`/`route_trends`/`activity`/`recent_logs`. The request
  /// path try_sends one event; a full or absent queue falls back to writing
  /// inline, so nothing is ever lost, only contended for the old way.
  pub(crate) telemetry_tx: tokio::sync::mpsc::Sender<TelemetryEvent>,
  /// QoS 1 messages handed to a client process and not yet acknowledged,
  /// keyed by that process. Bounded in count and in age: it covers a
  /// connection that died between the write and the acknowledgement, not a
  /// subscriber that is away.
  pub(crate) pending_messages: Mutex<HashMap<String, Vec<crate::tunnel::pubsub::Pending>>>,
  /// Counters for the messaging path, rendered by the metrics endpoint.
  pub(crate) message_metrics: crate::tunnel::pubsub::MessageMetrics,
  pub(crate) client_connected: watch::Sender<bool>,
  /// Persisted autoscaling records, armed per bind.
  pub(crate) scaling_store: Mutex<crate::store::scaling::ScalingStore>,
  /// In-memory per-bind scaling state (single flight, cooldown, breaker).
  pub(crate) scaling_runtime: Mutex<crate::scaling::ScalingRuntime>,
  /// Caps how many capacity calls may be in flight across every bind at once.
  pub(crate) scaling_calls: Arc<tokio::sync::Semaphore>,
  pub(crate) connection_state: Mutex<ConnectionState>,
  pub(crate) server_start_time: Instant,
  pub(crate) pending_requests: Mutex<HashMap<String, PendingRequest>>,
  pub(crate) stats: Mutex<ServerStats>,
  pub(crate) recent_logs: Mutex<VecDeque<RequestLog>>,
  /// Live traffic fan-out: each proxied request's `RequestLog` is broadcast to
  /// any connected dashboard SSE subscribers (`/aperio/api/stream`). Dropped
  /// when there are no subscribers.
  pub(crate) traffic_tx: broadcast::Sender<RequestLog>,
  /// Live server configuration. Dashboard-editable settings swap in a new
  /// `Arc<ServerConfig>`; every access takes a cheap read-lock snapshot via
  /// [`AppState::config`].
  pub(crate) config_store: std::sync::RwLock<Arc<ServerConfig>>,
  /// Configuration as derived from environment variables only, used as the
  /// base that persisted overrides apply on top of (and for "reset").
  pub(crate) config_env_defaults: Arc<ServerConfig>,
  /// Currently persisted dashboard overrides (subset of settings).
  pub(crate) settings_overrides: Mutex<SettingsOverrides>,
  /// Path of the persisted overrides file (`<data_dir>/settings.json`).
  pub(crate) settings_path: std::path::PathBuf,
  /// True when the admin dashboard is served (APERIO_DASHBOARD != 0); the
  /// first-run helper redirect to /aperio only makes sense when it is.
  pub(crate) dashboard_enabled: bool,
  /// Flipped to true once a shutdown signal arrives; long-lived streams
  /// (dashboard SSE) watch it and end so graceful shutdown can complete.
  pub(crate) shutdown: watch::Sender<bool>,
  /// Currently in-flight proxied requests, checked against the (live,
  /// dashboard-editable) max_concurrent_requests limit. A plain counter
  /// instead of a semaphore so the limit can change at runtime.
  pub(crate) active_proxied_requests: Arc<AtomicUsize>,
  /// Currently-live proxied public WebSockets, checked against
  /// `max_ws_connections`. WebSockets are long-lived, so they get their own
  /// counter separate from the (short-lived) HTTP request slots above.
  pub(crate) active_ws_connections: Arc<AtomicUsize>,
  pub(crate) path_rr: Mutex<HashMap<RouteGroupKey, usize>>,
  /// Dashboard sessions, persisted in SQLite so restarts don't sign
  /// everyone out.
  pub(crate) sessions: Mutex<crate::store::sessions::SessionStore>,
  pub(crate) rate_limiter: Mutex<HashMap<IpAddr, RateLimitState>>,
  /// Escalating per-IP failed-login lockout (brute-force protection).
  pub(crate) login_lockout: Mutex<crate::auth::LockoutTracker>,
  /// Per-token request rate buckets (key = dynamic token record id),
  /// enforcing the token's optional `max_rps`.
  pub(crate) token_rate: Mutex<HashMap<String, RateLimitState>>,
  /// Per-token daily byte usage: token id → (day key, bytes). In-memory
  /// only, a restart resets the current day's usage.
  pub(crate) token_daily_bytes: Mutex<HashMap<String, (String, u64)>>,
  /// Source IPs a dynamic token has connected from (token id → set of IPs).
  /// In-memory only; drives the `token_new_ip` alert when a token connects
  /// from an address it has not been seen from before this run.
  pub(crate) token_seen_ips: Mutex<HashMap<String, HashSet<IpAddr>>>,
  /// Per-route request-rate buckets (key = matched `rate_limits:` rule),
  /// enforcing the section's aggregate rps/burst per host+path. GC'd on size.
  pub(crate) route_rate: Mutex<HashMap<String, RateLimitState>>,
  pub(crate) active_tunnel_count: AtomicUsize,
  /// Active WebSocket proxy streams: stream_id → sender to relay tunnel WsData to public WS.
  pub(crate) ws_streams: Mutex<HashMap<String, WsStreamHandle>>,
  /// Pending WebSocket upgrade responses: upgrade_id → oneshot to resolve when client responds.
  pub(crate) pending_upgrades: Mutex<HashMap<String, PendingRequest>>,
  /// Persistent store of dashboard-created dynamic API tokens.
  pub(crate) token_store: Mutex<TokenStore>,
  /// Persistent store of programmatic admin API keys (Bearer auth for the
  /// dashboard API; scoped by role + organization).
  pub(crate) admin_key_store: Mutex<crate::store::admin_keys::AdminKeyStore>,
  /// Persistent inbound-webhook inbox (`webhook_inbox: true` services).
  pub(crate) inbox_store: Mutex<crate::store::inbox::InboxStore>,
  /// Dashboard users (role-based access; separate from tunnel tokens).
  pub(crate) users: Mutex<crate::store::users::UserStore>,
  /// In-flight streamed response bodies: request_id → chunk sender.
  pub(crate) response_streams: Mutex<HashMap<String, ResponseStreamHandle>>,
  /// Recently captured HTTP transactions for the dashboard inspector.
  pub(crate) captured_requests: Mutex<VecDeque<CapturedRequest>>,
  /// Persistent audit log of administrative/security events.
  pub(crate) audit: Mutex<AuditLog>,
  /// Restart-surviving traffic statistics (all-time + period buckets).
  pub(crate) persistent_stats: Mutex<StatsStore>,
  /// Persistent webhook definitions for the event system.
  pub(crate) webhook_store: Mutex<WebhookStore>,
  /// Child organizations (multi-tenancy); master is implicit (org_id None).
  pub(crate) org_store: Mutex<crate::store::orgs::OrgStore>,
  /// Persistent log of webhook delivery outcomes (shared with the delivery
  /// tasks, which record after their retries finish).
  pub(crate) webhook_deliveries: std::sync::Arc<Mutex<webhooks::DeliveryLog>>,
  /// WebAuthn verifier for passkey sign-in (None until
  /// APERIO_WEBAUTHN_ORIGIN is configured).
  pub(crate) webauthn: Option<webauthn_rs::Webauthn>,
  /// In-flight WebAuthn registration/authentication ceremonies.
  pub(crate) webauthn_ceremonies: Mutex<crate::webauthn::WebauthnCeremonies>,
  /// Per-service availability history (uptime/SLA reporting).
  pub(crate) uptime: Mutex<crate::store::uptime::UptimeStore>,
  /// OIDC SSO runtime config (None = feature disabled).
  pub(crate) oidc: Option<oidc::OidcRuntime>,
  /// Per-organization OIDC runtimes, built lazily from each org's stored
  /// config and cached by org id (invalidated when the org's OIDC is updated).
  pub(crate) org_oidc: Mutex<HashMap<String, oidc::OidcRuntime>>,
  /// Pending OIDC login flows: state token → (original redirect, bound org id
  /// for a per-org login, the callback URL sent to the provider, expiry).
  pub(crate) oidc_states: Mutex<HashMap<String, OidcStateEntry>>,
  /// Active experimental TCP tunnel streams: stream_id → consumer sender.
  pub(crate) tcp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
  /// Active UDP relay streams (declared `protocol: udp` tunnels):
  /// stream_id → consumer sender. Same handle shape as TCP; the payloads are
  /// whole datagrams instead of stream bytes.
  pub(crate) udp_streams: Mutex<HashMap<String, UdpStreamHandle>>,
  /// Server-side GET response cache (APERIO_CACHE; see the cache module).
  pub(crate) response_cache: Mutex<crate::cache::ResponseCache>,
  /// Cacheable GET misses currently being fetched, keyed like the response
  /// cache (`host|uri`). Concurrent identical misses subscribe to the
  /// leader's watch channel and re-check the cache when it completes
  /// (single-flight coalescing). Sync mutex: only held for map ops.
  pub(crate) cache_inflight:
    std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Receiver<bool>>>,
  /// Rolling per-stage latency statistics per route (in-memory).
  pub(crate) stage_stats: Mutex<StageStats>,
  /// Rolling per-endpoint latency windows (slowest-endpoints report).
  pub(crate) endpoint_stats: Mutex<EndpointStats>,
  /// Rolling per-route minute-bucketed status trends (dashboard sparklines).
  pub(crate) route_trends: Mutex<RouteTrends>,
  /// Request volume in five-second slices over the last quarter hour, per
  /// organization: the long view of the dashboard's live activity chart, kept
  /// here so it survives a reload and is the same for everyone looking.
  pub(crate) activity: Mutex<Activity>,
  /// Hostnames and patterns currently in maintenance mode (`*` = every
  /// hostname, `*.example.com` = every subdomain of it), mapped to what is
  /// known about each flag. Requests to them get a 503 page even while
  /// clients are connected. In-memory only, like bind overrides: cleared by a
  /// server restart.
  pub(crate) maintenance: Mutex<std::collections::HashMap<String, MaintenanceFlag>>,
  /// Structured access log file (APERIO_ACCESS_LOG): one JSON line per
  /// proxied request, ready for Loki/ClickHouse ingestion. The same data is
  /// always emitted as structured `aperio_access` tracing events on stdout.
  pub(crate) access_log: Option<tokio::sync::mpsc::Sender<crate::access_log::AccessLogCmd>>,
  /// Request duration histogram exposed on `/aperio/metrics`.
  pub(crate) duration_histogram: DurationHistogram,
  /// Refusals by limit, for `aperio_rate_limited_total`. A load test asks
  /// "which limit is firing", and that is a counter's question, not a
  /// header's.
  pub(crate) limit_counters: crate::limits::LimitCounters,
  /// Streamed responses currently open, per visitor address
  /// (planned_features #20). A `std` mutex rather than a `tokio` one: every
  /// operation is a hash lookup and an integer, and the guard has to be
  /// releasable from `Drop`, which cannot await.
  pub(crate) stream_counts: Arc<std::sync::Mutex<HashMap<std::net::IpAddr, u32>>>,
}

/// What one call costs against the per-IP bucket (planned_features #64).
///
/// The prices are ratios rather than measurements: what matters is that the
/// three are ordered and separated, not that `Expensive` is exactly five
/// requests' worth. Sizing the bucket is still `ip_limit_max`, and an operator
/// who wants a different shape moves that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RateCost {
  /// A read, a proxied request, a tunnel handshake. The ordinary price, and
  /// what the whole surface used to be charged.
  Cheap,
  /// Something that authenticates a credential, so a wrong answer is a guess
  /// somebody made: login, WebAuthn, TOTP. Priced up because the bucket is
  /// the only thing standing between a stolen password list and this server.
  Guessable,
  /// Something that writes, provisions or reads the whole store: creating a
  /// token, provisioning an ephemeral tunnel, an export. Priced up because it
  /// costs the server far more than a page view, not because it is suspicious.
  Expensive,
}

impl RateCost {
  /// The price, in tokens.
  ///
  /// Deliberately gentle multiples. The bucket was sized when every call cost
  /// one, so multiplying a class by five does not make that class cost five,
  /// it tightens the limit on it fivefold against a ceiling nobody re-chose.
  /// The e2e suite found this the hard way: one address making a couple of
  /// hundred calls, fourteen of them logins, went from comfortable to
  /// throttled. An office of fifty people behind one NAT signing in on a
  /// Monday morning is the same shape, and being ordered and separated is
  /// what this needs to be, not steep.
  pub(crate) fn tokens(self) -> f64 {
    match self {
      RateCost::Cheap => 1.0,
      RateCost::Guessable => 2.0,
      RateCost::Expensive => 5.0,
    }
  }
}

/// RAII slot in the global proxied-request concurrency limit; the slot is
/// released when dropped.
pub(crate) struct RequestSlot(Arc<AtomicUsize>);

impl Drop for RequestSlot {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}

/// RAII slot in the per-visitor streamed-response limit (planned_features
/// #20). Released when the streamed body it was moved into is dropped, which
/// is what makes it a *concurrency* limit rather than a rate limit: a visitor
/// that opens and closes streams as fast as it likes never trips it, and one
/// that holds them open does.
///
/// The counter is removed at zero rather than left at zero, so the map holds
/// only the addresses currently streaming. Without that it would grow one
/// entry per address ever seen, which is a slow leak driven by strangers.
pub(crate) struct StreamSlot {
  ip: std::net::IpAddr,
  counts: Arc<std::sync::Mutex<HashMap<std::net::IpAddr, u32>>>,
}

impl Drop for StreamSlot {
  fn drop(&mut self) {
    let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = counts.get_mut(&self.ip) {
      *n = n.saturating_sub(1);
      if *n == 0 {
        counts.remove(&self.ip);
      }
    }
  }
}

/// RAII slot in the live-WebSocket limit; released when the proxied WebSocket
/// (and this permit, moved into its relay) drops.
pub(crate) struct WsSlot(Arc<AtomicUsize>);

impl Drop for WsSlot {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}

impl AppState {
  /// Whether `client_id` announced a protocol version with per-stream flow
  /// control (v3+), i.e. it honors `StreamPause`/`StreamResume`. Read when a
  /// stream starts; a client is only routable once its first Ping announced
  /// the version, so the answer is stable by then.
  /// The tunnel protocol version this connection announced (via Ping), or 1
  /// for a client that has not announced yet. Read once per relay stream:
  /// the announcement precedes routability, so the answer is stable by the
  /// time any stream targets the connection.
  pub(crate) async fn client_protocol(&self, client_id: &str) -> u32 {
    self
      .clients
      .read()
      .await
      .get(client_id)
      .and_then(|h| h.client_protocol)
      .unwrap_or(1)
  }

  pub(crate) async fn client_supports_pause(&self, client_id: &str) -> bool {
    self
      .clients
      .read()
      .await
      .get(client_id)
      .and_then(|h| h.client_protocol)
      .unwrap_or(0)
      >= 3
  }

  /// The flow-control watermarks new streams start with: the live config's
  /// values, repaired into a consistent trio. Snapshotted per stream, so a
  /// settings change applies to streams started after it.
  pub(crate) fn stream_limits(&self) -> StreamLimits {
    let c = self.config();
    StreamLimits::sanitized(
      c.stream_pause_bytes,
      c.stream_resume_bytes,
      c.stream_backlog_limit,
      c.stream_min_throughput,
    )
  }

  /// Rebuilds the live config from the layers (env defaults ->
  /// `aperio-server.yaml` live settings -> dashboard overrides) with the
  /// current structured `headers`/`routes`, and applies it. Called on file
  /// hot-reload. Structural keys (host/port/data_dir, proxy trust, OIDC,
  /// `expose` ports) are not re-applied live and need a restart.
  /// Returns the list of live-setting keys that changed (`key: old→new`, with
  /// secrets masked) so the caller can record it in the `config_reloaded`
  /// audit entry.
  pub(crate) async fn reload_from_file(self: &Arc<Self>) -> Vec<String> {
    let file_layer = crate::settings::file_overrides();
    let dashboard = self.settings_overrides.lock().await.clone();
    let base = crate::settings::apply_settings_overrides(&self.config_env_defaults, &file_layer);
    let mut effective = crate::settings::apply_settings_overrides(&base, &dashboard);
    effective.header_rules = crate::headers::from_config_file();
    effective.static_routes = crate::static_routes::from_config_file();
    effective.error_pages = crate::error_pages::from_config_file();
    effective.route_limits = crate::route_limits::from_config_file();
    effective.fallbacks = crate::fallbacks::from_config_file();
    effective.waf = crate::waf::from_config_file();
    effective.maintenance_windows = crate::maintenance_windows::from_config_file();
    effective.alert_rules = crate::alert_rules::from_config_file();
    effective.denied_ips = crate::deny_list::from_config();
    let old = self.config();
    let diff = crate::settings::config_reload_diff(&old, &effective);
    crate::api::settings::swap_config(self, effective).await;
    diff
  }

  /// Snapshot of the live configuration (cheap Arc clone).
  pub(crate) fn config(&self) -> Arc<ServerConfig> {
    // Recover from a poisoned lock rather than panicking: config() is on
    // essentially every proxied request, so a single panic under the write
    // lock must not turn into a total outage, the stored config is a valid
    // Arc regardless of who poisoned the lock.
    self
      .config_store
      .read()
      .unwrap_or_else(|e| e.into_inner())
      .clone()
  }

  /// Claims a slot under `max_concurrent_requests`, or None when the server
  /// is at capacity. The limit is read live, so dashboard edits apply to the
  /// very next request.
  pub(crate) fn try_acquire_request_slot(&self) -> Option<RequestSlot> {
    let limit = self.config().max_concurrent_requests;
    self
      .active_proxied_requests
      .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
        if cur < limit { Some(cur + 1) } else { None }
      })
      .ok()
      .map(|_| RequestSlot(self.active_proxied_requests.clone()))
  }

  /// Claims a slot under `max_streams_per_ip` for one visitor address, or
  /// `None` when that address already holds its share.
  ///
  /// `Some` with no accounting when the limit is off, which is the default:
  /// the ceiling protects against one host holding many slow streams, and the
  /// number that does so without cutting off a legitimate visitor depends
  /// entirely on the deployment. A NAT or a CGNAT puts many real people behind
  /// one address, so a default here would be a guess with a queue of users
  /// behind it.
  pub(crate) fn try_acquire_stream_slot(&self, ip: std::net::IpAddr) -> Option<StreamSlot> {
    let limit = self.config().max_streams_per_ip;
    if limit == 0 {
      return None;
    }
    let mut counts = self.stream_counts.lock().unwrap_or_else(|e| e.into_inner());
    let entry = counts.entry(ip).or_insert(0);
    if *entry >= limit {
      return None;
    }
    *entry += 1;
    Some(StreamSlot {
      ip,
      counts: self.stream_counts.clone(),
    })
  }

  /// Claims a live-WebSocket slot under `max_ws_connections`, or None at
  /// capacity. Held (via the returned [`WsSlot`]) for the whole life of the
  /// proxied WebSocket, so long-lived connections can't pile up unbounded.
  pub(crate) fn try_acquire_ws_slot(&self) -> Option<WsSlot> {
    let limit = self.config().max_ws_connections;
    self
      .active_ws_connections
      .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
        if cur < limit { Some(cur + 1) } else { None }
      })
      .ok()
      .map(|_| WsSlot(self.active_ws_connections.clone()))
  }

  /// Records a server-global (master-organization) audit event: config
  /// reloads, export/import, failed logins, and other events not tied to a
  /// child organization. Org-scoped actions use [`audit_in`] or the
  /// [`audit_session`] convenience instead.
  pub(crate) async fn audit(&self, event: &str, actor: &str, actor_ip: &str, details: &str) {
    self
      .audit
      .lock()
      .await
      .record(event, actor, actor_ip, None, details);
  }

  /// Records an audit event scoped to a specific organization (`None` = the
  /// implicit master org). Use when the event belongs to a child org, e.g. a
  /// client of that org connecting, or a token of that org being created.
  pub(crate) async fn audit_in(
    &self,
    event: &str,
    actor: &str,
    actor_ip: &str,
    org: Option<String>,
    details: &str,
  ) {
    self
      .audit
      .lock()
      .await
      .record(event, actor, actor_ip, org, details);
  }

  /// Records an audit event for a dashboard action, resolving both the acting
  /// user and the caller's effective organization from the request. This is the
  /// common path for session-driven admin actions, so the event is filed under
  /// whichever org the caller is currently acting in.
  pub(crate) async fn audit_session(
    &self,
    event: &str,
    headers: &axum::http::HeaderMap,
    actor_ip: &str,
    details: &str,
  ) {
    let actor = self.session_actor(headers).await;
    let org = crate::auth::effective_org(self, headers).await;
    self
      .audit
      .lock()
      .await
      .record(event, &actor, actor_ip, org, details);
  }

  /// Resolves the acting dashboard user for an audit record from the request:
  /// the signed-in username, "aperio" for the built-in admin (master token /
  /// dashboard password / OIDC), or "-" when there is no valid session.
  pub(crate) async fn session_actor(&self, headers: &axum::http::HeaderMap) -> String {
    match crate::auth::dashboard_role(self, headers).await {
      Some(_) => {
        if let Some(user) = crate::auth::dashboard_username(self, headers).await {
          user
        } else if let Some((_, _, name)) = crate::auth::admin_key_identity(self, headers).await {
          // Programmatic admin key: attribute the action to the key by name.
          format!("key:{name}")
        } else {
          "aperio".to_string()
        }
      }
      None => "-".to_string(),
    }
  }

  /// Delivers a server-global (master-organization) event to its subscribed
  /// webhooks. Org-scoped events use [`emit_event_in`].
  pub(crate) async fn emit_event(&self, event: &str, data: serde_json::Value) {
    self.emit_event_in(event, data, None).await;
  }

  /// Delivers an event to the webhooks of one organization (`None` = master):
  /// a webhook only ever fires for events in its own org, so a child org's
  /// webhook never learns about master's or another org's activity.
  pub(crate) async fn emit_event_in(
    &self,
    event: &str,
    data: serde_json::Value,
    org: Option<String>,
  ) {
    let subs: Vec<_> = self
      .webhook_store
      .lock()
      .await
      .subscribers(event)
      .into_iter()
      .filter(|w| w.org_id == org)
      .collect();
    webhooks::dispatch(
      subs,
      event,
      data.clone(),
      self.webhook_deliveries.clone(),
      self.config().outbound_policy.clone(),
    );
    self.publish_event_topic(event, data, org).await;
  }

  /// Mirrors a server event onto its `$aperio/` topic, so a client can react
  /// to infrastructure the same way it reacts to anything else.
  ///
  /// The events already existed and already fed webhooks; putting them on
  /// topics is what lets a client hear about a peer connecting without
  /// standing up an HTTP receiver for it. `client_draining` becomes
  /// `$aperio/client/draining`: the underscore separates a level, the way a
  /// topic reads.
  ///
  /// Nothing is spent when nobody is listening: `publish` walks the client map
  /// and finds no subscriber, and a bare `#` deliberately does not match this
  /// namespace, so a client debugging with a wildcard is not enrolled in it.
  async fn publish_event_topic(&self, event: &str, data: serde_json::Value, org: Option<String>) {
    let topic = format!(
      "{}{}",
      aperio_config::RESERVED_TOPIC_PREFIX,
      event.replace('_', "/")
    );
    let payload = serde_json::to_vec(&data).unwrap_or_default();
    let _ = crate::tunnel::pubsub::publish(
      self,
      org.as_deref(),
      &topic,
      &payload,
      crate::tunnel::pubsub::Publisher::Server,
      // Server events are a stream of what is happening now. A client that
      // was not connected did not miss anything it can act on.
      0,
    )
    .await;
  }

  /// Force-disconnects every live tunnel connection authenticated with the
  /// given dynamic token: their read loops end and they leave the routing pool
  /// immediately, instead of serving until they next reconnect (when the
  /// revoked token would be rejected anyway). Returns how many were dropped.
  pub(crate) async fn disconnect_token_clients(&self, token_id: &str) -> usize {
    let mut dropped = 0usize;
    {
      let clients = self.clients.read().await;
      for handle in clients.values() {
        if handle.perms.token_id.as_deref() == Some(token_id) {
          handle.disconnect.notify_one();
          dropped += 1;
        }
      }
    }
    // The token is being revoked/deleted; drop its source-IP tracking so the
    // in-memory `token_seen_ips` map does not accumulate entries for tokens
    // that no longer exist.
    self.token_seen_ips.lock().await.remove(token_id);
    dropped
  }

  /// Applies a changed organization hostname allowlist to that org's live
  /// tunnel connections: each one's cached copy is refreshed, and any
  /// connection now serving a hostname outside the fence is dropped. Returns
  /// how many were dropped.
  ///
  /// The allowlist is copied into `ClientPerms` at connect time so that later
  /// bind checks stay a pure in-memory comparison. Without this, tightening
  /// the fence only took effect the next time each client happened to
  /// reconnect, while the endpoint's own documentation promised it applied at
  /// once, so a hostname an operator had just revoked kept being served,
  /// potentially for as long as the client stayed up.
  pub(crate) async fn apply_org_hostnames(&self, org_id: &str, hostnames: &[String]) -> usize {
    let mut dropped = 0usize;
    let mut clients = self.clients.write().await;
    for handle in clients.values_mut() {
      if handle.perms.org_id.as_deref() != Some(org_id) {
        continue;
      }
      handle.perms.org_hostnames = hostnames.to_vec();
      let serving: Vec<String> = handle
        .assigned_hostnames
        .iter()
        .chain(handle.declared_hostnames.iter())
        .cloned()
        .collect();
      if serving
        .iter()
        .any(|h| !crate::store::orgs::hostname_in_org_allowlist(h, hostnames))
      {
        handle.disconnect.notify_one();
        dropped += 1;
      }
    }
    dropped
  }
}

impl AppState {
  /// In-memory thread-safe Per-IP Token Bucket Rate Limiter.
  /// Returns `true` if request is allowed, `false` if rate-limited.
  /// Enforces the serving token's optional rate limit and daily byte quota.
  /// Returns Err with a short reason when the request must be rejected with
  /// 429. Master-token traffic (token_id = None) is never limited.
  /// The error names *which* limit refused, so the refusal can name itself
  /// in a header rather than leaving the caller to parse a sentence.
  pub(crate) async fn check_token_limits(
    &self,
    token_id: Option<&str>,
  ) -> Result<(), crate::limits::Limit> {
    let Some(id) = token_id else {
      return Ok(());
    };
    // Limits are read from the store per request so dashboard edits apply
    // live; the store is small (dozens of tokens at most).
    let (max_rps, daily_max_bytes) = {
      let store = self.token_store.lock().await;
      match store.list().iter().find(|t| t.id == id) {
        Some(t) => (t.max_rps, t.daily_max_bytes),
        // Token revoked while its tunnel stays up: no limits to apply.
        None => return Ok(()),
      }
    };

    if let Some(rps) = max_rps.filter(|v| *v > 0.0) {
      let mut buckets = self.token_rate.lock().await;
      let now = Instant::now();
      gc_token_rate(&mut buckets, now);
      let burst = rps.max(1.0);
      let bucket = buckets.entry(id.to_string()).or_insert(RateLimitState {
        tokens: burst,
        last_updated: now,
      });
      let elapsed = now.duration_since(bucket.last_updated).as_secs_f64();
      bucket.tokens = (bucket.tokens + elapsed * rps).min(burst);
      bucket.last_updated = now;
      if bucket.tokens < 1.0 {
        return Err(crate::limits::Limit::TokenRate);
      }
      bucket.tokens -= 1.0;
    }

    if let Some(quota) = daily_max_bytes.filter(|v| *v > 0) {
      let today = crate::store::stats::period_keys()[0].clone();
      let usage = self.token_daily_bytes.lock().await;
      if let Some((day, used)) = usage.get(id)
        && *day == today
        && *used >= quota
      {
        return Err(crate::limits::Limit::TokenQuota);
      }
    }
    Ok(())
  }

  /// Attributes payload bytes to the serving token's daily usage (feeds the
  /// `daily_max_bytes` quota). The counter rolls over at local midnight.
  pub(crate) async fn add_token_bytes(&self, token_id: Option<&str>, bytes: u64) {
    let Some(id) = token_id else {
      return;
    };
    if bytes == 0 {
      return;
    }
    let today = crate::store::stats::period_keys()[0].clone();
    let mut usage = self.token_daily_bytes.lock().await;
    gc_token_daily_bytes(&mut usage, &today);
    let entry = usage
      .entry(id.to_string())
      .or_insert_with(|| (today.clone(), 0));
    if entry.0 != today {
      *entry = (today, bytes);
    } else {
      entry.1 = entry.1.saturating_add(bytes);
    }
  }

  /// May this organization act on `target` (put it into maintenance, mint a
  /// share link for it)? This is the isolation fence for the hostname-scoped
  /// operations, so that one tenant cannot 503 or hand out access to
  /// another's site.
  ///
  /// `target` is an exact hostname or a subdomain wildcard (`*.acme.com`),
  /// the same two shapes an organization's allowlist is written in. A
  /// wildcard is a claim over a whole subtree, so it takes an entry that owns
  /// the subtree: `acme.com` does not authorize `*.acme.com`, it authorizes
  /// one name and the request covers all the others.
  ///
  /// The question it answers is "may this org *serve* those names", not "is
  /// one of its clients serving them right now". Those came apart in
  /// practice: an org fenced to `x.com` could not put `x.com` into
  /// maintenance until a client for it was connected, which is precisely the
  /// case where maintenance mode is wanted, and a share link had to wait for
  /// the client to come back.
  ///
  /// A fenced org is judged by its allowlist. An unfenced child org has no
  /// allowlist to judge, so it falls back to the older test, one of its own
  /// connected clients serving the hostname, which is the only isolation left
  /// when the operator never drew a boundary. That test cannot prove a
  /// subtree, so an unfenced org cannot claim one.
  ///
  /// Master is fenced by the other organizations and by nothing else:
  /// everything no tenant claims is master's, which is what lets it act on a
  /// hostname with nothing connected, while a tenant's site stays the
  /// tenant's even from the super-admin's own screen. For a subtree that
  /// means no tenant may be anywhere inside it.
  pub(crate) async fn org_may_claim_hostname(&self, org: Option<&str>, target: &str) -> bool {
    use crate::store::orgs::{pattern_covers_pattern, patterns_overlap};

    let fences: Vec<(String, Vec<String>)> = {
      let store = self.org_store.lock().await;
      store
        .list()
        .iter()
        .map(|o| (o.id.clone(), o.hostnames.clone()))
        .collect()
    };
    // Hostnames of the clients of the organizations `org_matches` selects.
    let served = |org_matches: &dyn Fn(Option<&str>) -> bool,
                  clients: &std::collections::HashMap<String, ClientHandle>| {
      clients
        .values()
        .filter(|c| org_matches(c.perms.org_id.as_deref()))
        .flat_map(|c| {
          c.effective_hostnames()
            .into_iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
        })
        .collect::<Vec<String>>()
    };

    match org {
      Some(id) => {
        let own = fences
          .iter()
          .find(|(oid, _)| oid == id)
          .map(|(_, list)| list.clone())
          .unwrap_or_default();
        if !own.is_empty() {
          return own
            .iter()
            .any(|entry| pattern_covers_pattern(entry, target));
        }
        // No fence: the only claim left is a client of this org serving the
        // name, which says nothing about the rest of a subtree.
        if target.starts_with("*.") {
          return false;
        }
        let clients = self.clients.read().await;
        served(&|owner| owner == Some(id), &clients)
          .iter()
          .any(|h| h == target)
      }
      None => {
        if fences
          .iter()
          .flat_map(|(_, list)| list)
          .any(|entry| patterns_overlap(entry, target))
        {
          return false;
        }
        let clients = self.clients.read().await;
        !served(&|owner| owner.is_some(), &clients)
          .iter()
          .any(|h| pattern_covers_pattern(target, h))
      }
    }
  }

  /// One beat of the background garbage collector: sweeps the per-IP and
  /// per-route rate buckets and the expired sessions.
  ///
  /// These retains used to run inline, under their lock, on the back of
  /// whichever request happened to draw the five-minute tick; a big map made
  /// that request (and everyone queued behind the lock) pay for the sweep.
  /// The size failsafes at the call sites stay, they are what bounds the maps
  /// between beats; this is the routine sweep that keeps the failsafes from
  /// ever being the mechanism.
  pub(crate) async fn gc_tick_once(&self, now: Instant) {
    self
      .rate_limiter
      .lock()
      .await
      .retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    self
      .route_rate
      .lock()
      .await
      .retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    let now_secs = crate::store::sessions::now_secs();
    self
      .sessions
      .lock()
      .await
      .retain(|info| info.expires_at > now_secs);
  }

  /// The maintenance flag in force for `host`, if any: an exact entry, a
  /// `*.suffix` wildcard covering it, or the server-wide `*`.
  ///
  /// One place rather than two, because there were two and they disagreed:
  /// the proxy matched wildcards and the autoscaler still asked
  /// `contains_key`, so a `*.robogon.com` flag served the 503 page and woke a
  /// scaled-to-zero service behind it at the same time.
  ///
  /// An expired flag simply does not match. It is not swept here: this is the
  /// read path holding a lock every request shares, and the write paths
  /// (setting a flag, listing them, deleting an organization) drop them.
  pub(crate) async fn maintenance_for(&self, host: Option<&str>) -> Option<MaintenanceFlag> {
    let now = crate::store::tokens::now_secs();
    // A scheduled window from the config file, if one is running. Checked
    // first and without taking the flag lock, so the common case of a server
    // with windows and no ad-hoc flag costs one list scan. A runtime flag can
    // still be raised during a window; it simply says the same thing.
    let cfg = self.config();
    if !cfg.maintenance_windows.is_empty()
      && let Some((window, until)) = cfg.maintenance_windows.active_for(host, now)
    {
      return Some(MaintenanceFlag {
        org: None,
        reason: window.reason.clone(),
        until: Some(until),
        since: now,
        // Named so the dashboard and the 503 page can tell an operator's
        // switch from the schedule doing what it was told.
        actor: "schedule".to_string(),
      });
    }
    let set = self.maintenance.lock().await;
    if set.is_empty() {
      return None;
    }
    set
      .iter()
      .filter(|(_, flag)| !flag.expired(now))
      .find(|(pattern, _)| {
        if *pattern == "*" {
          return true;
        }
        host.is_some_and(|h| {
          // An exact flag first, since that is the common case and needs no
          // matching, then anything with a placeholder in it: `*.robogon.com`
          // is one switch for every service under a domain, and the partial
          // shape (`*-pi.robogon.com`) is accepted by the same normalizer the
          // set handler runs, so it has to match here too. This arm used to
          // take only the `*.` shape, which made a partial flag a stored 200
          // that never served a single 503.
          *pattern == h
            || (pattern.contains('*') && crate::store::orgs::pattern_matches_host(pattern, h))
        })
      })
      .map(|(_, flag)| flag.clone())
  }

  /// The quota record for a child org (None for master or an unknown id).
  pub(crate) async fn org_quota(
    &self,
    org: Option<&str>,
  ) -> Option<crate::store::orgs::Organization> {
    let id = org?;
    self.org_store.lock().await.find(id).cloned()
  }

  // The token and user org quotas are enforced atomically inside their create
  // handlers (api/tokens.rs, api/users.rs), the cap count and the insert run
  // under one held store lock so concurrent creates can't overshoot the cap.

  /// Enforces the org's `max_clients` quota against currently-connected
  /// clients. Err(msg) when at the cap.
  pub(crate) async fn check_org_client_quota(&self, org: Option<&str>) -> Result<(), String> {
    let Some(max) = self.org_quota(org).await.and_then(|q| q.max_clients) else {
      return Ok(());
    };
    let count = self
      .clients
      .read()
      .await
      .values()
      .filter(|c| c.perms.org_id.as_deref() == org)
      .count() as u64;
    if count >= max {
      Err(format!("organization client quota reached ({max})"))
    } else {
      Ok(())
    }
  }

  /// True when the org is over its `max_bytes_month` quota for the current
  /// calendar month (proxied bytes in + out). False when no quota / no org.
  pub(crate) async fn org_over_month_bytes(&self, org: Option<&str>) -> bool {
    let Some(max) = self.org_quota(org).await.and_then(|q| q.max_bytes_month) else {
      return false;
    };
    let month_key = crate::store::stats::period_keys()[2].clone();
    let used = {
      let stats = self.persistent_stats.lock().await;
      stats
        .snapshot_for_org(org)
        .periods
        .get(&month_key)
        .map(|p| p.bytes_sent + p.bytes_received)
        .unwrap_or(0)
    };
    used >= max
  }

  /// Enforces the per-route rate limit for a request. Returns true when the
  /// request may proceed, false when the matched route's shared token bucket
  /// is empty (the caller answers 429). No configured rule for the
  /// host+path+method = always allowed.
  ///
  /// Two sources feed this, and a route's own `rate_limit:` wins over a
  /// `rate_limits:` entry matching the same request: the inline one is written
  /// next to the route it governs, so an operator reading that entry has seen
  /// the whole story.
  pub(crate) async fn check_route_rate_limit(
    &self,
    host: Option<&str>,
    path: &str,
    method: &str,
  ) -> bool {
    let cfg = self.config();
    let inline = cfg.static_routes.policy_for(host, path).and_then(|rule| {
      let rl = rule.rate_limit.as_ref()?;
      crate::route_limits::method_matches(rl.methods.as_ref(), Some(method)).then(|| {
        (
          rl.rps,
          rl.burst.filter(|b| *b > 0.0).unwrap_or(rl.rps).max(1.0),
          rule.rate_key.clone(),
        )
      })
    });
    let matched = match inline {
      Some(found) => Some(found),
      None if cfg.route_limits.is_empty() => None,
      None => cfg
        .route_limits
        .matched(host, path, Some(method))
        .map(|r| (r.rps, r.burst, r.key.clone())),
    };
    let Some((rps, burst, key)) = matched else {
      return true;
    };
    let mut buckets = self.route_rate.lock().await;
    let now = Instant::now();
    if buckets.len() > TOKEN_MAP_GC_THRESHOLD {
      buckets.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    }
    let bucket = buckets.entry(key).or_insert(RateLimitState {
      tokens: burst,
      last_updated: now,
    });
    let elapsed = now.duration_since(bucket.last_updated).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rps).min(burst);
    bucket.last_updated = now;
    if bucket.tokens < 1.0 {
      return false;
    }
    bucket.tokens -= 1.0;
    true
  }

  pub(crate) async fn check_rate_limit(&self, ip: IpAddr) -> bool {
    self.check_rate_limit_cost(ip, RateCost::Cheap).await
  }

  /// The same bucket, charged by what the request costs to serve
  /// (planned_features #64).
  ///
  /// Every admin call used to take exactly one token, so a login attempt, a
  /// token creation and a full export were charged the same. Two things follow
  /// from that, and both are wrong in the same way. A brute-force attempt gets
  /// the whole budget of a bucket sized for ordinary reads, and an operator
  /// running one legitimate export spends the same as one page view while
  /// costing the server a thousand times more.
  ///
  /// One bucket, different prices, rather than a bucket per class: separate
  /// buckets would let an attacker spend a full allowance on *each*, and the
  /// thing being protected, this server's capacity, is shared anyway.
  pub(crate) async fn check_rate_limit_cost(&self, ip: IpAddr, cost: RateCost) -> bool {
    self.charge_rate_limit(ip, cost.tokens()).await
  }

  async fn charge_rate_limit(&self, ip: IpAddr, cost: f64) -> bool {
    let mut limit_map = self.rate_limiter.lock().await;
    let now = Instant::now();

    // Stale buckets are swept by the background gc beat (`gc_tick_once`), not
    // here: a retain over tens of thousands of entries used to run on the
    // back of whichever request drew the five-minute tick, with the lock
    // held, which is a tail-latency spike by design. Only the size failsafe
    // stays inline, since it is what bounds the map between beats.
    if limit_map.len() > 1000 {
      limit_map.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    }

    let max_tokens = self.config().ip_limit_max;
    let refill_rate = self.config().ip_limit_refill;

    let state = limit_map.entry(ip).or_insert_with(|| RateLimitState {
      tokens: max_tokens,
      last_updated: now,
    });

    let elapsed = now.duration_since(state.last_updated).as_secs_f64();
    state.tokens = (state.tokens + elapsed * refill_rate).min(max_tokens);
    state.last_updated = now;

    if state.tokens >= cost {
      state.tokens -= cost;
      true
    } else {
      false
    }
  }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
