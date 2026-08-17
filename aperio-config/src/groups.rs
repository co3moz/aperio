use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::*;

/// Alerting thresholds: when the server logs and emits an alert.
///
/// Written as a `alert:` block; the flat `alert_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlertGroup {
  /// Error-rate alert threshold, 0..1. Default: off.
  #[schemars(extend("examples" = [0.25]))]
  pub error_rate: Option<f64>,
  /// Alert sliding-window seconds. Default: `300`.
  #[schemars(extend("examples" = [300]))]
  pub window: Option<u64>,
  /// Minimum requests in the window before the error-rate alert fires.
  /// Default: `20`.
  #[schemars(extend("examples" = [20]))]
  pub min_requests: Option<u64>,
  /// Seconds a known service may stay down before the client-down alert
  /// fires; it resolves when the service comes back. `0` = off. Default: off.
  #[schemars(extend("examples" = [60]))]
  pub client_down: Option<u64>,
}

/// Audit-log file rotation.
///
/// Written as a `audit:` block; the flat `audit_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditGroup {
  /// Audit log rotation size in bytes, 0 disables.
  /// Default: `10485760` (10 MB).
  #[schemars(extend("examples" = [10485760]))]
  pub max_size: Option<u64>,
  /// Rotated audit log files kept. Default: `3`.
  #[schemars(extend("examples" = [3]))]
  pub max_files: Option<u64>,
}

/// The server-side GET response cache.
///
/// Written as a `cache:` block; the flat `cache_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheGroup {
  /// Enable the server-side GET response cache. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Response-cache budget in bytes. Default: `67108864` (64 MB).
  #[schemars(extend("examples" = [67108864]))]
  pub max_bytes: Option<u64>,
  /// Serve-stale window in seconds for resilient services. Default: `3600`.
  #[schemars(extend("examples" = [3600]))]
  pub max_stale: Option<u64>,
  /// Seconds to briefly cache error / negative responses (e.g. `404`), so a
  /// hot missing URL cannot hammer the backend. `0` = disabled.
  /// Default: `0`.
  #[schemars(extend("examples" = [10]))]
  pub negative_ttl: Option<u64>,
}

/// The built-in dashboard.
///
/// Written as a `dashboard:` block; the flat `dashboard_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DashboardGroup {
  /// Serve the admin dashboard. Default: `true`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
}

/// Edge-proxy integration: publishing the served hostnames to a dynamic reverse proxy in front of this server.
///
/// Written as a `edge:` block; the flat `edge_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EdgeGroup {
  /// Credential enabling the edge-integration endpoints, which publish the
  /// live hostname inventory to a reverse proxy in front of this server
  ///.
  #[schemars(extend("examples" = ["change-me-to-a-long-random-string"]))]
  pub token: Option<String>,
  /// URL the edge proxy forwards matched traffic to.
  #[schemars(extend("examples" = ["http://aperio:8080"]))]
  pub service_url: Option<String>,
  /// Traefik entry points for the generated routers.
  #[schemars(extend("examples" = ["websecure"]))]
  pub entrypoints: Option<String>,
  /// Traefik certificate resolver for the generated routers.
  #[schemars(extend("examples" = ["letsencrypt"]))]
  pub cert_resolver: Option<String>,
  /// Also publish hostnames a token permits but no client serves yet.
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub include_offline: Option<bool>,
}

/// In-flight failover: what happens to a request whose client disappears mid-flight.
///
/// Written as a `failover:` block; the flat `failover_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FailoverGroup {
  /// In-flight failover mode. Default: `fail`.
  #[schemars(extend("examples" = ["fail", "retry", "wait", "retry-wait"]))]
  pub mode: Option<String>,
  /// Maximum failover re-dispatches per request. Default: `2`.
  #[schemars(extend("examples" = [2]))]
  pub max_jumps: Option<u32>,
  /// Failover window in seconds. Default: `15`.
  #[schemars(extend("examples" = [300]))]
  pub window: Option<u64>,
  /// Allow failover for non-idempotent methods too. Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub all_methods: Option<bool>,
}

