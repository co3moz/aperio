use schemars::JsonSchema;
use serde::Deserialize;

use crate::*;

/// One grouped block of `aperio-server.yaml`.
///
/// The server is environment-driven, so a block is not a separate parsing
/// path: each child materializes into `APERIO_<GROUP>_<CHILD>`, exactly the
/// variable the flat key of the same meaning maps to. The table is shared
/// with the loader so the schema above and what the server actually reads
/// cannot drift.
pub struct ServerGroup {
  /// The block's key, e.g. `alert`.
  pub key: &'static str,
  /// Child standing for the group's own variable rather than a
  /// `GROUP_CHILD` one: `cache: { enabled: true }` is `APERIO_CACHE`.
  pub self_key: Option<&'static str>,
}

/// Every grouped block of `aperio-server.yaml`, in schema order.
pub const SERVER_GROUPS: &[ServerGroup] = &[
  ServerGroup {
    key: "alert",
    self_key: None,
  },
  ServerGroup {
    key: "request_id",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "audit",
    self_key: None,
  },
  ServerGroup {
    key: "backup",
    self_key: None,
  },
  ServerGroup {
    key: "cache",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "dashboard",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "edge",
    self_key: None,
  },
  ServerGroup {
    key: "failover",
    self_key: Some("mode"),
  },
  ServerGroup {
    key: "gateway",
    self_key: None,
  },
  ServerGroup {
    key: "ip_limit",
    self_key: None,
  },
  ServerGroup {
    key: "login_lockout",
    self_key: None,
  },
  ServerGroup {
    key: "metrics",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "oidc",
    self_key: None,
  },
  ServerGroup {
    key: "otel",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "outbound",
    self_key: None,
  },
  ServerGroup {
    key: "retention",
    self_key: None,
  },
  ServerGroup {
    key: "jwks",
    self_key: None,
  },
  ServerGroup {
    key: "scaling",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "server",
    self_key: None,
  },
  ServerGroup {
    key: "stream",
    self_key: None,
  },
];

