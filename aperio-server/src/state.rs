use axum::extract::ws::Message;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast, mpsc, oneshot, watch};

use crate::oidc;
use crate::store::audit::AuditLog;
use crate::store::stats::{self, StatsStore};
use crate::store::tokens::TokenStore;
use crate::store::webhooks::{self, WebhookStore};

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
  /// Name of the dynamic token this client authenticated with (None = master).
  pub(crate) token_name: Option<String>,
  /// Organization this client belongs to, from its token (None = master).
  pub(crate) org_id: Option<String>,
  /// Temporary server-side path bind override (dashboard overrule).
  pub(crate) override_path_bind: Option<String>,
  /// Temporary server-side hostname bind override (dashboard overrule).
  pub(crate) override_hostname_bind: Option<String>,
  /// Seconds elapsed since the last heartbeat Ping was received.
  pub(crate) last_ping_seconds_ago: Option<u64>,
  /// Concurrency limit announced by the client (None = unlimited).
  pub(crate) max_concurrent: Option<u32>,
  /// Client build version announced via Ping (None until the first Ping).
  pub(crate) version: Option<String>,
  /// Service name announced via Ping (multi-service clients).
  pub(crate) service: Option<String>,
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
  /// Announced load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: u32,
  /// Announced downstream link capacity in bytes/second (None = unlimited).
  pub(crate) bandwidth_bps: Option<u64>,
  /// False when the client missed its heartbeat window and is out of the pool.
  pub(crate) healthy: bool,
  /// True while the client is gracefully draining before shutdown.
  pub(crate) draining: bool,
  /// Dashboard kill switch state (false = excluded from routing).
  pub(crate) enabled: bool,
  /// Client-process instance id self-reported via Ping (`--client-id`).
  pub(crate) instance_id: Option<String>,
  /// True when another live connection reports the same instance id — a
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
  /// Organization of the client that served the request (None = master). The
  /// inspector and replay are gated to the caller's effective org on this.
  #[serde(skip)]
  pub(crate) org_id: Option<String>,
}

/// Maximum number of captured requests kept in memory.
pub(crate) const CAPTURE_MAX_ENTRIES: usize = 50;
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
  /// Temporary hostname bind override set from the dashboard. Not persisted.
  pub(crate) override_hostname_bind: Option<String>,
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
  /// Announced downstream link capacity in bytes/second (0 = unlimited).
  /// Shared with the connection's writer task, which paces outgoing frames.
  pub(crate) bandwidth_bps: Arc<AtomicU64>,
  /// Display name of the service this connection exposes (announced via
  /// Ping by multi-service clients).
  pub(crate) service_name: Option<String>,
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
  /// Tunnels declared by the client via Ping (`tunnels:` list): normally
  /// unexposed local services a peer client may bind with `--bind-tunnels`
  /// (same token, explicit client id required).
  pub(crate) tunnels: Vec<crate::protocol::TunnelDecl>,
  /// The client opted its service into the server-side response cache
  /// (`cache: true` via Ping). Effective only with APERIO_CACHE on.
  pub(crate) cache: bool,
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
  /// Organization this token (and therefore this client) belongs to
  /// (None = master).
  pub(crate) org_id: Option<String>,
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
      org_id: None,
    }
  }

  pub(crate) fn hostname_allowed(&self, host: &str) -> bool {
    self.master || self.hostnames.is_empty() || self.hostnames.iter().any(|h| h == "*" || h == host)
  }

  pub(crate) fn path_allowed(&self, path: &str) -> bool {
    self.master || self.paths.is_empty() || self.paths.iter().any(|p| p == "*" || p == path)
  }

  /// Specific (non-wildcard) hostnames granted by the token; these are
  /// auto-bound to the client on connect.
  pub(crate) fn granted_hostnames(&self) -> Vec<String> {
    self
      .hostnames
      .iter()
      .filter(|h| *h != "*")
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
    if let Some(o) = &self.override_hostname_bind {
      return vec![o];
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

/// A pending OIDC login: (post-login redirect, bound org id, expiry).
pub(crate) type OidcStateEntry = (String, Option<String>, Instant);

/// One frame of a streamed response body relayed from the tunnel: data
/// chunks, then optionally one trailer block (e.g. gRPC's `grpc-status`).
pub(crate) enum BodyFrame {
  Data(Vec<u8>),
  Trailers(Vec<(String, String)>),
}

/// Standard response payload returned by tunnel client.
pub(crate) struct TunnelResponse {
  /// HTTP status code.
  pub(crate) status: u16,
  /// List of response headers (preserves duplicates like Set-Cookie).
  pub(crate) headers: Vec<(String, String)>,
  /// Base64 encoded payload body (buffered responses only).
  pub(crate) body: Option<String>,
  /// HTTP trailers of a buffered response (e.g. `grpc-status` for gRPC).
  pub(crate) trailers: Option<Vec<(String, String)>>,
  /// For streamed responses: receiver of decoded body frames. The proxy
  /// handler turns this into a streaming HTTP body.
  pub(crate) stream_rx: Option<mpsc::Receiver<Result<BodyFrame, std::io::Error>>>,
  /// Client-side stage durations (buffered responses of timing-aware clients).
  pub(crate) timings: Option<crate::protocol::ClientTimings>,
}

/// High-resolution timeline of one proxied request: microsecond offsets from
/// t0 = the server first receiving the request. Client-side stages are
/// measured on the client's own monotonic clock and anchored here by
/// splitting the unaccounted tunnel transit evenly between the two
/// directions — clocks are never mixed, and the estimate is flagged.
#[derive(Serialize, Clone, Copy)]
pub(crate) struct RequestTimeline {
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
        anchor + c.backend_done_us,
        anchor + c.respond_us,
      )
    });
    RequestTimeline {
      dispatched_us,
      client_received_us: anchored.map(|a| a.0),
      backend_sent_us: anchored.map(|a| a.1),
      backend_first_byte_us: anchored.map(|a| a.2),
      backend_done_us: anchored.map(|a| a.3),
      client_responded_us: anchored.map(|a| a.4),
      response_received_us,
      finished_us,
      estimated_anchor: anchored.is_some(),
    }
  }
}

