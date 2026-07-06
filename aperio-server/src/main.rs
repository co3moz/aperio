use axum::{
  Json, Router,
  body::Body,
  extract::{
    ConnectInfo, FromRequest, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
  response::{Html, IntoResponse, Response},
  routing::{any, get},
};
use chrono::Local;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

mod audit;
mod oidc;
mod stats;
mod tokens;
mod webhooks;
use audit::AuditLog;
use stats::StatsStore;
use tokens::TokenStore;
use webhooks::WebhookStore;

/// Message structure exchanged over the WebSocket reverse tunnel.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum TunnelMessage {
  Ping {
    client_id: String,
    timestamp: u64,
    path_bind: Option<String>,
    #[serde(default)]
    hostname_bind: Option<String>,
    /// Maximum concurrent requests the client is willing to process.
    /// The server queues excess requests instead of dispatching them.
    #[serde(default)]
    max_concurrent: Option<u32>,
    /// True when the client has a TCP target configured (APERIO_CLIENT_TCP_TARGET).
    #[serde(default)]
    tcp: bool,
  },
  Pong {
    timestamp: u64,
  },
  Request {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
  },
  Response {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
  },
  /// Start of a streamed response: status and headers only. The body follows
  /// as `ResponseChunk` messages terminated by `ResponseEnd`. Used by clients
  /// for large bodies so neither side buffers the full payload in memory.
  ResponseStart {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
  },
  /// A chunk of a streamed response body (Base64 encoded).
  ResponseChunk {
    id: String,
    data: String,
  },
  /// Marks the end of a streamed response body.
  ResponseEnd {
    id: String,
  },
  /// Sent by server to instruct a client to open a WebSocket connection to the local backend.
  UpgradeRequest {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
  },
  /// Sent by client after the backend WebSocket upgrade handshake completes (or fails).
  UpgradeResponse {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
  },
  /// Bidirectional WebSocket frame relayed through the tunnel.
  WsData {
    stream_id: String,
    data: String, // Base64 for binary frames, plain text for text frames
    is_text: bool,
  },
  /// Signals that a WebSocket stream has been closed.
  WsClose {
    stream_id: String,
    code: u16,
    reason: String,
  },
  /// Server → client: informs the client of a hostname automatically
  /// assigned to it (random subdomain feature).
  HostnameAssigned {
    hostname: String,
  },
  /// Client → server: the client received a shutdown signal and is draining.
  /// The server stops routing new requests to it; in-flight requests finish.
  Draining {},
  /// Server → client: open a raw TCP connection to the client's configured
  /// TCP target for this stream (experimental TCP tunneling).
  TcpOpen {
    stream_id: String,
  },
  /// Raw TCP bytes relayed through the tunnel (Base64).
  TcpData {
    stream_id: String,
    data: String,
  },
  /// Signals that a TCP stream has been closed (either side).
  TcpClose {
    stream_id: String,
  },
  /// Server → client: offers zlib compression for subsequent tunnel frames.
  CompressionStart {},
  /// Client → server: compression accepted; both sides may now send
  /// compressed binary frames.
  CompressionAck {},
}

/// Configuration settings for the Aperio server.
#[derive(Clone)]
struct ServerConfig {
  token: String,
  gateway_timeout: Duration,
  gateway_response_timeout: Duration,
  max_body_size: usize,
  max_tunnels: usize,
  ip_limit_max: f64,
  ip_limit_refill: f64,
  auth_credentials: Option<String>,
  /// When true, the server trusts `X-Forwarded-For` / `X-Real-IP` headers for
  /// client IP resolution. Only enable when running behind a trusted reverse
  /// proxy, otherwise clients can spoof these headers to bypass rate limiting.
  trust_proxy: bool,
  /// When true, session cookies include the `Secure` flag so browsers only
  /// send them over HTTPS connections. Defaults to the value of `trust_proxy`
  /// (i.e. enabled when running behind a TLS-terminating reverse proxy).
  secure_cookies: bool,
  /// When true, only clients that declared (or were overruled with) a
  /// hostname bind participate in load balancing. Clients without a hostname
  /// bind never receive proxied traffic.
  require_hostname_bind: bool,
  /// Optional bearer token required to scrape the `/aperio/metrics` endpoint.
  metrics_token: Option<String>,
  /// Domain suffix for automatic random subdomains (from
  /// `APERIO_RANDOM_SUBDOMAIN="*.example.com"`). When set, every connecting
  /// client is assigned `<random>.<suffix>` in addition to any token-granted
  /// or declared hostname binds.
  random_subdomain_suffix: Option<String>,
  /// A client whose last heartbeat is older than this is considered down and
  /// removed from the load-balancing pool until it pings again.
  client_down_threshold: Duration,
  /// When true, the server offers zlib compression to connecting clients;
  /// tunnel frames are compressed once the client acknowledges.
  tunnel_compression: bool,
  /// Custom HTML page served on 504 gateway-timeout responses
  /// (loaded once from APERIO_504_PAGE at startup).
  custom_504_page: Option<String>,
}

/// In-memory server-wide traffic statistics.
#[derive(Serialize, Clone)]
struct ServerStats {
  /// Total count of incoming proxied requests.
  total_requests: u64,
  /// Count of successful request forwards.
  successful_requests: u64,
  /// Count of failed request forwards.
  failed_requests: u64,
  /// Total bytes of payloads transferred through the server.
  total_bytes_transferred: u64,
}

/// Details of an active tunnel client connection.
#[derive(Serialize, Clone)]
struct ClientDetail {
  /// Unique client UUID.
  id: String,
  /// Remote socket IP address of the client connection.
  ip: String,
  /// Number of seconds elapsed since connection establishment.
  connected_for_seconds: u64,
  /// Total request count processed by this client connection.
  request_count: u64,
  /// Path bind in effect (declared by the client or granted by its token).
  path_bind: Option<String>,
  /// Hostnames in effect (declared, token-granted, and random-subdomain).
  hostname_binds: Vec<String>,
  /// Name of the dynamic token this client authenticated with (None = master).
  token_name: Option<String>,
  /// Temporary server-side path bind override (dashboard overrule).
  override_path_bind: Option<String>,
  /// Temporary server-side hostname bind override (dashboard overrule).
  override_hostname_bind: Option<String>,
  /// Seconds elapsed since the last heartbeat Ping was received.
  last_ping_seconds_ago: Option<u64>,
  /// Concurrency limit announced by the client (None = unlimited).
  max_concurrent: Option<u32>,
  /// False when the client missed its heartbeat window and is out of the pool.
  healthy: bool,
  /// True while the client is gracefully draining before shutdown.
  draining: bool,
  /// Dashboard kill switch state (false = excluded from routing).
  enabled: bool,
}

/// Enhanced metrics stats combined with active client details.
#[derive(Serialize, Clone)]
struct EnhancedServerStats {
  /// Total incoming request count.
  total_requests: u64,
  /// Successful requests count.
  successful_requests: u64,
  /// Failed requests count.
  failed_requests: u64,
  /// Total bytes transferred.
  total_bytes_transferred: u64,
  /// Current count of connected tunnel clients.
  connected_clients_count: usize,
  /// Uptime in seconds.
  uptime_seconds: u64,
  /// Request count waiting in the reconnection buffer.
  pending_requests_count: usize,
  /// List of client connection details.
  active_clients: Vec<ClientDetail>,
  /// Restart-surviving counters and period buckets.
  persistent: stats::PersistentStats,
  /// All-time average response time in milliseconds.
  avg_response_ms: f64,
  /// Today.s traffic bucket.
  today: stats::PeriodStats,
}

/// Structure representing a logged HTTP transaction.
#[derive(Serialize, Clone)]
struct RequestLog {
  /// Request UUID.
  id: String,
  /// Timestamp formatted as string.
  timestamp: String,
  /// HTTP method (GET, POST, etc.).
  method: String,
  /// Request URI path.
  uri: String,
  /// Status code returned.
  status: Option<u16>,
  /// Duration of processing in milliseconds.
  duration_ms: u128,
  /// Reason string if request failed.
  error: Option<String>,
}

/// A fully captured HTTP transaction for the dashboard inspector. Bodies are
/// capped at [`CAPTURE_BODY_LIMIT`] bytes; larger bodies are truncated for
/// display and cannot be replayed.
#[derive(Serialize, Clone)]
struct CapturedRequest {
  /// Request UUID (matches the RequestLog id).
  id: String,
  /// Timestamp formatted as string.
  timestamp: String,
  method: String,
  /// Full request URI including query string.
  uri: String,
  /// Request headers as forwarded to the tunnel client.
  req_headers: Vec<(String, String)>,
  /// Base64 request body (possibly truncated).
  req_body: Option<String>,
  /// True when the request body exceeded the capture limit.
  req_body_truncated: bool,
  status: u16,
  resp_headers: Vec<(String, String)>,
  /// Base64 response body (buffered responses only, possibly truncated).
  resp_body: Option<String>,
  resp_body_truncated: bool,
  /// True when the response body was streamed (not captured).
  resp_streamed: bool,
  duration_ms: u128,
}

/// Maximum number of captured requests kept in memory.
const CAPTURE_MAX_ENTRIES: usize = 50;
/// Maximum captured body size per direction (decoded bytes).
const CAPTURE_BODY_LIMIT: usize = 64 * 1024;

/// Handle tracking active WebSocket sender channel and metadata.
struct ClientHandle {
  /// Sender channel to push messages to the client.
  tx: mpsc::Sender<Message>,
  /// Instant when client connection was established.
  connected_at: Instant,
  /// Client remote IP address.
  client_ip: String,
  /// Total request count processed by this specific client connection.
  request_count: Arc<AtomicU64>,
  /// Path prefix the client declared via Ping (from APERIO_PATH_BIND),
  /// validated against the token permissions.
  declared_path: Option<String>,
  /// Path bind granted by the token permissions when the client declared none.
  assigned_path: Option<String>,
  /// Hostname the client declared via Ping (from APERIO_HOSTNAME_BIND),
  /// validated against the token permissions.
  declared_hostname: Option<String>,
  /// Hostnames granted automatically: token-bound hostnames and/or the
  /// randomly assigned subdomain.
  assigned_hostnames: Vec<String>,
  /// Temporary path bind override set from the dashboard. Not persisted:
  /// lost when the client reconnects or the server restarts.
  override_path_bind: Option<String>,
  /// Temporary hostname bind override set from the dashboard. Not persisted.
  override_hostname_bind: Option<String>,
  /// Instant of the last heartbeat Ping received from this client.
  last_ping_at: Option<Instant>,
  /// Permissions attached to the token this client authenticated with.
  perms: ClientPerms,
  /// Announced concurrency limit of the client (from Ping), for display.
  max_concurrent: Option<u32>,
  /// Semaphore enforcing the client's announced concurrency limit. Requests
  /// beyond the limit wait here (bounded by the gateway timeout) instead of
  /// being dispatched, so the server never floods the client's backend.
  inflight_limiter: Option<Arc<Semaphore>>,
  /// True after the client announced a graceful shutdown: no new requests
  /// are routed to it while in-flight ones finish.
  draining: bool,
  /// Dashboard kill switch: false = temporarily excluded from routing even
  /// though the connection and heartbeats remain healthy.
  admin_enabled: bool,
  /// True when the client announced a TCP target (experimental TCP tunneling).
  tcp_enabled: bool,
}

/// Permissions resolved at connection time from the presented token.
#[derive(Clone)]
struct ClientPerms {
  /// True for the master `APERIO_SERVER_TOKEN`: no restrictions.
  master: bool,
  /// Allowed hostname binds. Empty or containing "*" = unrestricted.
  hostnames: Vec<String>,
  /// Allowed path binds. Empty or containing "*" = unrestricted.
  paths: Vec<String>,
  /// Name of the dynamic token used (None for the master token).
  token_name: Option<String>,
}

impl ClientPerms {
  fn master() -> Self {
    ClientPerms {
      master: true,
      hostnames: Vec::new(),
      paths: Vec::new(),
      token_name: None,
    }
  }

  fn hostname_allowed(&self, host: &str) -> bool {
    self.master
      || self.hostnames.is_empty()
      || self.hostnames.iter().any(|h| h == "*" || h == host)
  }

  fn path_allowed(&self, path: &str) -> bool {
    self.master || self.paths.is_empty() || self.paths.iter().any(|p| p == "*" || p == path)
  }

  /// Specific (non-wildcard) hostnames granted by the token; these are
  /// auto-bound to the client on connect.
  fn granted_hostnames(&self) -> Vec<String> {
    self
      .hostnames
      .iter()
      .filter(|h| *h != "*")
      .cloned()
      .collect()
  }

  /// First specific path granted by the token, used as the automatic path
  /// bind when the client did not declare one.
  fn granted_path(&self) -> Option<String> {
    self.paths.iter().find(|p| *p != "*").cloned()
  }
}

impl ClientHandle {
  /// Path bind used for routing: dashboard override wins over the declared
  /// value, which wins over the token-granted value.
  fn effective_path_bind(&self) -> Option<&String> {
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

  fn matches_host(&self, host: &str) -> bool {
    self.effective_hostnames().iter().any(|h| h.as_str() == host)
  }

  fn has_hostname_bind(&self) -> bool {
    !self.effective_hostnames().is_empty()
  }

  /// A client is healthy while its last heartbeat (or, before the first
  /// Ping, its connection time) is within the down threshold.
  fn is_healthy(&self, down_threshold: Duration) -> bool {
    let reference = self.last_ping_at.unwrap_or(self.connected_at);
    reference.elapsed() < down_threshold
  }
}

/// Round-robin group key: (hostname group, path group) of the selected pool.
type RouteGroupKey = (Option<String>, Option<String>);

/// Standard response payload returned by tunnel client.
struct TunnelResponse {
  /// HTTP status code.
  status: u16,
  /// List of response headers (preserves duplicates like Set-Cookie).
  headers: Vec<(String, String)>,
  /// Base64 encoded payload body (buffered responses only).
  body: Option<String>,
  /// For streamed responses: receiver of decoded body chunks. The proxy
  /// handler turns this into a streaming HTTP body.
  stream_rx: Option<mpsc::Receiver<Result<Vec<u8>, std::io::Error>>>,
}

/// Sender half of an in-flight streamed response body, kept so the tunnel
/// read loop can push chunks and so disconnect cleanup can drop it.
struct ResponseStreamHandle {
  tx: mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
  client_id: String,
}

/// Message relayed from the tunnel to a public TCP consumer WebSocket.
enum TcpConsumerMsg {
  Data(Vec<u8>),
  Close,
}

/// Handle to an active TCP tunnel stream (consumer side).
struct TcpStreamHandle {
  tx: mpsc::Sender<TcpConsumerMsg>,
  client_id: String,
}

/// Structure tracking requests waiting for client execution.
struct PendingRequest {
  /// Oneshot channel sender to return client response to proxy handler thread.
  tx: oneshot::Sender<TunnelResponse>,
  /// Target client UUID.
  client_id: String,
}

/// A WebSocket frame relayed from the tunnel client, to be forwarded to the public WS.
enum WsStreamMessage {
  /// A data frame (text or binary) to forward to the public WebSocket.
  Data(Message),
  /// Close the public WebSocket stream.
  Close,
}

/// Bucket tracking current tokens and refill state for rate limiting.
struct RateLimitState {
  /// Current token balance.
  tokens: f64,
  /// Last instant when tokens were updated.
  last_updated: Instant,
}

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
struct SessionInfo {
  expires_at: Instant,
}

/// Connection liveness state, kept under a single lock for consistent snapshots.
struct ConnectionState {
  connected: bool,
  last_disconnect: Option<Instant>,
}

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
struct AppState {
  clients: Mutex<HashMap<String, ClientHandle>>,
  client_connected: watch::Sender<bool>,
  connection_state: Mutex<ConnectionState>,
  server_start_time: Instant,
  pending_requests: Mutex<HashMap<String, PendingRequest>>,
  stats: Mutex<ServerStats>,
  recent_logs: Mutex<VecDeque<RequestLog>>,
  config: ServerConfig,
  concurrency_semaphore: Semaphore,
  path_rr: Mutex<HashMap<RouteGroupKey, usize>>,
  sessions: Mutex<HashMap<String, SessionInfo>>,
  rate_limiter: Mutex<HashMap<IpAddr, RateLimitState>>,
  last_session_gc: Mutex<Instant>,
  last_rate_gc: Mutex<Instant>,
  active_tunnel_count: AtomicUsize,
  /// Active WebSocket proxy streams: stream_id → sender to relay tunnel WsData to public WS.
  ws_streams: Mutex<HashMap<String, mpsc::Sender<WsStreamMessage>>>,
  /// Pending WebSocket upgrade responses: upgrade_id → oneshot to resolve when client responds.
  pending_upgrades: Mutex<HashMap<String, PendingRequest>>,
  /// Persistent store of dashboard-created dynamic API tokens.
  token_store: Mutex<TokenStore>,
  /// In-flight streamed response bodies: request_id → chunk sender.
  response_streams: Mutex<HashMap<String, ResponseStreamHandle>>,
  /// Recently captured HTTP transactions for the dashboard inspector.
  captured_requests: Mutex<VecDeque<CapturedRequest>>,
  /// Persistent audit log of administrative/security events.
  audit: Mutex<AuditLog>,
  /// Restart-surviving traffic statistics (all-time + period buckets).
  persistent_stats: Mutex<StatsStore>,
  /// Persistent webhook definitions for the event system.
  webhook_store: Mutex<WebhookStore>,
  /// OIDC SSO runtime config (None = feature disabled).
  oidc: Option<oidc::OidcRuntime>,
  /// Pending OIDC login flows: state token → (original redirect, expiry).
  oidc_states: Mutex<HashMap<String, (String, Instant)>>,
  /// Active experimental TCP tunnel streams: stream_id → consumer sender.
  tcp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
}

impl AppState {
  /// Records an audit event (file + in-memory ring).
  async fn audit(&self, event: &str, actor_ip: &str, details: &str) {
    self.audit.lock().await.record(event, actor_ip, details);
  }

  /// Delivers an event to all subscribed webhooks (fire-and-forget).
  async fn emit_event(&self, event: &str, data: serde_json::Value) {
    let subs = self.webhook_store.lock().await.subscribers(event);
    webhooks::dispatch(subs, event, data);
  }
}

impl AppState {
  /// In-memory thread-safe Per-IP Token Bucket Rate Limiter.
  /// Returns `true` if request is allowed, `false` if rate-limited.
  async fn check_rate_limit(&self, ip: IpAddr) -> bool {
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

    let max_tokens = self.config.ip_limit_max;
    let refill_rate = self.config.ip_limit_refill;

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

#[tokio::main]
/// Entry point for the Aperio server.
/// Sets up logging, reads env config, registers paths/middleware, and binds the TCP listener.
async fn main() {
  // Initialize tracing with structured JSON output (pino.js style)
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });

  tracing_subscriber::fmt()
    .json()
    .with_current_span(false)
    .with_span_list(false)
    .flatten_event(true)
    .with_env_filter(log_filter)
    .init();

  info!("Starting Aperio Server...");

