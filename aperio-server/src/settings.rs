use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

use crate::routing::normalize_random_subdomain_pattern;

/// Configuration settings for the Aperio server.
#[derive(Clone)]
pub(crate) struct ServerConfig {
  pub(crate) token: String,
  pub(crate) gateway_timeout: Duration,
  pub(crate) gateway_response_timeout: Duration,
  pub(crate) max_body_size: usize,
  pub(crate) max_tunnels: usize,
  /// Parallel tunnel connections one client may open for a single service
  /// (`connections:` in its aperio.yaml). The ceiling belongs here rather than
  /// in the client, since the connections are the server's resource; a token
  /// may lower it for its holder but never raise it.
  pub(crate) max_connections_per_service: u32,
  /// Record every proxied transaction for the dashboard's request inspector.
  /// On by default: it is the feature people reach for when something is
  /// wrong. Off costs a mutex, two header clones and a capture entry per
  /// request, which is the trade a server running flat out may want.
  pub(crate) inspector: bool,
  /// Emit the per-request structured access event for a successful request
  /// (`target: aperio_access`, level `info`). On by default. Distinct from
  /// `log_level`: turning this off silences the one-per-request line without
  /// silencing warnings and errors with it, so a refused or failed request
  /// still logs at `warn`.
  pub(crate) access_events: bool,
  pub(crate) ip_limit_max: f64,
  pub(crate) ip_limit_refill: f64,
  /// The `auth:` block or list as the file wrote it, when it wrote one.
  /// `None` means the scalar spellings (`APERIO_SERVER_AUTH`, the dashboard's
  /// editable field) are the whole of what was configured.
  pub(crate) visitor_auth_block: Option<aperio_config::AuthSetting>,
  /// The visitor gate in force. **The only stored form of it**: the scalar
  /// `auth_credentials` the dashboard shows is derived from this rather than
  /// held beside it, because two fields describing one gate can disagree, and
  /// the way they disagree is that the request path reads one of them and the
  /// login path reads the other.
  pub(crate) visitor_auth: crate::visitor_auth::Policy,
  /// When true, the server trusts `X-Forwarded-For` / `X-Real-IP` headers for
  /// client IP resolution. Only enable when running behind a trusted reverse
  /// proxy, otherwise clients can spoof these headers to bypass rate limiting.
  pub(crate) trust_proxy: bool,
  /// When true, the server ignores any client-declared visitor password
  /// override (Ping `visitor_auth`) and keeps full control of the visitor gate
  /// with its own APERIO_SERVER_AUTH / OIDC. Env-only (APERIO_IGNORE_CLIENT_AUTH).
  pub(crate) ignore_client_auth: bool,
  /// Header consulted first for the real client IP when trust_proxy is on
  /// (APERIO_REAL_IP_HEADER, e.g. `CF-Connecting-IP` behind Cloudflare).
  pub(crate) real_ip_header: Option<String>,
  /// Trusted reverse-proxy / CDN egress ranges (APERIO_TRUSTED_PROXIES, a
  /// comma-separated list of IPs or CIDRs). When set (and `trust_proxy` is on),
  /// the real client IP is resolved by walking the `X-Forwarded-For` chain plus
  /// the direct socket peer from right to left and taking the first address
  /// that is NOT one of these, the standard "trust proxy" model that works for
  /// any CDN/proxy chain, not just Cloudflare. Empty = legacy behavior (first
  /// XFF entry).
  pub(crate) trusted_proxies: Vec<(IpAddr, u32)>,
  /// Source IPs/CIDRs allowed to reach the admin surface, the `/aperio`
  /// dashboard and `/aperio/api/*` endpoints (APERIO_ADMIN_ALLOWED_IPS, a
  /// comma-separated list of IPs or CIDRs). Empty = no network restriction.
  /// The login page and its auth endpoints stay reachable from any address so
  /// password-gated services keep working; only the authenticated dashboard and
  /// its APIs are network-fenced. Proxy traffic and tunnel connections are
  /// never affected.
  pub(crate) admin_allowed_ips: Vec<(IpAddr, u32)>,
  /// When true, session cookies include the `Secure` flag so browsers only
  /// send them over HTTPS connections. Defaults to the value of `trust_proxy`
  /// (i.e. enabled when running behind a TLS-terminating reverse proxy).
  pub(crate) secure_cookies: bool,
  /// When true, only clients that declared (or were overruled with) a
  /// hostname bind participate in load balancing. Clients without a hostname
  /// bind never receive proxied traffic.
  pub(crate) require_hostname_bind: bool,
  /// What a route nobody gated means (`planned_features.md` #108).
  ///
  /// `Allow` is what the server has always done and stays the default: with
  /// no `auth:` anywhere, every route serves everyone. `Deny` inverts it, so
  /// a route is reachable because something said so rather than because
  /// nothing said otherwise, and `public: true` / `method: none` become the
  /// sentence that opens one rather than an exemption from a gate that may
  /// not exist.
  pub(crate) default_access: DefaultAccess,
  /// Optional bearer token required to scrape the `/aperio/metrics` endpoint.
  pub(crate) metrics_token: Option<String>,
  /// Honor `scaling:` blocks announced by clients. Off disables the whole
  /// autoscaling feature server-side, whatever a client declares.
  pub(crate) scaling_enabled: bool,
  /// Allow a plain-http autoscaling endpoint. Off by default: the URL comes
  /// from a client, so it is an SSRF vector and https is the floor.
  pub(crate) scaling_allow_http: bool,
  /// Allow an autoscaling endpoint on a private or loopback address. Off by
  /// default because the URL comes from a client, which makes it an SSRF
  /// vector; on when the provider API genuinely lives on the internal network.
  pub(crate) scaling_allow_private: bool,
  /// Drop autoscaling records nothing has re-announced for this many seconds.
  pub(crate) scaling_record_ttl: Duration,
  /// Credential gating the edge-integration endpoints (`/aperio/api/edge/*`),
  /// which publish the live hostname inventory to a reverse proxy in front of
  /// this server. None disables those endpoints entirely.
  pub(crate) edge_token: Option<String>,
  /// URL the edge proxy should forward matched traffic to (this server as the
  /// edge sees it, e.g. `http://aperio:8080`). Required by the Traefik
  /// document; the `ask` endpoint does not need it.
  pub(crate) edge_service_url: Option<String>,
  /// Traefik entry points the generated routers bind to (empty = let Traefik
  /// apply its own defaults).
  pub(crate) edge_entrypoints: Vec<String>,
  /// Traefik certificate resolver named on the generated routers (None = no
  /// TLS block, e.g. when the edge terminates TLS some other way).
  pub(crate) edge_cert_resolver: Option<String>,
  /// Include hostnames a token permits but no client currently serves, so a
  /// certificate can exist before the first client connects. Off by default:
  /// it lets a tenant provoke an ACME request for any hostname its token
  /// covers.
  pub(crate) edge_include_offline: bool,
  /// Canonical pattern for automatic random subdomains (from
  /// `APERIO_RANDOM_SUBDOMAIN`): a hostname whose leftmost label contains a
  /// `*` placeholder, e.g. `*.example.com` or `*-test.example.com`. When
  /// set, every connecting client is assigned the pattern with `*` replaced
  /// by a random label, in addition to any token-granted or declared
  /// hostname binds.
  pub(crate) random_subdomain_suffix: Option<String>,
  /// A client whose last heartbeat is older than this is considered down and
  /// removed from the load-balancing pool until it pings again.
  pub(crate) client_down_threshold: Duration,
  /// When true, the server offers zlib compression to connecting clients;
  /// tunnel frames are compressed once the client acknowledges.
  pub(crate) tunnel_compression: bool,
  /// Custom HTML page served on 504 gateway-timeout responses
  /// (loaded once from APERIO_504_PAGE at startup).
  pub(crate) custom_504_page: Option<String>,
  /// Custom HTML page served while a hostname is in maintenance mode
  /// (loaded once from APERIO_503_PAGE at startup).
  pub(crate) custom_503_page: Option<String>,
  /// How a client is picked from the routed pool (APERIO_LB_STRATEGY).
  pub(crate) lb_strategy: LbStrategy,
  /// What to do when a client is lost while a request is in flight.
  pub(crate) failover_mode: FailoverMode,
  /// Max re-dispatch attempts per request (APERIO_FAILOVER_MAX_JUMPS).
  pub(crate) failover_max_jumps: u32,
  /// Total time budget for waiting on candidates across all jumps
  /// (APERIO_FAILOVER_WINDOW, seconds).
  pub(crate) failover_window: Duration,
  /// Allow failover for non-idempotent methods too
  /// (APERIO_FAILOVER_ALL_METHODS).
  pub(crate) failover_all_methods: bool,
  /// When true (`APERIO_RETRY_ON_5XX`), a fully-buffered response whose status
  /// is a retryable server error (see `retry_statuses`) is transparently
  /// re-dispatched to another client instead of being returned to the visitor.
  /// This is a *server-side* retry, distinct from `failover_mode` (which
  /// governs connection-loss behavior): it triggers on an actual error
  /// response, not a dropped connection. It reuses the failover budget,
  /// bounded by `failover_max_jumps`, and honoring method retryability
  /// (`failover_all_methods`). Streamed responses/requests are never retried,
  /// since bytes may already have reached the visitor.
  pub(crate) retry_on_5xx: bool,
  /// Specific status codes that trigger a retry when `retry_on_5xx` is on
  /// (`APERIO_RETRY_STATUSES`, comma-separated). Empty = every 5xx (500-599).
  pub(crate) retry_statuses: Vec<u16>,
  /// Passive outlier ejection (`APERIO_OUTLIER_EJECTION`): when on, a client
  /// that returns too many server errors / times out repeatedly in a short
  /// window is temporarily removed from the routing pool, independent of the
  /// active `/health` probe. Re-admitted automatically after `outlier_eject`.
  pub(crate) outlier_ejection: bool,
  /// Failures within `outlier_window` that trigger an ejection
  /// (`APERIO_OUTLIER_MAX_FAILURES`, default 5).
  pub(crate) outlier_max_failures: u32,
  /// Rolling window failures are counted over (`APERIO_OUTLIER_WINDOW`, secs).
  pub(crate) outlier_window: Duration,
  /// How long an ejected client stays out of rotation before re-admission
  /// (`APERIO_OUTLIER_EJECT_SECS`).
  pub(crate) outlier_eject: Duration,
  /// Server-side GET response cache (APERIO_CACHE). Effective only for
  /// clients that announced `cache: true`, and only for responses whose
  /// `Cache-Control` explicitly allows shared caching.
  pub(crate) cache_enabled: bool,
  /// Total response-cache budget in bytes (APERIO_CACHE_MAX_BYTES,
  /// default 64 MiB).
  pub(crate) cache_max_bytes: u64,
  /// Seconds an expired resilient cache entry stays servable while its
  /// route has no healthy client (APERIO_CACHE_MAX_STALE, default 3600;
  /// 0 disables serve-stale entirely).
  pub(crate) cache_max_stale: u64,
  /// Streamed-data backlog at which the producing client is asked to pause
  /// a stream (APERIO_STREAM_PAUSE_BYTES, default 2 MiB). Protocol v3 flow
  /// control; see `state::StreamLimits`.
  /// Bytes per second a streamed response's consumer must take while data is
  /// waiting for it (`0` = no floor).
  pub(crate) stream_min_throughput: u64,
  pub(crate) stream_pause_bytes: usize,
  /// Backlog under which a paused producer is asked to resume
  /// (APERIO_STREAM_RESUME_BYTES, default 512 KiB).
  pub(crate) stream_resume_bytes: usize,
  /// Hard per-stream backlog cap in bytes (APERIO_STREAM_BACKLOG_LIMIT,
  /// default 16 MiB); drops producers that cannot be paused.
  pub(crate) stream_backlog_limit: usize,
  /// Destinations a client may ask this server to reach itself
  /// (`server_side: true` on a service), from `server_side_targets:`.
  ///
  /// Empty permits nothing, which is the opposite of `outbound_policy`'s
  /// empty allowlist and deliberate: that one governs callbacks the server
  /// makes on its own initiative, this one opens a request and response
  /// channel a tenant steers.
  pub(crate) server_side_targets: Vec<crate::outbound::OutboundPattern>,
  /// Optional policy over outbound callbacks, i.e. where webhook deliveries
  /// and autoscaling hooks may be sent (`APERIO_OUTBOUND_ALLOWLIST`,
  /// `APERIO_OUTBOUND_BLOCK_PRIVATE`). Default: unrestricted.
  pub(crate) outbound_policy: crate::outbound::OutboundPolicy,
  /// Concurrent proxied requests limit (APERIO_MAX_CONCURRENT_REQUESTS);
  /// requests beyond it are rejected with 429.
  pub(crate) max_concurrent_requests: usize,
  /// Max concurrently-live proxied public WebSockets (APERIO_MAX_WS_CONNECTIONS).
  /// Unlike HTTP requests these are long-lived, so they get their own ceiling
  /// (separate from `max_concurrent_requests`); beyond it an upgrade is
  /// rejected with 503. Env-only.
  pub(crate) max_ws_connections: usize,
  /// Consecutive login failures per IP before a lockout starts
  /// (APERIO_LOGIN_LOCKOUT_THRESHOLD).
  pub(crate) login_lockout_threshold: u32,
  /// Base lockout window in seconds, doubled per repeat offense
  /// (APERIO_LOGIN_LOCKOUT_SECS).
  pub(crate) login_lockout_secs: u64,
  /// Audit log rotation threshold in bytes, 0 disables rotation
  /// (APERIO_AUDIT_MAX_SIZE).
  pub(crate) audit_max_size: u64,
  /// Rotated audit log generations to keep (APERIO_AUDIT_MAX_FILES).
  pub(crate) audit_max_files: usize,
  /// Default dashboard/login UI language (APERIO_UI_LANGUAGE), used when the
  /// visitor's browser language is not among the supported ones.
  pub(crate) ui_language: String,
  /// Compiled server-side header rewrite rules (the `headers:` section of
  /// aperio-server.yaml); file-only, not a dashboard override.
  pub(crate) header_rules: crate::headers::HeaderTransforms,
  /// Compiled client-less routes (the `routes:` section of
  /// aperio-server.yaml); file-only, not a dashboard override.
  pub(crate) static_routes: crate::static_routes::StaticRoutes,
  /// Per-hostname custom 504/503 pages (the `error_pages:` section of
  /// aperio-server.yaml); file-only, overrides the global pages per host.
  pub(crate) error_pages: crate::error_pages::ErrorPages,
  /// Per-route request rate limits (the `rate_limits:` section of
  /// aperio-server.yaml); file-only, caps aggregate rps to a host+path.
  pub(crate) route_limits: crate::route_limits::RouteLimits,
  /// Per-hostname fallback URLs (the `fallbacks:` section of aperio-server.yaml).
  pub(crate) fallbacks: crate::fallbacks::Fallbacks,
  /// WAF-lite deny/size rules (the `waf:` section of aperio-server.yaml).
  pub(crate) waf: crate::waf::WafRules,
  /// Operator-defined alert rules (`alert_rules:`).
  pub(crate) alert_rules: crate::alert_rules::AlertRules,
  /// Recurring maintenance windows (`maintenance_windows:`).
  pub(crate) maintenance_windows: crate::maintenance_windows::MaintenanceWindows,
  /// Source IPs refused at the outermost layer (`denied_ips:`).
  pub(crate) denied_ips: crate::deny_list::DenyList,
  /// Announce the serving client, organization and token to the backend.
  pub(crate) identity_headers: bool,
  /// Announce who the *visitor* is to the backend. The gate has always known
  /// and never said, so an application behind a tunnel had to build a second
  /// login beside Aperio's to greet anyone. Off by default, the same new
  /// trust surface as `identity_headers` and adopted the same way, on purpose.
  pub(crate) visitor_identity_headers: bool,
  /// Fraction of successful requests that produce an access line (1.0 =
  /// all). Failures are never sampled.
  pub(crate) access_log_sample_rate: f64,
  /// Seconds to let in-flight proxied requests finish before shutdown ends
  /// long-lived connections. `Some(n)` = wait up to n seconds, `None` = size
  /// it from what connected clients announce (`auto`), capped.
  /// Accept OTLP exports from tunnel clients and forward them to the
  /// configured collector.
  /// Streamed responses one visitor address may hold open at once
  /// (`0` = no limit).
  /// Other servers a client may fall back to, announced in the handshake.
  pub(crate) alternate_servers: Vec<String>,
  pub(crate) max_streams_per_ip: u32,
  pub(crate) otel_bridge: bool,
  pub(crate) shutdown_drain: Option<u64>,
  /// True when `shutdown_drain` was written as `auto`.
  pub(crate) shutdown_drain_auto: bool,
  /// Seconds after which shutdown stops waiting for anything and exits.
  pub(crate) shutdown_timeout: u64,
  /// Send the request id to the backend and echo it to the visitor.
  pub(crate) request_id_enabled: bool,
  /// Header it travels in, lowercased.
  pub(crate) request_id_header: String,
  /// Adopt a visitor-supplied value instead of ignoring it.
  pub(crate) request_id_trust_inbound: bool,
  /// Trust-on-first-use token pinning (`APERIO_TOKEN_PINNING`). When on, the
  /// first client device key announced for a dynamic token is pinned, and a
  /// later connection with a different (or missing) key for that token is
  /// rejected, so a leaked token replayed from another machine cannot serve.
  /// This effectively binds a pinned token to a single client device; moving
  /// the token to another box means carrying its device key (or rotating the
  /// token, which clears the pin). Env-only.
  pub(crate) token_pinning: bool,
  /// When true, services reached through their random subdomain are marked
  /// non-indexable: an `X-Robots-Tag: noindex, nofollow` response header
  /// plus a disallow-all `/robots.txt`, so preview environments never end
  /// up in search engines (APERIO_PREVIEW_NOINDEX).
  pub(crate) preview_noindex: bool,
}

