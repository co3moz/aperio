//! The sections of the server's settings document, one struct each.
//!
//! Split from the document itself so the module that says what is special
//! about a path (security, restart, credentials) and how two documents are
//! compared is not read past a thousand lines of field declarations.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::units::{ByteSize, Span};
use crate::{AlertRule, AuthSetting};

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
  /// Largest request body a visitor may send. Default `10MB`.
  pub request_body: ByteSize,
  /// Proxied requests in flight across every tunnel; beyond it a request is
  /// answered `429`. Default `100`.
  pub concurrent_requests: u32,
  /// Proxied WebSockets open at once; beyond it an upgrade is answered `503`.
  /// `0` = no cap. Default `10000`.
  pub websockets: u32,
  /// Tunnel connections at once. A client holds one per service unless it
  /// multiplexes, and a reload briefly wants two. Default `64`.
  pub tunnels: u32,
  /// Parallel connections one client may open for one service; a token may
  /// lower it, never raise it. Default `16`.
  pub connections_per_service: u32,
  /// Streamed responses one visitor address may hold open at once. `0` = no
  /// limit. Default `0`.
  pub streams_per_ip: u32,
  /// The per-visitor-IP token bucket every request spends from.
  pub ip_rate: IpRate,
}

impl Default for Limits {
  fn default() -> Self {
    Limits {
      request_body: ByteSize::mb(10),
      concurrent_requests: 100,
      websockets: 10_000,
      tunnels: 64,
      connections_per_service: 16,
      streams_per_ip: 0,
      ip_rate: IpRate::default(),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IpRate {
  /// Burst capacity. Default `100`.
  pub burst: f64,
  /// Requests per second the bucket refills by. Default `5`.
  pub per_second: f64,
}

impl Default for IpRate {
  fn default() -> Self {
    IpRate {
      burst: 100.0,
      per_second: 5.0,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Gateway {
  /// How long a request waits for a client to (re)connect before failing.
  /// Default `10s`.
  pub timeout: Span,
  /// How long a dispatched request waits for the client's answer. Default
  /// `30s`.
  pub response_timeout: Span,
  /// Silence after which a client is dropped from routing (it rejoins on the
  /// next heartbeat); four times this closes its connection. Default `15s`.
  pub client_down_threshold: Span,
}

impl Default for Gateway {
  fn default() -> Self {
    Gateway {
      timeout: Span::secs(10),
      response_timeout: Span::secs(30),
      client_down_threshold: Span::secs(15),
    }
  }
}

/// How a client is picked from the routed pool.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(inline)]
pub enum LbStrategy {
  /// Rotate through the candidates.
  #[default]
  RoundRobin,
  /// Prefer the lowest `priority` tier the clients announce.
  PrimaryStandby,
  /// Keep a visitor on one client with an affinity cookie.
  Sticky,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Routing {
  /// How a client is picked from the routed pool. Default `round-robin`.
  pub lb_strategy: LbStrategy,
  /// Clients without a hostname bind receive no traffic, instead of acting
  /// as the fallback pool. Default `false`.
  pub require_hostname_bind: bool,
  /// Give every client a random hostname from this pattern, `*` being the
  /// random label: `*.example.com` or `*-test.example.com`.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["*-preview.example.com"]))]
  pub random_subdomain: Option<String>,
  /// Mark services reached through their random subdomain as not to be
  /// indexed (`X-Robots-Tag` and a disallow-all `robots.txt`). Default
  /// `false`.
  pub preview_noindex: bool,
  /// Other Aperio servers (`ws://` / `wss://`) a client may fall back to,
  /// announced on every handshake. At most 8.
  #[schemars(extend("examples" = [["wss://eu.tunnel.example.com/tunnel"]]))]
  pub alternate_servers: Vec<String>,
}

/// What to do when a client is lost while a request is in flight.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(inline)]
pub enum FailoverMode {
  /// Answer `502` at once.
  #[default]
  Fail,
  /// Re-dispatch to another available candidate.
  Retry,
  /// Wait for the same client to reconnect.
  Wait,
  /// Re-dispatch now, or wait for a candidate when there is none.
  RetryWait,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Failover {
  /// What to do when a client dies mid-request. Default `fail`.
  pub mode: FailoverMode,
  /// Re-dispatch attempts per request. Default `2`.
  pub max_jumps: u32,
  /// Total time the waiting modes may spend across all jumps. Default `15s`.
  pub window: Span,
  /// Also fail over non-idempotent methods, which may then reach a backend
  /// twice. Default `false`.
  pub all_methods: bool,
  /// Re-dispatch a buffered response whose status is a retryable server
  /// error to another client. Shares the jump budget. Default `false`.
  pub retry_on_5xx: bool,
  /// The statuses that count for `retry_on_5xx`; empty = every 5xx.
  #[schemars(extend("examples" = [[502, 503]]))]
  pub retry_statuses: Vec<u16>,
}

impl Default for Failover {
  fn default() -> Self {
    Failover {
      mode: FailoverMode::Fail,
      max_jumps: 2,
      window: Span::secs(15),
      all_methods: false,
      retry_on_5xx: false,
      retry_statuses: Vec::new(),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Outlier {
  /// Turn passive ejection on. Default `false`.
  pub enabled: bool,
  /// Failures inside `window` that eject a client. Default `5`.
  pub max_failures: u32,
  /// Window the failures are counted over. Default `30s`.
  pub window: Span,
  /// How long an ejected client stays out. Default `30s`.
  pub eject_for: Span,
}

impl Default for Outlier {
  fn default() -> Self {
    Outlier {
      enabled: false,
      max_failures: 5,
      window: Span::secs(30),
      eject_for: Span::secs(30),
    }
  }
}

/// What a route nobody gated means.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(inline)]
pub enum DefaultAccess {
  /// A route is served because something declared it reachable.
  #[default]
  Deny,
  /// Every route serves everyone unless something gates it.
  Allow,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Access {
  /// What a route nobody gated means: `deny` (the default) or `allow`.
  /// Security-relevant.
  pub default: DefaultAccess,
  /// The visitor gate in front of every proxied route that does not carry
  /// its own: a `user:password`, one method block, or a list of them.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["visitor:a-strong-password", {"method": "aperio"}]))]
  pub gate: Option<AuthSetting>,
  /// Ignore the gates clients declare for their own services, and keep sole
  /// control with `gate`. Security-relevant. Default `false`.
  pub ignore_client_auth: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Proxy {
  /// Believe `X-Forwarded-For` from the direct peer. Turn on only behind a
  /// proxy that overwrites it. Default `false`; implied by `trusted`.
  pub trust: bool,
  /// The proxies and CDN egress ranges in front of this server (IPs or
  /// CIDRs). The visitor's address is found by walking `X-Forwarded-For`
  /// back past these.
  #[schemars(extend("examples" = [["10.0.0.0/8", "173.245.48.0/20"]]))]
  pub trusted: Vec<String>,
  /// A header read before `X-Forwarded-For` for the visitor's address, such
  /// as `CF-Connecting-IP` behind Cloudflare. Only with a trusted proxy.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["CF-Connecting-IP"]))]
  pub real_ip_header: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Security {
  /// Addresses (IPs or CIDRs) allowed to reach the dashboard and its API.
  /// Empty = no restriction.
  #[schemars(extend("examples" = [["10.0.0.0/8", "203.0.113.7"]]))]
  pub admin_allowed_ips: Vec<String>,
  /// Addresses refused everything, checked before any other rule.
  #[schemars(extend("examples" = [["198.51.100.0/24"]]))]
  pub denied_ips: Vec<String>,
  /// Mark session cookies `Secure`. Unset follows `proxy.trust`.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub secure_cookies: Option<bool>,
  /// Pin a dynamic token to the first device key that presents it. Default
  /// `false`.
  pub token_pinning: bool,
  /// How long before a token expires the `token_expiring` event fires. `0s`
  /// = never. Default `1d`.
  pub token_expiry_warning: Span,
  /// Destinations (hosts, `*.suffix`, IPs or CIDRs) a client may ask this
  /// server to reach itself with `server_side: true`. Empty permits nothing.
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub server_side_targets: Vec<String>,
  /// Locking out an address after failed sign-ins.
  pub login_lockout: LoginLockout,
  /// How far a client-declared `jwt` gate's key-set URL may reach.
  pub jwks: Jwks,
}

impl Default for Security {
  fn default() -> Self {
    Security {
      admin_allowed_ips: Vec::new(),
      denied_ips: Vec::new(),
      secure_cookies: None,
      token_pinning: false,
      token_expiry_warning: Span::secs(86_400),
      server_side_targets: Vec::new(),
      login_lockout: LoginLockout::default(),
      jwks: Jwks::default(),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LoginLockout {
  /// Consecutive failed sign-ins from one address before it is locked out.
  /// Default `5`.
  pub threshold: u32,
  /// The first lockout's length; it doubles with each repeat, up to an
  /// hour. Default `60s`.
  pub duration: Span,
}

impl Default for LoginLockout {
  fn default() -> Self {
    LoginLockout {
      threshold: 5,
      duration: Span::secs(60),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Jwks {
  /// Allow a plain-http key-set URL in a client-declared gate. Default
  /// `false`.
  pub allow_http: bool,
  /// Allow a key-set URL resolving to a private or loopback address in a
  /// client-declared gate. Default `false`.
  pub allow_private: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Outbound {
  /// Destinations (hosts, `*.suffix`, IPs or CIDRs) callbacks may reach;
  /// empty = no restriction.
  #[schemars(extend("examples" = [["hooks.example.com", "*.provider.example"]]))]
  pub allowlist: Vec<String>,
  /// With no allowlist, refuse destinations that are or resolve to internal
  /// addresses. Default `false`.
  pub block_private: bool,
  /// HTTP proxy callbacks go through: `host:port` or `http://host:port`.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["http://proxy.internal:3128"]))]
  pub proxy: Option<String>,
  /// Destinations called directly rather than through `proxy`: names, or
  /// `.suffix` / `*.suffix`.
  #[schemars(extend("examples" = [["auth.internal", ".svc.cluster.local"]]))]
  pub no_proxy: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Dashboard {
  /// Serve the dashboard. The API stays up either way. Default `true`.
  pub enabled: bool,
  /// A hostname whose root is the dashboard. One exact name.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["admin.example.com"]))]
  pub hostname: Option<String>,
  /// Fence the sign-in to the hostname's organization. Default `false`.
  pub fenced_login: bool,
  /// Language the dashboard and the login page open in when the browser's
  /// is not supported: en, de, es, fr, tr, ru, zh or ja. Default `en`.
  pub ui_language: String,
  /// Passkey sign-in.
  pub webauthn: Webauthn,
}

impl Default for Dashboard {
  fn default() -> Self {
    Dashboard {
      enabled: true,
      hostname: None,
      fenced_login: false,
      ui_language: "en".to_string(),
      webauthn: Webauthn::default(),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Webauthn {
  /// The public URL the dashboard is reached at; setting it turns passkeys
  /// on. Its domain becomes the relying-party id.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
  pub origin: Option<String>,
  /// A parent domain of the origin's host, so one passkey covers its
  /// subdomains.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["example.com"]))]
  pub rp_id: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Oidc {
  /// Issuer URL; setting it turns sign-on on.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["https://accounts.google.com"]))]
  pub issuer: Option<String>,
  /// OAuth client id registered at the issuer.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["aperio-dashboard"]))]
  pub client_id: Option<String>,
  /// OAuth client secret.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["replace-with-the-client-secret"]))]
  pub client_secret: Option<String>,
  /// Who may sign in: exact addresses, `*@domain`, or `*`.
  #[schemars(extend("examples" = [["*@example.com", "contractor@partner.org"]]))]
  pub allowed_emails: Vec<String>,
  /// Requested scopes. Default `openid email profile`.
  pub scopes: String,
  /// Fixed callback URL (`https://<host>/aperio/oidc/callback`). Set it:
  /// derived from the request otherwise.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["https://tunnel.example.com/aperio/oidc/callback"]))]
  pub redirect_url: Option<String>,
  /// What an email with no user record is granted at its first sign-in,
  /// `<org>:<role>` entries. Empty = refused until granted.
  #[schemars(extend("examples" = [["master:viewer"]]))]
  pub default_grants: Vec<String>,
  /// The claim carrying the provider's groups. Default `groups`.
  pub groups_claim: String,
  /// What each group means, `<group>=<org>:<role>` entries.
  #[schemars(extend("examples" = [["aperio-admins=master:admin", "auditors=*:viewer"]]))]
  pub group_grants: Vec<String>,
}

impl Default for Oidc {
  fn default() -> Self {
    Oidc {
      issuer: None,
      client_id: None,
      client_secret: None,
      allowed_emails: Vec::new(),
      scopes: "openid email profile".to_string(),
      redirect_url: None,
      default_grants: Vec::new(),
      groups_claim: "groups".to_string(),
      group_grants: Vec::new(),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Cache {
  /// Turn the cache on for services that set `cache: true`. Default `false`.
  pub enabled: bool,
  /// Memory budget of the cache. Default `64MB`.
  pub size: ByteSize,
  /// How long past its lifetime a cached response may still answer while a
  /// `resilience: true` service has no healthy client. Default `1h`.
  pub max_stale: Span,
  /// How long an error or negative response (a `404`) is cached. `0s` =
  /// never. Default `0s`.
  pub negative_ttl: Span,
}

impl Default for Cache {
  fn default() -> Self {
    Cache {
      enabled: false,
      size: ByteSize::mb(64),
      max_stale: Span::secs(3600),
      negative_ttl: Span::secs(0),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stream {
  /// Backlog of one stream at which its client is asked to pause. Default
  /// `2MB`.
  pub pause_at: ByteSize,
  /// Backlog under which a paused client is asked to resume. Default
  /// `512KB`.
  pub resume_at: ByteSize,
  /// Backlog past which a stream whose client ignores the pause is dropped.
  /// Default `16MB`.
  pub backlog_limit: ByteSize,
  /// Bytes per second a stream's reader must take while data waits for it,
  /// or the stream is ended. `0` = no floor. Default `0`.
  pub min_throughput: ByteSize,
}

impl Default for Stream {
  fn default() -> Self {
    Stream {
      pause_at: ByteSize::mb(2),
      resume_at: ByteSize::kb(512),
      backlog_limit: ByteSize::mb(16),
      min_throughput: ByteSize(0),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Tunnel {
  /// Offer per-message zlib compression to clients. Default `false`.
  pub compression: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Inspector {
  /// Record proxied transactions for the request inspector. Default `true`.
  pub enabled: bool,
  /// Mask credential headers and secret-looking body fields in the
  /// inspector and its exports. Default `true`.
  pub redact: bool,
}

impl Default for Inspector {
  fn default() -> Self {
    Inspector {
      enabled: true,
      redact: true,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
  /// Emit the per-request access event for successful requests (failures
  /// always log). Default `true`.
  pub access_events: bool,
  /// File the structured access log is appended to, one JSON line per
  /// request.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["./data/access.jsonl"]))]
  pub access_log: Option<String>,
  /// Fraction of successful requests that produce an access line, `0.0` to
  /// `1.0`. Default `1.0`.
  pub sample_rate: f64,
}

impl Default for Logging {
  fn default() -> Self {
    Logging {
      access_events: true,
      access_log: None,
      sample_rate: 1.0,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RequestId {
  /// Send the request id to the backend and echo it to the visitor. Default
  /// `true`.
  pub enabled: bool,
  /// Header it travels in. Default `x-request-id`.
  pub header: String,
  /// Adopt the visitor's own value when the request carries one. Default
  /// `false`.
  pub trust_inbound: bool,
}

impl Default for RequestId {
  fn default() -> Self {
    RequestId {
      enabled: true,
      header: "x-request-id".to_string(),
      trust_inbound: false,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IdentityHeaders {
  /// Tell the backend which client, organization and token served the
  /// request (`x-aperio-client-id`, `x-aperio-org`, `x-aperio-token`).
  /// Default `false`.
  pub serving: bool,
  /// Tell the backend who the visitor is (`x-aperio-visitor-how`,
  /// `x-aperio-visitor-id`). Default `false`.
  pub visitor: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Metrics {
  /// Serve `/aperio/metrics`. Default `false`.
  pub enabled: bool,
  /// Token required to scrape. Unset = a random one, generated once and
  /// kept in the data directory.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["a-long-random-metrics-token"]))]
  pub token: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Otel {
  /// Export a span per proxied request. Default `false`.
  pub enabled: bool,
  /// Collector base URL. Default `http://localhost:4318`, or
  /// `http://localhost:4317` with `protocol: grpc`.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["http://localhost:4318"]))]
  pub endpoint: Option<String>,
  /// `http` or `grpc`. Unset: port 4317 is grpc, anything else http.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["http", "grpc"]))]
  pub protocol: Option<String>,
  /// `service.name` on exported spans. Default `aperio-server`.
  pub service_name: String,
  /// Fraction of traces recorded, `0.0` to `1.0`. Default `1.0`.
  pub sample_rate: f64,
  /// Extra headers on every OTLP request, where a collector's credential
  /// goes.
  #[schemars(extend("examples" = [{"authorization": "Bearer xxx"}]))]
  pub headers: BTreeMap<String, String>,
  /// Accept OTLP exports from tunnel clients and forward them to `endpoint`.
  /// Default `false`.
  pub bridge: bool,
}

impl Default for Otel {
  fn default() -> Self {
    Otel {
      enabled: false,
      endpoint: None,
      protocol: None,
      service_name: "aperio-server".to_string(),
      sample_rate: 1.0,
      headers: BTreeMap::new(),
      bridge: false,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Audit {
  /// Rotate the audit log past this size; `0` = never. Default `10MB`.
  pub max_size: ByteSize,
  /// Rotated generations kept. Default `3`.
  pub max_files: u32,
}

impl Default for Audit {
  fn default() -> Self {
    Audit {
      max_size: ByteSize::mb(10),
      max_files: 3,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
  /// How long inspector captures and webhook inbox entries are kept. Unset =
  /// kept, bounded only by their caps.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["7d"]))]
  pub captures: Option<Span>,
  /// How long access-log lines are kept.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["30d"]))]
  pub access_log: Option<Span>,
  /// How long audit events are kept; the hash chain stays verifiable.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["365d"]))]
  pub audit: Option<Span>,
  /// How long day-granularity statistics are kept.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["90d"]))]
  pub stats: Option<Span>,
  /// Cap on the database file; past it the oldest low-priority data is
  /// pruned. Unset = unbounded.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["2GB"]))]
  pub db_max_size: Option<ByteSize>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Backup {
  /// Time between snapshots. Unset = no backups.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["6h"]))]
  pub interval: Option<Span>,
  /// Directory the snapshots are written to. Required with `interval`.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["/var/backups/aperio"]))]
  pub dir: Option<String>,
  /// Snapshots kept. Default `7`.
  pub keep: u32,
}

impl Default for Backup {
  fn default() -> Self {
    Backup {
      interval: None,
      dir: None,
      keep: 7,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Alerts {
  /// Failed-request fraction (`0.0` to `1.0`) that fires an `error_rate`
  /// alert. Unset = off.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [0.05]))]
  pub error_rate: Option<f64>,
  /// Window the error rate is measured over. Default `5m`.
  pub window: Span,
  /// Requests the window must hold before the error rate may fire. Default
  /// `20`.
  pub min_requests: u32,
  /// How long a known service may stay down before a `client_down` alert.
  /// Unset = off.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["2m"]))]
  pub client_down: Option<Span>,
  /// Operator-defined alerts over what the server measures.
  #[schemars(extend("examples" = [[{"name": "disk-filling", "metric": "store_bytes", "above": 536870912, "for": 300}]]))]
  pub rules: Vec<AlertRule>,
}

impl Default for Alerts {
  fn default() -> Self {
    Alerts {
      error_rate: None,
      window: Span::secs(300),
      min_requests: 20,
      client_down: None,
      rules: Vec::new(),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Webhooks {
  /// Waits between redelivery attempts after a failed delivery; empty = no
  /// retries. Default `[1s, 5s, 25s, 1m]`.
  #[schemars(extend("examples" = [["1s", "5s", "25s", "1m"]]))]
  pub retry_schedule: Vec<Span>,
}

impl Default for Webhooks {
  fn default() -> Self {
    Webhooks {
      retry_schedule: vec![Span::secs(1), Span::secs(5), Span::secs(25), Span::secs(60)],
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Scaling {
  /// Honor the `scaling:` blocks clients announce. Default `false`.
  pub enabled: bool,
  /// Allow a plain-http autoscaling endpoint. Default `false`.
  pub allow_http: bool,
  /// Allow an autoscaling endpoint on a private or loopback address. Default
  /// `false`.
  pub allow_private: bool,
  /// How long a record nothing re-announces is kept. Default `30d`.
  pub record_ttl: Span,
}

impl Default for Scaling {
  fn default() -> Self {
    Scaling {
      enabled: false,
      allow_http: false,
      allow_private: false,
      record_ttl: Span::secs(30 * 86_400),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Edge {
  /// Credential the edge proxy presents; setting it turns the
  /// `/aperio/api/edge/*` endpoints on.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["a-long-random-edge-token"]))]
  pub token: Option<String>,
  /// This server as the edge reaches it, e.g. `http://aperio:8080`.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["http://aperio:8080"]))]
  pub service_url: Option<String>,
  /// Traefik entry points on the generated routers.
  #[schemars(extend("examples" = [["websecure"]]))]
  pub entrypoints: Vec<String>,
  /// Traefik certificate resolver on the generated routers.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["letsencrypt"]))]
  pub cert_resolver: Option<String>,
  /// Also publish hostnames a token permits but no client serves. Default
  /// `false`.
  pub include_offline: bool,
}

/// How long a shutdown waits for in-flight requests: a duration, or `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drain {
  /// Wait this long.
  For(Span),
  /// The longest drain budget connected clients announce, capped at 30s.
  Auto,
}

impl Default for Drain {
  fn default() -> Self {
    Drain::For(Span::secs(0))
  }
}

impl Serialize for Drain {
  fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    match self {
      Drain::Auto => s.serialize_str("auto"),
      Drain::For(span) => span.serialize(s),
    }
  }
}

impl<'de> Deserialize<'de> for Drain {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let value = serde_yaml::Value::deserialize(d)?;
    if value.as_str().is_some_and(|s| s.trim() == "auto") {
      return Ok(Drain::Auto);
    }
    Span::deserialize(value)
      .map(Drain::For)
      .map_err(serde::de::Error::custom)
  }
}

impl JsonSchema for Drain {
  fn schema_name() -> std::borrow::Cow<'static, str> {
    "Drain".into()
  }

  fn inline_schema() -> bool {
    true
  }

  fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let span = generator.subschema_for::<Span>();
    schemars::json_schema!({
      "description": "A duration, or `auto` for the longest drain connected clients announce, capped at 30s.",
      "anyOf": [ { "const": "auto" }, span ],
      "examples": ["30s", "auto"]
    })
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Shutdown {
  /// How long in-flight requests may finish before their connections are
  /// ended, or `auto`. Default `0s`.
  pub drain: Drain,
  /// When shutdown stops waiting for anything still open and exits.
  /// Default `10s`.
  pub timeout: Span,
}

impl Default for Shutdown {
  fn default() -> Self {
    Shutdown {
      drain: Drain::default(),
      timeout: Span::secs(10),
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Uptime {
  /// Time between availability-history ticks. Default `10s`.
  pub tick: Span,
}

impl Default for Uptime {
  fn default() -> Self {
    Uptime {
      tick: Span::secs(10),
    }
  }
}

/// A site's own answer pages, the HTML itself. The `*` site's are the
/// server's, used wherever no narrower site sets one.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Pages {
  /// HTML answered when no client serves a route (`504`), instead of the
  /// plain-text default. At most 512KB.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["<h1>Nothing is serving this right now</h1>"]))]
  pub unavailable: Option<String>,
  /// HTML answered while a hostname is in maintenance (`503`). At most
  /// 512KB.
  #[serde(skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["<h1>Back soon</h1>"]))]
  pub maintenance: Option<String>,
}