  // Enforce APERIO_SERVER_TOKEN environment variable
  let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_else(|_| {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN environment variable must be set!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN cannot be empty!");
    std::process::exit(1);
  }

  let gateway_timeout_secs = std::env::var("APERIO_SERVER_GATEWAY_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(10);

  let gateway_response_timeout_secs = std::env::var("APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(30);

  // Limit on max request body size (default: 10MB)
  let max_body_size = std::env::var("APERIO_MAX_BODY_SIZE")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10 * 1024 * 1024);

  // Concurrency limit on tunnel requests (default: 100 concurrent)
  let max_concurrent_requests = std::env::var("APERIO_MAX_CONCURRENT_REQUESTS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(100);

  // Max connected tunnel clients limit (default: 10 active clients)
  let max_tunnels = std::env::var("APERIO_MAX_TUNNELS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10);

  // Max IP token bucket capacity burst (default: 100 requests)
  let ip_limit_max = std::env::var("APERIO_IP_LIMIT_MAX")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .unwrap_or(100.0);

  // IP token bucket refill rate per second (default: 5.0 requests/sec, which is 300 req/min)
  let ip_limit_refill = std::env::var("APERIO_IP_LIMIT_REFILL")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .unwrap_or(5.0);

  // Optional Basic Auth credentials for proxied requests ("username:password")
  let auth_credentials = std::env::var("APERIO_SERVER_AUTH").ok();

  // Trust proxy headers (X-Forwarded-For / X-Real-IP) for client IP resolution.
  // Only enable when running behind a trusted reverse proxy that overwrites
  // these headers; otherwise clients can spoof them to bypass rate limiting.
  let trust_proxy = std::env::var("APERIO_TRUST_PROXY")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // When true, session cookies include the `Secure` flag (HTTPS-only).
  // Defaults to `trust_proxy` since a TLS-terminating reverse proxy implies HTTPS.
  let secure_cookies = std::env::var("APERIO_SECURE_COOKIES")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(trust_proxy);

  // When enabled, clients that did not declare a hostname bind (and were not
  // given one via dashboard overrule) are excluded from load balancing.
  let require_hostname_bind = std::env::var("APERIO_REQUIRE_HOSTNAME_BIND")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // Prometheus metrics endpoint (default: disabled). Auth is always required:
  // either APERIO_METRICS_TOKEN, or a random token generated once and
  // persisted in the data directory (a truly public metrics endpoint brings
  // no benefit and leaks operational details).
  let metrics_enabled = std::env::var("APERIO_METRICS")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let metrics_token = std::env::var("APERIO_METRICS_TOKEN")
    .ok()
    .filter(|t| !t.trim().is_empty());

  // Tunnel frame compression (zlib). Offered to clients on connect; enabled
  // per connection once the client acknowledges support.
  let tunnel_compression = std::env::var("APERIO_TUNNEL_COMPRESSION")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if tunnel_compression {
    info!("Tunnel compression is enabled (zlib per-message)");
  }

  // Optional custom 504 error page (e.g. APERIO_504_PAGE=/app/error_504.html).
  // Loaded once at startup; on read failure the default plain-text 504 is kept.
  let custom_504_page = std::env::var("APERIO_504_PAGE").ok().and_then(|path| {
    match std::fs::read_to_string(&path) {
      Ok(html) => {
        info!("Custom 504 page loaded from {}", path);
        Some(html)
      }
      Err(e) => {
        error!("Failed to read APERIO_504_PAGE {}: {} — using default 504 text", path, e);
        None
      }
    }
  });

  // Heartbeat-based health: clients whose last Ping is older than this many
  // seconds are treated as down and excluded from load balancing.
  let client_down_threshold_secs = std::env::var("APERIO_CLIENT_DOWN_THRESHOLD")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .filter(|n| *n > 0)
    .unwrap_or(15);

  // Random subdomain assignment: APERIO_RANDOM_SUBDOMAIN="*.example.com"
  // gives every connecting client a random hostname under that suffix.
  let random_subdomain_suffix = std::env::var("APERIO_RANDOM_SUBDOMAIN")
    .ok()
    .and_then(|val| {
      let trimmed = val.trim();
      let suffix = trimmed.strip_prefix("*.").unwrap_or(trimmed);
      match normalize_hostname_bind(suffix) {
        Some(s) => Some(s),
        None => {
          error!(
            "Invalid APERIO_RANDOM_SUBDOMAIN value '{}' (expected e.g. \"*.example.com\"); ignoring",
            val
          );
          None
        }
      }
    });
  if let Some(ref suffix) = random_subdomain_suffix {
    info!(
      "Random subdomain assignment enabled: every client gets <random>.{}",
      suffix
    );
  }

  // Data directory for persisted state (dynamic tokens, etc.). In Docker,
  // mount a volume here (e.g. ./data:/app/data) so tokens survive restarts.
  let data_dir = std::env::var("APERIO_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
  let token_store = TokenStore::load(&data_dir);

  // Resolve the effective metrics token: env var wins; otherwise generate a
  // random token once and persist it so every restart uses the same value.
  let metrics_token = if metrics_enabled && metrics_token.is_none() {
    let path = std::path::Path::new(&data_dir).join("metrics_token");
    let persisted = std::fs::read_to_string(&path)
      .ok()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
    match persisted {
      Some(tok) => {
        warn!(
          "APERIO_METRICS_TOKEN not set; using the persisted random metrics token from {:?}. \
           Scrape with /aperio/metrics?token=<token> or an Authorization: Bearer header.",
          path
        );
        Some(tok)
      }
      None => {
        let tok = format!("mtr_{}", uuid::Uuid::new_v4().simple());
        if let Err(e) = std::fs::write(&path, &tok) {
          error!("Failed to persist generated metrics token to {:?}: {}", path, e);
        }
        warn!(
          "APERIO_METRICS_TOKEN not set; generated a random metrics token: {} (persisted in {:?}). \
           Scrape with /aperio/metrics?token=<token>. This value is logged only on first generation.",
          tok, path
        );
        Some(tok)
      }
    }
  } else {
    metrics_token
  };

  let config = ServerConfig {
    token: token.clone(),
    gateway_timeout: Duration::from_secs(gateway_timeout_secs),
    gateway_response_timeout: Duration::from_secs(gateway_response_timeout_secs),
    max_body_size,
    max_tunnels,
    ip_limit_max,
    ip_limit_refill,
    auth_credentials,
    trust_proxy,
    secure_cookies,
    require_hostname_bind,
    metrics_token,
    random_subdomain_suffix,
    client_down_threshold: Duration::from_secs(client_down_threshold_secs),
    tunnel_compression,
    custom_504_page,
  };

  if require_hostname_bind {
    info!("Hostname bind requirement is ENABLED: clients without a hostname bind will not receive traffic.");
  }

  // OIDC SSO configuration (optional).
  let oidc_runtime = oidc::load_from_env().await;

  let (client_connected_tx, _) = watch::channel(false);

  let state = Arc::new(AppState {
    clients: Mutex::new(HashMap::new()),
    client_connected: client_connected_tx,
    connection_state: Mutex::new(ConnectionState {
      connected: false,
      last_disconnect: None,
    }),
    server_start_time: Instant::now(),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 0,
      successful_requests: 0,
      failed_requests: 0,
      total_bytes_transferred: 0,
    }),
    recent_logs: Mutex::new(VecDeque::with_capacity(100)),
    config,
    concurrency_semaphore: Semaphore::new(max_concurrent_requests),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(HashMap::new()),
    rate_limiter: Mutex::new(HashMap::new()),
    last_session_gc: Mutex::new(Instant::now()),
    last_rate_gc: Mutex::new(Instant::now()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(token_store),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::with_capacity(CAPTURE_MAX_ENTRIES)),
    audit: Mutex::new(AuditLog::load(&data_dir)),
    persistent_stats: Mutex::new(StatsStore::load(&data_dir)),
    webhook_store: Mutex::new(WebhookStore::load(&data_dir)),
    oidc: oidc_runtime,
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
  });

  let mut app = Router::new().fallback(any(proxy_handler));

  // Dashboard defaults to enabled. Set APERIO_DASHBOARD=0 to disable.
  let dashboard_enabled = !std::env::var("APERIO_DASHBOARD")
    .map(|val| val == "0" || val.to_lowercase() == "false")
    .unwrap_or(false);

  if dashboard_enabled {
    let dashboard_auth = std::env::var("APERIO_DASHBOARD_AUTH").ok();
    if dashboard_auth.is_none() || dashboard_auth.as_ref().unwrap().trim().is_empty() {
      warn!(
        "APERIO_DASHBOARD is enabled but APERIO_DASHBOARD_AUTH is not set! \
           The dashboard can still be accessed using aperio:<APERIO_SERVER_TOKEN>."
      );
    }

    let mut dash_router = Router::new()
      .route("/", get(dashboard_handler))
      .route("/api/stats", get(stats_handler))
      .route("/api/logs", get(logs_handler))
      .route(
        "/api/clients/:id/override",
        axum::routing::post(client_override_handler),
      )
      .route(
        "/api/clients/:id/enabled",
        axum::routing::post(client_enabled_handler),
      )
      .route(
        "/api/tokens",
        get(tokens_list_handler).post(tokens_create_handler),
      )
      .route(
        "/api/tokens/:id",
        axum::routing::put(tokens_update_handler).delete(tokens_revoke_handler),
      )
      .route("/api/requests/:id", get(request_detail_handler))
      .route(
        "/api/requests/:id/replay",
        axum::routing::post(request_replay_handler),
      )
      .route("/api/audit", get(audit_handler))
      .route(
        "/api/webhooks",
        get(webhooks_list_handler).post(webhooks_create_handler),
      )
      .route(
        "/api/webhooks/:id",
        axum::routing::delete(webhooks_delete_handler),
      );

    let state_clone = state.clone();
    dash_router = dash_router.layer(axum::middleware::from_fn(
      move |req: axum::extract::Request, next: axum::middleware::Next| {
        let state = state_clone.clone();
        async move {
          // Check for valid session cookie
          if validate_session(&state, req.headers()).await {
            return next.run(req).await;
          }
          // Redirect to login page, preserving the original path
          let redirect_url = format!(
            "/aperio/auth?redirect={}",
            safe_redirect_path(req.uri().path())
          );
          Response::builder()
            .status(StatusCode::FOUND)
            .header("Location", redirect_url)
            .body(Body::empty())
            .unwrap()
        }
      },
    ));

    app = app.nest("/aperio", dash_router);
  }

  // Health endpoint is intentionally registered outside the dashboard auth
  // middleware so that external load balancers / monitoring tools can probe
  // server liveness without dashboard credentials.
  app = app.route("/aperio/health", get(health_handler));
  app = app.route(
    "/aperio/auth",
    get(auth_page_handler).post(auth_login_handler),
  );
  app = app.route("/aperio/ws", get(ws_handler));
  app = app.route("/aperio/tcp", get(tcp_ws_handler));
  app = app.route("/aperio/oidc/login", get(oidc_login_handler));
  app = app.route("/aperio/oidc/callback", get(oidc_callback_handler));

  // Prometheus metrics endpoint, registered outside the dashboard session
  // middleware. Access control is handled by APERIO_METRICS_TOKEN if set.
  if metrics_enabled {
    app = app.route("/aperio/metrics", get(metrics_handler));
    info!("Prometheus metrics endpoint enabled at /aperio/metrics");
  }

  // Flush persistent stats periodically and once more on shutdown.
  let stats_flush_state = state.clone();
  tokio::spawn(async move {
    loop {
      tokio::time::sleep(Duration::from_secs(30)).await;
      stats_flush_state
        .persistent_stats
        .lock()
        .await
        .save_if_dirty();
    }
  });
  let shutdown_state = state.clone();

  let app = app.with_state(state);

  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

  let port = std::env::var("PORT")
    .ok()
    .and_then(|p| p.parse::<u16>().ok())
    .unwrap_or(8080);

  let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
    .await
    .unwrap();

  info!(
    "Server listening on {}:{} with connection info tracing enabled",
    host, port
  );

  axum::serve(
    listener,
    app.into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal())
  .await
  .unwrap();

  // Final stats flush so nothing recorded since the last tick is lost.
  shutdown_state.persistent_stats.lock().await.save_if_dirty();
}

/// Graceful shutdown listener for receiving SIGINT or SIGTERM signals.
async fn shutdown_signal() {
  let ctrl_c = async {
    tokio::signal::ctrl_c()
      .await
      .expect("Failed to install Ctrl+C handler");
  };

  #[cfg(unix)]
  let terminate = async {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      .expect("Failed to install SIGTERM handler")
      .recv()
      .await;
  };

  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
      _ = ctrl_c => {},
      _ = terminate => {},
  }

  info!("Shutdown signal received, closing Aperio Server connections...");
}

/// Handler serving the embedded HTML dashboard dashboard.
async fn dashboard_handler() -> Html<&'static str> {
  Html(include_str!("dashboard.html"))
}

/// Handler returning live statistics and active connections detail in JSON.
async fn stats_handler(State(state): State<Arc<AppState>>) -> Json<EnhancedServerStats> {
  let raw_stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;

  let active_clients = clients
    .iter()
    .map(|(id, handle)| ClientDetail {
      id: id.clone(),
      ip: handle.client_ip.clone(),
      connected_for_seconds: handle.connected_at.elapsed().as_secs(),
      request_count: handle.request_count.load(Ordering::SeqCst),
      path_bind: handle
        .declared_path
        .clone()
        .or_else(|| handle.assigned_path.clone()),
      hostname_binds: {
        let mut set = handle.assigned_hostnames.clone();
        if let Some(d) = &handle.declared_hostname
          && !set.contains(d)
        {
          set.push(d.clone());
        }
        set
      },
      token_name: handle.perms.token_name.clone(),
      override_path_bind: handle.override_path_bind.clone(),
      override_hostname_bind: handle.override_hostname_bind.clone(),
      last_ping_seconds_ago: handle.last_ping_at.map(|t| t.elapsed().as_secs()),
      max_concurrent: handle.max_concurrent,
      healthy: handle.is_healthy(state.config.client_down_threshold),
      draining: handle.draining,
      enabled: handle.admin_enabled,
    })
    .collect();

  let pending_count = state.pending_requests.lock().await.len();
  let persistent = state.persistent_stats.lock().await.snapshot();
  let avg_response_ms = persistent.avg_response_ms();
  let today = persistent
    .periods
    .get(&stats::period_keys()[0])
    .cloned()
    .unwrap_or_default();

  Json(EnhancedServerStats {
    total_requests: raw_stats.total_requests,
    successful_requests: raw_stats.successful_requests,
    failed_requests: raw_stats.failed_requests,
    total_bytes_transferred: raw_stats.total_bytes_transferred,
    connected_clients_count: clients.len(),
    uptime_seconds: state.server_start_time.elapsed().as_secs(),
    pending_requests_count: pending_count,
    active_clients,
    persistent,
    avg_response_ms,
    today,
  })
}

/// Handler returning the list of recent HTTP logs in JSON.
async fn logs_handler(State(state): State<Arc<AppState>>) -> Json<Vec<RequestLog>> {
  let logs = state.recent_logs.lock().await;
  Json(logs.iter().cloned().collect())
}

/// Request payload for the dashboard client override (overrule) endpoint.
/// Each field fully replaces the corresponding override: a non-empty string
/// sets it, an empty string or `null` clears it. Overrides are in-memory only
/// and disappear when the client reconnects or the server restarts.
#[derive(Deserialize)]
struct ClientOverrideRequest {
  hostname_bind: Option<String>,
  path_bind: Option<String>,
}

/// Applies a temporary hostname/path bind override to a connected client.
/// Protected by the dashboard session middleware.
async fn client_override_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientOverrideRequest>,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  // Validate before mutating: reject invalid values with 400.
  let new_hostname = match payload.hostname_bind.as_deref() {
    None | Some("") => None,
    Some(raw) => match normalize_hostname_bind(raw) {
      Some(h) => Some(h),
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid hostname_bind value").into_response();
      }
    },
  };
  let new_path = match payload.path_bind.as_deref() {
    None | Some("") => None,
    Some(raw) => match normalize_path_bind(raw) {
      Some(p) => Some(p),
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid path_bind value").into_response();
      }
    },
  };

  let found = {
    let mut clients = state.clients.lock().await;
    match clients.get_mut(&client_id) {
      Some(handle) => {
        handle.override_hostname_bind = new_hostname.clone();
        handle.override_path_bind = new_path.clone();
        true
      }
      None => false,
    }
  };
  if found {
    info!(
      "Dashboard overrule applied to client {}: hostname_bind={:?} path_bind={:?}",
      client_id, new_hostname, new_path
    );
    state
      .audit(
        "client_overrule",
        &actor_ip,
        &format!(
          "client={} hostname={:?} path={:?}",
          client_id, new_hostname, new_path
        ),
      )
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

/// Returns recent audit events (dashboard).
async fn audit_handler(State(state): State<Arc<AppState>>) -> Json<Vec<audit::AuditEvent>> {
  Json(state.audit.lock().await.recent())
}

/// Payload for creating a webhook definition.
#[derive(Deserialize)]
struct WebhookCreateRequest {
  name: String,
  url: String,
  /// Subscribed events; `["*"]` (or empty) = all events.
  #[serde(default)]
  events: Vec<String>,
}

/// Lists webhook definitions.
async fn webhooks_list_handler(State(state): State<Arc<AppState>>) -> Json<Vec<webhooks::Webhook>> {
  Json(state.webhook_store.lock().await.list().to_vec())
}

/// Creates a webhook definition. Only http/https URLs are accepted.
async fn webhooks_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<WebhookCreateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  let name = payload.name.trim().to_string();
  if name.is_empty() || name.len() > 64 {
    return (StatusCode::BAD_REQUEST, "Webhook name must be 1-64 characters").into_response();
  }
  let url = payload.url.trim().to_string();
  if !(url.starts_with("http://") || url.starts_with("https://")) {
    return (StatusCode::BAD_REQUEST, "Webhook URL must be http(s)").into_response();
  }
  let events: Vec<String> = payload
    .events
    .iter()
    .map(|e| e.trim().to_string())
    .filter(|e| !e.is_empty())
    .collect();

  let hook = state.webhook_store.lock().await.create(name, url, events);
  info!("Webhook created: {} -> {}", hook.name, hook.url);
  state
    .audit(
      "webhook_created",
      &actor_ip,
      &format!("name={} url={} events={:?}", hook.name, hook.url, hook.events),
    )
    .await;
  Json(serde_json::json!({"status": "ok", "id": hook.id})).into_response()
}

/// Deletes a webhook definition.
async fn webhooks_delete_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  if state.webhook_store.lock().await.delete(&id) {
    state
      .audit("webhook_deleted", &actor_ip, &format!("id={}", id))
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Webhook not found").into_response()
  }
}

/// Payload for the client enable/disable toggle.
#[derive(Deserialize)]
struct ClientEnabledRequest {
  enabled: bool,
}