/// UI languages shipped with the dashboard.
pub(crate) const UI_LANGUAGES: &[&str] = &["en", "de", "es", "fr", "tr", "ru", "zh", "ja"];

/// What happens when a tunnel client is lost while a request is in flight
/// and no response bytes have reached the visitor yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FailoverMode {
  /// Fail immediately with 502 (default).
  Fail,
  /// Re-dispatch to another currently available candidate; fail when none.
  Retry,
  /// Wait for the same client instance to reconnect and re-dispatch (any
  /// candidate qualifies when the instance is unknown).
  Wait,
  /// Re-dispatch to another candidate right away; when none exists, wait
  /// for one to appear.
  RetryWait,
}

/// Dashboard-editable configuration overrides, persisted as
/// `<data_dir>/settings.json`. Every field is optional: `None` (or a missing
/// key) keeps the environment-derived default; empty strings clear optional
/// values (pages, subdomain suffix, visitor auth). The master token,
/// HOST/PORT, proxy trust, cookie security and OIDC remain env-only.
#[derive(Serialize, Deserialize, Default, Clone, utoipa::ToSchema)]
pub(crate) struct SettingsOverrides {
  pub(crate) gateway_timeout_secs: Option<u64>,
  pub(crate) gateway_response_timeout_secs: Option<u64>,
  pub(crate) max_body_size: Option<usize>,
  pub(crate) max_tunnels: Option<usize>,
  pub(crate) max_connections_per_service: Option<u32>,
  pub(crate) inspector: Option<bool>,
  pub(crate) access_events: Option<bool>,
  pub(crate) require_hostname_bind: Option<bool>,
  pub(crate) lb_strategy: Option<String>,
  pub(crate) failover_mode: Option<String>,
  pub(crate) failover_max_jumps: Option<u32>,
  pub(crate) failover_window_secs: Option<u64>,
  pub(crate) failover_all_methods: Option<bool>,
  pub(crate) client_down_threshold_secs: Option<u64>,
  pub(crate) ip_limit_max: Option<f64>,
  pub(crate) ip_limit_refill: Option<f64>,
  pub(crate) tunnel_compression: Option<bool>,
  pub(crate) random_subdomain_suffix: Option<String>,
  /// Raw HTML (not a file path, unlike APERIO_504_PAGE).
  pub(crate) custom_504_page: Option<String>,
  /// Raw HTML (not a file path, unlike APERIO_503_PAGE).
  pub(crate) custom_503_page: Option<String>,
  /// Visitor password in `user:password` form ("" = disabled).
  pub(crate) auth_credentials: Option<String>,
  pub(crate) cache_enabled: Option<bool>,
  pub(crate) cache_max_bytes: Option<u64>,
  pub(crate) cache_max_stale: Option<u64>,
  pub(crate) stream_pause_bytes: Option<u64>,
  pub(crate) stream_resume_bytes: Option<u64>,
  pub(crate) stream_backlog_limit: Option<u64>,
  pub(crate) max_concurrent_requests: Option<usize>,
  pub(crate) login_lockout_threshold: Option<u32>,
  pub(crate) login_lockout_secs: Option<u64>,
  /// 0 disables rotation.
  pub(crate) audit_max_size: Option<u64>,
  pub(crate) audit_max_files: Option<usize>,
  pub(crate) ui_language: Option<String>,
  pub(crate) preview_noindex: Option<bool>,
}