/// Sender half of an in-flight streamed response body, kept so the tunnel
/// read loop can push chunks and so disconnect cleanup can drop it.
pub(crate) struct ResponseStreamHandle {
  pub(crate) tx: mpsc::Sender<Result<BodyFrame, std::io::Error>>,
  pub(crate) client_id: String,
}

/// Message relayed from the tunnel to a public TCP consumer WebSocket.
pub(crate) enum TcpConsumerMsg {
  Data(Vec<u8>),
  Close,
}

/// Handle to an active TCP tunnel stream (consumer side).
pub(crate) struct TcpStreamHandle {
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

/// Registered relay for a proxied public WebSocket: the sender that pushes
/// tunnel frames to the public side, tagged with the serving client's id so a
/// `WsData`/`WsClose` frame can be verified to come from the owning client.
pub(crate) struct WsStreamHandle {
  pub(crate) tx: mpsc::Sender<WsStreamMessage>,
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

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
pub(crate) struct AppState {
  pub(crate) clients: Mutex<HashMap<String, ClientHandle>>,
  pub(crate) client_connected: watch::Sender<bool>,
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
  /// only — a restart resets the current day's usage.
  pub(crate) token_daily_bytes: Mutex<HashMap<String, (String, u64)>>,
  /// Source IPs a dynamic token has connected from (token id → set of IPs).
  /// In-memory only; drives the `token_new_ip` alert when a token connects
  /// from an address it has not been seen from before this run.
  pub(crate) token_seen_ips: Mutex<HashMap<String, HashSet<IpAddr>>>,
  /// Per-route request-rate buckets (key = matched `rate_limits:` rule),
  /// enforcing the section's aggregate rps/burst per host+path. GC'd on size.
  pub(crate) route_rate: Mutex<HashMap<String, RateLimitState>>,
  pub(crate) last_session_gc: Mutex<Instant>,
  pub(crate) last_rate_gc: Mutex<Instant>,
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
  /// for a per-org login, expiry).
  pub(crate) oidc_states: Mutex<HashMap<String, OidcStateEntry>>,
  /// Active experimental TCP tunnel streams: stream_id → consumer sender.
  pub(crate) tcp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
  /// Active UDP relay streams (declared `protocol: udp` tunnels):
  /// stream_id → consumer sender. Same handle shape as TCP; the payloads are
  /// whole datagrams instead of stream bytes.
  pub(crate) udp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
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
  /// Hostnames currently in maintenance mode (`*` = every hostname), mapped to
  /// the organization that enabled it (`None` = master). Requests to them get a
  /// 503 page even while clients are connected. In-memory only, like bind
  /// overrides: cleared by a server restart.
  pub(crate) maintenance: Mutex<std::collections::HashMap<String, Option<String>>>,
  /// Structured access log file (APERIO_ACCESS_LOG): one JSON line per
  /// proxied request, ready for Loki/ClickHouse ingestion. The same data is
  /// always emitted as structured `aperio_access` tracing events on stdout.
  pub(crate) access_log: Option<std::sync::Mutex<std::fs::File>>,
  /// Path of the structured access log file (kept alongside the handle so
  /// the right-to-erasure purge can rewrite the file in place).
  pub(crate) access_log_path: Option<String>,
  /// Request duration histogram exposed on `/aperio/metrics`.
  pub(crate) duration_histogram: DurationHistogram,
}

/// RAII slot in the global proxied-request concurrency limit; the slot is
/// released when dropped.
pub(crate) struct RequestSlot(Arc<AtomicUsize>);

impl Drop for RequestSlot {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
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
    let old = self.config();
    let diff = crate::settings::config_reload_diff(&old, &effective);
    crate::api::settings::swap_config(self, effective).await;
    diff
  }