/// Dashboard kill switch: temporarily removes a connected client from the
/// routing pool (or puts it back). In-flight requests always complete.
async fn client_enabled_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientEnabledRequest>,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  let found = {
    let mut clients = state.clients.lock().await;
    match clients.get_mut(&client_id) {
      Some(handle) => {
        handle.admin_enabled = payload.enabled;
        true
      }
      None => false,
    }
  };
  if found {
    info!(
      "Client {} {} via dashboard",
      client_id,
      if payload.enabled { "enabled" } else { "disabled" }
    );
    state
      .audit(
        if payload.enabled {
          "client_enabled"
        } else {
          "client_disabled"
        },
        &actor_ip,
        &format!("client={}", client_id),
      )
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

/// Public view of a dynamic token record (never includes hash or secret).
#[derive(Serialize)]
struct TokenView {
  id: String,
  name: String,
  token_prefix: String,
  hostnames: Vec<String>,
  paths: Vec<String>,
  allowed_ips: Vec<String>,
  created_at: u64,
  expires_at: Option<u64>,
  expired: bool,
}

/// Lists dynamic API tokens (metadata only, secrets are never returned).
async fn tokens_list_handler(State(state): State<Arc<AppState>>) -> Json<Vec<TokenView>> {
  let store = state.token_store.lock().await;
  let views = store
    .list()
    .iter()
    .map(|t| TokenView {
      id: t.id.clone(),
      name: t.name.clone(),
      token_prefix: t.token_prefix.clone(),
      hostnames: t.hostnames.clone(),
      paths: t.paths.clone(),
      allowed_ips: t.allowed_ips.clone(),
      created_at: t.created_at,
      expires_at: t.expires_at,
      expired: t.is_expired(),
    })
    .collect();
  Json(views)
}

/// Payload for creating a dynamic token from the dashboard.
#[derive(Deserialize)]
struct TokenCreateRequest {
  name: String,
  /// Allowed hostnames; `["*"]` (or empty) = all hostnames.
  #[serde(default)]
  hostnames: Vec<String>,
  /// Allowed path binds; `["*"]` (or empty) = all paths.
  #[serde(default)]
  paths: Vec<String>,
  /// Source IPs/CIDRs allowed to connect. Defaults to `["0.0.0.0/0"]` (any).
  #[serde(default)]
  allowed_ips: Vec<String>,
  /// Optional lifetime in seconds; omitted = never expires.
  ttl_seconds: Option<u64>,
}

/// Payload for editing an existing token's scope without changing the secret.
/// Absent fields are left untouched; `ttl_seconds: 0` clears the expiry.
#[derive(Deserialize)]
struct TokenUpdateRequest {
  name: Option<String>,
  hostnames: Option<Vec<String>>,
  paths: Option<Vec<String>>,
  allowed_ips: Option<Vec<String>>,
  /// Some(0) = never expires; Some(n) = expires n seconds from now.
  ttl_seconds: Option<u64>,
}

/// Normalized (hostnames, paths, allowed_ips) permission lists.
type TokenPermLists = (Vec<String>, Vec<String>, Vec<String>);

/// Validates and normalizes token permission lists. Returns an error message
/// when an entry is invalid.
fn validate_token_perms(
  hostnames: &[String],
  paths: &[String],
  allowed_ips: &[String],
) -> Result<TokenPermLists, String> {
  let mut out_hosts = Vec::new();
  for h in hostnames {
    let trimmed = h.trim();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed == "*" {
      out_hosts.push("*".to_string());
      continue;
    }
    match normalize_hostname_bind(trimmed) {
      Some(valid) => out_hosts.push(valid),
      None => return Err(format!("Invalid hostname permission: {}", trimmed)),
    }
  }
  let mut out_paths = Vec::new();
  for p in paths {
    let trimmed = p.trim();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed == "*" {
      out_paths.push("*".to_string());
      continue;
    }
    match normalize_path_bind(trimmed) {
      Some(valid) => out_paths.push(valid),
      None => return Err(format!("Invalid path permission: {}", trimmed)),
    }
  }
  let mut out_ips = Vec::new();
  for entry in allowed_ips {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
      continue;
    }
    if !valid_ip_entry(trimmed) {
      return Err(format!("Invalid IP/CIDR entry: {}", trimmed));
    }
    out_ips.push(trimmed.to_string());
  }
  if out_ips.is_empty() {
    out_ips.push("0.0.0.0/0".to_string());
  }
  Ok((out_hosts, out_paths, out_ips))
}

/// Creates a dynamic token. The plaintext secret is returned exactly once.
async fn tokens_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenCreateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  let name = payload.name.trim().to_string();
  if name.is_empty() || name.len() > 64 {
    return (StatusCode::BAD_REQUEST, "Token name must be 1-64 characters").into_response();
  }

  let (hostnames, paths, allowed_ips) =
    match validate_token_perms(&payload.hostnames, &payload.paths, &payload.allowed_ips) {
      Ok(v) => v,
      Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

  let (record, secret) = {
    let mut store = state.token_store.lock().await;
    store.create(name, hostnames, paths, allowed_ips, payload.ttl_seconds)
  };
  info!(
    "Dynamic token created: {} (id={}, hostnames={:?}, paths={:?}, ips={:?}, expires_at={:?})",
    record.name, record.id, record.hostnames, record.paths, record.allowed_ips, record.expires_at
  );
  state
    .audit(
      "token_created",
      &actor_ip,
      &format!(
        "name={} id={} hostnames={:?} paths={:?} ips={:?} expires_at={:?}",
        record.name, record.id, record.hostnames, record.paths, record.allowed_ips, record.expires_at
      ),
    )
    .await;
  state
    .emit_event(
      "token_created",
      serde_json::json!({"id": record.id, "name": record.name}),
    )
    .await;
  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": record.id,
      "name": record.name,
      "token": secret,
      "hostnames": record.hostnames,
      "paths": record.paths,
      "allowed_ips": record.allowed_ips,
      "expires_at": record.expires_at,
    })),
  )
    .into_response()
}

/// Edits an existing token's scope (name, hostnames, paths, allowed IPs,
/// expiry) without changing the secret. Live connections are unaffected.
async fn tokens_update_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenUpdateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();

  if let Some(ref n) = payload.name {
    let n = n.trim();
    if n.is_empty() || n.len() > 64 {
      return (StatusCode::BAD_REQUEST, "Token name must be 1-64 characters").into_response();
    }
  }
  let (hostnames, paths, allowed_ips) = match validate_token_perms(
    payload.hostnames.as_deref().unwrap_or(&[]),
    payload.paths.as_deref().unwrap_or(&[]),
    payload.allowed_ips.as_deref().unwrap_or(&[]),
  ) {
    Ok(v) => v,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };

  // ttl_seconds: absent = keep; 0 = never expires; n = now + n.
  let ttl = payload
    .ttl_seconds
    .map(|n| if n == 0 { None } else { Some(n) });

  let updated = state.token_store.lock().await.update(
    &id,
    payload.name.map(|n| n.trim().to_string()),
    payload.hostnames.map(|_| hostnames),
    payload.paths.map(|_| paths),
    payload.allowed_ips.map(|_| allowed_ips),
    ttl,
  );

  match updated {
    Some(record) => {
      info!(
        "Dynamic token updated: {} (id={}, hostnames={:?}, paths={:?}, ips={:?}, expires_at={:?})",
        record.name, record.id, record.hostnames, record.paths, record.allowed_ips, record.expires_at
      );
      state
        .audit(
          "token_updated",
          &actor_ip,
          &format!(
            "name={} id={} hostnames={:?} paths={:?} ips={:?} expires_at={:?}",
            record.name,
            record.id,
            record.hostnames,
            record.paths,
            record.allowed_ips,
            record.expires_at
          ),
        )
        .await;
      Json(serde_json::json!({"status": "ok"})).into_response()
    }
    None => (StatusCode::NOT_FOUND, "Token not found").into_response(),
  }
}

/// Revokes (deletes) a dynamic token. Existing tunnel connections that used
/// the token stay connected; only new connections are rejected.
async fn tokens_revoke_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  let revoked = state.token_store.lock().await.revoke(&id);
  if revoked {
    info!("Dynamic token revoked: {}", id);
    state
      .audit("token_revoked", &actor_ip, &format!("id={}", id))
      .await;
    state
      .emit_event("token_revoked", serde_json::json!({"id": id}))
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Token not found").into_response()
  }
}

/// Returns the full captured detail of a recent request (dashboard inspector).
async fn request_detail_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
  let captured = state.captured_requests.lock().await;
  match captured.iter().find(|c| c.id == id) {
    Some(entry) => Json(entry.clone()).into_response(),
    None => (
      StatusCode::NOT_FOUND,
      "Request not captured (only recent proxied requests are kept)",
    )
      .into_response(),
  }
}

/// Replays a captured request through the tunnel and returns the new outcome.
async fn request_replay_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy).to_string();
  let captured = {
    let store = state.captured_requests.lock().await;
    store.iter().find(|c| c.id == id).cloned()
  };
  let captured = match captured {
    Some(c) => c,
    None => return (StatusCode::NOT_FOUND, "Request not captured").into_response(),
  };
  if captured.req_body_truncated {
    return (
      StatusCode::BAD_REQUEST,
      "Request body was truncated at capture time; replay would be incomplete",
    )
      .into_response();
  }

  // Select a tunnel client with the same routing rules as live traffic.
  let uri_path = captured.uri.split('?').next().unwrap_or(&captured.uri);
  let request_host = captured
    .req_headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case("host"))
    .and_then(|(_, v)| {
      let lower = v.trim().to_ascii_lowercase();
      lower.split(':').next().map(|s| s.to_string())
    });
  let client_info = {
    let clients = state.clients.lock().await;
    match select_client_pool(
      &clients,
      uri_path,
      request_host.as_deref(),
      state.config.require_hostname_bind,
      state.config.client_down_threshold,
    ) {
      None => None,
      Some((pool, group_key)) => {
        let mut rr_map = state.path_rr.lock().await;
        let idx = rr_map.entry(group_key).or_insert(0);
        let chosen_id = &pool[*idx % pool.len()];
        *idx = (*idx + 1) % pool.len();
        clients
          .get(chosen_id)
          .map(|c| (chosen_id.clone(), c.tx.clone(), c.request_count.clone()))
      }
    }
  };
  let (chosen_client_id, client_tx, client_req_counter) = match client_info {
    Some(info) => info,
    None => {
      return (
        StatusCode::GATEWAY_TIMEOUT,
        "No tunnel client available for replay",
      )
        .into_response();
    }
  };

  let replay_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    replay_id.clone(),
    PendingRequest {
      tx: tx_response,
      client_id: chosen_client_id,
    },
  );

  let tunnel_req = TunnelMessage::Request {
    id: replay_id.clone(),
    method: captured.method.clone(),
    uri: captured.uri.clone(),
    headers: captured.req_headers.clone(),
    body: captured.req_body.clone(),
  };
  let req_json = match serde_json::to_string(&tunnel_req) {
    Ok(json) => json,
    Err(_) => {
      state.pending_requests.lock().await.remove(&replay_id);
      return (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response();
    }
  };
  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_requests.lock().await.remove(&replay_id);
    return (StatusCode::BAD_GATEWAY, "Tunnel client socket error").into_response();
  }
  client_req_counter.fetch_add(1, Ordering::SeqCst);
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
  }

  let start = Instant::now();
  let result = tokio::time::timeout(state.config.gateway_response_timeout, rx_response).await;
  state.pending_requests.lock().await.remove(&replay_id);

  match result {
    Ok(Ok(tunnel_res)) => {
      // Streamed replay bodies are discarded: dropping stream_rx makes the
      // tunnel read loop clean the stream up on the next chunk.
      {
        let mut stats = state.stats.lock().await;
        if tunnel_res.status >= 500 {
          stats.failed_requests += 1;
        } else {
          stats.successful_requests += 1;
        }
      }
      info!(
        "Replayed request {} → {} ({} ms)",
        id,
        tunnel_res.status,
        start.elapsed().as_millis()
      );
      state
        .audit(
          "request_replayed",
          &actor_ip,
          &format!(
            "id={} {} {} -> {}",
            id, captured.method, captured.uri, tunnel_res.status
          ),
        )
        .await;
      Json(serde_json::json!({
        "replayed_id": id,
        "status": tunnel_res.status,
        "duration_ms": start.elapsed().as_millis() as u64,
      }))
      .into_response()
    }
    Ok(Err(_)) => (StatusCode::BAD_GATEWAY, "Client connection lost during replay").into_response(),
    Err(_) => (StatusCode::GATEWAY_TIMEOUT, "Replay response timeout").into_response(),
  }
}

/// Prometheus text-format metrics endpoint (`/aperio/metrics`).
/// Enabled with `APERIO_METRICS=1`. Requires a token, presented either as
/// `?token=<value>` (convenient for Prometheus scrape configs) or as an
/// `Authorization: Bearer <value>` header.
async fn metrics_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  headers: HeaderMap,
) -> Response {
  if let Some(ref token) = state.config.metrics_token {
    let bearer_ok = headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.strip_prefix("Bearer "))
      .is_some_and(|t| constant_time_eq_str(t, token));
    let query_ok = query
      .get("token")
      .is_some_and(|t| constant_time_eq_str(t, token));
    if !bearer_ok && !query_ok {
      return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
  }

  let stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;
  let connected = clients.len();
  let per_client: Vec<(String, u64)> = clients
    .iter()
    .map(|(id, c)| (id.clone(), c.request_count.load(Ordering::SeqCst)))
    .collect();
  drop(clients);
  let pending = state.pending_requests.lock().await.len();
  let ws_streams = state.ws_streams.lock().await.len();
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut out = String::with_capacity(1024);
  out.push_str("# HELP aperio_requests_total Total proxied requests received.\n");
  out.push_str("# TYPE aperio_requests_total counter\n");
  out.push_str(&format!("aperio_requests_total {}\n", stats.total_requests));
  out.push_str("# HELP aperio_requests_success_total Successfully proxied requests.\n");
  out.push_str("# TYPE aperio_requests_success_total counter\n");
  out.push_str(&format!(
    "aperio_requests_success_total {}\n",
    stats.successful_requests
  ));
  out.push_str("# HELP aperio_requests_failed_total Failed proxied requests (5xx / gateway errors).\n");
  out.push_str("# TYPE aperio_requests_failed_total counter\n");
  out.push_str(&format!(
    "aperio_requests_failed_total {}\n",
    stats.failed_requests
  ));
  out.push_str("# HELP aperio_bytes_transferred_total Total payload bytes transferred.\n");
  out.push_str("# TYPE aperio_bytes_transferred_total counter\n");
  out.push_str(&format!(
    "aperio_bytes_transferred_total {}\n",
    stats.total_bytes_transferred
  ));
  out.push_str("# HELP aperio_connected_clients Currently connected tunnel clients.\n");
  out.push_str("# TYPE aperio_connected_clients gauge\n");
  out.push_str(&format!("aperio_connected_clients {}\n", connected));
  out.push_str("# HELP aperio_pending_requests Requests currently awaiting a client response.\n");
  out.push_str("# TYPE aperio_pending_requests gauge\n");
  out.push_str(&format!("aperio_pending_requests {}\n", pending));
  out.push_str("# HELP aperio_ws_streams_active Active proxied WebSocket streams.\n");
  out.push_str("# TYPE aperio_ws_streams_active gauge\n");
  out.push_str(&format!("aperio_ws_streams_active {}\n", ws_streams));
  out.push_str("# HELP aperio_uptime_seconds Server uptime in seconds.\n");
  out.push_str("# TYPE aperio_uptime_seconds gauge\n");
  out.push_str(&format!("aperio_uptime_seconds {}\n", uptime));
  out.push_str("# HELP aperio_client_requests_total Requests handled per connected tunnel client.\n");
  out.push_str("# TYPE aperio_client_requests_total counter\n");
  for (id, count) in per_client {
    out.push_str(&format!(
      "aperio_client_requests_total{{client_id=\"{}\"}} {}\n",
      id, count
    ));
  }

  (
    StatusCode::OK,
    [(
      "content-type",
      "text/plain; version=0.0.4; charset=utf-8",
    )],
    out,
  )
    .into_response()
}

/// Health check endpoint returning status, active connection counts, and uptime.
async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
  let clients_count = state.clients.lock().await.len();
  let stats = state.stats.lock().await;
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut health_info = HashMap::new();
  health_info.insert("status", serde_json::json!("healthy"));
  health_info.insert("connected_clients", serde_json::json!(clients_count));
  health_info.insert("uptime_seconds", serde_json::json!(uptime));
  health_info.insert("total_requests", serde_json::json!(stats.total_requests));

  (StatusCode::OK, Json(health_info))
}

/// Serves the login page.
async fn auth_page_handler() -> Html<&'static str> {
  Html(include_str!("authentication.html"))
}

/// Handles login form submission. Validates credentials and sets a session cookie.
async fn auth_login_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Result<Response, StatusCode> {
  // Rate limit login attempts per IP to mitigate brute-force attacks.
  let client_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy);
  if !state.check_rate_limit(client_ip).await {
    return Err(StatusCode::TOO_MANY_REQUESTS);
  }

  let mut authenticated = false;
  if let Some(auth_header) = headers.get("authorization")
    && let Ok(auth_str) = auth_header.to_str()
    && let Some(stripped) = auth_str.strip_prefix("Basic ")
  {
    use base64::prelude::*;
    if let Ok(decoded) = BASE64_STANDARD.decode(stripped)
      && let Ok(decoded_str) = String::from_utf8(decoded)
    {
      // Allow APERIO_SERVER_AUTH credentials if configured
      if let Some(ref creds) = state.config.auth_credentials
        && constant_time_eq_str(&decoded_str, creds)
      {
        authenticated = true;
      }
      // Always allow token as password with username "aperio"
      if !authenticated
        && constant_time_eq_str(&decoded_str, &format!("aperio:{}", state.config.token))
      {
        authenticated = true;
      }
      // Allow APERIO_DASHBOARD_AUTH as password with username "aperio"
      if !authenticated
        && let Ok(dash_pass) = std::env::var("APERIO_DASHBOARD_AUTH")
        && constant_time_eq_str(&decoded_str, &format!("aperio:{}", dash_pass))
      {
        authenticated = true;
      }
    }
  }

  if !authenticated {
    state
      .audit("login_failed", &client_ip.to_string(), "invalid credentials")
      .await;
    return Err(StatusCode::UNAUTHORIZED);
  }
  state
    .audit("login_success", &client_ip.to_string(), "session created")
    .await;

  // Create session
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    session_token.clone(),
    SessionInfo {
      expires_at: Instant::now() + Duration::from_secs(86400),
    },
  );

  let secure_flag = if state.config.secure_cookies {
    "; Secure"
  } else {
    ""
  };
  let cookie = format!(
    "aperio_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_token, secure_flag
  );

  Ok(
    Response::builder()
      .status(StatusCode::OK)
      .header("Set-Cookie", cookie)
      .body(Body::empty())
      .unwrap(),
  )
}