/// Gateway timeouts applied to a proxied request.
///
/// Written as a `gateway:` block; the flat `gateway_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayGroup {
  /// Seconds to wait for a client connection. Default: `10`.
  #[schemars(extend("examples" = [10]))]
  pub timeout: Option<u64>,
  /// Seconds to wait for a client response. Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub response_timeout: Option<u64>,
}

/// Per-visitor-IP rate limiting (token bucket).
///
/// Written as a `ip_limit:` block; the flat `ip_limit_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IpLimitGroup {
  /// Per-IP rate-limit burst. Default: `100`.
  #[schemars(extend("examples" = [120]))]
  pub max: Option<u64>,
  /// Per-IP rate-limit refill per second. Default: `5`.
  #[schemars(extend("examples" = [20.0]))]
  pub refill: Option<f64>,
}

/// Dashboard login lockout after repeated failures.
///
/// Written as a `login_lockout:` block; the flat `login_lockout_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginLockoutGroup {
  /// Failed logins per IP before a lockout. Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub threshold: Option<u32>,
  /// Base lockout seconds, doubled per repeat. Default: `60`.
  #[schemars(extend("examples" = [60]))]
  pub secs: Option<u64>,
}

/// The Prometheus metrics endpoint.
///
/// Written as a `metrics:` block; the flat `metrics_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricsGroup {
  /// Prometheus metrics endpoint toggle. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Bearer token gating the metrics endpoint.
  #[schemars(extend("examples" = ["change-me-to-a-long-random-string"]))]
  pub token: Option<String>,
}

/// OIDC single sign-on for the dashboard.
///
/// Written as a `oidc:` block; the flat `oidc_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OidcGroup {
  /// OIDC issuer URL.
  #[schemars(extend("examples" = ["https://accounts.example.com"]))]
  pub issuer: Option<String>,
  /// OIDC client id.
  #[schemars(extend("examples" = ["aperio"]))]
  pub client_id: Option<String>,
  /// OIDC client secret.
  #[schemars(extend("examples" = ["${OIDC_SECRET}"]))]
  pub client_secret: Option<String>,
  /// Allowed OIDC login emails.
  #[schemars(extend("examples" = [["alice@example.com", "ops@example.com"]]))]
  pub allowed_emails: Option<Vec<String>>,
  /// OIDC scopes. Default: `openid email profile`.
  #[schemars(extend("examples" = [["openid", "email", "profile"]]))]
  pub scopes: Option<Vec<String>>,
  /// OIDC redirect URL override.
  #[schemars(extend("examples" = ["https://tunnel.example.com/aperio/oidc/callback"]))]
  pub redirect_url: Option<String>,
}

/// OpenTelemetry trace export.
///
/// Written as a `otel:` block; the flat `otel_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OtelGroup {
  /// Enable OpenTelemetry OTLP export. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// OTLP endpoint. The conventional ports are 4318 for OTLP/HTTP and 4317
  /// for OTLP/gRPC; `protocol` follows from the port unless it is set.
  /// Default: `http://localhost:4318`.
  #[schemars(extend("examples" = ["http://localhost:4318", "http://localhost:4317"]))]
  pub endpoint: Option<String>,
  /// OTLP transport: `http` (protobuf over HTTP) or `grpc`. Unset picks `grpc`
  /// for an endpoint on port 4317 and `http` everywhere else, a collector
  /// answering the wrong protocol drops every span silently, so pin this when
  /// the endpoint runs on a non-standard port.
  #[schemars(extend("examples" = ["http", "grpc"]))]
  pub protocol: Option<String>,
  /// OTLP service name. Default: `aperio-server`.
  #[schemars(extend("examples" = ["aperio"]))]
  pub service_name: Option<String>,
  /// Extra headers for every outgoing OTLP request, as `k=v,k=v`. This is
  /// where a collector's credential goes, and it stays on the server: the
  /// point of the OTel bridge is that an edge host does not hold one.
  #[schemars(extend("examples" = ["authorization=Bearer xxx"]))]
  pub headers: Option<String>,
  /// Fraction of traces to record, 0.0 to 1.0. At `1.0` every request
  /// builds a span tree and hands it to the exporter, which is the setting
  /// that makes tracing show up in a benchmark. `0.01` samples one request in
  /// a hundred, which answers the same questions about latency and error
  /// shape for a hundredth of the cost. The decision is made once per request
  /// and every span of that request follows it. Default: `1.0`.
  #[schemars(extend("examples" = [0.01]))]
  pub sample_rate: Option<f64>,
}

