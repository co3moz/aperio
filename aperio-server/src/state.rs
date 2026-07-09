use axum::extract::ws::Message;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
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
#[derive(Serialize, Clone)]
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
  /// Tunnel protocol version announced via Ping.
  pub(crate) protocol: Option<u32>,
  /// True when the announced protocol version differs from the server's.
  pub(crate) protocol_mismatch: bool,
  /// Latest backend health verdict reported by the client's own probe.
  pub(crate) backend_healthy: bool,
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
}

/// Enhanced metrics stats combined with active client details.
#[derive(Serialize, Clone)]
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
#[derive(Serialize, Clone)]
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
}

/// Maximum number of captured requests kept in memory.
pub(crate) const CAPTURE_MAX_ENTRIES: usize = 50;
/// Maximum captured body size per direction (decoded bytes).
pub(crate) const CAPTURE_BODY_LIMIT: usize = 64 * 1024;
/// Request bodies above this size are streamed to v2 clients as
/// RequestStart/Chunk/End frames instead of being buffered in memory.
pub(crate) const REQUEST_STREAM_THRESHOLD: u64 = 256 * 1024;

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
  /// Path prefix the client declared via Ping (from APERIO_PATH_BIND),
  /// validated against the token permissions.
  pub(crate) declared_path: Option<String>,
  /// Path bind granted by the token permissions when the client declared none.
  pub(crate) assigned_path: Option<String>,
  /// Hostname the client declared via Ping (from APERIO_HOSTNAME_BIND),
  /// validated against the token permissions.
  pub(crate) declared_hostname: Option<String>,
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
  /// (APERIO_CLIENT_TARGET_HEALTH). False = excluded from routing while the
  /// tunnel connection itself stays up.
  pub(crate) backend_healthy: bool,
  /// Announced load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: u32,
  /// Client-process instance ID self-reported via Ping. Unlike the
  /// server-assigned connection ID it survives reconnects of the same
  /// process, letting the failover `wait` mode recognize a returning client.
  pub(crate) reported_instance_id: Option<String>,
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
  /// Ensures the "visitor_auth requested but not permitted/invalid" warning
  /// logs once per connection.
  pub(crate) visitor_auth_denied_warned: bool,
  /// Tunnels declared by the client via Ping (`tunnels:` list): normally
  /// unexposed local services a peer client may bind with `--bind-tunnels`
  /// (same token, explicit client id required).
  pub(crate) tunnels: Vec<crate::protocol::TunnelDecl>,
  /// The client opted its service into the server-side response cache
  /// (`cache: true` via Ping). Effective only with APERIO_CACHE on.
  pub(crate) cache: bool,
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
  fn effective_hostnames(&self) -> Vec<&String> {
    if let Some(o) = &self.override_hostname_bind {
      return vec![o];
    }
    let mut set: Vec<&String> = self.assigned_hostnames.iter().collect();
    if let Some(d) = &self.declared_hostname
      && !set.contains(&d)
    {
      set.push(d);
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

/// Standard response payload returned by tunnel client.
pub(crate) struct TunnelResponse {
  /// HTTP status code.
  pub(crate) status: u16,
  /// List of response headers (preserves duplicates like Set-Cookie).
  pub(crate) headers: Vec<(String, String)>,
  /// Base64 encoded payload body (buffered responses only).
  pub(crate) body: Option<String>,
  /// For streamed responses: receiver of decoded body chunks. The proxy
  /// handler turns this into a streaming HTTP body.
  pub(crate) stream_rx: Option<mpsc::Receiver<Result<Vec<u8>, std::io::Error>>>,
}

/// Sender half of an in-flight streamed response body, kept so the tunnel
/// read loop can push chunks and so disconnect cleanup can drop it.
pub(crate) struct ResponseStreamHandle {
  pub(crate) tx: mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
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

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
pub(crate) struct SessionInfo {
  pub(crate) expires_at: Instant,
  /// When `Some(host)`, the session only authorizes proxied traffic for that
  /// exact request host — a login against a client-set visitor password. It
  /// never authorizes the dashboard or other hosts. `None` = a full/global
  /// session (server password, dashboard password, master token, or OIDC).
  pub(crate) scope_host: Option<String>,
}

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
  pub(crate) concurrency_semaphore: Semaphore,
  pub(crate) path_rr: Mutex<HashMap<RouteGroupKey, usize>>,
  pub(crate) sessions: Mutex<HashMap<String, SessionInfo>>,
  pub(crate) rate_limiter: Mutex<HashMap<IpAddr, RateLimitState>>,
  /// Escalating per-IP failed-login lockout (brute-force protection).
  pub(crate) login_lockout: Mutex<crate::auth::LockoutTracker>,
  /// Per-token request rate buckets (key = dynamic token record id),
  /// enforcing the token's optional `max_rps`.
  pub(crate) token_rate: Mutex<HashMap<String, RateLimitState>>,
  /// Per-token daily byte usage: token id → (day key, bytes). In-memory
  /// only — a restart resets the current day's usage.
  pub(crate) token_daily_bytes: Mutex<HashMap<String, (String, u64)>>,
  pub(crate) last_session_gc: Mutex<Instant>,
  pub(crate) last_rate_gc: Mutex<Instant>,
  pub(crate) active_tunnel_count: AtomicUsize,
  /// Active WebSocket proxy streams: stream_id → sender to relay tunnel WsData to public WS.
  pub(crate) ws_streams: Mutex<HashMap<String, mpsc::Sender<WsStreamMessage>>>,
  /// Pending WebSocket upgrade responses: upgrade_id → oneshot to resolve when client responds.
  pub(crate) pending_upgrades: Mutex<HashMap<String, PendingRequest>>,
  /// Persistent store of dashboard-created dynamic API tokens.
  pub(crate) token_store: Mutex<TokenStore>,
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
  /// OIDC SSO runtime config (None = feature disabled).
  pub(crate) oidc: Option<oidc::OidcRuntime>,
  /// Pending OIDC login flows: state token → (original redirect, expiry).
  pub(crate) oidc_states: Mutex<HashMap<String, (String, Instant)>>,
  /// Active experimental TCP tunnel streams: stream_id → consumer sender.
  pub(crate) tcp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
  /// Active UDP relay streams (declared `protocol: udp` tunnels):
  /// stream_id → consumer sender. Same handle shape as TCP; the payloads are
  /// whole datagrams instead of stream bytes.
  pub(crate) udp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
  /// Server-side GET response cache (APERIO_CACHE; see the cache module).
  pub(crate) response_cache: Mutex<crate::cache::ResponseCache>,
  /// Hostnames currently in maintenance mode (`*` = every hostname).
  /// Requests to them get a 503 page even while clients are connected.
  /// In-memory only, like bind overrides: cleared by a server restart.
  pub(crate) maintenance: Mutex<std::collections::HashSet<String>>,
  /// Structured access log file (APERIO_ACCESS_LOG): one JSON line per
  /// proxied request, ready for Loki/ClickHouse ingestion. The same data is
  /// always emitted as structured `aperio_access` tracing events on stdout.
  pub(crate) access_log: Option<std::sync::Mutex<std::fs::File>>,
  /// Request duration histogram exposed on `/aperio/metrics`.
  pub(crate) duration_histogram: DurationHistogram,
}

impl AppState {
  /// Snapshot of the live configuration (cheap Arc clone).
  pub(crate) fn config(&self) -> Arc<ServerConfig> {
    self
      .config_store
      .read()
      .expect("config lock poisoned")
      .clone()
  }

  /// Records an audit event (file + in-memory ring).
  pub(crate) async fn audit(&self, event: &str, actor_ip: &str, details: &str) {
    self.audit.lock().await.record(event, actor_ip, details);
  }

  /// Delivers an event to all subscribed webhooks (fire-and-forget).
  pub(crate) async fn emit_event(&self, event: &str, data: serde_json::Value) {
    let subs = self.webhook_store.lock().await.subscribers(event);
    webhooks::dispatch(subs, event, data);
  }

  /// Force-disconnects every live tunnel connection authenticated with the
  /// given dynamic token: their read loops end and they leave the routing pool
  /// immediately, instead of serving until they next reconnect (when the
  /// revoked token would be rejected anyway). Returns how many were dropped.
  pub(crate) async fn disconnect_token_clients(&self, token_id: &str) -> usize {
    let clients = self.clients.lock().await;
    let mut dropped = 0usize;
    for handle in clients.values() {
      if handle.perms.token_id.as_deref() == Some(token_id) {
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
    let entry = usage
      .entry(id.to_string())
      .or_insert_with(|| (today.clone(), 0));
    if entry.0 != today {
      *entry = (today, bytes);
    } else {
      entry.1 = entry.1.saturating_add(bytes);
    }
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