/// Extracts a Bearer token or `x-auth-token` value from request headers.
fn extract_token(headers: &HeaderMap) -> Option<String> {
  if let Some(auth_header) = headers.get("authorization")
    && let Ok(auth_str) = auth_header.to_str()
    && let Some(stripped) = auth_str.strip_prefix("Bearer ")
  {
    return Some(stripped.to_string());
  }
  if let Some(x_token) = headers.get("x-auth-token")
    && let Ok(x_token_str) = x_token.to_str()
  {
    return Some(x_token_str.to_string());
  }
  None
}

/// Helper function to extract Bearer token or `x-auth-token` from header values
/// and verify if it matches the configured server security token.
#[cfg(test)]
fn extract_and_verify_token(headers: &HeaderMap, server_token: &str) -> bool {
  match extract_token(headers) {
    Some(tok) => constant_time_eq_str(&tok, server_token),
    None => false,
  }
}

/// Resolves the permissions for a presented tunnel token: the master token
/// grants unrestricted access; otherwise the dynamic token store is consulted
/// (rejecting unknown and expired tokens).
async fn authorize_tunnel_token(
  state: &AppState,
  headers: &HeaderMap,
  client_ip: IpAddr,
) -> Option<ClientPerms> {
  let presented = extract_token(headers)?;
  if constant_time_eq_str(&presented, &state.config.token) {
    return Some(ClientPerms::master());
  }
  let store = state.token_store.lock().await;
  let token = store.verify(&presented)?;
  // Dynamic tokens can be restricted to source IPs/CIDRs.
  if !ip_allowed(client_ip, &token.allowed_ips) {
    warn!(
      "Token '{}' rejected: source IP {} not in allowed list {:?}",
      token.name, client_ip, token.allowed_ips
    );
    return None;
  }
  Some(ClientPerms {
    master: false,
    hostnames: token.hostnames.clone(),
    paths: token.paths.clone(),
    token_name: Some(token.name.clone()),
  })
}

/// Checks whether `ip` matches an allowlist of plain IPs and CIDR ranges.
/// An empty list, `*`, `0.0.0.0/0` or `::/0` allow any address.
fn ip_allowed(ip: IpAddr, allowed: &[String]) -> bool {
  if allowed.is_empty() {
    return true;
  }
  allowed.iter().any(|entry| {
    let entry = entry.trim();
    if entry == "*" || entry == "0.0.0.0/0" || entry == "::/0" || entry == "0.0.0.0" {
      return true;
    }
    match entry.split_once('/') {
      Some((base, prefix)) => {
        let (Ok(base_ip), Ok(bits)) = (base.parse::<IpAddr>(), prefix.parse::<u32>()) else {
          return false;
        };
        cidr_contains(base_ip, bits, ip)
      }
      None => entry.parse::<IpAddr>().is_ok_and(|allowed_ip| allowed_ip == ip),
    }
  })
}

/// True when `ip` falls inside the CIDR `base/bits` (families must match).
fn cidr_contains(base: IpAddr, bits: u32, ip: IpAddr) -> bool {
  match (base, ip) {
    (IpAddr::V4(b), IpAddr::V4(i)) => {
      if bits > 32 {
        return false;
      }
      if bits == 0 {
        return true;
      }
      let mask = u32::MAX << (32 - bits);
      (u32::from(b) & mask) == (u32::from(i) & mask)
    }
    (IpAddr::V6(b), IpAddr::V6(i)) => {
      if bits > 128 {
        return false;
      }
      if bits == 0 {
        return true;
      }
      let mask = u128::MAX << (128 - bits);
      (u128::from(b) & mask) == (u128::from(i) & mask)
    }
    _ => false,
  }
}

/// Validates an allowlist entry (plain IP or CIDR, or a wildcard form).
fn valid_ip_entry(entry: &str) -> bool {
  let entry = entry.trim();
  if entry == "*" {
    return true;
  }
  match entry.split_once('/') {
    Some((base, prefix)) => {
      let Ok(base_ip) = base.parse::<IpAddr>() else {
        return false;
      };
      match prefix.parse::<u32>() {
        Ok(bits) => match base_ip {
          IpAddr::V4(_) => bits <= 32,
          IpAddr::V6(_) => bits <= 128,
        },
        Err(_) => false,
      }
    }
    None => entry.parse::<IpAddr>().is_ok(),
  }
}

/// Constant-time string comparison to mitigate timing attacks on secrets.
/// Hashes both inputs with SHA-256 first so that length differences do not
/// leak through the comparison timing, then compares the digests using
/// `subtle::ConstantTimeEq`.
fn constant_time_eq_str(a: &str, b: &str) -> bool {
  use subtle::ConstantTimeEq;
  let mut ha = Sha256::default();
  ha.update(a.as_bytes());
  let mut hb = Sha256::default();
  hb.update(b.as_bytes());
  let da = ha.finalize();
  let db = hb.finalize();
  da.ct_eq(&db).into()
}

/// Compresses a tunnel text frame into a zlib binary frame.
fn compress_frame(text: &str) -> Vec<u8> {
  use flate2::{Compression, write::ZlibEncoder};
  use std::io::Write;
  let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
  let _ = enc.write_all(text.as_bytes());
  enc.finish().unwrap_or_default()
}

/// Inflates a zlib binary frame back into a text frame, bounding the output
/// size to protect against decompression bombs.
fn decompress_frame(data: &[u8], max_out: usize) -> Option<String> {
  use flate2::read::ZlibDecoder;
  use std::io::Read;
  let mut out = String::new();
  let mut dec = ZlibDecoder::new(data).take(max_out as u64 + 1);
  dec.read_to_string(&mut out).ok()?;
  if out.len() > max_out {
    warn!("Dropped tunnel frame: decompressed size exceeds limit");
    return None;
  }
  Some(out)
}

/// Normalizes a path bind by ensuring it starts with `/` and stripping any
/// trailing slashes. Returns `None` for the empty/root bind or for values
/// that fail validation (too long, path traversal, or unsafe characters).
fn normalize_path_bind(bind: &str) -> Option<String> {
  const MAX_PATH_BIND_LEN: usize = 256;

  let trimmed = bind.trim().trim_end_matches('/');
  if trimmed.is_empty() || trimmed == "/" {
    return None;
  }
  if trimmed.len() > MAX_PATH_BIND_LEN {
    warn!(
      "Rejected path_bind exceeding maximum length ({} > {})",
      trimmed.len(),
      MAX_PATH_BIND_LEN
    );
    return None;
  }
  // Reject path traversal segments and require URL-safe path characters only.
  for segment in trimmed.split('/') {
    if segment.is_empty() {
      continue;
    }
    if segment == ".." || segment == "." {
      warn!("Rejected path_bind containing traversal segment: {}", bind);
      return None;
    }
    if !segment
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
    {
      warn!("Rejected path_bind with unsafe characters: {}", bind);
      return None;
    }
  }
  let with_slash = if trimmed.starts_with('/') {
    trimmed.to_string()
  } else {
    format!("/{}", trimmed)
  };
  Some(with_slash)
}

/// Checks whether `uri_path` matches a path `bind` on a segment boundary,
/// preventing `/apixyz` from matching a bind of `/api`.
fn path_matches_bind(uri_path: &str, bind: &str) -> bool {
  uri_path == bind || uri_path.starts_with(&format!("{}/", bind))
}

/// Normalizes a hostname bind: lowercases, trims whitespace, strips a
/// trailing dot and an optional port suffix. Returns `None` for empty values
/// or values containing characters outside the DNS-safe set.
fn normalize_hostname_bind(host: &str) -> Option<String> {
  const MAX_HOSTNAME_LEN: usize = 253;

  let trimmed = host.trim().trim_end_matches('.').to_ascii_lowercase();
  // Strip a port suffix (not applicable to bracketed IPv6 literals).
  let without_port = match trimmed.split_once(':') {
    Some((h, port)) if !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h.to_string(),
    _ => trimmed,
  };
  if without_port.is_empty() || without_port.len() > MAX_HOSTNAME_LEN {
    return None;
  }
  let valid = without_port
    .split('.')
    .all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
  if !valid {
    warn!("Rejected hostname_bind with invalid format: {}", host);
    return None;
  }
  Some(without_port)
}

/// Extracts the request hostname from the `Host` header (lowercased, port
/// stripped). Returns `None` when the header is absent or malformed.
fn extract_request_host(headers: &HeaderMap) -> Option<String> {
  let raw = headers.get("host")?.to_str().ok()?;
  let trimmed = raw.trim().to_ascii_lowercase();
  // Bracketed IPv6 literal: [::1]:8080 → ::1 is not a valid hostname bind
  // anyway, but strip the port consistently.
  let host = if let Some(stripped) = trimmed.strip_prefix('[') {
    stripped.split(']').next().unwrap_or("").to_string()
  } else {
    trimmed.split(':').next().unwrap_or("").to_string()
  };
  if host.is_empty() { None } else { Some(host) }
}

/// Selects the pool of candidate client IDs for a request, honoring hostname
/// binds first, then path binds within the hostname group. Returns the pool
/// together with the round-robin group key.
///
/// Hostname stage:
/// - Clients whose effective hostname bind equals the request host win.
/// - Otherwise, when `require_hostname_bind` is off, clients without any
///   hostname bind act as the fallback pool. When the flag is on, clients
///   without a hostname bind never receive traffic.
///
/// Path stage (within the hostname pool): longest matching path bind wins;
/// clients without a path bind are the fallback.
fn select_client_pool(
  clients: &HashMap<String, ClientHandle>,
  uri_path: &str,
  request_host: Option<&str>,
  require_hostname_bind: bool,
  down_threshold: Duration,
) -> Option<(Vec<String>, RouteGroupKey)> {
  // --- Eligibility stage: unhealthy, draining, or admin-disabled clients
  // never receive new traffic (in-flight requests still complete) ---
  let eligible: Vec<(&String, &ClientHandle)> = clients
    .iter()
    .filter(|(_, c)| c.is_healthy(down_threshold) && !c.draining && c.admin_enabled)
    .collect();

  // --- Hostname stage ---
  let host_matched: Vec<(&String, &ClientHandle)> = match request_host {
    Some(host) => eligible
      .iter()
      .filter(|(_, c)| c.matches_host(host))
      .cloned()
      .collect(),
    None => Vec::new(),
  };

  let (host_pool, host_key): (Vec<(&String, &ClientHandle)>, Option<String>) =
    if !host_matched.is_empty() {
      (host_matched, request_host.map(|h| h.to_string()))
    } else if require_hostname_bind {
      // Strict mode: unbound clients are never eligible.
      return None;
    } else {
      let unbound: Vec<(&String, &ClientHandle)> = eligible
        .iter()
        .filter(|(_, c)| !c.has_hostname_bind())
        .cloned()
        .collect();
      (unbound, None)
    };

  if host_pool.is_empty() {
    return None;
  }

  // --- Path stage ---
  let path_matched: Vec<(&String, &String)> = host_pool
    .iter()
    .filter_map(|(id, c)| {
      c.effective_path_bind()
        .filter(|bind| path_matches_bind(uri_path, bind))
        .map(|bind| (*id, bind))
    })
    .collect();

  let (pool, path_key): (Vec<String>, Option<String>) = if !path_matched.is_empty() {
    // Longest matching bind wins; only clients with that exact bind pool together.
    let longest = path_matched
      .iter()
      .map(|(_, b)| (*b).clone())
      .max_by_key(|b| b.len())
      .unwrap();
    let ids = path_matched
      .iter()
      .filter(|(_, b)| **b == longest)
      .map(|(id, _)| (*id).clone())
      .collect();
    (ids, Some(longest))
  } else {
    let ids: Vec<String> = host_pool
      .iter()
      .filter(|(_, c)| c.effective_path_bind().is_none())
      .map(|(id, _)| (*id).clone())
      .collect();
    (ids, None)
  };

  if pool.is_empty() {
    None
  } else {
    Some((pool, (host_key, path_key)))
  }
}

/// Resolves the real client IP, honoring `X-Forwarded-For` / `X-Real-IP` only
/// when `trust_proxy` is enabled (i.e. the server runs behind a trusted reverse
/// proxy). Otherwise the direct socket address is used, since clients could
/// otherwise spoof these headers to bypass rate limiting.
fn extract_client_ip(headers: &HeaderMap, fallback: IpAddr, trust_proxy: bool) -> IpAddr {
  if trust_proxy {
    if let Some(xff) = headers.get("x-forwarded-for")
      && let Ok(xff_str) = xff.to_str()
      && let Some(first) = xff_str.split(',').next()
      && let Ok(parsed) = first.trim().parse::<IpAddr>()
    {
      return parsed;
    }
    if let Some(real_ip) = headers.get("x-real-ip")
      && let Ok(real_str) = real_ip.to_str()
      && let Ok(parsed) = real_str.trim().parse::<IpAddr>()
    {
      return parsed;
    }
  }
  fallback
}

/// Upgrade WebSocket endpoint. Extracts and verifies security tokens.
async fn ws_handler(
  ws: WebSocketUpgrade,
  headers: HeaderMap,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  State(state): State<Arc<AppState>>,
) -> Response {
  let tunnel_client_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy);
  let perms = match authorize_tunnel_token(&state, &headers, tunnel_client_ip).await {
    Some(p) => p,
    None => {
      info!("Unauthorized connection attempt blocked.");
      return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
  };

  // Validate maximum active tunnels limit (protects against file descriptor exhaustion).
  // Uses an atomic counter so that concurrent upgrade attempts cannot race past the limit.
  loop {
    let current = state.active_tunnel_count.load(Ordering::SeqCst);
    if current >= state.config.max_tunnels {
      warn!(
        "WebSocket upgrade connection rejected from {}: Maximum tunnels count reached ({}/{})",
        addr, current, state.config.max_tunnels
      );
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable - Maximum active tunnels limit reached",
      )
        .into_response();
    }
    // Atomically reserve our slot; retry if another connection raced ahead.
    if state
      .active_tunnel_count
      .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
    {
      break;
    }
  }

  // Use saturating arithmetic to prevent usize overflow with very large max_body_size.
  ws.max_message_size(state.config.max_body_size.saturating_mul(2))
    .max_frame_size(state.config.max_body_size)
    .on_upgrade(move |socket| handle_socket(socket, addr.to_string(), state, perms))
}