  /// Snapshot of the live configuration (cheap Arc clone).
  pub(crate) fn config(&self) -> Arc<ServerConfig> {
    // Recover from a poisoned lock rather than panicking: config() is on
    // essentially every proxied request, so a single panic under the write
    // lock must not turn into a total outage — the stored config is a valid
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
  /// implicit master org). Use when the event belongs to a child org — e.g. a
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
    webhooks::dispatch(subs, event, data, self.webhook_deliveries.clone());
  }

  /// Force-disconnects every live tunnel connection authenticated with the
  /// given dynamic token: their read loops end and they leave the routing pool
  /// immediately, instead of serving until they next reconnect (when the
  /// revoked token would be rejected anyway). Returns how many were dropped.
  pub(crate) async fn disconnect_token_clients(&self, token_id: &str) -> usize {
    let mut dropped = 0usize;
    {
      let clients = self.clients.lock().await;
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
}

impl AppState {
  /// In-memory thread-safe Per-IP Token Bucket Rate Limiter.
  /// Returns `true` if request is allowed, `false` if rate-limited.
  /// Enforces the serving token's optional rate limit and daily byte quota.
  /// Returns Err with a short reason when the request must be rejected with
  /// 429. Master-token traffic (token_id = None) is never limited.
  pub(crate) async fn check_token_limits(
    &self,
    token_id: Option<&str>,
  ) -> Result<(), &'static str> {
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
        return Err("token rate limit exceeded");
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
        return Err("token daily byte quota exceeded");
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

  /// The quota record for a child org (None for master or an unknown id).
  pub(crate) async fn org_quota(
    &self,
    org: Option<&str>,
  ) -> Option<crate::store::orgs::Organization> {
    let id = org?;
    self.org_store.lock().await.find(id).cloned()
  }

  // The token and user org quotas are enforced atomically inside their create
  // handlers (api/tokens.rs, api/users.rs) — the cap count and the insert run
  // under one held store lock so concurrent creates can't overshoot the cap.

  /// Enforces the org's `max_clients` quota against currently-connected
  /// clients. Err(msg) when at the cap.
  pub(crate) async fn check_org_client_quota(&self, org: Option<&str>) -> Result<(), String> {
    let Some(max) = self.org_quota(org).await.and_then(|q| q.max_clients) else {
      return Ok(());
    };
    let count = self
      .clients
      .lock()
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

  /// Enforces the per-route rate limit (`rate_limits:` section) for a request.
  /// Returns true when the request may proceed, false when the matched route's
  /// shared token bucket is empty (the caller answers 429). No configured rule
  /// for the host+path = always allowed.
  pub(crate) async fn check_route_rate_limit(&self, host: Option<&str>, path: &str) -> bool {
    let cfg = self.config();
    if cfg.route_limits.is_empty() {
      return true;
    }
    let Some(rule) = cfg.route_limits.matched(host, path) else {
      return true;
    };
    let mut buckets = self.route_rate.lock().await;
    let now = Instant::now();
    if buckets.len() > TOKEN_MAP_GC_THRESHOLD {
      buckets.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
    }
    let bucket = buckets.entry(rule.key.clone()).or_insert(RateLimitState {
      tokens: rule.burst,
      last_updated: now,
    });
    let elapsed = now.duration_since(bucket.last_updated).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rule.rps).min(rule.burst);
    bucket.last_updated = now;
    if bucket.tokens < 1.0 {
      return false;
    }
    bucket.tokens -= 1.0;
    true
  }

  pub(crate) async fn check_rate_limit(&self, ip: IpAddr) -> bool {
    let mut limit_map = self.rate_limiter.lock().await;
    let now = Instant::now();

    // Periodic garbage collection of stale IP buckets to prevent memory leak.
    // Runs at most once per 5 minutes; evicts entries untouched for over 10 minutes.
    {
      let mut last_gc = self.last_rate_gc.lock().await;
      if last_gc.elapsed() > Duration::from_secs(300) {
        limit_map.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
        *last_gc = now;
      }
    }

    // Failsafe: if the map still grew too large between GC runs, trim aggressively.
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

    if state.tokens >= 1.0 {
      state.tokens -= 1.0;
      true
    } else {
      false
    }
  }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