/// The `aperio-server.yaml` configuration file. The server is environment-
/// driven; every scalar key here is materialized into its `APERIO_*`
/// environment variable at startup (the file takes precedence over the
/// environment). Structured sections (`headers`, `routes`, `expose`) are read
/// directly. Unknown keys are allowed and passed through as env vars.
#[derive(Deserialize, Default, JsonSchema)]
pub struct ServerFileConfig {
  // --- Grouped blocks (preferred over the flat keys below) ---
  /// Alerting thresholds: when the server logs and emits an alert
  #[serde(default)]
  #[schemars(extend("examples" = [{"error_rate": 0.25, "window": 300, "min_requests": 20, "client_down": 60}]))]
  pub alert: Option<AlertGroup>,
  /// Audit-log file rotation
  #[serde(default)]
  #[schemars(extend("examples" = [{"max_size": 10485760, "max_files": 3}]))]
  pub audit: Option<AuditGroup>,
  /// Scheduled snapshots of the SQLite store
  #[serde(default)]
  #[schemars(extend("examples" = [{"dir": "/app/data/backups", "interval": 86400, "keep": 7}]))]
  pub backup: Option<BackupGroup>,
  /// The server-side GET response cache
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "max_bytes": 67108864, "max_stale": 3600}]))]
  pub cache: Option<CacheSetting>,
  /// The built-in dashboard
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true}]))]
  pub dashboard: Option<DashboardSetting>,
  /// Edge-proxy integration: publishing the served hostnames to a dynamic reverse proxy in front of this server
  #[serde(default)]
  #[schemars(extend("examples" = [{"token": "edge_xxxxxxxx", "service_url": "http://aperio:8080", "entrypoints": "websecure"}]))]
  pub edge: Option<EdgeGroup>,
  /// In-flight failover: what happens to a request whose client disappears mid-flight
  #[serde(default)]
  #[schemars(extend("examples" = ["retry-wait", {"mode": "retry", "max_jumps": 2, "window": 30}]))]
  pub failover: Option<FailoverSetting>,
  /// Gateway timeouts applied to a proxied request
  #[serde(default)]
  #[schemars(extend("examples" = [{"timeout": 10, "response_timeout": 30}]))]
  pub gateway: Option<GatewayGroup>,
  /// Per-visitor-IP rate limiting (token bucket)
  #[serde(default)]
  #[schemars(extend("examples" = [{"max": 120, "refill": 20.0}]))]
  pub ip_limit: Option<IpLimitGroup>,
  /// Dashboard login lockout after repeated failures
  #[serde(default)]
  #[schemars(extend("examples" = [{"threshold": 5, "secs": 60}]))]
  pub login_lockout: Option<LoginLockoutGroup>,
  /// The Prometheus metrics endpoint
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "token": "scrape_xxxxxxxx"}]))]
  pub metrics: Option<MetricsSetting>,
  /// OIDC single sign-on for the dashboard
  #[serde(default)]
  #[schemars(extend("examples" = [{"issuer": "https://accounts.example.com", "client_id": "aperio", "client_secret": "${OIDC_SECRET}", "redirect_url": "https://tunnel.example.com/aperio/oidc/callback"}]))]
  pub oidc: Option<OidcGroup>,
  /// OpenTelemetry trace export
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "endpoint": "http://collector:4317", "protocol": "grpc", "service_name": "aperio"}]))]
  pub otel: Option<OtelSetting>,
  /// Where the server may send outbound callbacks (webhooks, autoscaling hooks)
  #[serde(default)]
  #[schemars(extend("examples" = [{"block_private": true, "allowlist": ["hooks.example.com", "*.provider.example"]}]))]
  pub outbound: Option<OutboundGroup>,
  /// How long each kind of recorded data is kept, in days
  #[serde(default)]
  #[schemars(extend("examples" = [{"stats": 365, "audit": 90, "captures": 7, "access_log": 30}]))]
  pub retention: Option<RetentionGroup>,
  /// How far a client-declared `jwt` gate's key-set URL may reach
  #[serde(default)]
  #[schemars(extend("examples" = [{"allow_http": false, "allow_private": false}]))]
  pub jwks: Option<JwksGroup>,
  /// Autoscaling: the server signalling desired capacity to an endpoint you control
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "allow_http": false, "record_ttl": 2592000}]))]
  pub scaling: Option<ScalingSetting>,
  /// The server's own credentials
  #[serde(default)]
  #[schemars(extend("examples" = [{"token": "change-me-to-a-long-random-string"}]))]
  pub server: Option<ServerCredentials>,
  /// Request-id correlation: the id sent to the backend and echoed to the visitor
  #[serde(default)]
  #[schemars(extend("examples" = [{"enabled": true, "trust_inbound": false}]))]
  pub request_id: Option<RequestIdGroup>,
  /// Flat spelling of `request_id.header` (env: APERIO_REQUEST_ID_HEADER).
  #[schemars(extend("examples" = ["x-correlation-id"]))]
  pub request_id_header: Option<String>,
  /// Flat spelling of `request_id.trust_inbound`
  /// (env: APERIO_REQUEST_ID_TRUST_INBOUND).
  #[schemars(extend("examples" = [true]))]
  pub request_id_trust_inbound: Option<bool>,
  /// Per-stream flow control for streamed data (responses, WebSocket, TCP)
  #[serde(default)]
  #[schemars(extend("examples" = [{"pause_bytes": 2097152, "resume_bytes": 524288, "backlog_limit": 16777216}]))]
  pub stream: Option<StreamGroup>,
  // --- Core ---
  /// The Aperio version this file was written for, e.g. `0.5.0`. On startup
  /// the server compares it against its own build and reports every recorded
  /// change to the configuration format that landed in between, refusing to
  /// start when one of them has security consequences. Unset disables the
  /// check (env: APERIO_VERSION).
  #[schemars(extend("examples" = ["0.5.0"]))]
  pub version: Option<String>,
  /// Deprecated spelling of `server.token` (env: APERIO_SERVER_TOKEN).
  pub server_token: Option<String>,
  /// Address to bind (bare env: HOST). Default: `0.0.0.0`.
  #[schemars(extend("examples" = ["0.0.0.0"]))]
  pub host: Option<String>,
  /// Port to listen on (bare env: PORT). Default: `8080`.
  #[schemars(extend("examples" = [8080]))]
  pub port: Option<u16>,
  /// Directory for the SQLite store and logs (env: APERIO_DATA_DIR).
  /// Default: `./data`.
  #[schemars(extend("examples" = ["/app/data"]))]
  pub data_dir: Option<String>,
  /// Log level (bare env: LOG_LEVEL). Default: `info`.
  #[schemars(extend("examples" = ["info", "debug"]))]
  pub log_level: Option<String>,

  /// What a route nobody gated means: `allow` (today's behaviour, and the
  /// default) or `deny`. With `deny` a route is reachable because something
  /// said so, an `auth:` policy that admits the visitor or an explicit
  /// `method: none` / `public: true`, rather than because nothing said
  /// otherwise (env: APERIO_DEFAULT_ACCESS). Default: `allow`.
  #[schemars(extend("examples" = ["deny"]))]
  pub default_access: Option<String>,

  // --- Routing & load balancing ---
  /// Require every client to carry a hostname bind
  /// (env: APERIO_REQUIRE_HOSTNAME_BIND). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub require_hostname_bind: Option<bool>,
  /// Wildcard pattern granting each client a random subdomain (env: APERIO_RANDOM_SUBDOMAIN).
  #[schemars(extend("examples" = ["*.example.com"]))]
  pub random_subdomain: Option<String>,
  /// Inject noindex headers for random-subdomain preview services
  /// (env: APERIO_PREVIEW_NOINDEX). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub preview_noindex: Option<bool>,
  /// Seconds without a heartbeat before a client is considered down
  /// (env: APERIO_CLIENT_DOWN_THRESHOLD). Default: `15`.
  #[schemars(extend("examples" = [15]))]
  pub client_down_threshold: Option<u64>,
  /// Load-balancing strategy (env: APERIO_LB_STRATEGY).
  /// Default: `round-robin`.
  #[schemars(extend("examples" = ["round-robin", "primary-standby", "sticky"]))]
  pub lb_strategy: Option<String>,
  /// Passive outlier ejection: temporarily drop a client from the pool when it
  /// returns too many errors in a window (env: APERIO_OUTLIER_EJECTION).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub outlier_ejection: Option<bool>,
  /// Failures inside the window before a client is ejected
  /// (env: APERIO_OUTLIER_MAX_FAILURES). Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub outlier_max_failures: Option<u32>,
  /// Sliding window in seconds the failures are counted over
  /// (env: APERIO_OUTLIER_WINDOW). Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub outlier_window: Option<u64>,
  /// Seconds an ejected client stays out before re-admission
  /// (env: APERIO_OUTLIER_EJECT_SECS). Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub outlier_eject_secs: Option<u64>,

  // --- Failover ---
  /// Deprecated spelling of `failover.max_jumps` (env: APERIO_FAILOVER_MAX_JUMPS).
  pub failover_max_jumps: Option<u32>,
  /// Deprecated spelling of `failover.window` (env: APERIO_FAILOVER_WINDOW).
  pub failover_window: Option<u64>,
  /// Deprecated spelling of `failover.all_methods` (env: APERIO_FAILOVER_ALL_METHODS).
  pub failover_all_methods: Option<bool>,
  /// Re-dispatch a buffered response whose status is a retryable server error
  /// to another client instead of returning it (env: APERIO_RETRY_ON_5XX).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub retry_on_5xx: Option<bool>,
  /// Status codes that trigger that retry; empty = every 5xx
  /// (env: APERIO_RETRY_STATUSES).
  #[schemars(extend("examples" = [[502, 503]]))]
  pub retry_statuses: Option<Vec<u16>>,

  // --- Alerting ---
  /// Deprecated spelling of `alert.error_rate` (env: APERIO_ALERT_ERROR_RATE).
  pub alert_error_rate: Option<f64>,
  /// Deprecated spelling of `alert.window` (env: APERIO_ALERT_WINDOW).
  pub alert_window: Option<u64>,
  /// Deprecated spelling of `alert.min_requests` (env: APERIO_ALERT_MIN_REQUESTS).
  pub alert_min_requests: Option<u64>,
  /// Deprecated spelling of `alert.client_down` (env: APERIO_ALERT_CLIENT_DOWN).
  pub alert_client_down: Option<u64>,

  // --- Capacity & limits ---
  /// Largest request body in bytes (env: APERIO_MAX_BODY_SIZE).
  /// Default: `10485760` (10 MB).
  #[schemars(extend("examples" = [10485760]))]
  pub max_body_size: Option<u64>,
  /// Concurrent proxied requests limit (env: APERIO_MAX_CONCURRENT_REQUESTS).
  /// Default: `100`.
  #[schemars(extend("examples" = [512]))]
  pub max_concurrent_requests: Option<u64>,
  /// Maximum simultaneously connected clients (env: APERIO_MAX_TUNNELS).
  /// Default: `10`.
  #[schemars(extend("examples" = [10]))]
  pub max_tunnels: Option<u64>,
  /// Parallel tunnel connections one client may open for a single service
  /// (its `connections:`). A token may lower this for its own holder, never
  /// raise it (env: APERIO_MAX_CONNECTIONS_PER_SERVICE). Default: `16`.
  #[schemars(extend("examples" = [16]))]
  pub max_connections_per_service: Option<u64>,
  /// Record every proxied transaction for the dashboard's request inspector.
  /// `false` gives back a mutex, two header clones and a
  /// capture entry per request, and nothing can be inspected or replayed
  /// (env: APERIO_INSPECTOR). Default: `true`.
  #[schemars(extend("examples" = [false]))]
  pub inspector: Option<bool>,
  /// Emit the per-request structured access event for a successful request.
  /// Distinct from `log_level`: `false` silences that
  /// one-per-request line and leaves warnings and errors alone, so a refused
  /// or failed request still logs at `warn` (env: APERIO_ACCESS_EVENTS).
  /// Default: `true`.
  #[schemars(extend("examples" = [false]))]
  pub access_events: Option<bool>,
  /// Maximum concurrently-live proxied public WebSockets; they are long-lived,
  /// so they get their own ceiling separate from `max_concurrent_requests`.
  /// `0` = uncapped (env: APERIO_MAX_WS_CONNECTIONS). Default: `10000`.
  #[schemars(extend("examples" = [1000]))]
  pub max_ws_connections: Option<u64>,
  /// Deprecated spelling of `ip_limit.max` (env: APERIO_IP_LIMIT_MAX).
  pub ip_limit_max: Option<u64>,
  /// Deprecated spelling of `ip_limit.refill` (env: APERIO_IP_LIMIT_REFILL).
  pub ip_limit_refill: Option<f64>,
  /// Deprecated spelling of `login_lockout.threshold` (env: APERIO_LOGIN_LOCKOUT_THRESHOLD).
  pub login_lockout_threshold: Option<u32>,
  /// Deprecated spelling of `login_lockout.secs` (env: APERIO_LOGIN_LOCKOUT_SECS).
  pub login_lockout_secs: Option<u64>,
  /// Deprecated spelling of `gateway.timeout` (env: APERIO_GATEWAY_TIMEOUT).
  pub gateway_timeout: Option<u64>,
  /// Deprecated spelling of `gateway.response_timeout` (env: APERIO_GATEWAY_RESPONSE_TIMEOUT).
  pub gateway_response_timeout: Option<u64>,

  // --- Proxy trust & cookies ---
  /// Trust `X-Forwarded-For` from proxies (env: APERIO_TRUST_PROXY).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub trust_proxy: Option<bool>,
  /// Trusted proxy IPs/CIDRs (env: APERIO_TRUSTED_PROXIES).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub trusted_proxies: Option<Vec<String>>,
  /// Header carrying the real client IP (env: APERIO_REAL_IP_HEADER).
  #[schemars(extend("examples" = ["CF-Connecting-IP"]))]
  pub real_ip_header: Option<String>,
  /// Trust the Cloudflare client-IP header (env: APERIO_TRUST_CF_HEADER).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub trust_cf_header: Option<bool>,
  /// Mark session cookies `Secure` (env: APERIO_SECURE_COOKIES).
  /// Default: the `trust_proxy` value.
  #[schemars(extend("examples" = [true]))]
  pub secure_cookies: Option<bool>,
  /// Source IPs/CIDRs allowed to reach the authenticated admin surface (the
  /// dashboard and `/aperio/api/*`); empty = no restriction. An invalid entry
  /// refuses startup rather than applying a partial allowlist
  /// (env: APERIO_ADMIN_ALLOWED_IPS).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub admin_allowed_ips: Option<Vec<String>>,
  /// Source IPs/CIDRs refused everything, checked before every other rule:
  /// proxied traffic, the dashboard and its API, and the tunnel endpoints
  /// alike. The inverse of `allowed_ips`, for blocking an abusive address
  /// without turning on an allowlist that would lock out everyone unnamed.
  /// Answers `403`. Hot-reloadable, so an address can be blocked without a
  /// restart. An invalid entry refuses startup rather than applying a partial
  /// deny list (env: APERIO_DENIED_IPS).
  #[schemars(extend("examples" = [["203.0.113.7", "198.51.100.0/24"]]))]
  pub denied_ips: Option<Vec<String>>,
  /// Tell the backend which client, organization and token served the
  /// request, as `x-aperio-client-id`, `x-aperio-org` and `x-aperio-token`.
  /// Off by default: they are new trust surface, and a backend that starts
  /// believing them should do so deliberately. Inbound `x-aperio-*` headers
  /// are stripped from every proxied request whatever this is set to, so a
  /// visitor can never forge one (env: APERIO_IDENTITY_HEADERS).
  #[schemars(extend("examples" = [true]))]
  pub identity_headers: Option<bool>,
  /// Announce to the backend who the *visitor* is, as `x-aperio-visitor-how`
  /// (`session` / `bearer` / `share`) and `x-aperio-visitor-id` (the email or
  /// username behind a session, where there is one). Off by default and the
  /// same trust surface as `identity_headers`; an ungated or deliberately
  /// open route identifies nobody and sends neither header
  /// (env: APERIO_VISITOR_IDENTITY_HEADERS).
  #[schemars(extend("examples" = [true]))]
  pub visitor_identity_headers: Option<bool>,
  /// Fraction of *successful* requests that produce an access line, 0.0 to
  /// 1.0. Default `1.0` = every request, which is what this always did.
  /// Failures are never sampled out: a sampled-away error is the one line
  /// somebody needed. Applies to the `aperio_access` event and the
  /// `access_log` file alike (env: APERIO_ACCESS_LOG_SAMPLE_RATE).
  #[schemars(extend("examples" = [0.1]))]
  pub access_log_sample_rate: Option<f64>,
  /// Seconds to let in-flight proxied requests finish before shutdown ends
  /// the connections carrying them. Behind a load balancer this is the number
  /// that decides whether a deploy is invisible or shows up as a handful of
  /// 502s. `auto` sizes it from the drain budgets connected clients announce,
  /// capped at 30 seconds. Default: `0` (do not wait)
  /// (env: APERIO_SHUTDOWN_DRAIN).
  #[schemars(extend("examples" = [10, "auto"]))]
  pub shutdown_drain: Option<ShutdownDrain>,
  /// Other Aperio servers a client of this one may fall back to, announced in
  /// the handshake. A planned migration or a regional failover otherwise means
  /// editing every client's config; announce the new server here and clients
  /// learn it on their next connection.
  ///
  /// Advice, not instruction: a client appends these *after* the servers its
  /// own config names, so the operator's list still decides the order. A
  /// client that has never reached this server learns nothing, which is why
  /// this is for a migration announced in advance rather than a rescue
  /// (env: APERIO_ALTERNATE_SERVERS, comma-separated).
  #[schemars(extend("examples" = [["wss://eu.tunnel.example.com/tunnel"]]))]
  pub alternate_servers: Option<Vec<String>>,
  /// Streamed responses one visitor address may hold open at once. `0` (the
  /// default) = no limit. Saturating a service's concurrency budget otherwise
  /// takes one host holding many slow streams; this makes it take a botnet.
  ///
  /// No default value is chosen for you on purpose: a NAT or a carrier-grade
  /// NAT puts many real people behind one address, so any number here is a
  /// guess with a queue of users behind it. Set it from what your own traffic
  /// looks like, and make sure `trust_proxy` is right first, or every visitor
  /// behind your CDN shares one address (env: APERIO_MAX_STREAMS_PER_IP).
  #[schemars(extend("examples" = [8]))]
  pub max_streams_per_ip: Option<u32>,
  /// Accept OpenTelemetry exports from tunnel clients and forward them to the
  /// collector this server exports its own spans to (`otel.endpoint`). Off by
  /// default: it is an outbound path a client can drive, so it is a decision
  /// rather than a consequence of having `otel` on. While it is on, any client
  /// with a valid tunnel token may export; every export is attributed to the
  /// token that sent it and both size and rate are capped
  /// (env: APERIO_OTEL_BRIDGE).
  #[schemars(extend("examples" = [true]))]
  pub otel_bridge: Option<bool>,
  /// Seconds after which shutdown stops waiting for anything still holding a
  /// connection open (a proxied WebSocket, a TCP relay, a stalled peer) and
  /// exits. Default: `10` (env: APERIO_SHUTDOWN_TIMEOUT).
  #[schemars(extend("examples" = [30]))]
  pub shutdown_timeout: Option<u64>,

  // --- Tunnel & cache ---
  /// zlib-compress tunnel frames (env: APERIO_TUNNEL_COMPRESSION).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub tunnel_compression: Option<bool>,
  /// Deprecated spelling of `cache.max_bytes` (env: APERIO_CACHE_MAX_BYTES).
  pub cache_max_bytes: Option<u64>,
  /// Deprecated spelling of `cache.max_stale` (env: APERIO_CACHE_MAX_STALE).
  pub cache_max_stale: Option<u64>,
  /// Flat spelling of `outbound.allowlist` (env: APERIO_OUTBOUND_ALLOWLIST).
  #[schemars(extend("examples" = [["hooks.example.com", "*.provider.example"]]))]
  pub outbound_allowlist: Option<Vec<String>>,
  /// Flat spelling of `outbound.block_private` (env: APERIO_OUTBOUND_BLOCK_PRIVATE).
  #[schemars(extend("examples" = [true]))]
  pub outbound_block_private: Option<bool>,
  /// Flat spelling of `outbound.proxy` (env: APERIO_OUTBOUND_PROXY).
  #[schemars(extend("examples" = ["proxy.corp:3128"]))]
  pub outbound_proxy: Option<String>,
  /// Flat spelling of `outbound.no_proxy` (env: APERIO_OUTBOUND_NO_PROXY).
  #[schemars(extend("examples" = [["auth.internal", ".svc.cluster.local"]]))]
  pub outbound_no_proxy: Option<Vec<String>>,
  /// Flat spelling of `stream.pause_bytes` (env: APERIO_STREAM_PAUSE_BYTES).
  #[schemars(extend("examples" = [2097152]))]
  pub stream_pause_bytes: Option<u64>,
  /// Flat spelling of `stream.resume_bytes` (env: APERIO_STREAM_RESUME_BYTES).
  #[schemars(extend("examples" = [524288]))]
  pub stream_resume_bytes: Option<u64>,
  /// Flat spelling of `stream.backlog_limit` (env: APERIO_STREAM_BACKLOG_LIMIT).
  #[schemars(extend("examples" = [16777216]))]
  pub stream_backlog_limit: Option<u64>,
  /// Flat spelling of `stream.min_throughput` (env: APERIO_STREAM_MIN_THROUGHPUT).
  #[schemars(extend("examples" = [1024]))]
  pub stream_min_throughput: Option<u64>,
  /// Flat spelling of `cache.negative_ttl` (env: APERIO_CACHE_NEGATIVE_TTL).
  #[schemars(extend("examples" = [10]))]
  pub cache_negative_ttl: Option<u64>,
  /// Flat spelling of `jwks.allow_http` (env: APERIO_JWKS_ALLOW_HTTP).
  #[schemars(extend("examples" = [false]))]
  pub jwks_allow_http: Option<bool>,
  /// Flat spelling of `jwks.allow_private` (env: APERIO_JWKS_ALLOW_PRIVATE).
  #[schemars(extend("examples" = [false]))]
  pub jwks_allow_private: Option<bool>,
  /// Flat spelling of `scaling.allow_private` (env: APERIO_SCALING_ALLOW_PRIVATE).
  #[schemars(extend("examples" = [false]))]
  pub scaling_allow_private: Option<bool>,
  /// Flat spelling of `backup.interval` (env: APERIO_BACKUP_INTERVAL).
  #[schemars(extend("examples" = [86400]))]
  pub backup_interval: Option<u64>,
  /// Flat spelling of `backup.dir` (env: APERIO_BACKUP_DIR).
  #[schemars(extend("examples" = ["/app/data/backups"]))]
  pub backup_dir: Option<String>,
  /// Flat spelling of `backup.keep` (env: APERIO_BACKUP_KEEP).
  #[schemars(extend("examples" = [7]))]
  pub backup_keep: Option<u64>,
  /// Flat spelling of `backup.key` (env: APERIO_BACKUP_KEY).
  #[schemars(extend("examples" = ["${APERIO_BACKUP_KEY}"]))]
  pub backup_key: Option<String>,
  /// Flat spelling of `backup.key_file` (env: APERIO_BACKUP_KEY_FILE).
  #[schemars(extend("examples" = ["/etc/aperio/backup.key"]))]
  pub backup_key_file: Option<String>,

  // --- Process & startup ---
  /// Watch the config file and apply live-editable changes without a restart
  /// (env: APERIO_CONFIG_HOT_RELOAD). Default: `true`.
  #[schemars(extend("examples" = [true]))]
  pub config_hot_reload: Option<bool>,
  /// Bind the listener with SO_REUSEPORT, so a second process can take over
  /// the port for a zero-downtime handover (env: APERIO_REUSEPORT).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub reuseport: Option<bool>,

  // --- Pages ---
  /// Custom 504 error page path (env: APERIO_504_PAGE).
  #[serde(rename = "504_page")]
  #[schemars(extend("examples" = ["./pages/504.html"]))]
  pub error_page_504: Option<String>,
  /// Custom 503 maintenance page path (env: APERIO_503_PAGE).
  #[serde(rename = "503_page")]
  #[schemars(extend("examples" = ["./pages/503.html"]))]
  pub error_page_503: Option<String>,

  // --- Logging & telemetry ---
  /// Structured access log path (env: APERIO_ACCESS_LOG).
  #[schemars(extend("examples" = ["/app/data/access.jsonl"]))]
  pub access_log: Option<String>,
  /// Mask credential headers and secret-looking body fields in the request
  /// inspector, the cURL copy and the HAR export
  /// (env: APERIO_INSPECTOR_REDACT). Default: `true`.
  #[schemars(extend("examples" = [true]))]
  pub inspector_redact: Option<bool>,
  /// Seconds between webhook delivery retries; empty = no retries
  /// (env: APERIO_WEBHOOK_RETRY_SCHEDULE). Default: `1,5,25,60`.
  #[schemars(extend("examples" = [[1, 5, 25, 60]]))]
  pub webhook_retry_schedule: Option<Vec<u64>>,
  /// Seconds between availability-history ticks for the dashboard's Uptime
  /// panel, minimum 1 (env: APERIO_UPTIME_TICK_SECS). Default: `10`.
  #[schemars(extend("examples" = [60]))]
  pub uptime_tick_secs: Option<u64>,
  /// Deprecated spelling of `retention.captures` (env: APERIO_RETENTION_CAPTURES).
  pub retention_captures: Option<u64>,
  /// Deprecated spelling of `retention.access_log` (env: APERIO_RETENTION_ACCESS_LOG).
  pub retention_access_log: Option<u64>,
  /// Deprecated spelling of `retention.audit` (env: APERIO_RETENTION_AUDIT).
  pub retention_audit: Option<u64>,
  /// Deprecated spelling of `retention.stats` (env: APERIO_RETENTION_STATS).
  pub retention_stats: Option<u64>,
  /// Cap on aperio.db (+WAL/SHM) in bytes; nearing it emits a warning, exceeding it auto-prunes low-priority data (env: APERIO_DB_MAX_BYTES).
  #[schemars(extend("examples" = [1073741824]))]
  pub db_max_bytes: Option<u64>,
  /// Deprecated spelling of `audit.max_size` (env: APERIO_AUDIT_MAX_SIZE).
  pub audit_max_size: Option<u64>,
  /// Deprecated spelling of `audit.max_files` (env: APERIO_AUDIT_MAX_FILES).
  pub audit_max_files: Option<u64>,
  /// Deprecated spelling of `otel.endpoint` (env: APERIO_OTEL_ENDPOINT).
  pub otel_endpoint: Option<String>,
  /// Deprecated spelling of `otel.protocol` (env: APERIO_OTEL_PROTOCOL).
  pub otel_protocol: Option<String>,
  /// Deprecated spelling of `otel.service_name` (env: APERIO_OTEL_SERVICE_NAME).
  pub otel_service_name: Option<String>,
  /// Flat spelling of `otel.headers` (env: APERIO_OTEL_HEADERS).
  #[schemars(extend("examples" = ["authorization=Bearer xxx"]))]
  pub otel_headers: Option<String>,
  /// Deprecated spelling of `otel.sample_rate` (env: APERIO_OTEL_SAMPLE_RATE).
  pub otel_sample_rate: Option<f64>,
  /// Deprecated spelling of `metrics.token` (env: APERIO_METRICS_TOKEN).
  pub metrics_token: Option<String>,
  /// Deprecated spelling of `scaling.allow_http` (env: APERIO_SCALING_ALLOW_HTTP).
  pub scaling_allow_http: Option<bool>,
  /// Deprecated spelling of `scaling.record_ttl` (env: APERIO_SCALING_RECORD_TTL).
  pub scaling_record_ttl: Option<u64>,
  /// Deprecated spelling of `edge.token` (env: APERIO_EDGE_TOKEN).
  pub edge_token: Option<String>,
  /// Deprecated spelling of `edge.service_url` (env: APERIO_EDGE_SERVICE_URL).
  pub edge_service_url: Option<String>,
  /// Deprecated spelling of `edge.entrypoints` (env: APERIO_EDGE_ENTRYPOINTS).
  pub edge_entrypoints: Option<String>,
  /// Deprecated spelling of `edge.cert_resolver` (env: APERIO_EDGE_CERT_RESOLVER).
  pub edge_cert_resolver: Option<String>,
  /// Deprecated spelling of `edge.include_offline` (env: APERIO_EDGE_INCLUDE_OFFLINE).
  pub edge_include_offline: Option<bool>,

  // --- Auth, dashboard & SSO ---
  /// Deprecated spelling of `server.auth` (env: APERIO_SERVER_AUTH).
  pub server_auth: Option<AuthSetting>,
  /// Public dashboard URL enabling passkeys; its domain is the RP ID (env: APERIO_WEBAUTHN_ORIGIN).
  #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
  pub webauthn_origin: Option<String>,
  /// Passkey relying-party ID, when it must differ from the origin's domain
  /// (env: APERIO_WEBAUTHN_RP_ID).
  #[schemars(extend("examples" = ["example.com"]))]
  pub webauthn_rp_id: Option<String>,
  /// Pin a dynamic token to the first device key that presents it; a later
  /// connection with a different (or missing) key is rejected
  /// (env: APERIO_TOKEN_PINNING). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub token_pinning: Option<bool>,
  /// Ignore client-declared visitor passwords (env: APERIO_IGNORE_CLIENT_AUTH).
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub ignore_client_auth: Option<bool>,
  /// Default dashboard/login UI language (env: APERIO_UI_LANGUAGE).
  /// Default: `en`.
  #[schemars(extend("examples" = ["en", "tr"]))]
  pub ui_language: Option<String>,
  /// Seconds before a token's expiry to start warning
  /// (env: APERIO_TOKEN_EXPIRY_WARNING). Default: `86400` (24 hours).
  #[schemars(extend("examples" = [7]))]
  pub token_expiry_warning: Option<u64>,
  /// Deprecated spelling of `oidc.issuer` (env: APERIO_OIDC_ISSUER).
  pub oidc_issuer: Option<String>,
  /// Deprecated spelling of `oidc.client_id` (env: APERIO_OIDC_CLIENT_ID).
  pub oidc_client_id: Option<String>,
  /// Deprecated spelling of `oidc.allowed_emails` (env: APERIO_OIDC_ALLOWED_EMAILS).
  pub oidc_allowed_emails: Option<Vec<String>>,
  /// Deprecated spelling of `oidc.scopes` (env: APERIO_OIDC_SCOPES).
  pub oidc_scopes: Option<Vec<String>>,
  /// Deprecated spelling of `oidc.redirect_url` (env: APERIO_OIDC_REDIRECT_URL).
  pub oidc_redirect_url: Option<String>,
  /// Deprecated spelling of `oidc.client_secret` (env: APERIO_OIDC_CLIENT_SECRET).
  pub oidc_client_secret: Option<String>,

  // --- Structured sections (read directly, not env-mapped) ---
  /// Server-wide request/response header rewrite rules applied to all traffic.
  #[schemars(extend("examples" = [{
    "request": {"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]},
    "response": {"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}
  }]))]
  pub headers: Option<HeaderRules>,
  /// Client-less routes: bind a hostname/path to a redirect or fixed response.
  #[schemars(extend("examples" = [[{"hostname": "old.example.com", "redirect": "https://new.example.com", "permanent": true}]]))]
  pub routes: Option<Vec<RouteRule>>,
  /// Operator-defined alert rules over what the server measures
  #[schemars(extend("examples" = [[{"name": "disk-filling", "metric": "store_bytes", "above": 536870912, "for": 300}]]))]
  pub alert_rules: Option<Vec<AlertRule>>,
  /// Recurring maintenance windows, evaluated in their own time zone
  #[schemars(extend("examples" = [[{"hostname": "*.example.com", "from": "02:00", "to": "04:00", "days": ["sat"], "tz": "Europe/Istanbul"}]]))]
  pub maintenance_windows: Option<Vec<MaintenanceWindow>>,
  /// Per-hostname custom 504/503 error pages (override the global
  /// `504_page`/`503_page` for that hostname).
  #[schemars(extend("examples" = [[{"hostname": "app.example.com", "504_page": "./pages/app-504.html"}]]))]
  pub error_pages: Option<Vec<ErrorPageRule>>,
  /// Experimental public TCP expose ports.
  #[schemars(extend("examples" = [[{"port": 5432, "tunnel": "pg_main", "protocol": "tcp"}]]))]
  pub expose: Option<Vec<ExposeEntry>>,
  /// Per-route request rate limits, capping aggregate rps to a host+path.
  #[schemars(extend("examples" = [[{"hostname": "api.example.com", "path": "/api/login", "rps": 5.0, "burst": 10.0}]]))]
  pub rate_limits: Option<Vec<RateLimitRule>>,
  /// Per-hostname fallback URLs answered when no client serves the route.
  #[schemars(extend("examples" = [[{"hostname": "app.example.com", "url": "https://status.example.com", "preserve_path": true}]]))]
  pub fallbacks: Option<Vec<FallbackRule>>,
  /// WAF-lite deny/size rules evaluated before a request is proxied.
  #[schemars(extend("examples" = [[{"path": "^/wp-admin"}, {"header": {"name": "user-agent", "regex": "(?i)sqlmap|nikto"}}, {"path": "^/upload", "max_body": 1048576}]]))]
  pub waf: Option<Vec<WafRule>>,
}