/// WebSocket processing logic. Listens for client frame inputs (Responses/Pings).
async fn handle_socket(
  socket: WebSocket,
  client_ip: String,
  state: Arc<AppState>,
  perms: ClientPerms,
) {
  let (mut ws_sender, mut ws_receiver) = socket.split();
  let client_id = uuid::Uuid::new_v4().to_string();

  // Create channel to handle writes asynchronously
  let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);

  // Per-connection compression state: outgoing frames are compressed once
  // the client acknowledges the CompressionStart offer.
  let compress_out = Arc::new(AtomicBool::new(false));

  // Spawn a writer task for this connection
  let writer_client_id = client_id.clone();
  let compress_out_writer = compress_out.clone();
  let writer_task = tokio::spawn(async move {
    while let Some(msg) = rx_write.recv().await {
      let msg = match msg {
        Message::Text(t) if compress_out_writer.load(Ordering::SeqCst) => {
          Message::Binary(compress_frame(&t))
        }
        other => other,
      };
      if let Err(e) = ws_sender.send(msg).await {
        error!(
          "Error writing to websocket client {}: {:?}",
          writer_client_id, e
        );
        break;
      }
    }
  });

  info!("Tunnel client connected: {} (IP: {})", client_id, client_ip);
  state
    .audit(
      "client_connected",
      &client_ip,
      &format!(
        "client={} token={}",
        client_id,
        perms.token_name.as_deref().unwrap_or("master")
      ),
    )
    .await;
  state
    .emit_event(
      "client_connected",
      serde_json::json!({
        "client_id": client_id,
        "ip": client_ip,
        "token": perms.token_name.as_deref().unwrap_or("master"),
      }),
    )
    .await;

  let client_req_count = Arc::new(AtomicU64::new(0));

  // Token-granted binds apply immediately, before the first Ping. When the
  // random subdomain feature is on, the random hostname is added on top of
  // any token-granted hostnames — the client serves both.
  let mut assigned_hostnames = perms.granted_hostnames();
  let random_hostname = state.config.random_subdomain_suffix.as_ref().map(|suffix| {
    let label: String = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    format!("{}.{}", label, suffix)
  });
  if let Some(ref h) = random_hostname {
    assigned_hostnames.push(h.clone());
  }

  // Register active client
  {
    let mut clients = state.clients.lock().await;
    clients.insert(
      client_id.clone(),
      ClientHandle {
        tx: tx_write.clone(),
        connected_at: Instant::now(),
        client_ip: client_ip.clone(),
        request_count: client_req_count.clone(),
        declared_path: None,
        assigned_path: perms.granted_path(),
        declared_hostname: None,
        assigned_hostnames,
        override_path_bind: None,
        override_hostname_bind: None,
        last_ping_at: None,
        perms: perms.clone(),
        max_concurrent: None,
        inflight_limiter: None,
        draining: false,
        admin_enabled: true,
        tcp_enabled: false,
      },
    );
    drop(clients);
    let mut conn = state.connection_state.lock().await;
    conn.connected = true;
    conn.last_disconnect = None;
    state.client_connected.send_replace(true);
  }

  // Inform the client of its randomly assigned hostname (if any).
  if let Some(hostname) = random_hostname {
    info!(
      "Assigned random hostname {} to client {}",
      hostname, client_id
    );
    let msg = TunnelMessage::HostnameAssigned { hostname };
    if let Ok(json) = serde_json::to_string(&msg) {
      let _ = tx_write.send(Message::Text(json)).await;
    }
  }

  // Offer tunnel compression; frames stay uncompressed until the client Acks.
  if state.config.tunnel_compression
    && let Ok(json) = serde_json::to_string(&TunnelMessage::CompressionStart {})
  {
    let _ = tx_write.send(Message::Text(json)).await;
  }

  // Cap for decompressed tunnel frames (defends against zlib bombs).
  let max_inflated = state
    .config
    .max_body_size
    .saturating_mul(4)
    .max(8 * 1024 * 1024);

  // Read loop
  while let Some(result) = ws_receiver.next().await {
    match result {
      Ok(msg) => {
        let text_opt = match msg {
          Message::Text(t) => Some(t),
          Message::Binary(b) => decompress_frame(&b, max_inflated),
          _ => None,
        };
        if let Some(text) = text_opt
          && let Ok(tunnel_msg) = serde_json::from_str::<TunnelMessage>(&text)
        {
          match tunnel_msg {
            TunnelMessage::Response {
              id,
              status,
              headers,
              body,
            } => {
              let mut pending = state.pending_requests.lock().await;
              // Verify that this response originates from the client that was
              // assigned the request. Prevents a malicious tunnel client from
              // injecting spoofed responses for another client's requests.
              let is_owner = pending
                .get(&id)
                .is_some_and(|req| req.client_id == client_id);
              if !is_owner {
                if pending.contains_key(&id) {
                  warn!(
                    "Response for request ID {} rejected: sent by client {} but owned by a different client",
                    id, client_id
                  );
                }
              } else if let Some(req) = pending.remove(&id)
                && req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body,
                    stream_rx: None,
                  })
                  .is_err()
              {
                warn!(
                  "Pending request oneshot receiver was dropped for request ID: {}",
                  id
                );
              }
            }
            TunnelMessage::ResponseStart {
              id,
              status,
              headers,
            } => {
              let mut pending = state.pending_requests.lock().await;
              let is_owner = pending
                .get(&id)
                .is_some_and(|req| req.client_id == client_id);
              if !is_owner {
                if pending.contains_key(&id) {
                  warn!(
                    "ResponseStart for request ID {} rejected: sent by client {} but owned by a different client",
                    id, client_id
                  );
                }
              } else if let Some(req) = pending.remove(&id) {
                // Register the chunk channel before resolving the head so no
                // ResponseChunk can race past an unregistered stream.
                let (chunk_tx, chunk_rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
                state.response_streams.lock().await.insert(
                  id.clone(),
                  ResponseStreamHandle {
                    tx: chunk_tx,
                    client_id: client_id.clone(),
                  },
                );
                if req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body: None,
                    stream_rx: Some(chunk_rx),
                  })
                  .is_err()
                {
                  warn!(
                    "Pending request oneshot receiver was dropped for streamed request ID: {}",
                    id
                  );
                  state.response_streams.lock().await.remove(&id);
                }
              }
            }
            TunnelMessage::ResponseChunk { id, data } => {
              // Look up the stream and verify the sender owns it.
              let chunk_tx = {
                let streams = state.response_streams.lock().await;
                match streams.get(&id) {
                  Some(handle) if handle.client_id == client_id => Some(handle.tx.clone()),
                  Some(_) => {
                    warn!(
                      "ResponseChunk for request ID {} rejected: not owned by client {}",
                      id, client_id
                    );
                    None
                  }
                  None => None,
                }
              };
              if let Some(chunk_tx) = chunk_tx {
                use base64::prelude::*;
                match BASE64_STANDARD.decode(&data) {
                  Ok(bytes) => {
                    let len = bytes.len() as u64;
                    // Bounded send with timeout: if the public consumer stalls
                    // for too long, drop the stream instead of blocking the
                    // whole tunnel read loop forever.
                    let send_res = tokio::time::timeout(
                      state.config.gateway_response_timeout,
                      chunk_tx.send(Ok(bytes)),
                    )
                    .await;
                    match send_res {
                      Ok(Ok(())) => {
                        let mut stats = state.stats.lock().await;
                        stats.total_bytes_transferred += len;
                        drop(stats);
                        state.persistent_stats.lock().await.record_bytes_sent(len);
                      }
                      _ => {
                        debug!(
                          "Dropping streamed response {} (consumer gone or stalled)",
                          id
                        );
                        state.response_streams.lock().await.remove(&id);
                      }
                    }
                  }
                  Err(_) => {
                    warn!("Failed to decode Base64 ResponseChunk for request {}", id);
                    state.response_streams.lock().await.remove(&id);
                  }
                }
              }
            }
            TunnelMessage::ResponseEnd { id } => {
              // Dropping the sender ends the public body stream.
              let removed = state.response_streams.lock().await.remove(&id);
              if let Some(handle) = removed
                && handle.client_id != client_id
              {
                // Ownership violation: re-insert and ignore.
                warn!(
                  "ResponseEnd for request ID {} rejected: not owned by client {}",
                  id, client_id
                );
                state.response_streams.lock().await.insert(id, handle);
              }
            }
            TunnelMessage::TcpData { stream_id, data } => {
              let consumer_tx = {
                let streams = state.tcp_streams.lock().await;
                match streams.get(&stream_id) {
                  Some(h) if h.client_id == client_id => Some(h.tx.clone()),
                  Some(_) => {
                    warn!("TcpData for stream {} rejected: not owned by client {}", stream_id, client_id);
                    None
                  }
                  None => None,
                }
              };
              if let Some(consumer_tx) = consumer_tx {
                use base64::prelude::*;
                match BASE64_STANDARD.decode(&data) {
                  Ok(bytes) => {
                    if consumer_tx.send(TcpConsumerMsg::Data(bytes)).await.is_err() {
                      state.tcp_streams.lock().await.remove(&stream_id);
                    }
                  }
                  Err(_) => {
                    warn!("Failed to decode Base64 TcpData for stream {}", stream_id);
                  }
                }
              }
            }
            TunnelMessage::TcpClose { stream_id } => {
              let removed = state.tcp_streams.lock().await.remove(&stream_id);
              if let Some(h) = removed {
                if h.client_id == client_id {
                  let _ = h.tx.send(TcpConsumerMsg::Close).await;
                } else {
                  state.tcp_streams.lock().await.insert(stream_id, h);
                }
              }
            }
            TunnelMessage::CompressionAck {} => {
              info!("Client {} acknowledged tunnel compression", client_id);
              compress_out.store(true, Ordering::SeqCst);
            }
            TunnelMessage::Draining {} => {
              info!("Client {} is draining: no new requests will be routed to it", client_id);
              {
                let mut clients = state.clients.lock().await;
                if let Some(handle) = clients.get_mut(&client_id) {
                  handle.draining = true;
                }
              }
              state
                .audit("client_draining", &client_ip, &format!("client={}", client_id))
                .await;
              state
                .emit_event(
                  "client_draining",
                  serde_json::json!({"client_id": client_id, "ip": client_ip}),
                )
                .await;
            }
            TunnelMessage::Ping {
              client_id: cid,
              timestamp,
              path_bind,
              hostname_bind,
              max_concurrent,
              tcp,
            } => {
              debug!("Heartbeat from client {}: {}", cid, timestamp);
              // Update client's reported binds and heartbeat time. Only the
              // server-assigned connection ID is trusted for state updates;
              // the client-declared `cid` is ignored to prevent a client from
              // mutating another connection's state.
              let normalized_path = path_bind.and_then(|b| normalize_path_bind(&b));
              let normalized_host = hostname_bind.and_then(|h| normalize_hostname_bind(&h));
              {
                let mut clients = state.clients.lock().await;
                if let Some(handle) = clients.get_mut(&client_id) {
                  // Declared binds must be permitted by the token used to connect.
                  if let Some(p) = normalized_path {
                    if handle.perms.path_allowed(&p) {
                      handle.declared_path = Some(p);
                    } else {
                      warn!(
                        "Client {} declared path bind {} not permitted by its token; ignored",
                        client_id, p
                      );
                    }
                  }
                  if let Some(h) = normalized_host {
                    if handle.perms.hostname_allowed(&h) {
                      handle.declared_hostname = Some(h);
                    } else {
                      warn!(
                        "Client {} declared hostname bind {} not permitted by its token; ignored",
                        client_id, h
                      );
                    }
                  }
                  // Create the concurrency limiter on the first Ping that
                  // announces a limit; the limit is fixed for the connection.
                  if handle.inflight_limiter.is_none()
                    && let Some(n) = max_concurrent
                    && n > 0
                  {
                    handle.max_concurrent = Some(n);
                    handle.inflight_limiter = Some(Arc::new(Semaphore::new(n as usize)));
                    info!(
                      "Client {} announced concurrency limit: {} — excess requests will be queued",
                      client_id, n
                    );
                  }
                  handle.tcp_enabled = tcp;
                  handle.last_ping_at = Some(Instant::now());
                }
              }
              let pong = TunnelMessage::Pong { timestamp };
              if let Ok(pong_str) = serde_json::to_string(&pong) {
                let _ = tx_write.send(Message::Text(pong_str)).await;
              }
            }
            TunnelMessage::UpgradeResponse {
              id,
              status,
              headers,
            } => {
              let mut pending = state.pending_upgrades.lock().await;
              let is_owner = pending
                .get(&id)
                .is_some_and(|req| req.client_id == client_id);
              if !is_owner {
                if pending.contains_key(&id) {
                  warn!(
                    "UpgradeResponse for stream ID {} rejected: sent by client {} but owned by a different client",
                    id, client_id
                  );
                }
              } else if let Some(req) = pending.remove(&id)
                && req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body: None,
                    stream_rx: None,
                  })
                  .is_err()
              {
                warn!(
                  "Pending upgrade oneshot receiver was dropped for stream ID: {}",
                  id
                );
              }
            }
            TunnelMessage::WsData {
              stream_id,
              data,
              is_text,
            } => {
              // Relay WebSocket frame to the public WS via the registered channel
              let streams = state.ws_streams.lock().await;
              if let Some(tx) = streams.get(&stream_id) {
                let ws_msg = if is_text {
                  Message::Text(data)
                } else {
                  use base64::prelude::*;
                  match BASE64_STANDARD.decode(&data) {
                    Ok(bytes) => Message::Binary(bytes),
                    Err(_) => {
                      warn!("Failed to decode Base64 WsData for stream {}", stream_id);
                      continue;
                    }
                  }
                };
                if tx.send(WsStreamMessage::Data(ws_msg)).await.is_err() {
                  debug!("WsStream channel closed for stream {}", stream_id);
                }
              }
            }
            TunnelMessage::WsClose {
              stream_id,
              code: _,
              reason: _,
            } => {
              let streams = state.ws_streams.lock().await;
              if let Some(tx) = streams.get(&stream_id) {
                let _ = tx.send(WsStreamMessage::Close).await;
              }
            }
            _ => {}
          }
        }
      }
      Err(e) => {
        error!("WebSocket reading error for client {}: {:?}", client_id, e);
        break;
      }
    }
  }

  // Client cleanup
  writer_task.abort();
  info!("Tunnel client disconnected: {}", client_id);
  state
    .audit(
      "client_disconnected",
      &client_ip,
      &format!("client={}", client_id),
    )
    .await;
  state
    .emit_event(
      "client_disconnected",
      serde_json::json!({"client_id": client_id, "ip": client_ip}),
    )
    .await;
  {
    let mut clients = state.clients.lock().await;
    let removed = clients.remove(&client_id);
    let now_empty = clients.is_empty();

    // Prune round-robin indices for routing groups that no longer have any
    // matching client (prevents unbounded growth of the rr map). Clients can
    // belong to multiple hostname groups, so re-evaluate all keys.
    if removed.is_some() {
      let mut rr_map = state.path_rr.lock().await;
      rr_map.retain(|(host_key, path_key), _| {
        clients.values().any(|c| {
          let host_ok = match host_key {
            Some(h) => c.matches_host(h),
            None => !c.has_hostname_bind(),
          };
          host_ok && c.effective_path_bind() == path_key.as_ref()
        })
      });
    }

    drop(clients);

    if now_empty {
      let mut conn = state.connection_state.lock().await;
      conn.connected = false;
      conn.last_disconnect = Some(Instant::now());
      drop(conn);
      state.client_connected.send_replace(false);
    }
  }
  // Release the reserved tunnel slot.
  state.active_tunnel_count.fetch_sub(1, Ordering::SeqCst);

  // Instantly abort pending requests that were routed to the disconnected client
  {
    let mut pending = state.pending_requests.lock().await;
    let keys_to_remove: Vec<String> = pending
      .iter()
      .filter(|(_, req)| req.client_id == client_id)
      .map(|(k, _)| k.clone())
      .collect();

    for k in keys_to_remove {
      if let Some(_req) = pending.remove(&k) {
        // Drop the sender channel, triggering an immediate channel cancellation / 502 Bad Gateway
        debug!(
          "Aborted pending request ID {} due to active client connection loss",
          k
        );
        // The oneshot channel dropping will wake the handler thread to reply immediately.
      }
    }
  }

  // Abort pending upgrade responses routed to the disconnected client
  {
    let mut pending = state.pending_upgrades.lock().await;
    let keys_to_remove: Vec<String> = pending
      .iter()
      .filter(|(_, req)| req.client_id == client_id)
      .map(|(k, _)| k.clone())
      .collect();
    for k in keys_to_remove {
      pending.remove(&k);
    }
  }

  // Terminate in-flight streamed response bodies from the disconnected client
  // (dropping the senders ends the corresponding public HTTP bodies).
  {
    let mut streams = state.response_streams.lock().await;
    streams.retain(|_, handle| handle.client_id != client_id);
  }

  // Close TCP tunnel streams owned by the disconnected client.
  {
    let mut streams = state.tcp_streams.lock().await;
    let closing: Vec<_> = streams
      .iter()
      .filter(|(_, h)| h.client_id == client_id)
      .map(|(_, h)| h.tx.clone())
      .collect();
    streams.retain(|_, h| h.client_id != client_id);
    drop(streams);
    for tx in closing {
      let _ = tx.send(TcpConsumerMsg::Close).await;
    }
  }
}

/// Validates the `aperio_session` cookie and returns true if the session is still active.
async fn validate_session(state: &AppState, headers: &HeaderMap) -> bool {
  // Lazy garbage collection of expired sessions (runs at most once per 5 minutes).
  {
    let mut last_gc = state.last_session_gc.lock().await;
    if last_gc.elapsed() > Duration::from_secs(300) {
      let mut sessions = state.sessions.lock().await;
      let now = Instant::now();
      sessions.retain(|_, info| info.expires_at > now);
      *last_gc = now;
    }
  }

  if let Some(cookie_header) = headers.get("cookie")
    && let Ok(cookie_str) = cookie_header.to_str()
  {
    for part in cookie_str.split(';') {
      let kv: Vec<&str> = part.trim().splitn(2, '=').collect();
      if kv.len() == 2 && kv[0] == "aperio_session" {
        // Reject cookie values that are not valid UUIDs (session tokens are
        // always generated with uuid::Uuid::new_v4). This avoids unnecessary
        // HashMap lookups and prevents injection of malformed keys.
        if uuid::Uuid::parse_str(kv[1]).is_err() {
          return false;
        }
        let mut sessions = state.sessions.lock().await;
        if let Some(info) = sessions.get(kv[1]) {
          if info.expires_at > Instant::now() {
            return true;
          }
          sessions.remove(kv[1]);
        }
        return false;
      }
    }
  }
  false
}

/// Builds a 504 response: the custom APERIO_504_PAGE HTML when configured,
/// otherwise the given plain-text message.
fn gateway_timeout_response(state: &AppState, fallback: &str) -> Response {
  match state.config.custom_504_page {
    Some(ref html) => (
      StatusCode::GATEWAY_TIMEOUT,
      [("content-type", "text/html; charset=utf-8")],
      html.clone(),
    )
      .into_response(),
    None => (StatusCode::GATEWAY_TIMEOUT, fallback.to_string()).into_response(),
  }
}

/// Checks if an HTTP request is a WebSocket upgrade request.
fn is_websocket_upgrade(method: &Method, headers: &HeaderMap) -> bool {
  if method != Method::GET {
    return false;
  }
  let has_upgrade_header = headers
    .get("upgrade")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
  let has_connection_upgrade = headers
    .get("connection")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.to_lowercase().contains("upgrade"));
  has_upgrade_header && has_connection_upgrade
}

