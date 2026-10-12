use std::net::IpAddr;
use std::time::Duration;

pub(crate) use aperio_config::server_settings::{FailoverMode, LbStrategy};

/// What a route nothing declares means, read off the server's `access`:
/// `deny` refuses it, and `public` or a gate (`visitor_auth`) serves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DefaultAccess {
  /// A route is served because something declared it reachable.
  Deny,
  /// Every route serves everyone unless something gates it.
  Allow,
}

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
  /// Header consulted first for the real client IP when trust_proxy is on
  /// (proxy.real_ip_header, e.g. `CF-Connecting-IP` behind Cloudflare).
  pub(crate) real_ip_header: Option<String>,
  /// Trusted reverse-proxy / CDN egress ranges (proxy.trusted, a
  /// comma-separated list of IPs or CIDRs). When set (and `trust_proxy` is on),
  /// the real client IP is resolved by walking the `X-Forwarded-For` chain plus
  /// the direct socket peer from right to left and taking the first address
  /// that is NOT one of these, the standard "trust proxy" model that works for
  /// any CDN/proxy chain, not just Cloudflare. Empty = legacy behavior (first
  /// XFF entry).
  pub(crate) trusted_proxies: Vec<(IpAddr, u32)>,
  /// Source IPs/CIDRs allowed to reach the admin surface, the `/aperio`
  /// dashboard and `/aperio/api/*` endpoints (`security.admin_allowed_ips`, a
  /// comma-separated list of IPs or CIDRs). Empty = no network restriction.
  /// The login page and its auth endpoints stay reachable from any address so
  /// password-gated services keep working; only the authenticated dashboard and
  /// its APIs are network-fenced. Proxy traffic and tunnel connections are
  /// never affected.
  pub(crate) admin_allowed_ips: Vec<(IpAddr, u32)>,
  /// A hostname whose root is the dashboard (dashboard.hostname,
  /// `planned_features.md` #152): `panel.example.com` opens the panel the
  /// way `panel.example.com/aperio` does, and `/aperio` stays on every
  /// hostname regardless. The name serves the panel and nothing else, so no
  /// bind may claim it. `None` = no such hostname.
  pub(crate) dashboard_hostname: Option<String>,
  /// Fence the dashboard login to the hostname's organization
  /// (dashboard.fenced_login, `planned_features.md` #151). On a
  /// hostname inside an organization's allowlist, the login admits that
  /// organization's identities and anyone reaching master; a hostname no
  /// fence claims admits master's identities and the users of organizations
  /// without a fence; and a session is good only on the hostname it was
  /// minted on. Off by default: a deployment whose tenants sign in at the
  /// server's own name would be locked out by turning it on unasked.
  pub(crate) fenced_login: bool,
  /// When true, session cookies include the `Secure` flag so browsers only
  /// send them over HTTPS connections. Defaults to the value of `trust_proxy`
  /// (i.e. enabled when running behind a TLS-terminating reverse proxy).
  pub(crate) secure_cookies: bool,
  /// What a route nobody gated means (`planned_features.md` #108): `Deny`
  /// when the server's `access` is `deny`, so a route is reachable because
  /// something said so rather than because nothing said otherwise, and
  /// `access: public` becomes the sentence that opens one. `Allow` when it is
  /// `public`; a gate in `visitor_auth` covers the rest either way.
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
  /// `routing.random_subdomain`): a hostname whose leftmost label contains a
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
  /// How a client is picked from the routed pool (routing.lb_strategy).
  pub(crate) lb_strategy: LbStrategy,
  /// What to do when a client is lost while a request is in flight.
  pub(crate) failover_mode: FailoverMode,
  /// Max re-dispatch attempts per request (failover.max_jumps).
  pub(crate) failover_max_jumps: u32,
  /// Total time budget for waiting on candidates across all jumps
  /// (`failover.window`).
  pub(crate) failover_window: Duration,
  /// Allow failover for non-idempotent methods too
  /// (failover.all_methods).
  pub(crate) failover_all_methods: bool,
  /// When true (`failover.retry_on_5xx`), a fully-buffered response whose status
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
  /// (`failover.retry_statuses`). Empty = every 5xx (500-599).
  pub(crate) retry_statuses: Vec<u16>,
  /// Passive outlier ejection (`outlier.enabled`): when on, a client
  /// that returns too many server errors / times out repeatedly in a short
  /// window is temporarily removed from the routing pool, independent of the
  /// active `/health` probe. Re-admitted automatically after `outlier_eject`.
  pub(crate) outlier_ejection: bool,
  /// Failures within `outlier_window` that trigger an ejection
  /// (`outlier.max_failures`, default 5).
  pub(crate) outlier_max_failures: u32,
  /// Rolling window failures are counted over (`outlier.window`).
  pub(crate) outlier_window: Duration,
  /// How long an ejected client stays out of rotation before re-admission
  /// (`outlier.eject_for`).
  pub(crate) outlier_eject: Duration,
  /// Server-side GET response cache (cache.enabled). Effective only for
  /// clients that announced `cache: true`, and only for responses whose
  /// `Cache-Control` explicitly allows shared caching.
  pub(crate) cache_enabled: bool,
  /// Total response-cache budget in bytes (cache.size,
  /// default 64 MiB).
  pub(crate) cache_max_bytes: u64,
  /// Seconds an expired resilient cache entry stays servable while its
  /// route has no healthy client (cache.max_stale, default 3600;
  /// 0 disables serve-stale entirely).
  pub(crate) cache_max_stale: u64,
  /// Streamed-data backlog at which the producing client is asked to pause
  /// a stream (advanced.stream.pause_at, default 2 MiB). Protocol v3 flow
  /// control; see `state::StreamLimits`.
  /// Bytes per second a streamed response's consumer must take while data is
  /// waiting for it (`0` = no floor).
  pub(crate) stream_min_throughput: u64,
  pub(crate) stream_pause_bytes: usize,
  /// Backlog under which a paused producer is asked to resume
  /// (advanced.stream.resume_at, default 512 KiB).
  pub(crate) stream_resume_bytes: usize,
  /// Hard per-stream backlog cap in bytes (advanced.stream.backlog_limit,
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
  /// and autoscaling hooks may be sent (`outbound.allowlist`,
  /// `outbound.block_private`). Default: unrestricted.
  pub(crate) outbound_policy: crate::outbound::OutboundPolicy,
  /// Concurrent proxied requests limit (limits.concurrent_requests);
  /// requests beyond it are rejected with 429.
  pub(crate) max_concurrent_requests: usize,
  /// Max concurrently-live proxied public WebSockets (limits.websockets).
  /// Unlike HTTP requests these are long-lived, so they get their own ceiling
  /// (separate from `max_concurrent_requests`); beyond it an upgrade is
  /// rejected with 503. Env-only.
  pub(crate) max_ws_connections: usize,
  /// Consecutive login failures per IP before a lockout starts
  /// (security.login_lockout.threshold).
  pub(crate) login_lockout_threshold: u32,
  /// Base lockout window in seconds, doubled per repeat offense
  /// (security.login_lockout.duration).
  pub(crate) login_lockout_secs: u64,
  /// Audit log rotation threshold in bytes, 0 disables rotation
  /// (audit.max_size).
  pub(crate) audit_max_size: u64,
  /// Rotated audit log generations to keep (audit.max_files).
  pub(crate) audit_max_files: usize,
  /// Default dashboard/login UI language (dashboard.ui_language), used when the
  /// visitor's browser language is not among the supported ones.
  pub(crate) ui_language: String,
  /// What the server does for a hostname by itself: answers, policy on
  /// proxied traffic, fallbacks, pages and maintenance windows (`sites`).
  pub(crate) sites: crate::sites::Sites,
  /// Operator-defined alert rules (`alert_rules:`).
  pub(crate) alert_rules: crate::alert_rules::AlertRules,
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
  /// Trust-on-first-use token pinning (`security.token_pinning`). When on, the
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
  /// up in search engines (routing.preview_noindex).
  pub(crate) preview_noindex: bool,
}

/// UI languages shipped with the dashboard.
pub(crate) const UI_LANGUAGES: &[&str] = &["en", "de", "es", "fr", "tr", "ru", "zh", "ja"];