/// The subset of `aperio-server.yaml` keys that map to live-editable
/// settings, parsed straight from the file for hot-reload. Field names are
/// the yaml keys; each maps onto a [`SettingsOverrides`] field. Keys with a
/// non-trivial transform (`random_subdomain` normalization, `504_page`/
/// `503_page` file loading) are intentionally excluded, they need a restart.
// Serialize as well as Deserialize: the field names are the yaml keys, and a
// test walks them to prove the pane offers the names the file actually reads.
#[derive(serde::Deserialize, serde::Serialize, Default)]
struct FileSettings {
  gateway_timeout: Option<u64>,
  gateway_response_timeout: Option<u64>,
  max_body_size: Option<usize>,
  max_tunnels: Option<usize>,
  max_connections_per_service: Option<u32>,
  inspector: Option<bool>,
  access_events: Option<bool>,
  require_hostname_bind: Option<bool>,
  lb_strategy: Option<String>,
  failover: Option<String>,
  failover_max_jumps: Option<u32>,
  failover_window: Option<u64>,
  failover_all_methods: Option<bool>,
  client_down_threshold: Option<u64>,
  ip_limit_max: Option<f64>,
  ip_limit_refill: Option<f64>,
  tunnel_compression: Option<bool>,
  server_auth: Option<String>,
  cache: Option<bool>,
  cache_max_bytes: Option<u64>,
  cache_max_stale: Option<u64>,
  stream_pause_bytes: Option<u64>,
  stream_resume_bytes: Option<u64>,
  stream_backlog_limit: Option<u64>,
  max_concurrent_requests: Option<usize>,
  login_lockout_threshold: Option<u32>,
  login_lockout_secs: Option<u64>,
  audit_max_size: Option<u64>,
  audit_max_files: Option<usize>,
  ui_language: Option<String>,
  preview_noindex: Option<bool>,
}