/// Proxy handler for forwarding all incoming HTTP requests to active client.
/// Also detects WebSocket upgrade requests and proxies them as persistent streams.
async fn proxy_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  req: axum::extract::Request<Body>,
) -> Response {
  let method = req.method().clone();
  let uri = req.uri().clone();
  let headers = req.headers().clone();
  let caller_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy);

  // Detect WebSocket upgrade requests and handle separately
  if is_websocket_upgrade(&method, &headers) {
    return handle_ws_proxy(state, req, method, uri, headers, addr, caller_ip).await;
  }

  // --- Normal HTTP proxy below ---

  let method_str = method.to_string();
  let uri_str = uri.to_string();
  let body = req.into_body();
  let start_time = Instant::now();

  // 1. Per-IP Rate Limiting (Token Bucket)
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("Rate Limit Exceeded for IP {}", caller_ip)),
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - IP rate limit exceeded",
    )
      .into_response();
  }

  // 2. Session/Auth check (if configured)
  let auth_required = state.config.auth_credentials.is_some() || state.oidc.is_some();
  if auth_required && !validate_session(&state, &headers).await {
    // Prefer the OIDC SSO flow when configured; fall back to the built-in
    // password login page otherwise.
    let login_path = if state.oidc.is_some() {
      "/aperio/oidc/login"
    } else {
      "/aperio/auth"
    };
    let redirect_url = format!("{}?redirect={}", login_path, safe_redirect_path(&uri_str));
    return Response::builder()
      .status(StatusCode::FOUND)
      .header("Location", redirect_url)
      .body(Body::empty())
      .unwrap();
  }

  // 3. Wait for connection if client is disconnected.
  // Take a consistent snapshot of connection state under a single lock to avoid TOCTOU.
  let (is_connected, _last_disc) = {
    let conn = state.connection_state.lock().await;
    (conn.connected, conn.last_disconnect)
  };
  if !is_connected {
    // Wait for a client to reconnect, bounded by the configured gateway timeout.
    let mut rx = state.client_connected.subscribe();
    let timeout_fut = tokio::time::sleep(state.config.gateway_timeout);
    tokio::pin!(timeout_fut);

    let mut reconnected = false;
    loop {
      tokio::select! {
          _ = &mut timeout_fut => {
              break;
          }
          res = rx.changed() => {
              if res.is_ok() && *rx.borrow() {
                  reconnected = true;
                  break;
              }
          }
      }
    }

    if !reconnected {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("Gateway Timeout - Reconnect wait expired"),
      )
      .await;
      return gateway_timeout_response(&state, "504 Gateway Timeout - No client connected in time");
    }
  }

  // 4. Limit concurrency to prevent resource starvation / DoS
  let _permit = match state.concurrency_semaphore.try_acquire() {
    Ok(p) => p,
    Err(_) => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        429,
        start_time.elapsed(),
        Some("Concurrency limit exceeded"),
      )
      .await;
      return (
        StatusCode::TOO_MANY_REQUESTS,
        "429 Too Many Requests - Concurrency limit reached on tunnel server",
      )
        .into_response();
    }
  };

  // 4. Get an active client, preferring hostname- and path-bound matches
  // with per-group round-robin.
  let request_host = extract_request_host(&headers);
  let client_info = {
    let clients = state.clients.lock().await;
    let uri_path = uri_str.split('?').next().unwrap_or(&uri_str);

    match select_client_pool(
      &clients,
      uri_path,
      request_host.as_deref(),
      state.config.require_hostname_bind,
      state.config.client_down_threshold,
    ) {
      None => None,
      Some((pool, group_key)) => {
        let mut rr_map = state.path_rr.lock().await;
        let idx = rr_map.entry(group_key).or_insert(0);
        let chosen_id = &pool[*idx % pool.len()];
        *idx = (*idx + 1) % pool.len();
        clients.get(chosen_id).map(|c| {
          (
            chosen_id.clone(),
            c.tx.clone(),
            c.request_count.clone(),
            c.inflight_limiter.clone(),
          )
        })
      }
    }
  };

  let (chosen_client_id, client_tx, client_req_counter, inflight_limiter) = match client_info {
    Some(info) => info,
    None => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("No active client connection available"),
      )
      .await;
      return gateway_timeout_response(
        &state,
        "504 Gateway Timeout - Client disconnected before request dispatch",
      );
    }
  };

  // Honor the client's announced concurrency limit: wait (up to the gateway
  // timeout) for an in-flight slot instead of flooding the client's backend.
  let _inflight_permit = match inflight_limiter {
    Some(limiter) => {
      match tokio::time::timeout(state.config.gateway_timeout, limiter.acquire_owned()).await {
        Ok(Ok(permit)) => Some(permit),
        _ => {
          log_request_failure(
            &state,
            &method_str,
            &uri_str,
            429,
            start_time.elapsed(),
            Some("Client concurrency limit: no slot freed within gateway timeout"),
          )
          .await;
          return (
            StatusCode::TOO_MANY_REQUESTS,
            "429 Too Many Requests - Tunnel client concurrency limit reached",
          )
            .into_response();
        }
      }
    }
    None => None,
  };

  // Increment request stats for client
  client_req_counter.fetch_add(1, Ordering::SeqCst);

  // 5. Read body with limit to prevent OOM / DoS
  let body_bytes = match axum::body::to_bytes(body, state.config.max_body_size).await {
    Ok(bytes) => bytes,
    Err(e) => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        413,
        start_time.elapsed(),
        Some(&format!("Payload too large or read failure: {}", e)),
      )
      .await;
      return (
        StatusCode::PAYLOAD_TOO_LARGE,
        "413 Payload Too Large - Request body size exceeds limit",
      )
        .into_response();
    }
  };

  let base64_body = if body_bytes.is_empty() {
    None
  } else {
    use base64::prelude::*;
    Some(BASE64_STANDARD.encode(&body_bytes))
  };

  // Map headers (preserve duplicates by collecting into a Vec).
  // Filter out internal aperio session cookies to prevent leaking dashboard
  // session tokens to tunnel clients.
  let mut serialized_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      if k.as_str() == "cookie" {
        let filtered: String = val_str
          .split(';')
          .filter(|part| !part.trim().starts_with("aperio_session="))
          .map(|part| part.trim())
          .collect::<Vec<&str>>()
          .join("; ");
        if !filtered.is_empty() {
          serialized_headers.push((k.to_string(), filtered));
        }
        continue;
      }
      serialized_headers.push((k.to_string(), val_str.to_string()));
    }
  }

  // Capture (truncated) request data for the dashboard inspector before the
  // originals are moved into the tunnel message.
  let capture_req_headers = serialized_headers.clone();
  let (capture_req_body, capture_req_truncated) = {
    use base64::prelude::*;
    if body_bytes.is_empty() {
      (None, false)
    } else if body_bytes.len() > CAPTURE_BODY_LIMIT {
      (
        Some(BASE64_STANDARD.encode(&body_bytes[..CAPTURE_BODY_LIMIT])),
        true,
      )
    } else {
      (Some(BASE64_STANDARD.encode(&body_bytes)), false)
    }
  };

  let request_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();

  // Insert oneshot receiver to await response mapping
  {
    let mut pending = state.pending_requests.lock().await;
    pending.insert(
      request_id.clone(),
      PendingRequest {
        tx: tx_response,
        client_id: chosen_client_id,
      },
    );
  }

  let tunnel_req = TunnelMessage::Request {
    id: request_id.clone(),
    method: method_str.clone(),
    uri: uri_str.clone(),
    headers: serialized_headers,
    body: base64_body,
  };

  let req_json = match serde_json::to_string(&tunnel_req) {
    Ok(json) => json,
    Err(e) => {
      state.pending_requests.lock().await.remove(&request_id);
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        500,
        start_time.elapsed(),
        Some(&format!("Request serialization failed: {}", e)),
      )
      .await;
      return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
    }
  };

  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_requests.lock().await.remove(&request_id);
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      502,
      start_time.elapsed(),
      Some("Failed to send tunnel frame to websocket client"),
    )
    .await;
    return (
      StatusCode::BAD_GATEWAY,
      "502 Bad Gateway - Client socket error",
    )
      .into_response();
  }

  // Update traffic metrics
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
    stats.total_bytes_transferred += body_bytes.len() as u64;
  }

  // 6. Await response from client with response timeout
  let timeout_fut = tokio::time::sleep(state.config.gateway_response_timeout);
  tokio::pin!(timeout_fut);

  tokio::select! {
      _ = &mut timeout_fut => {
          state.pending_requests.lock().await.remove(&request_id);
          log_request_failure(
              &state,
              &method_str,
              &uri_str,
              504,
              start_time.elapsed(),
              Some("Client response timeout expired"),
          )
          .await;
          state.persistent_stats.lock().await.record_request(false, body_bytes.len() as u64, 0, start_time.elapsed().as_millis() as u64);
          gateway_timeout_response(&state, "504 Gateway Timeout - Gateway response timeout expired")
      }
      res_opt = rx_response => {
          let duration = start_time.elapsed();
          match res_opt {
              Ok(mut tunnel_res) => {
                  let status_code = StatusCode::from_u16(tunnel_res.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

                  let res_bytes = if let Some(ref encoded_body) = tunnel_res.body {
                      use base64::prelude::*;
                      BASE64_STANDARD.decode(encoded_body).unwrap_or_default()
                  } else {
                      Vec::new()
                  };

                  let body_len = res_bytes.len() as u64;

                  let mut response_builder = Response::builder().status(status_code);

                  for (k, v) in tunnel_res.headers.iter() {
                      let k_lower = k.to_lowercase();
                      // Strip connection management headers
                      if k_lower == "connection" || k_lower == "keep-alive" || k_lower == "transfer-encoding" {
                          continue;
                      }
                      if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                          response_builder = response_builder.header(name, value);
                      }
                  }

                  {
                      let mut stats = state.stats.lock().await;
                      // Only count server errors (5xx) as failed. 2xx/3xx/4xx are
                      // legitimate responses successfully proxied through the tunnel.
                      if status_code.is_server_error() {
                          stats.failed_requests += 1;
                      } else {
                          stats.successful_requests += 1;
                      }
                      // Streamed bodies are counted chunk-by-chunk as they arrive.
                      stats.total_bytes_transferred += body_len;
                  }

                  // Persistent (restart-surviving) counters.
                  {
                      let mut ps = state.persistent_stats.lock().await;
                      ps.record_request(
                          !status_code.is_server_error(),
                          body_bytes.len() as u64,
                          body_len,
                          duration.as_millis() as u64,
                      );
                  }

                  log_request_success(&state, request_id.clone(), &method_str, &uri_str, tunnel_res.status, duration).await;

                  // Capture the transaction for the dashboard inspector.
                  {
                      use base64::prelude::*;
                      let resp_streamed = tunnel_res.stream_rx.is_some();
                      let (resp_body_cap, resp_truncated) = if resp_streamed || res_bytes.is_empty() {
                          (None, false)
                      } else if res_bytes.len() > CAPTURE_BODY_LIMIT {
                          (Some(BASE64_STANDARD.encode(&res_bytes[..CAPTURE_BODY_LIMIT])), true)
                      } else {
                          (Some(BASE64_STANDARD.encode(&res_bytes)), false)
                      };
                      let mut captured = state.captured_requests.lock().await;
                      if captured.len() >= CAPTURE_MAX_ENTRIES {
                          captured.pop_front();
                      }
                      captured.push_back(CapturedRequest {
                          id: request_id.clone(),
                          timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                          method: method_str.clone(),
                          uri: uri_str.clone(),
                          req_headers: capture_req_headers,
                          req_body: capture_req_body,
                          req_body_truncated: capture_req_truncated,
                          status: tunnel_res.status,
                          resp_headers: tunnel_res.headers.clone(),
                          resp_body: resp_body_cap,
                          resp_body_truncated: resp_truncated,
                          resp_streamed,
                          duration_ms: duration.as_millis(),
                      });
                  }

                  // Streamed response: forward chunks as they arrive without buffering.
                  let body = if let Some(chunk_rx) = tunnel_res.stream_rx.take() {
                      let stream = futures_util::stream::unfold(chunk_rx, |mut rx| async move {
                          rx.recv().await.map(|item| (item, rx))
                      });
                      Body::from_stream(stream)
                  } else {
                      Body::from(res_bytes)
                  };

                  match response_builder.body(body) {
                      Ok(r) => r,
                      Err(e) => {
                          error!("Error constructing response: {:?}", e);
                          (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
                      }
                  }
              }
              Err(_) => {
                  log_request_failure(
                      &state,
                      &method_str,
                      &uri_str,
                      502,
                      duration,
                      Some("Communication channel with client closed abruptly"),
                  )
                  .await;
                  state.persistent_stats.lock().await.record_request(false, body_bytes.len() as u64, 0, duration.as_millis() as u64);
                  (StatusCode::BAD_GATEWAY, "502 Bad Gateway - Client connection lost in flight").into_response()
              }
          }
      }
  }
}

/// Handles a WebSocket upgrade request from a public client.
/// Performs the same rate-limiting, auth, and client selection as normal HTTP proxy,
/// then establishes a bidirectional relay between the public WebSocket and the tunnel.
async fn handle_ws_proxy(
  state: Arc<AppState>,
  req: axum::extract::Request<Body>,
  method: Method,
  uri: Uri,
  headers: HeaderMap,
  _addr: SocketAddr,
  caller_ip: IpAddr,
) -> Response {
  let method_str = method.to_string();
  let uri_str = uri.to_string();
  let start_time = Instant::now();

  // 1. Per-IP Rate Limiting
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("Rate Limit Exceeded for IP {}", caller_ip)),
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - IP rate limit exceeded",
    )
      .into_response();
  }

  // 2. Session/Auth check
  let auth_required = state.config.auth_credentials.is_some() || state.oidc.is_some();
  if auth_required && !validate_session(&state, &headers).await {
    // Prefer the OIDC SSO flow when configured; fall back to the built-in
    // password login page otherwise.
    let login_path = if state.oidc.is_some() {
      "/aperio/oidc/login"
    } else {
      "/aperio/auth"
    };
    let redirect_url = format!("{}?redirect={}", login_path, safe_redirect_path(&uri_str));
    return Response::builder()
      .status(StatusCode::FOUND)
      .header("Location", redirect_url)
      .body(Body::empty())
      .unwrap();
  }

  // 3. Wait for connection
  let (is_connected, _last_disc) = {
    let conn = state.connection_state.lock().await;
    (conn.connected, conn.last_disconnect)
  };
  if !is_connected {
    let mut rx = state.client_connected.subscribe();
    let timeout_fut = tokio::time::sleep(state.config.gateway_timeout);
    tokio::pin!(timeout_fut);

    let mut reconnected = false;
    loop {
      tokio::select! {
          _ = &mut timeout_fut => {
              break;
          }
          res = rx.changed() => {
              if res.is_ok() && *rx.borrow() {
                  reconnected = true;
                  break;
              }
          }
      }
    }

    if !reconnected {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("Gateway Timeout - Reconnect wait expired"),
      )
      .await;
      return gateway_timeout_response(&state, "504 Gateway Timeout - No client connected in time");
    }
  }

  // 4. Select a tunnel client (same hostname/path-aware routing as HTTP proxy)
  let uri_path = uri_str.split('?').next().unwrap_or(&uri_str);
  let request_host = extract_request_host(&headers);
  let client_info = {
    let clients = state.clients.lock().await;
    match select_client_pool(
      &clients,
      uri_path,
      request_host.as_deref(),
      state.config.require_hostname_bind,
      state.config.client_down_threshold,
    ) {
      None => None,
      Some((pool, group_key)) => {
        let mut rr_map = state.path_rr.lock().await;
        let idx = rr_map.entry(group_key).or_insert(0);
        let chosen_id = &pool[*idx % pool.len()];
        *idx = (*idx + 1) % pool.len();
        clients
          .get(chosen_id)
          .map(|c| (chosen_id.clone(), c.tx.clone(), c.request_count.clone()))
      }
    }
  };

  let (chosen_client_id, client_tx, client_req_counter) = match client_info {
    Some(info) => info,
    None => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("No active client for WebSocket upgrade"),
      )
      .await;
      return gateway_timeout_response(
        &state,
        "504 Gateway Timeout - No client available for WebSocket upgrade",
      );
    }
  };

  client_req_counter.fetch_add(1, Ordering::SeqCst);

  // Serialize headers (same filtering as normal proxy)
  let mut serialized_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      if k.as_str() == "cookie" {
        let filtered: String = val_str
          .split(';')
          .filter(|part| !part.trim().starts_with("aperio_session="))
          .map(|part| part.trim())
          .collect::<Vec<&str>>()
          .join("; ");
        if !filtered.is_empty() {
          serialized_headers.push((k.to_string(), filtered));
        }
        continue;
      }
      serialized_headers.push((k.to_string(), val_str.to_string()));
    }
  }

  let stream_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();

  // Register pending upgrade response
  {
    let mut pending = state.pending_upgrades.lock().await;
    pending.insert(
      stream_id.clone(),
      PendingRequest {
        tx: tx_response,
        client_id: chosen_client_id.clone(),
      },
    );
  }

  // Send UpgradeRequest to client via tunnel
  let upgrade_req = TunnelMessage::UpgradeRequest {
    id: stream_id.clone(),
    method: method_str.clone(),
    uri: uri_str.clone(),
    headers: serialized_headers,
  };

  let req_json = match serde_json::to_string(&upgrade_req) {
    Ok(json) => json,
    Err(e) => {
      state.pending_upgrades.lock().await.remove(&stream_id);
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        500,
        start_time.elapsed(),
        Some(&format!("UpgradeRequest serialization failed: {}", e)),
      )
      .await;
      return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
    }
  };

  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_upgrades.lock().await.remove(&stream_id);
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      502,
      start_time.elapsed(),
      Some("Failed to send UpgradeRequest to client"),
    )
    .await;
    return (
      StatusCode::BAD_GATEWAY,
      "502 Bad Gateway - Client socket error",
    )
      .into_response();
  }

  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
  }

  // Await UpgradeResponse from client
  let timeout_fut = tokio::time::sleep(state.config.gateway_response_timeout);
  tokio::pin!(timeout_fut);

  let client_response = tokio::select! {
      _ = &mut timeout_fut => {
          state.pending_upgrades.lock().await.remove(&stream_id);
          log_request_failure(
              &state,
              &method_str,
              &uri_str,
              504,
              start_time.elapsed(),
              Some("WebSocket upgrade response timeout"),
          )
          .await;
          return (StatusCode::GATEWAY_TIMEOUT, "504 Gateway Timeout - Upgrade response timeout").into_response();
      }
      res = rx_response => {
          match res {
              Ok(r) => r,
              Err(_) => {
                  log_request_failure(
                      &state,
                      &method_str,
                      &uri_str,
                      502,
                      start_time.elapsed(),
                      Some("Client disconnected during WebSocket upgrade"),
                  )
                  .await;
                  return (StatusCode::BAD_GATEWAY, "502 Bad Gateway - Client lost during upgrade").into_response();
              }
          }
      }
  };

  if client_response.status != 101 {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      client_response.status,
      start_time.elapsed(),
      Some("Client failed to establish backend WebSocket"),
    )
    .await;
    return (
      StatusCode::from_u16(client_response.status).unwrap_or(StatusCode::BAD_GATEWAY),
      "Backend WebSocket connection failed",
    )
      .into_response();
  }

  // Client confirmed upgrade. Now perform the public-side WebSocket upgrade.
  let (parts, body) = req.into_parts();
  let req = axum::extract::Request::from_parts(parts, body);

  let upgrade_result: Result<WebSocketUpgrade, _> =
    WebSocketUpgrade::from_request(req, &state).await;

  match upgrade_result {
    Ok(ws) => {
      let state_clone = state.clone();
      let stream_id_clone = stream_id.clone();
      let client_tx_clone = client_tx.clone();
      let method_clone = method_str.clone();
      let uri_clone = uri_str.clone();
      let start_time_clone = start_time;

      ws.on_upgrade(move |public_ws| {
        relay_ws_stream(
          state_clone,
          stream_id_clone,
          public_ws,
          client_tx_clone,
          method_clone,
          uri_clone,
          start_time_clone,
        )
      })
    }
    Err(rejection) => {
      // Send WsClose so the client tears down its backend connection
      let close_msg = TunnelMessage::WsClose {
        stream_id: stream_id.clone(),
        code: 1011,
        reason: "Server upgrade rejected".to_string(),
      };
      if let Ok(json) = serde_json::to_string(&close_msg) {
        let _ = client_tx.send(Message::Text(json)).await;
      }
      state.ws_streams.lock().await.remove(&stream_id);
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        400,
        start_time.elapsed(),
        Some(&format!("WebSocket upgrade rejected: {:?}", rejection)),
      )
      .await;
      rejection.into_response()
    }
  }
}

/// Relays WebSocket frames bidirectionally between the public WebSocket and the tunnel.
async fn relay_ws_stream(
  state: Arc<AppState>,
  stream_id: String,
  public_ws: WebSocket,
  tunnel_tx: mpsc::Sender<Message>,
  method: String,
  uri: String,
  start_time: Instant,
) {
  let (mut ws_sender, mut ws_receiver) = public_ws.split();

  // Channel for relaying frames from tunnel → public WS
  let (relay_tx, mut relay_rx) = mpsc::channel::<WsStreamMessage>(64);

  // Register the relay channel so handle_socket can push WsData frames to us
  {
    let mut streams = state.ws_streams.lock().await;
    streams.insert(stream_id.clone(), relay_tx);
  }

  let stream_id_clone = stream_id.clone();
  let tunnel_tx_clone = tunnel_tx.clone();

  // Task: read from public WS → send WsData through tunnel
  let ws_to_tunnel = tokio::spawn(async move {
    while let Some(result) = ws_receiver.next().await {
      match result {
        Ok(msg) => {
          let tunnel_msg = match msg {
            Message::Text(text) => TunnelMessage::WsData {
              stream_id: stream_id_clone.clone(),
              data: text.to_string(),
              is_text: true,
            },
            Message::Binary(data) => {
              use base64::prelude::*;
              TunnelMessage::WsData {
                stream_id: stream_id_clone.clone(),
                data: BASE64_STANDARD.encode(&data),
                is_text: false,
              }
            }
            Message::Close(frame) => TunnelMessage::WsClose {
              stream_id: stream_id_clone.clone(),
              code: frame.as_ref().map(|f| f.code).unwrap_or(1000),
              reason: frame
                .as_ref()
                .map(|f| f.reason.to_string())
                .unwrap_or_default(),
            },
            Message::Ping(_) | Message::Pong(_) => {
              // Auto-handled by Axum, no need to forward
              continue;
            }
          };

          if let Ok(json) = serde_json::to_string(&tunnel_msg)
            && tunnel_tx_clone.send(Message::Text(json)).await.is_err()
          {
            break;
          }
        }
        Err(e) => {
          debug!(
            "Public WS read error for stream {}: {:?}",
            stream_id_clone, e
          );
          break;
        }
      }
    }

    // Send WsClose to tunnel when public WS disconnects
    let close_msg = TunnelMessage::WsClose {
      stream_id: stream_id_clone.clone(),
      code: 1000,
      reason: String::new(),
    };
    if let Ok(json) = serde_json::to_string(&close_msg) {
      let _ = tunnel_tx_clone.send(Message::Text(json)).await;
    }
  });

  // Task: read from relay channel (tunnel → public WS) → write to public WS
  let ws_writer = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        WsStreamMessage::Data(ws_msg) => {
          if ws_sender.send(ws_msg).await.is_err() {
            break;
          }
        }
        WsStreamMessage::Close => {
          let _ = ws_sender.send(Message::Close(None)).await;
          break;
        }
      }
    }
  });

  let ws_to_tunnel_abort = ws_to_tunnel.abort_handle();
  let ws_writer_abort = ws_writer.abort_handle();

  // Wait for either task to finish; abort the other
  tokio::select! {
      _ = ws_to_tunnel => {
          ws_writer_abort.abort();
      }
      _ = ws_writer => {
          ws_to_tunnel_abort.abort();
      }
  }

  state.ws_streams.lock().await.remove(&stream_id);

  let duration = start_time.elapsed();
  let safe_uri = sanitize_uri(&uri);
  info!(
    "WebSocket stream {} closed: {} {} after {}ms",
    stream_id,
    method,
    safe_uri,
    duration.as_millis()
  );
}