/// How long each kind of recorded data is kept, in days.
///
/// Written as a `retention:` block; the flat `retention_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetentionGroup {
  /// Days to keep inspector captures and webhook inbox entries; `0` =
  /// forever. Default: off (keep).
  #[schemars(extend("examples" = [7]))]
  pub captures: Option<u64>,
  /// Days to keep access-log file lines; `0` = forever. Default: off (keep).
  #[schemars(extend("examples" = [30]))]
  pub access_log: Option<u64>,
  /// Days to keep audit events; `0` = forever. Default: off (keep).
  #[schemars(extend("examples" = [90]))]
  pub audit: Option<u64>,
  /// Days to keep day-granularity stats buckets; `0` = the built-in caps.
  /// Default: off (built-in caps).
  #[schemars(extend("examples" = [365]))]
  pub stats: Option<u64>,
}

/// Autoscaling: the server signalling desired capacity to an endpoint you control.
/// How far a **client-declared** `jwt` gate's key-set URL may reach.
///
/// A client may declare its own visitor gate, and a `jwt` method names
/// `jwks_url`, which this server fetches from this server's network before
/// any signature is checked. That is the same shape as a client-declared
/// autoscaling endpoint, so it gets the same fence and the same two escapes.
/// A `jwt` gate in the server's *own* configuration is not subject to it: an
/// operator naming an issuer on their own network is describing their
/// deployment, not aiming the server at something.
///
/// Written as a `jwks:` block; the flat `jwks_*` keys mean the same thing.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JwksGroup {
  /// Allow a plain-http `jwks_url` on a client-declared gate.
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub allow_http: Option<bool>,
  /// Allow a client-declared `jwks_url` that resolves to a private, loopback
  /// or link-local address, for an issuer that genuinely lives on the
  /// internal network. Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub allow_private: Option<bool>,
}

///
/// Written as a `scaling:` block; the flat `scaling_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScalingGroup {
  /// Honor client `scaling:` declarations. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Allow a plain-http autoscaling endpoint. Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub allow_http: Option<bool>,
  /// Allow an autoscaling endpoint on a private or loopback address, for a
  /// provider API that genuinely lives on the internal network.
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub allow_private: Option<bool>,
  /// Seconds after which an unrefreshed autoscaling record is dropped.
  /// Default: `2592000` (30 days).
  #[schemars(extend("examples" = [2592000]))]
  pub record_ttl: Option<u64>,
}

/// The server's own credentials.
///
/// Written as a `server:` block; the flat `server_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerCredentials {
  /// Master token; also the fallback dashboard password.
  #[schemars(extend("examples" = ["change-me-to-a-long-random-string"]))]
  pub token: Option<String>,
  /// The server's default visitor gate: a `user:password` scalar, one
  /// `{method: ...}` block, or a list of them, any of which admits a visitor.
  #[schemars(extend("examples" = ["admin:s3cret", [{"method": "basic", "users": ["admin:s3cret"]}]]))]
  pub auth: Option<AuthSetting>,
}