/// Reads the live-editable settings from the current `aperio-server.yaml`
/// document as a [`SettingsOverrides`] layer. Applied on top of the
/// environment defaults and beneath the dashboard overrides, so file edits
/// take effect on hot-reload while dashboard edits still win.
pub(crate) fn file_overrides() -> SettingsOverrides {
  // The *flattened* document: grouped blocks (`cache: { max_bytes: … }`)
  // folded into the flat spellings this struct is typed for. Deserializing
  // the raw document made the whole layer fail on any grouped file, since
  // `cache`/`failover` are scalars here, and the fallback silently dropped
  // every file setting from hot-reload until a restart.
  let Some(doc) = crate::config_file::flattened_document() else {
    return SettingsOverrides::default();
  };
  let fs: FileSettings = match serde_yaml::from_value(serde_yaml::Value::Mapping(doc)) {
    Ok(fs) => fs,
    Err(e) => {
      // A value of the wrong shape must not silently discard the rest of
      // the file layer without a trace.
      tracing::warn!(
        "aperio-server.yaml: live-editable settings not readable ({}); file layer skipped",
        e
      );
      FileSettings::default()
    }
  };
  SettingsOverrides {
    gateway_timeout_secs: fs.gateway_timeout,
    gateway_response_timeout_secs: fs.gateway_response_timeout,
    max_body_size: fs.max_body_size,
    max_tunnels: fs.max_tunnels,
    max_connections_per_service: fs.max_connections_per_service,
    inspector: fs.inspector,
    access_events: fs.access_events,
    require_hostname_bind: fs.require_hostname_bind,
    lb_strategy: fs.lb_strategy,
    failover_mode: fs.failover,
    failover_max_jumps: fs.failover_max_jumps,
    failover_window_secs: fs.failover_window,
    failover_all_methods: fs.failover_all_methods,
    client_down_threshold_secs: fs.client_down_threshold,
    ip_limit_max: fs.ip_limit_max,
    ip_limit_refill: fs.ip_limit_refill,
    tunnel_compression: fs.tunnel_compression,
    random_subdomain_suffix: None,
    custom_504_page: None,
    custom_503_page: None,
    auth_credentials: fs.server_auth,
    cache_enabled: fs.cache,
    cache_max_bytes: fs.cache_max_bytes,
    cache_max_stale: fs.cache_max_stale,
    stream_pause_bytes: fs.stream_pause_bytes,
    stream_resume_bytes: fs.stream_resume_bytes,
    stream_backlog_limit: fs.stream_backlog_limit,
    max_concurrent_requests: fs.max_concurrent_requests,
    login_lockout_threshold: fs.login_lockout_threshold,
    login_lockout_secs: fs.login_lockout_secs,
    audit_max_size: fs.audit_max_size,
    audit_max_files: fs.audit_max_files,
    ui_language: fs.ui_language,
    preview_noindex: fs.preview_noindex,
  }
}