/// Experimental TCP tunneling endpoint (`GET /aperio/tcp`, WebSocket).
/// Consumers authenticate with a tunnel token (master or dynamic) and get a
/// raw byte relay to the TCP target configured on a TCP-enabled client.
/// Binary WebSocket frames = raw TCP bytes.
async fn tcp_ws_handler(
  ws: WebSocketUpgrade,
  headers: HeaderMap,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  State(state): State<Arc<AppState>>,
) -> Response {
  let caller_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy);
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  if authorize_tunnel_token(&state, &headers, caller_ip)
    .await
    .is_none()
  {
    info!("Unauthorized TCP tunnel attempt blocked.");
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  }

  // Select a TCP-capable, eligible client.
  let client_info = {
    let clients = state.clients.lock().await;
    clients
      .iter()
      .find(|(_, c)| {
        c.tcp_enabled
          && c.admin_enabled
          && !c.draining
          && c.is_healthy(state.config.client_down_threshold)
      })
      .map(|(id, c)| (id.clone(), c.tx.clone()))
  };
  let Some((client_id, client_tx)) = client_info else {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      "No TCP-capable tunnel client connected",
    )
      .into_response();
  };

  state
    .audit(
      "tcp_stream_opened",
      &caller_ip.to_string(),
      &format!("client={}", client_id),
    )
    .await;

  ws.on_upgrade(move |socket| relay_tcp_consumer(state, socket, client_id, client_tx))
}

/// Relays bytes between a public TCP consumer WebSocket and the tunnel.
async fn relay_tcp_consumer(
  state: Arc<AppState>,
  consumer_ws: WebSocket,
  client_id: String,
  client_tx: mpsc::Sender<Message>,
) {
  let stream_id = uuid::Uuid::new_v4().to_string();
  let (relay_tx, mut relay_rx) = mpsc::channel::<TcpConsumerMsg>(64);
  state.tcp_streams.lock().await.insert(
    stream_id.clone(),
    TcpStreamHandle {
      tx: relay_tx,
      client_id: client_id.clone(),
    },
  );

  // Ask the client to open its TCP target.
  let open = TunnelMessage::TcpOpen {
    stream_id: stream_id.clone(),
  };
  if let Ok(json) = serde_json::to_string(&open) {
    if client_tx.send(Message::Text(json)).await.is_err() {
      state.tcp_streams.lock().await.remove(&stream_id);
      return;
    }
  }

  let (mut ws_sender, mut ws_receiver) = consumer_ws.split();

  // Consumer → tunnel
  let stream_id_up = stream_id.clone();
  let client_tx_up = client_tx.clone();
  let up_task = tokio::spawn(async move {
    use base64::prelude::*;
    while let Some(Ok(msg)) = ws_receiver.next().await {
      let bytes = match msg {
        Message::Binary(b) => b,
        Message::Text(t) => t.into_bytes(),
        Message::Close(_) => break,
        _ => continue,
      };
      let data_msg = TunnelMessage::TcpData {
        stream_id: stream_id_up.clone(),
        data: BASE64_STANDARD.encode(&bytes),
      };
      if let Ok(json) = serde_json::to_string(&data_msg)
        && client_tx_up.send(Message::Text(json)).await.is_err()
      {
        break;
      }
    }
    // Consumer went away → close the client side.
    let close = TunnelMessage::TcpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = client_tx_up.send(Message::Text(json)).await;
    }
  });

  // Tunnel → consumer
  let down_task = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        TcpConsumerMsg::Data(bytes) => {
          if ws_sender.send(Message::Binary(bytes)).await.is_err() {
            break;
          }
        }
        TcpConsumerMsg::Close => {
          let _ = ws_sender.send(Message::Close(None)).await;
          break;
        }
      }
    }
  });

  let up_abort = up_task.abort_handle();
  let down_abort = down_task.abort_handle();
  tokio::select! {
    _ = up_task => down_abort.abort(),
    _ = down_task => up_abort.abort(),
  }

  state.tcp_streams.lock().await.remove(&stream_id);
  debug!("TCP tunnel stream {} closed", stream_id);
}

/// Derives the OIDC redirect URI for this deployment: the explicit override
/// wins, otherwise it is built from the request Host header (and
/// X-Forwarded-Proto when running behind a trusted proxy).
fn oidc_redirect_uri(state: &AppState, headers: &HeaderMap) -> Option<String> {
  let rt = state.oidc.as_ref()?;
  if let Some(ref fixed) = rt.redirect_url_override {
    return Some(fixed.clone());
  }
  let host = headers.get("host").and_then(|v| v.to_str().ok())?;
  let proto = if state.config.trust_proxy {
    headers
      .get("x-forwarded-proto")
      .and_then(|v| v.to_str().ok())
      .unwrap_or("http")
  } else {
    "http"
  };
  Some(format!("{}://{}/aperio/oidc/callback", proto, host))
}

/// Starts the OIDC authorization code flow: stores a CSRF state token and
/// redirects the browser to the identity provider.
async fn oidc_login_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let Some(rt) = state.oidc.clone() else {
    return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
  };
  let caller_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy);
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let redirect_after = query
    .get("redirect")
    .map(|r| safe_redirect_path(r).to_string())
    .unwrap_or_else(|| "/".to_string());
  let Some(redirect_uri) = oidc_redirect_uri(&state, &headers) else {
    return (StatusCode::BAD_REQUEST, "Missing Host header").into_response();
  };

  // Register the CSRF state (10 min TTL, opportunistic GC).
  let state_token = uuid::Uuid::new_v4().to_string();
  {
    let mut states = state.oidc_states.lock().await;
    let now = Instant::now();
    states.retain(|_, (_, exp)| *exp > now);
    states.insert(
      state_token.clone(),
      (redirect_after, now + Duration::from_secs(600)),
    );
  }

  let authorize = url::Url::parse_with_params(
    &rt.authorization_endpoint,
    &[
      ("response_type", "code"),
      ("client_id", rt.client_id.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("scope", rt.scopes.as_str()),
      ("state", state_token.as_str()),
    ],
  );
  match authorize {
    Ok(u) => Response::builder()
      .status(StatusCode::FOUND)
      .header("Location", u.to_string())
      .body(Body::empty())
      .unwrap(),
    Err(e) => {
      error!("Failed to build OIDC authorize URL: {}", e);
      (StatusCode::INTERNAL_SERVER_ERROR, "OIDC configuration error").into_response()
    }
  }
}

/// OIDC callback: validates the CSRF state, exchanges the code for tokens,
/// fetches the userinfo email, checks it against the allowlist, and creates
/// a session identical to the password login.
async fn oidc_callback_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let Some(rt) = state.oidc.clone() else {
    return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
  };
  let caller_ip = extract_client_ip(&headers, addr.ip(), state.config.trust_proxy);
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let (Some(code), Some(state_param)) = (query.get("code"), query.get("state")) else {
    return (StatusCode::BAD_REQUEST, "Missing code/state parameter").into_response();
  };

  // Validate and consume the CSRF state.
  let redirect_after = {
    let mut states = state.oidc_states.lock().await;
    match states.remove(state_param) {
      Some((redirect, exp)) if exp > Instant::now() => redirect,
      _ => {
        return (StatusCode::BAD_REQUEST, "Invalid or expired OIDC state").into_response();
      }
    }
  };
  let Some(redirect_uri) = oidc_redirect_uri(&state, &headers) else {
    return (StatusCode::BAD_REQUEST, "Missing Host header").into_response();
  };

  // Exchange the authorization code for an access token.
  let http = reqwest::Client::builder()
    .timeout(Duration::from_secs(15))
    .build()
    .unwrap_or_default();
  let token_res = http
    .post(&rt.token_endpoint)
    .form(&[
      ("grant_type", "authorization_code"),
      ("code", code.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("client_id", rt.client_id.as_str()),
      ("client_secret", rt.client_secret.as_str()),
    ])
    .send()
    .await;
  #[derive(Deserialize)]
  struct TokenResponse {
    access_token: String,
  }
  let access_token = match token_res {
    Ok(res) if res.status().is_success() => match res.json::<TokenResponse>().await {
      Ok(t) => t.access_token,
      Err(e) => {
        error!("OIDC token response parse error: {}", e);
        return (StatusCode::BAD_GATEWAY, "OIDC token exchange failed").into_response();
      }
    },
    Ok(res) => {
      warn!("OIDC token endpoint returned {}", res.status());
      return (StatusCode::UNAUTHORIZED, "OIDC token exchange rejected").into_response();
    }
    Err(e) => {
      error!("OIDC token exchange failed: {}", e);
      return (StatusCode::BAD_GATEWAY, "OIDC token exchange failed").into_response();
    }
  };

  // Fetch the verified identity from the issuer (trusted via TLS).
  #[derive(Deserialize)]
  struct UserInfo {
    email: Option<String>,
  }
  let userinfo = http
    .get(&rt.userinfo_endpoint)
    .bearer_auth(&access_token)
    .send()
    .await;
  let email = match userinfo {
    Ok(res) if res.status().is_success() => match res.json::<UserInfo>().await {
      Ok(u) => u.email.unwrap_or_default(),
      Err(e) => {
        error!("OIDC userinfo parse error: {}", e);
        return (StatusCode::BAD_GATEWAY, "OIDC userinfo failed").into_response();
      }
    },
    _ => {
      return (StatusCode::BAD_GATEWAY, "OIDC userinfo failed").into_response();
    }
  };

  if !oidc::email_allowed(&email, &rt.allowed_emails) {
    warn!("OIDC login denied for {} (not in allowlist)", email);
    state
      .audit(
        "oidc_login_denied",
        &caller_ip.to_string(),
        &format!("email={}", email),
      )
      .await;
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - Your account is not allowed to access this service",
    )
      .into_response();
  }

  info!("OIDC login success for {}", email);
  state
    .audit(
      "oidc_login_success",
      &caller_ip.to_string(),
      &format!("email={}", email),
    )
    .await;

  // Create a session identical to the password login flow.
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    session_token.clone(),
    SessionInfo {
      expires_at: Instant::now() + Duration::from_secs(86400),
    },
  );
  let secure_flag = if state.config.secure_cookies {
    "; Secure"
  } else {
    ""
  };
  let cookie = format!(
    "aperio_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_token, secure_flag
  );
  Response::builder()
    .status(StatusCode::FOUND)
    .header("Set-Cookie", cookie)
    .header("Location", redirect_after)
    .body(Body::empty())
    .unwrap()
}

/// Validates a redirect path to prevent open redirect attacks.
/// Only allows same-origin relative paths (starting with `/`) and rejects
/// protocol-relative URLs (`//evil.com`) and backslash-based bypasses (`/\`).
fn safe_redirect_path(uri: &str) -> &str {
  if uri.starts_with('/') && !uri.starts_with("//") && !uri.starts_with("/\\") {
    uri
  } else {
    "/"
  }
}

/// Strips the query string from a URI to avoid logging sensitive data
/// (API keys, tokens, PII) that may be carried in query parameters.
fn sanitize_uri(uri: &str) -> &str {
  uri.split('?').next().unwrap_or(uri)
}

async fn log_request_success(
  state: &Arc<AppState>,
  id: String,
  method: &str,
  uri: &str,
  status: u16,
  duration: Duration,
) {
  let safe_uri = sanitize_uri(uri);
  let mut logs = state.recent_logs.lock().await;
  if logs.len() >= 100 {
    logs.pop_front();
  }
  let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
  logs.push_back(RequestLog {
    id: id.clone(),
    timestamp,
    method: method.to_string(),
    uri: safe_uri.to_string(),
    status: Some(status),
    duration_ms: duration.as_millis(),
    error: None,
  });
  info!(
    "Proxy SUCCESS: ID={} Method={} URI={} Status={} Duration={}ms",
    id,
    method,
    safe_uri,
    status,
    duration.as_millis()
  );
}

async fn log_request_failure(
  state: &Arc<AppState>,
  method: &str,
  uri: &str,
  status: u16,
  duration: Duration,
  error: Option<&str>,
) {
  let safe_uri = sanitize_uri(uri);
  let mut logs = state.recent_logs.lock().await;
  if logs.len() >= 100 {
    logs.pop_front();
  }
  let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
  let id = uuid::Uuid::new_v4().to_string();
  logs.push_back(RequestLog {
    id: id.clone(),
    timestamp,
    method: method.to_string(),
    uri: safe_uri.to_string(),
    status: Some(status),
    duration_ms: duration.as_millis(),
    error: error.map(|s| s.to_string()),
  });
  warn!(
    "Proxy FAILURE: ID={} Method={} URI={} Status={} Duration={}ms Error={:?}",
    id,
    method,
    safe_uri,
    status,
    duration.as_millis(),
    error
  );
}

#[cfg(test)]
mod tests {
  use super::*;
  use base64::Engine;
  use std::net::Ipv4Addr;

  #[test]
  fn test_token_authentication() {
    let mut headers = HeaderMap::new();
    assert!(!extract_and_verify_token(&headers, "secret"));

    headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
    assert!(extract_and_verify_token(&headers, "secret"));
    assert!(!extract_and_verify_token(&headers, "wrong_secret"));

    headers.clear();
    headers.insert("x-auth-token", HeaderValue::from_static("secret"));
    assert!(extract_and_verify_token(&headers, "secret"));
    assert!(!extract_and_verify_token(&headers, "wrong_secret"));
  }

  #[tokio::test]
  async fn test_rate_limiting() {
    let config = ServerConfig {
      token: "test".to_string(),
      gateway_timeout: Duration::from_secs(1),
      gateway_response_timeout: Duration::from_secs(1),
      max_body_size: 1024,
      max_tunnels: 1,
      ip_limit_max: 2.0,
      ip_limit_refill: 0.0, // No refill for testing strict burst limit
      auth_credentials: None,
      trust_proxy: false,
      secure_cookies: false,
      require_hostname_bind: false,
      metrics_token: None,
      random_subdomain_suffix: None,
      client_down_threshold: Duration::from_secs(3600),
      tunnel_compression: false,
      custom_504_page: None,
    };

    let (client_connected_tx, _) = watch::channel(false);
    let state = AppState {
      clients: Mutex::new(HashMap::new()),
      client_connected: client_connected_tx,
      connection_state: Mutex::new(ConnectionState {
        connected: false,
        last_disconnect: None,
      }),
      server_start_time: Instant::now(),
      pending_requests: Mutex::new(HashMap::new()),
      stats: Mutex::new(ServerStats {
        total_requests: 0,
        successful_requests: 0,
        failed_requests: 0,
        total_bytes_transferred: 0,
      }),
      recent_logs: Mutex::new(VecDeque::new()),
      config,
      concurrency_semaphore: Semaphore::new(10),
      path_rr: Mutex::new(HashMap::new()),
      sessions: Mutex::new(HashMap::new()),
      rate_limiter: Mutex::new(HashMap::new()),
      last_session_gc: Mutex::new(Instant::now()),
      last_rate_gc: Mutex::new(Instant::now()),
      active_tunnel_count: AtomicUsize::new(0),
      ws_streams: Mutex::new(HashMap::new()),
      pending_upgrades: Mutex::new(HashMap::new()),
      token_store: Mutex::new(test_token_store()),
      response_streams: Mutex::new(HashMap::new()),
      captured_requests: Mutex::new(VecDeque::new()),
      audit: Mutex::new(test_audit_log()),
      persistent_stats: Mutex::new(test_stats_store()),
      webhook_store: Mutex::new(test_webhook_store()),
      oidc: None,
      oidc_states: Mutex::new(HashMap::new()),
      tcp_streams: Mutex::new(HashMap::new()),
    };

    let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

    // First request should pass
    assert!(state.check_rate_limit(ip).await);
    // Second request should pass
    assert!(state.check_rate_limit(ip).await);
    // Third request should be rate limited (max burst is 2.0)
    assert!(!state.check_rate_limit(ip).await);
  }

  #[tokio::test]
  async fn test_proxy_handler_gateway_timeout_offline() {
    let config = ServerConfig {
      token: "test".to_string(),
      gateway_timeout: Duration::from_millis(100),
      gateway_response_timeout: Duration::from_millis(100),
      max_body_size: 1024,
      max_tunnels: 1,
      ip_limit_max: 100.0,
      ip_limit_refill: 10.0,
      auth_credentials: None,
      trust_proxy: false,
      secure_cookies: false,
      require_hostname_bind: false,
      metrics_token: None,
      random_subdomain_suffix: None,
      client_down_threshold: Duration::from_secs(3600),
      tunnel_compression: false,
      custom_504_page: None,
    };

    let (client_connected_tx, _) = watch::channel(false);
    let state = Arc::new(AppState {
      clients: Mutex::new(HashMap::new()),
      client_connected: client_connected_tx,
      connection_state: Mutex::new(ConnectionState {
        connected: false,
        last_disconnect: None,
      }),
      // Set start time to 2 minutes ago to trigger immediate timeout
      server_start_time: Instant::now() - Duration::from_secs(120),
      pending_requests: Mutex::new(HashMap::new()),
      stats: Mutex::new(ServerStats {
        total_requests: 0,
        successful_requests: 0,
        failed_requests: 0,
        total_bytes_transferred: 0,
      }),
      recent_logs: Mutex::new(VecDeque::new()),
      config,
      concurrency_semaphore: Semaphore::new(10),
      path_rr: Mutex::new(HashMap::new()),
      sessions: Mutex::new(HashMap::new()),
      rate_limiter: Mutex::new(HashMap::new()),
      last_session_gc: Mutex::new(Instant::now()),
      last_rate_gc: Mutex::new(Instant::now()),
      active_tunnel_count: AtomicUsize::new(0),
      ws_streams: Mutex::new(HashMap::new()),
      pending_upgrades: Mutex::new(HashMap::new()),
      token_store: Mutex::new(test_token_store()),
      response_streams: Mutex::new(HashMap::new()),
      captured_requests: Mutex::new(VecDeque::new()),
      audit: Mutex::new(test_audit_log()),
      persistent_stats: Mutex::new(test_stats_store()),
      webhook_store: Mutex::new(test_webhook_store()),
      oidc: None,
      oidc_states: Mutex::new(HashMap::new()),
      tcp_streams: Mutex::new(HashMap::new()),
    });

    let response = proxy_handler(
      State(state),
      ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
      axum::extract::Request::new(Body::empty()),
    )
    .await;

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
  }