/// Where the server may send outbound callbacks (webhook deliveries,
/// autoscaling hooks). Optional: empty/off keeps the permissive default.
///
/// Written as an `outbound:` block; the flat `outbound_*` keys mean the
/// same thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutboundGroup {
  /// Host/CIDR patterns the server may call (exact host, `*.suffix`, IP, or
  /// CIDR). When set, any other destination is refused; a matching entry is
  /// trusted even if private.
  #[schemars(extend("examples" = [["hooks.example.com", "*.provider.example"]]))]
  pub allowlist: Option<Vec<String>>,
  /// With no allowlist: refuse destinations resolving to internal addresses
  /// (loopback, RFC 1918, link-local/metadata, CGNAT, unique-local).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub block_private: Option<bool>,
  /// HTTP proxy these callbacks go through, on a network with no direct
  /// outbound connection: `host:port` or `http://host:port`, with an optional
  /// `user:password@`. The ambient `HTTP_PROXY` is *not* read; this is the
  /// only way to send them through a proxy. Note what it costs the checks
  /// above: through a proxy the destination's name is resolved by the proxy,
  /// so `block_private` covers literal addresses only and CIDR allowlist
  /// entries cannot admit a named destination. Unset: call directly.
  #[schemars(extend("examples" = ["proxy.corp:3128"]))]
  pub proxy: Option<String>,
  /// Destinations that skip the proxy and are called directly: exact names,
  /// or `.suffix` / `*.suffix` for a domain and everything under it. The
  /// usual case is an auth endpoint or an issuer inside your own network.
  /// Loopback is always direct, listed or not. These keep the whole policy,
  /// since the server chooses their addresses itself.
  #[schemars(extend("examples" = [["auth.internal", ".svc.cluster.local"]]))]
  pub no_proxy: Option<Vec<String>>,
}

/// Scheduled snapshots of the SQLite store.
///
/// Written as a `backup:` block; the flat `backup_*` keys mean the same thing
/// and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupGroup {
  /// Seconds between snapshots; `0` disables scheduled backups. Default: off.
  #[schemars(extend("examples" = [86400]))]
  pub interval: Option<u64>,
  /// Directory the snapshots are written to. Required to enable backups:
  /// without it (and a nonzero `interval`) no snapshots are taken.
  #[schemars(extend("examples" = ["/var/backups/aperio"]))]
  pub dir: Option<String>,
  /// Encryption key for snapshots, as 64 hex characters or base64 of 32
  /// bytes. Unset writes snapshots in the clear, as before. Prefer
  /// `key_file`: a key written here is a key in a config file, which backups
  /// and configuration management copy around
  /// (env: APERIO_BACKUP_KEY).
  #[schemars(extend("examples" = ["${APERIO_BACKUP_KEY}"]))]
  pub key: Option<String>,
  /// File holding the encryption key, which is what a secret manager mounts.
  /// Refused when it is inside `dir`: whoever has the backups would have the
  /// key, which is the one arrangement encryption cannot survive
  /// (env: APERIO_BACKUP_KEY_FILE).
  #[schemars(extend("examples" = ["/etc/aperio/backup.key"]))]
  pub key_file: Option<String>,
  /// Snapshots to keep; older ones are pruned. Default: `7`.
  #[schemars(extend("examples" = [7]))]
  pub keep: Option<u64>,
}

/// Per-stream flow control for streamed data (responses, WebSocket, TCP).
///
/// Written as a `stream:` block; the flat `stream_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamGroup {
  /// Backlog bytes at which the producing client is asked to pause a stream.
  /// Default: `2097152` (2 MB).
  #[schemars(extend("examples" = [2097152]))]
  pub pause_bytes: Option<u64>,
  /// Backlog bytes under which a paused producer is asked to resume.
  /// Default: `524288` (512 KB).
  #[schemars(extend("examples" = [524288]))]
  pub resume_bytes: Option<u64>,
  /// Hard per-stream backlog cap in bytes (drops producers that cannot pause).
  /// Default: `16777216` (16 MB).
  #[schemars(extend("examples" = [16777216]))]
  pub backlog_limit: Option<u64>,
  /// Bytes per second a streamed response's consumer must take **while data
  /// is waiting for it**, or the stream is ended. `0` (the default) = no
  /// floor.
  ///
  /// The pump already ends a stream whose consumer cannot take a single chunk
  /// within the gateway timeout, so a reader that takes nothing is covered.
  /// This closes the gap in between: a reader that accepts one chunk just
  /// inside the timeout, forever, holding a client concurrency slot and
  /// megabytes of buffer for as long as it likes.
  ///
  /// Only time the consumer kept data waiting counts, so a stream that is
  /// quiet because the *backend* has nothing to send, which is ordinary for
  /// server-sent events and long polling, is never ended for it
  /// (env: APERIO_STREAM_MIN_THROUGHPUT).
  #[schemars(extend("examples" = [1024]))]
  pub min_throughput: Option<u64>,
}