/// The `aperio-server.yaml` key each dashboard-editable setting is written
/// as, for the pane that offers the yaml which would make an override
/// permanent.
///
/// It is a table because the two names disagree more often than not:
/// `gateway_timeout_secs` is `gateway_timeout` in the file, `cache_enabled`
/// is `cache`, `auth_credentials` is `server_auth`. Emitting the setting's
/// own name would produce a file that looks right, materializes an
/// environment variable nobody reads, and changes nothing.
pub(crate) const YAML_KEYS: &[(&str, &str)] = &[
  ("gateway_timeout_secs", "gateway_timeout"),
  ("gateway_response_timeout_secs", "gateway_response_timeout"),
  ("max_body_size", "max_body_size"),
  ("max_tunnels", "max_tunnels"),
  ("max_connections_per_service", "max_connections_per_service"),
  ("inspector", "inspector"),
  ("access_events", "access_events"),
  ("require_hostname_bind", "require_hostname_bind"),
  ("lb_strategy", "lb_strategy"),
  ("failover_mode", "failover"),
  ("failover_max_jumps", "failover_max_jumps"),
  ("failover_window_secs", "failover_window"),
  ("failover_all_methods", "failover_all_methods"),
  ("client_down_threshold_secs", "client_down_threshold"),
  ("ip_limit_max", "ip_limit_max"),
  ("ip_limit_refill", "ip_limit_refill"),
  ("tunnel_compression", "tunnel_compression"),
  ("auth_credentials", "server_auth"),
  ("cache_enabled", "cache"),
  ("cache_max_bytes", "cache_max_bytes"),
  ("cache_max_stale", "cache_max_stale"),
  ("stream_pause_bytes", "stream_pause_bytes"),
  ("stream_resume_bytes", "stream_resume_bytes"),
  ("stream_backlog_limit", "stream_backlog_limit"),
  ("max_concurrent_requests", "max_concurrent_requests"),
  ("login_lockout_threshold", "login_lockout_threshold"),
  ("login_lockout_secs", "login_lockout_secs"),
  ("audit_max_size", "audit_max_size"),
  ("audit_max_files", "audit_max_files"),
  ("ui_language", "ui_language"),
  ("preview_noindex", "preview_noindex"),
  ("random_subdomain_suffix", "random_subdomain"),
];