  #[tokio::test]
  async fn test_proxy_handler_success() {
    let config = ServerConfig {
      token: "test".to_string(),
      gateway_timeout: Duration::from_millis(200),
      gateway_response_timeout: Duration::from_millis(500),
      max_body_size: 1024,
      max_tunnels: 2,
      ip_limit_max: 100.0,
      ip_limit_refill: 10.0,
      auth_credentials: None,
      trust_proxy: false,
      secure_cookies: false,
      require_hostname_bind: false,
      metrics_token: None,
      random_subdomain_suffix: None,
      client_down_threshold: Duration::from_secs(3600),
      tunnel_compression: false,
      custom_504_page: None,
    };

    let (client_connected_tx, _) = watch::channel(true);
    let state = Arc::new(AppState {
      clients: Mutex::new(HashMap::new()),
      client_connected: client_connected_tx,
      connection_state: Mutex::new(ConnectionState {
        connected: true,
        last_disconnect: None,
      }),
      server_start_time: Instant::now(),
      pending_requests: Mutex::new(HashMap::new()),
      stats: Mutex::new(ServerStats {
        total_requests: 0,
        successful_requests: 0,
        failed_requests: 0,
        total_bytes_transferred: 0,
      }),
      recent_logs: Mutex::new(VecDeque::new()),
      config,
      concurrency_semaphore: Semaphore::new(10),
      path_rr: Mutex::new(HashMap::new()),
      sessions: Mutex::new(HashMap::new()),
      rate_limiter: Mutex::new(HashMap::new()),
      last_session_gc: Mutex::new(Instant::now()),
      last_rate_gc: Mutex::new(Instant::now()),
      active_tunnel_count: AtomicUsize::new(0),
      ws_streams: Mutex::new(HashMap::new()),
      pending_upgrades: Mutex::new(HashMap::new()),
      token_store: Mutex::new(test_token_store()),
      response_streams: Mutex::new(HashMap::new()),
      captured_requests: Mutex::new(VecDeque::new()),
      audit: Mutex::new(test_audit_log()),
      persistent_stats: Mutex::new(test_stats_store()),
      webhook_store: Mutex::new(test_webhook_store()),
      oidc: None,
      oidc_states: Mutex::new(HashMap::new()),
      tcp_streams: Mutex::new(HashMap::new()),
    });

    let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);
    let client_req_count = Arc::new(AtomicU64::new(0));

    state.clients.lock().await.insert(
      "mock-client-1".to_string(),
      ClientHandle {
        tx: tx_write,
        connected_at: Instant::now(),
        client_ip: "127.0.0.1".to_string(),
        request_count: client_req_count,
        declared_path: None,
        assigned_path: None,
        declared_hostname: None,
        assigned_hostnames: Vec::new(),
        override_path_bind: None,
        override_hostname_bind: None,
        last_ping_at: None,
        perms: ClientPerms::master(),
        max_concurrent: None,
        inflight_limiter: None,
        draining: false,
        admin_enabled: true,
        tcp_enabled: false,
      },
    );

    let state_clone = state.clone();
    tokio::spawn(async move {
      if let Some(Message::Text(text)) = rx_write.recv().await
        && let Ok(TunnelMessage::Request { id, .. }) = serde_json::from_str::<TunnelMessage>(&text)
      {
        let mut pending = state_clone.pending_requests.lock().await;
        if let Some(req) = pending.remove(&id) {
          let headers = vec![("content-type".to_string(), "application/json".to_string())];
          let _ = req.tx.send(TunnelResponse {
            status: 200,
            headers,
            body: Some(base64::prelude::BASE64_STANDARD.encode(r#"{"status":"ok"}"#)),
            stream_rx: None,
          });
        }
      }
    });

    let response = proxy_handler(
      State(state),
      ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
      axum::extract::Request::new(Body::empty()),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
      response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap(),
      "application/json"
    );
  }

  #[test]
  fn test_path_matches_bind_segment_boundary() {
    // Exact match
    assert!(path_matches_bind("/api", "/api"));
    // Segment boundary: trailing slash should match
    assert!(path_matches_bind("/api/users", "/api"));
    // Non-boundary prefix must NOT match (the original bug)
    assert!(!path_matches_bind("/apixyz", "/api"));
    assert!(!path_matches_bind("/api-v2", "/api"));
    // Empty bind semantics
    assert!(!path_matches_bind("/", "/api"));
  }

  #[test]
  fn test_normalize_path_bind() {
    // Empty / root → None
    assert_eq!(normalize_path_bind(""), None);
    assert_eq!(normalize_path_bind("/"), None);
    assert_eq!(normalize_path_bind("   "), None);
    // Adds leading slash
    assert_eq!(normalize_path_bind("api"), Some("/api".to_string()));
    // Strips trailing slashes
    assert_eq!(normalize_path_bind("/api/"), Some("/api".to_string()));
    assert_eq!(normalize_path_bind("/api///"), Some("/api".to_string()));
    // Nested paths preserved
    assert_eq!(normalize_path_bind("/api/v2"), Some("/api/v2".to_string()));
    // Path traversal rejected
    assert_eq!(normalize_path_bind("/api/../etc"), None);
    assert_eq!(normalize_path_bind("/.."), None);
    assert_eq!(normalize_path_bind("/./api"), None);
    // Unsafe characters rejected
    assert_eq!(normalize_path_bind("/api;rm -rf"), None);
    assert_eq!(normalize_path_bind("/api?x=1"), None);
    // Allowed special characters
    assert_eq!(
      normalize_path_bind("/api_v2.1"),
      Some("/api_v2.1".to_string())
    );
    assert_eq!(normalize_path_bind("/a-b~c"), Some("/a-b~c".to_string()));
  }

  #[test]
  fn test_sanitize_uri_strips_query() {
    assert_eq!(sanitize_uri("/api/users?id=42&token=secret"), "/api/users");
    assert_eq!(sanitize_uri("/api"), "/api");
    assert_eq!(sanitize_uri("/api?"), "/api");
    // Multiple '?' → first split wins
    assert_eq!(sanitize_uri("/api?a=1?b=2"), "/api");
  }

  #[test]
  fn test_extract_client_ip_trusted() {
    let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

    // No headers → fallback to socket address
    let headers = HeaderMap::new();
    assert_eq!(extract_client_ip(&headers, direct, true), direct);

    // X-Forwarded-For with single IP
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
    assert_eq!(
      extract_client_ip(&headers, direct, true),
      "198.51.100.10".parse::<IpAddr>().unwrap()
    );

    // X-Forwarded-For with chained proxies → leftmost (original client)
    let mut headers = HeaderMap::new();
    headers.insert(
      "x-forwarded-for",
      HeaderValue::from_static("198.51.100.10, 10.0.0.1, 10.0.0.2"),
    );
    assert_eq!(
      extract_client_ip(&headers, direct, true),
      "198.51.100.10".parse::<IpAddr>().unwrap()
    );

    // X-Real-IP fallback when X-Forwarded-For absent
    let mut headers = HeaderMap::new();
    headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
    assert_eq!(
      extract_client_ip(&headers, direct, true),
      "198.51.100.20".parse::<IpAddr>().unwrap()
    );

    // Malformed X-Forwarded-For → fallback
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
    assert_eq!(extract_client_ip(&headers, direct, true), direct);
  }

  #[test]
  fn test_extract_client_ip_untrusted_ignores_headers() {
    let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

    // When trust_proxy is false, spoofed X-Forwarded-For must be ignored.
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
    assert_eq!(extract_client_ip(&headers, direct, false), direct);

    // Spoofed X-Real-IP must also be ignored.
    let mut headers = HeaderMap::new();
    headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
    assert_eq!(extract_client_ip(&headers, direct, false), direct);

    // No headers → fallback to socket address
    let headers = HeaderMap::new();
    assert_eq!(extract_client_ip(&headers, direct, false), direct);
  }

  /// Generous health threshold so mock clients (no pings) stay eligible.
  const TEST_THRESHOLD: Duration = Duration::from_secs(3600);

  fn test_token_store() -> TokenStore {
    let dir = std::env::temp_dir().join(format!("aperio-test-store-{}", uuid::Uuid::new_v4()));
    TokenStore::load(&dir.to_string_lossy())
  }

  fn test_audit_log() -> AuditLog {
    let dir = std::env::temp_dir().join(format!("aperio-test-audit-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&dir);
    AuditLog::load(&dir.to_string_lossy())
  }

  fn test_stats_store() -> StatsStore {
    let dir = std::env::temp_dir().join(format!("aperio-test-stats-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&dir);
    StatsStore::load(&dir.to_string_lossy())
  }

  fn test_webhook_store() -> WebhookStore {
    let dir = std::env::temp_dir().join(format!("aperio-test-hooks-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&dir);
    WebhookStore::load(&dir.to_string_lossy())
  }

  fn mock_client(
    hostname_bind: Option<&str>,
    path_bind: Option<&str>,
    override_hostname: Option<&str>,
    override_path: Option<&str>,
  ) -> ClientHandle {
    let (tx, _rx) = mpsc::channel::<Message>(1);
    ClientHandle {
      tx,
      connected_at: Instant::now(),
      client_ip: "127.0.0.1".to_string(),
      request_count: Arc::new(AtomicU64::new(0)),
      declared_path: path_bind.map(|s| s.to_string()),
      assigned_path: None,
      declared_hostname: hostname_bind.map(|s| s.to_string()),
      assigned_hostnames: Vec::new(),
      override_path_bind: override_path.map(|s| s.to_string()),
      override_hostname_bind: override_hostname.map(|s| s.to_string()),
      last_ping_at: None,
      perms: ClientPerms::master(),
      max_concurrent: None,
      inflight_limiter: None,
      draining: false,
      admin_enabled: true,
      tcp_enabled: false,
    }
  }

  #[test]
  fn test_select_client_pool_excludes_unhealthy() {
    let mut clients = HashMap::new();
    let mut stale = mock_client(None, None, None, None);
    // Last heartbeat far in the past -> down
    stale.last_ping_at = Some(Instant::now() - Duration::from_secs(120));
    clients.insert("stale".to_string(), stale);

    // Only client is unhealthy -> nothing selectable
    assert!(select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).is_none());

    // A fresh client joins -> traffic goes only to it
    let mut fresh = mock_client(None, None, None, None);
    fresh.last_ping_at = Some(Instant::now());
    clients.insert("fresh".to_string(), fresh);
    let (pool, _) =
      select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
    assert_eq!(pool, vec!["fresh".to_string()]);

    // The stale client recovers with a new ping -> back in the pool
    clients.get_mut("stale").unwrap().last_ping_at = Some(Instant::now());
    let (pool, _) =
      select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
    assert_eq!(pool.len(), 2);
  }

  #[test]
  fn test_ip_allowed() {
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();

    // Empty list or wildcards allow everything
    assert!(ip_allowed(ip("1.2.3.4"), &[]));
    assert!(ip_allowed(ip("1.2.3.4"), &["*".to_string()]));
    assert!(ip_allowed(ip("1.2.3.4"), &["0.0.0.0/0".to_string()]));
    assert!(ip_allowed(ip("::1"), &["::/0".to_string()]));

    // Exact IP match
    assert!(ip_allowed(ip("1.2.3.4"), &["1.2.3.4".to_string()]));
    assert!(!ip_allowed(ip("1.2.3.5"), &["1.2.3.4".to_string()]));

    // CIDR ranges
    assert!(ip_allowed(ip("10.1.2.3"), &["10.0.0.0/8".to_string()]));
    assert!(!ip_allowed(ip("11.1.2.3"), &["10.0.0.0/8".to_string()]));
    assert!(ip_allowed(ip("192.168.1.77"), &["192.168.1.0/24".to_string()]));
    assert!(!ip_allowed(ip("192.168.2.77"), &["192.168.1.0/24".to_string()]));

    // Multiple entries: any match wins
    assert!(ip_allowed(
      ip("203.0.113.9"),
      &["10.0.0.0/8".to_string(), "203.0.113.0/24".to_string()]
    ));

    // IPv6 CIDR
    assert!(ip_allowed(ip("fd00::1"), &["fd00::/8".to_string()]));
    assert!(!ip_allowed(ip("2001:db8::1"), &["fd00::/8".to_string()]));
    // Family mismatch never matches
    assert!(!ip_allowed(ip("1.2.3.4"), &["fd00::/8".to_string()]));

    // Malformed entries are ignored (do not match)
    assert!(!ip_allowed(ip("1.2.3.4"), &["not-an-ip".to_string()]));

    // Validation helper
    assert!(valid_ip_entry("10.0.0.0/8"));
    assert!(valid_ip_entry("1.2.3.4"));
    assert!(valid_ip_entry("::1"));
    assert!(valid_ip_entry("*"));
    assert!(!valid_ip_entry("10.0.0.0/33"));
    assert!(!valid_ip_entry("banana"));
  }

  #[test]
  fn test_normalize_hostname_bind() {
    assert_eq!(
      normalize_hostname_bind("a.example.com"),
      Some("a.example.com".to_string())
    );
    // Case-insensitive
    assert_eq!(
      normalize_hostname_bind("A.Example.COM"),
      Some("a.example.com".to_string())
    );
    // Port stripped
    assert_eq!(
      normalize_hostname_bind("a.example.com:8080"),
      Some("a.example.com".to_string())
    );
    // Trailing dot stripped
    assert_eq!(
      normalize_hostname_bind("a.example.com."),
      Some("a.example.com".to_string())
    );
    // Invalid values rejected
    assert_eq!(normalize_hostname_bind(""), None);
    assert_eq!(normalize_hostname_bind("   "), None);
    assert_eq!(normalize_hostname_bind("exa mple.com"), None);
    assert_eq!(normalize_hostname_bind("example..com"), None);
    assert_eq!(normalize_hostname_bind("exa_mple.com"), None);
    assert_eq!(normalize_hostname_bind(&"a".repeat(300)), None);
  }

  #[test]
  fn test_extract_request_host() {
    let mut headers = HeaderMap::new();
    assert_eq!(extract_request_host(&headers), None);

    headers.insert("host", HeaderValue::from_static("A.Example.com:443"));
    assert_eq!(
      extract_request_host(&headers),
      Some("a.example.com".to_string())
    );

    headers.insert("host", HeaderValue::from_static("[::1]:8080"));
    assert_eq!(extract_request_host(&headers), Some("::1".to_string()));
  }

  #[test]
  fn test_select_client_pool_hostname_routing() {
    let mut clients = HashMap::new();
    clients.insert(
      "a".to_string(),
      mock_client(Some("a.example.com"), None, None, None),
    );
    clients.insert(
      "b".to_string(),
      mock_client(Some("b.example.com"), None, None, None),
    );
    clients.insert("unbound".to_string(), mock_client(None, None, None, None));

    // Host matches a.example.com → only client "a"
    let (pool, key) =
      select_client_pool(&clients, "/", Some("a.example.com"), false, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["a".to_string()]);
    assert_eq!(key, (Some("a.example.com".to_string()), None));

    // Unknown host → falls back to unbound client
    let (pool, key) = select_client_pool(&clients, "/", Some("c.example.com"), false, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["unbound".to_string()]);
    assert_eq!(key, (None, None));

    // Strict mode: unknown host → no client at all
    assert!(select_client_pool(&clients, "/", Some("c.example.com"), true, TEST_THRESHOLD).is_none());
    // Strict mode: matching host still works
    let (pool, _) = select_client_pool(&clients, "/", Some("b.example.com"), true, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["b".to_string()]);
    // Strict mode: no Host header → no client
    assert!(select_client_pool(&clients, "/", None, true, TEST_THRESHOLD).is_none());
  }

  #[test]
  fn test_select_client_pool_hostname_and_path_combined() {
    let mut clients = HashMap::new();
    clients.insert(
      "host-api".to_string(),
      mock_client(Some("a.example.com"), Some("/api"), None, None),
    );
    clients.insert(
      "host-root".to_string(),
      mock_client(Some("a.example.com"), None, None, None),
    );

    // Path under /api on the bound host → path-bound client wins
    let (pool, key) =
      select_client_pool(&clients, "/api/users", Some("a.example.com"), false, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["host-api".to_string()]);
    assert_eq!(
      key,
      (
        Some("a.example.com".to_string()),
        Some("/api".to_string())
      )
    );

    // Other paths on the bound host → unbound-path client
    let (pool, _) =
      select_client_pool(&clients, "/other", Some("a.example.com"), false, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["host-root".to_string()]);
  }

  #[test]
  fn test_select_client_pool_override_wins() {
    let mut clients = HashMap::new();
    // Client reported no hostname, dashboard overruled it to a.example.com
    clients.insert(
      "overruled".to_string(),
      mock_client(None, None, Some("a.example.com"), None),
    );

    let (pool, _) =
      select_client_pool(&clients, "/", Some("a.example.com"), true, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["overruled".to_string()]);

    // With the override active, the client is no longer an unbound fallback
    assert!(select_client_pool(&clients, "/", Some("x.example.com"), false, TEST_THRESHOLD).is_none());
  }

  #[test]
  fn test_select_client_pool_longest_path_bind_wins() {
    let mut clients = HashMap::new();
    clients.insert(
      "short".to_string(),
      mock_client(None, Some("/api"), None, None),
    );
    clients.insert(
      "long".to_string(),
      mock_client(None, Some("/api/v2"), None, None),
    );

    let (pool, key) = select_client_pool(&clients, "/api/v2/users", None, false, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["long".to_string()]);
    assert_eq!(key, (None, Some("/api/v2".to_string())));

    let (pool, _) = select_client_pool(&clients, "/api/other", None, false, TEST_THRESHOLD).unwrap();
    assert_eq!(pool, vec!["short".to_string()]);
  }

  #[test]
  fn test_safe_redirect_path() {
    // Normal relative paths should pass through
    assert_eq!(safe_redirect_path("/"), "/");
    assert_eq!(safe_redirect_path("/dashboard"), "/dashboard");
    assert_eq!(
      safe_redirect_path("/api/v1/users?page=1"),
      "/api/v1/users?page=1"
    );

    // Protocol-relative URLs must be rejected (open redirect to external host)
    assert_eq!(safe_redirect_path("//evil.com"), "/");
    assert_eq!(safe_redirect_path("//evil.com/phishing"), "/");

    // Backslash-based bypass attempts must be rejected
    assert_eq!(safe_redirect_path("/\\evil.com"), "/");

    // Non-path values must be rejected
    assert_eq!(safe_redirect_path("https://evil.com"), "/");
    assert_eq!(safe_redirect_path("javascript:alert(1)"), "/");
    assert_eq!(safe_redirect_path(""), "/");
    assert_eq!(safe_redirect_path("evil.com"), "/");
  }
}