/// Settings whose yaml key exists but is read once at startup, so writing it
/// into the file is right and takes a restart to take effect.
pub(crate) const YAML_KEYS_NEEDING_RESTART: &[&str] = &["random_subdomain_suffix"];

/// Settings with no honest yaml spelling of the value held here. The file's
/// `504_page`/`503_page` take a *path*; the override is the HTML itself, so
/// the two cannot be copied from one to the other.
pub(crate) const NOT_EXPRESSIBLE_IN_YAML: &[&str] = &["custom_504_page", "custom_503_page"];

/// Parses an `APERIO_LB_STRATEGY`-style value.
pub(crate) fn parse_lb_strategy(raw: &str) -> Option<LbStrategy> {
  match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
    "" | "round-robin" => Some(LbStrategy::RoundRobin),
    "primary-standby" | "failover" => Some(LbStrategy::PrimaryStandby),
    "sticky" => Some(LbStrategy::Sticky),
    _ => None,
  }
}

/// Parses an `APERIO_FAILOVER`-style value.
pub(crate) fn parse_failover_mode(raw: &str) -> Option<FailoverMode> {
  match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
    "" | "fail" => Some(FailoverMode::Fail),
    "retry" => Some(FailoverMode::Retry),
    "wait" => Some(FailoverMode::Wait),
    "retry-wait" => Some(FailoverMode::RetryWait),
    _ => None,
  }
}

/// Applies persisted/dashboard overrides on top of the env-derived defaults,
/// producing the effective configuration. Invalid values are skipped.
pub(crate) fn apply_settings_overrides(base: &ServerConfig, o: &SettingsOverrides) -> ServerConfig {
  let mut c = base.clone();
  if let Some(v) = o.gateway_timeout_secs {
    c.gateway_timeout = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.gateway_response_timeout_secs {
    c.gateway_response_timeout = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.max_body_size {
    c.max_body_size = v.max(1024);
  }
  if let Some(v) = o.max_tunnels {
    c.max_tunnels = v.max(1);
  }
  if let Some(v) = o.max_connections_per_service {
    c.max_connections_per_service = v.max(1);
  }
  if let Some(v) = o.inspector {
    c.inspector = v;
  }
  if let Some(v) = o.access_events {
    c.access_events = v;
  }
  if let Some(v) = o.require_hostname_bind {
    c.require_hostname_bind = v;
  }
  if let Some(ref s) = o.lb_strategy
    && let Some(v) = parse_lb_strategy(s)
  {
    c.lb_strategy = v;
  }
  if let Some(ref s) = o.failover_mode
    && let Some(v) = parse_failover_mode(s)
  {
    c.failover_mode = v;
  }
  if let Some(v) = o.failover_max_jumps {
    c.failover_max_jumps = v;
  }
  if let Some(v) = o.failover_window_secs {
    c.failover_window = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.failover_all_methods {
    c.failover_all_methods = v;
  }
  if let Some(v) = o.client_down_threshold_secs {
    c.client_down_threshold = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.ip_limit_max
    && v > 0.0
  {
    c.ip_limit_max = v;
  }
  // Strictly positive, like the environment variable this shadows. A refill
  // of zero is not "no limiting", it is a bucket that never refills: once the
  // burst is spent every visitor of every proxied site gets a 429, for as
  // long as nobody notices. That is not a setting anyone means to write.
  if let Some(v) = o.ip_limit_refill
    && v > 0.0
  {
    c.ip_limit_refill = v;
  }
  if let Some(v) = o.tunnel_compression {
    c.tunnel_compression = v;
  }
  if let Some(ref s) = o.random_subdomain_suffix {
    c.random_subdomain_suffix = if s.trim().is_empty() {
      None
    } else {
      // An invalid pattern keeps the previous value rather than breaking
      // hostname generation mid-flight.
      normalize_random_subdomain_pattern(s).or_else(|| c.random_subdomain_suffix.clone())
    };
  }
  if let Some(ref html) = o.custom_504_page {
    c.custom_504_page = if html.is_empty() {
      None
    } else {
      Some(html.clone())
    };
  }
  if let Some(ref html) = o.custom_503_page {
    c.custom_503_page = if html.is_empty() {
      None
    } else {
      Some(html.clone())
    };
  }
  if let Some(v) = o.cache_enabled {
    c.cache_enabled = v;
  }
  if let Some(v) = o.cache_max_bytes
    && v > 0
  {
    c.cache_max_bytes = v;
  }
  if let Some(v) = o.cache_max_stale {
    c.cache_max_stale = v;
  }
  // The stream watermarks are only sanity-gated here (positive); the trio's
  // internal consistency is repaired at use by `StreamLimits::sanitized`,
  // since each layer may override a different member.
  if let Some(v) = o.stream_pause_bytes
    && v > 0
  {
    c.stream_pause_bytes = v as usize;
  }
  if let Some(v) = o.stream_resume_bytes
    && v > 0
  {
    c.stream_resume_bytes = v as usize;
  }
  if let Some(v) = o.stream_backlog_limit
    && v > 0
  {
    c.stream_backlog_limit = v as usize;
  }
  if let Some(v) = o.max_concurrent_requests {
    c.max_concurrent_requests = v.max(1);
  }
  if let Some(v) = o.login_lockout_threshold {
    c.login_lockout_threshold = v.max(1);
  }
  if let Some(v) = o.login_lockout_secs {
    c.login_lockout_secs = v.max(1);
  }
  if let Some(v) = o.audit_max_size {
    c.audit_max_size = v;
  }
  if let Some(v) = o.audit_max_files {
    c.audit_max_files = v;
  }
  if let Some(ref v) = o.ui_language
    && UI_LANGUAGES.contains(&v.as_str())
  {
    c.ui_language = v.clone();
  }
  if let Some(v) = o.preview_noindex {
    c.preview_noindex = v;
  }
  // Derived last, and here rather than at each of the places above, because
  // every effective configuration in the process comes through this function:
  // startup, a hot reload and a dashboard edit alike. A gate computed at only
  // some of them is a gate that is right until someone changes a setting.
  //
  // The dashboard edits the scalar, so an edit that clears or sets it wins
  // over a block the file wrote: the operator changing it is looking at the
  // field they changed, and silently keeping a file's block would make the
  // dashboard lie about what is in force.
  if let Some(ref creds) = o.auth_credentials {
    c.visitor_auth = crate::visitor_auth::Policy::from_credentials(creds);
  } else if let Some(ref block) = c.visitor_auth_block {
    c.visitor_auth = crate::visitor_auth::Policy::compile(block);
  }
  c
}

/// JSON view of the dashboard-editable subset of a configuration.
pub(crate) fn settings_view(c: &ServerConfig) -> serde_json::Value {
  serde_json::json!({
    "gateway_timeout_secs": c.gateway_timeout.as_secs(),
    "gateway_response_timeout_secs": c.gateway_response_timeout.as_secs(),
    "max_body_size": c.max_body_size,
    "max_tunnels": c.max_tunnels,
    "max_connections_per_service": c.max_connections_per_service,
    "inspector": c.inspector,
    "access_events": c.access_events,
    "require_hostname_bind": c.require_hostname_bind,
    "lb_strategy": match c.lb_strategy {
      LbStrategy::RoundRobin => "round-robin",
      LbStrategy::PrimaryStandby => "primary-standby",
      LbStrategy::Sticky => "sticky",
    },
    "failover_mode": match c.failover_mode {
      FailoverMode::Fail => "fail",
      FailoverMode::Retry => "retry",
      FailoverMode::Wait => "wait",
      FailoverMode::RetryWait => "retry-wait",
    },
    "failover_max_jumps": c.failover_max_jumps,
    "failover_window_secs": c.failover_window.as_secs(),
    "failover_all_methods": c.failover_all_methods,
    "client_down_threshold_secs": c.client_down_threshold.as_secs(),
    "ip_limit_max": c.ip_limit_max,
    "ip_limit_refill": c.ip_limit_refill,
    "tunnel_compression": c.tunnel_compression,
    "random_subdomain_suffix": c.random_subdomain_suffix,
    "custom_504_page": c.custom_504_page,
    "custom_503_page": c.custom_503_page,
    // The scalar spelling of the gate, which is what the dashboard's text
    // field edits. `null` when the gate is off *and* when it says something a
    // single credential cannot carry, so the methods travel beside it: a
    // field showing nothing while a gate is in force would read as "no gate".
    "auth_credentials": c.visitor_auth.as_single_credential(),
    "visitor_auth_methods": c.visitor_auth.method_names(),
    "cache_enabled": c.cache_enabled,
    "cache_max_bytes": c.cache_max_bytes,
    "cache_max_stale": c.cache_max_stale,
    "stream_pause_bytes": c.stream_pause_bytes,
    "stream_resume_bytes": c.stream_resume_bytes,
    "stream_backlog_limit": c.stream_backlog_limit,
    "max_concurrent_requests": c.max_concurrent_requests,
    "login_lockout_threshold": c.login_lockout_threshold,
    "login_lockout_secs": c.login_lockout_secs,
    "audit_max_size": c.audit_max_size,
    "audit_max_files": c.audit_max_files,
    "ui_language": c.ui_language,
    "preview_noindex": c.preview_noindex,
  })
}

/// Computes the human-readable diff between two effective configurations,
/// over the dashboard-editable / live-reloadable key set ([`settings_view`]).
/// Each changed key is rendered as `key: old→new`, with secret-looking keys
/// masked and long values (e.g. custom HTML pages) summarized by length. Used
/// to answer "why did behavior change?" from the `config_reloaded` audit entry.
pub(crate) fn config_reload_diff(old: &ServerConfig, new: &ServerConfig) -> Vec<String> {
  let render = |k: &str, v: &serde_json::Value| -> String {
    if v.is_null() {
      return "unset".to_string();
    }
    if crate::redact::config_key_is_secret(k) {
      return crate::redact::mask().to_string();
    }
    let s = match v {
      serde_json::Value::String(s) => s.clone(),
      other => other.to_string(),
    };
    if s.chars().count() > 60 {
      format!("<{} chars>", s.chars().count())
    } else {
      s
    }
  };

  let (serde_json::Value::Object(a), serde_json::Value::Object(b)) =
    (settings_view(old), settings_view(new))
  else {
    return Vec::new();
  };
  let mut keys: std::collections::BTreeSet<String> = a.keys().cloned().collect();
  keys.extend(b.keys().cloned());
  let null = serde_json::Value::Null;
  let mut diffs = Vec::new();
  for k in keys {
    let ov = a.get(&k).unwrap_or(&null);
    let nv = b.get(&k).unwrap_or(&null);
    if ov != nv {
      diffs.push(format!("{}: {}→{}", k, render(&k, ov), render(&k, nv)));
    }
  }
  diffs
}

/// What happens to a proxied route that no `auth:` policy covers.
///
/// Two values and deliberately no third: this is a posture, and the moment it
/// grows a middle setting it stops being answerable with "is this route
/// reachable, yes or no".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DefaultAccess {
  /// Serve it. What the server did by default until 0.10.0.
  Allow,
  /// Refuse it, indistinguishably from a route nobody serves. The default
  /// since 0.10.0: a route is reachable because something said so.
  #[default]
  Deny,
}

/// Parses an `APERIO_DEFAULT_ACCESS` value.
pub(crate) fn parse_default_access(raw: &str) -> Option<DefaultAccess> {
  match raw.trim().to_ascii_lowercase().as_str() {
    "" | "deny" | "closed" => Some(DefaultAccess::Deny),
    "allow" | "open" => Some(DefaultAccess::Allow),
    _ => None,
  }
}

/// Load-balancing behavior applied after routing narrows the candidate pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LbStrategy {
  /// Rotate through the whole pool (default).
  RoundRobin,
  /// Only the clients sharing the lowest announced priority receive traffic;
  /// higher-priority-number standbys take over when every more-primary
  /// client drops out of the pool (rotation still applies within a tier).
  PrimaryStandby,
  /// Round-robin, but visitors stick to the client that served them first
  /// via an `aperio_affinity` cookie (falling back to rotation when that
  /// client leaves the pool).
  Sticky,
}

/// Names of the fields a SettingsOverrides actually overrides.
pub(crate) fn override_keys(o: &SettingsOverrides) -> Vec<String> {
  match serde_json::to_value(o) {
    Ok(serde_json::Value::Object(map)) => map
      .into_iter()
      .filter(|(_, v)| !v.is_null())
      .map(|(k, _)| k)
      .collect(),
    _ => Vec::new(),
  }
}

/// Keys the file writes *and* the dashboard stored an override for.
///
/// These are the ones that used to be a lie: `aperio-server.yaml` said one
/// thing, a value changed once from the dashboard months earlier said another,
/// the second won, and nothing anywhere reported the difference. An operator
/// reads the file, versions it and reviews it; a value they wrote there must
/// not be quietly ignored.
///
/// Compared through serde rather than field by field, so a setting added to
/// `SettingsOverrides` is covered without anyone remembering to add it here.
pub(crate) fn conflicting_keys(
  file: &SettingsOverrides,
  stored: &SettingsOverrides,
) -> Vec<String> {
  let from_file: std::collections::BTreeSet<String> = override_keys(file).into_iter().collect();
  override_keys(stored)
    .into_iter()
    .filter(|k| from_file.contains(k))
    .collect()
}

/// Removes the overrides the file also sets, returning what was dropped.
///
/// Dropped rather than merely out-voted: an override that is stored but never
/// applied is the same invisible state in a different place, and it would come
/// back to life the day someone removes the key from the file.
pub(crate) fn drop_conflicting(
  file: &SettingsOverrides,
  stored: &mut SettingsOverrides,
) -> Vec<String> {
  let conflicts = conflicting_keys(file, stored);
  if conflicts.is_empty() {
    return conflicts;
  }
  // Through the same JSON shape the comparison uses: null a key and the field
  // is `None` again, whatever its type.
  let Ok(serde_json::Value::Object(mut map)) = serde_json::to_value(&*stored) else {
    return Vec::new();
  };
  for key in &conflicts {
    map.insert(key.clone(), serde_json::Value::Null);
  }
  match serde_json::from_value::<SettingsOverrides>(serde_json::Value::Object(map)) {
    Ok(pruned) => {
      *stored = pruned;
      conflicts
    }
    Err(_) => Vec::new(),
  }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
