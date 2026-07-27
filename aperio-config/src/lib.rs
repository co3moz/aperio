//! The `aperio.yaml` client configuration schema.
//!
//! These are the exact types `aperio-client` deserializes its config file into.
//! They live in their own crate so the client's build script can emit a JSON
//! Schema (`schemars`) straight from them — the editor schema and the parser can
//! never drift apart. The doc comments below become the `description` of each
//! field in the generated schema, so they double as the `aperio.yaml` reference;
//! keep them to a single purposeful sentence and add `examples` where the value
//! has a specific format.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serde default protocol of a declared tunnel.
fn default_tcp() -> String {
  "tcp".to_string()
}

/// A private local service (e.g. a database or SSH) this client makes reachable
/// to a peer running `--bind-tunnels`, without ever exposing it to the public web.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct TunnelDecl {
  /// Local address this client dials when a peer binds the tunnel.
  #[schemars(extend("examples" = ["127.0.0.1:27017"]))]
  pub target: String,
  /// Transport of the tunnel: `tcp` (default) or `udp` (best-effort datagram relay).
  #[serde(default = "default_tcp")]
  #[schemars(extend("examples" = ["tcp", "udp"]))]
  pub protocol: String,
  /// End-to-end encrypt this tunnel between the two clients (X25519 +
  /// ChaCha20-Poly1305); the server only relays ciphertext. TCP only.
  #[serde(default)]
  pub encrypt: bool,
  /// Pre-shared key mixed into the key derivation of an encrypted tunnel,
  /// protecting against an actively hostile server. Never sent anywhere —
  /// the binder configures the same value in its `bind-tunnels` entry.
  #[serde(default, skip_serializing)]
  pub psk: Option<String>,
  /// UDP only: seconds a relay may sit with no datagrams in either direction
  /// before it expires (default 60); binders learn it via tunnel discovery.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [300]))]
  pub idle_timeout: Option<u64>,
  /// Expose this tunnel on a public server port (experimental, TCP only):
  /// the value must equal the `key` of an `expose:` entry in the server's
  /// aperio-server.yaml; the server then relays that port here directly.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["k5fj2q-expose-secret"]))]
  pub expose: Option<String>,
}

/// Header edits applied to one direction of proxied traffic (request or
/// response): `add` sets headers (replacing any existing value of the same
/// name), `remove` strips headers by name (case-insensitive).
#[derive(Deserialize, Default, Clone, Debug, JsonSchema)]
pub struct HeaderDirectives {
  /// Headers to set, name → value; replaces an existing header of the same name.
  #[serde(default)]
  #[schemars(extend("examples" = [{"X-Forwarded-Env": "staging"}]))]
  pub add: HashMap<String, String>,
  /// Header names to strip (case-insensitive).
  #[serde(default)]
  #[schemars(extend("examples" = [["Server", "X-Powered-By"]]))]
  pub remove: Vec<String>,
}

/// Header add/remove rules for proxied HTTP traffic: `request` edits what the
/// local backend receives, `response` edits what the visitor receives.
/// Hop-by-hop and tunnel-critical headers stay managed by Aperio regardless.
#[derive(Deserialize, Default, Clone, Debug, JsonSchema)]
pub struct HeaderRules {
  /// Edits applied to forwarded requests before they reach the local backend.
  pub request: Option<HeaderDirectives>,
  /// Edits applied to backend responses before they return to the visitor.
  pub response: Option<HeaderDirectives>,
}

/// Security response-header preset: `security_headers: true` enables the
/// standard set (HSTS, `X-Frame-Options: DENY`, `X-Content-Type-Options:
/// nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`), a mapping
/// picks headers individually. Explicit `headers:` rules always win over the
/// preset.
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum SecurityHeaders {
  /// `true` enables the standard preset, `false` disables it (e.g. for one
  /// service when the top level enables it).
  Flag(bool),
  /// Granular per-header selection.
  Detailed(SecurityHeaderOptions),
}

/// Individually selected security response headers; only the set fields are
/// injected.
#[derive(Deserialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityHeaderOptions {
  /// Inject `Strict-Transport-Security` (only meaningful behind HTTPS).
  pub hsts: Option<bool>,
  /// HSTS `max-age` in seconds (default 63072000 = 2 years).
  #[schemars(extend("examples" = [31536000]))]
  pub hsts_max_age: Option<u64>,
  /// `X-Frame-Options` value to inject.
  #[schemars(extend("examples" = ["DENY", "SAMEORIGIN"]))]
  pub frame_options: Option<String>,
  /// Inject `X-Content-Type-Options: nosniff`.
  pub nosniff: Option<bool>,
  /// `Referrer-Policy` value to inject.
  #[schemars(extend("examples" = ["strict-origin-when-cross-origin"]))]
  pub referrer_policy: Option<String>,
  /// `Content-Security-Policy` value to inject (no default — CSP is
  /// application-specific).
  #[schemars(extend("examples" = ["default-src 'self'"]))]
  pub csp: Option<String>,
}

impl SecurityHeaders {
  /// Expands the preset into concrete response headers to inject.
  pub fn headers(&self) -> Vec<(String, String)> {
    const DEFAULT_HSTS_MAX_AGE: u64 = 63_072_000; // 2 years
    let mut out = Vec::new();
    match self {
      SecurityHeaders::Flag(false) => {}
      SecurityHeaders::Flag(true) => {
        out.push((
          "Strict-Transport-Security".to_string(),
          format!("max-age={DEFAULT_HSTS_MAX_AGE}"),
        ));
        out.push(("X-Frame-Options".to_string(), "DENY".to_string()));
        out.push(("X-Content-Type-Options".to_string(), "nosniff".to_string()));
        out.push((
          "Referrer-Policy".to_string(),
          "strict-origin-when-cross-origin".to_string(),
        ));
      }
      SecurityHeaders::Detailed(opts) => {
        if opts.hsts.unwrap_or(false) || opts.hsts_max_age.is_some() {
          let max_age = opts.hsts_max_age.unwrap_or(DEFAULT_HSTS_MAX_AGE);
          out.push((
            "Strict-Transport-Security".to_string(),
            format!("max-age={max_age}"),
          ));
        }
        if let Some(v) = opts.frame_options.as_ref().filter(|v| !v.trim().is_empty()) {
          out.push(("X-Frame-Options".to_string(), v.trim().to_string()));
        }
        if opts.nosniff.unwrap_or(false) {
          out.push(("X-Content-Type-Options".to_string(), "nosniff".to_string()));
        }
        if let Some(v) = opts
          .referrer_policy
          .as_ref()
          .filter(|v| !v.trim().is_empty())
        {
          out.push(("Referrer-Policy".to_string(), v.trim().to_string()));
        }
        if let Some(v) = opts.csp.as_ref().filter(|v| !v.trim().is_empty()) {
          out.push(("Content-Security-Policy".to_string(), v.trim().to_string()));
        }
      }
    }
    out
  }
}

/// Backend health probing for one service: the endpoint the client checks and
/// how patient it is before pulling the backend out of routing.
///
/// The flat `target_health` / `health_interval` / `health_timeout` /
/// `health_threshold` / `wait_for_backend` keys mean exactly the same thing
/// and still work; this block is the form to write new configs in, and wins
/// per field when both are present.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
  /// Endpoint to probe: a path like `/health` or a full URL. Unset = no probe,
  /// and the service is routable as soon as the tunnel is up.
  #[schemars(extend("examples" = ["/health"]))]
  pub endpoint: Option<String>,
  /// Seconds between probes.
  #[schemars(extend("examples" = [10]))]
  pub interval: Option<u64>,
  /// Seconds to wait for each probe before counting it as failed.
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy.
  #[schemars(extend("examples" = [3]))]
  pub threshold: Option<u32>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `endpoint` when that is set).
  pub wait_for_backend: Option<bool>,
}

impl HealthConfig {
  /// True when nothing in the block was set (an empty `health:` mapping).
  pub fn is_empty(&self) -> bool {
    self.endpoint.is_none()
      && self.interval.is_none()
      && self.timeout.is_none()
      && self.threshold.is_none()
      && self.wait_for_backend.is_none()
  }
}

/// Autoscaling declaration: the URL the *server* calls when this service needs
/// capacity it does not have. Aperio never starts or stops anything itself, it
/// only signals a desired capacity to an endpoint the operator controls.
///
/// The record outlives the client process on purpose: the whole point of
/// `min: 0` is that the server can call this URL when nothing is running.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
pub struct ScalingDecl {
  /// Endpoint the server POSTs to when it wants more capacity. HTTPS only
  /// unless the server allows plain HTTP; private and loopback addresses are
  /// refused, since the caller is a lower-trust credential than an operator.
  #[schemars(extend("examples" = ["https://api.provider.example/apps/web/scale"]))]
  pub url: String,
  /// Sent as `Authorization: Bearer` on the outgoing call. Write-only: the
  /// server never echoes it back and never logs it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub secret: Option<String>,
  /// Instances that should always be running. `0` opts into scale-to-zero:
  /// a request for an unserved hostname triggers a cold start instead of a
  /// 504.
  #[serde(default)]
  pub min: u32,
  /// Ceiling the server will never ask to exceed (0 = only cold starts, no
  /// scale-out).
  #[serde(default)]
  pub max: u32,
  /// How long a visitor request may be held while a cold start completes,
  /// e.g. `45s` (default 45s, 0 = do not hold, answer immediately).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["45s"]))]
  pub cold_start: Option<String>,
  /// Pool utilization above which the server asks for one more instance,
  /// between 0 and 1 (default 0.8).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [0.8]))]
  pub target_utilization: Option<f64>,
  /// How long utilization must stay above the target before scaling out,
  /// e.g. `15s` (default 15s). Guards against reacting to a single spike.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["15s"]))]
  pub window: Option<String>,
  /// Minimum gap between two calls for this bind, e.g. `60s` (default 60s).
  /// A new instance needs time to appear; without this the server would ask
  /// again while it is still starting.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["60s"]))]
  pub cooldown: Option<String>,
}

/// The Aperio server this client connects to: either a bare URL string, or a
/// `{ url, token }` section that also carries the tunnel token.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ServerValue {
  /// Server URL only — the token then comes from `token:` or the environment.
  Url(String),
  /// Server URL together with the tunnel token.
  Section {
    /// URL of the Aperio server this client dials out to.
    #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
    url: Option<String>,
    /// Tunnel token (master or a scoped dynamic token) that authorizes this client.
    #[schemars(extend("examples" = ["apr_xxxxxxxxxxxxxxxx"]))]
    token: Option<String>,
    /// Admin API key used by `aperio-client api ...` calls (never for the
    /// tunnel itself).
    #[schemars(extend("examples" = ["apk_xxxxxxxxxxxxxxxx"]))]
    api_key: Option<String>,
  },
}

/// A service's public hostname(s): either a single `hostname: app.example.com`
/// or a list `hostname: [app.example.com, www.example.com]`. Each must be
/// permitted by the client's token.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum Hostnames {
  /// A single hostname.
  One(String),
  /// Several hostnames routing to the same service.
  Many(Vec<String>),
}

impl Hostnames {
  /// Flattens to a list of trimmed, non-empty hostnames.
  pub fn into_vec(self) -> Vec<String> {
    let raw = match self {
      Hostnames::One(h) => vec![h],
      Hostnames::Many(hs) => hs,
    };
    raw
      .into_iter()
      .map(|h| h.trim().to_string())
      .filter(|h| !h.is_empty())
      .collect()
  }
}

/// One exposed backend when a single client serves several at once; any unset
/// field falls back to the top-level value.
#[derive(Deserialize, Default, Clone, JsonSchema)]
pub struct ServiceEntry {
  /// Label for this service in client logs and the dashboard clients table.
  #[schemars(extend("examples" = ["web"]))]
  pub name: Option<String>,
  /// Local backend this service exposes through the tunnel; `h2c://` /
  /// `h2://` targets are dialed over HTTP/2 (gRPC backends, trailers relayed);
  /// `unix://` targets forward over a Unix domain socket.
  #[schemars(extend("examples" = ["http://localhost:3000", "3000", "h2c://127.0.0.1:50051", "unix:///var/run/app.sock"]))]
  pub target: Option<String>,
  /// Serve a local directory of static files as this service instead of
  /// forwarding to a backend (mutually exclusive with `target`/`tcp_target`);
  /// directories serve their `index.html`.
  #[schemars(extend("examples" = ["./dist"]))]
  pub serve: Option<String>,
  /// Public hostname(s) that should route to this service: a single string
  /// or a list. Each must be permitted by the client's token.
  #[schemars(extend("examples" = ["app.example.com", ["app.example.com", "www.example.com"]]))]
  pub hostname: Option<Hostnames>,
  /// Public path prefix that should route to this service.
  #[schemars(extend("examples" = ["/api"]))]
  pub path: Option<String>,
  /// Strip the path prefix before forwarding, so the backend sees `/` not the bind.
  pub trim_bind: Option<bool>,
  /// Forward the visitor's original Host header instead of the target's.
  pub pass_hostname: Option<bool>,
  /// Most requests this service handles at once before the server queues the rest.
  #[schemars(extend("examples" = [8]))]
  pub max_concurrent: Option<u32>,
  /// Parallel tunnel connections opened for this service (1–16, default 1);
  /// the server load-balances across them like separate clients, so a single
  /// dropped connection leaves no visitor-facing gap.
  #[schemars(extend("examples" = [2]))]
  pub connections: Option<u32>,
  /// Failover tier for this service (0 = primary, higher numbers are standbys).
  #[schemars(extend("examples" = [0]))]
  pub priority: Option<u32>,
  /// This service's share of the link: the server never pushes it faster than
  /// this, and the share is split across its `connections`. Settled against the
  /// top-level `bandwidth` budget when there is one. Bit suffixes
  /// (`kbit`/`mbit`/`gbit`) count as /8, byte suffixes (`kb`/`mb`/`gb`, or bare
  /// `k`/`m`/`g`) as x1000.
  #[schemars(extend("examples" = ["8mbit", "500kbit", "2MB"]))]
  pub bandwidth: Option<String>,
  /// Seconds to wait for this backend to respond before failing the request.
  #[schemars(extend("examples" = [30]))]
  pub timeout: Option<u64>,
  /// Largest response body, in bytes, this service will relay to a visitor.
  #[schemars(extend("examples" = [10485760]))]
  pub max_response_body: Option<usize>,
  /// Largest request body, in bytes, visitors may upload to this service;
  /// the server rejects bigger uploads with 413 before they enter the tunnel.
  #[schemars(extend("examples" = [1048576]))]
  pub max_request_body: Option<u64>,
  /// Seconds the server should wait for this service to answer a dispatched
  /// request before failing it — a per-service override of the server's global
  /// gateway response timeout, for slow report/upload endpoints.
  #[schemars(extend("examples" = [120]))]
  pub response_timeout: Option<u64>,
  /// How many backend redirects to follow transparently before passing one through.
  #[schemars(extend("examples" = [5]))]
  pub max_redirects: Option<usize>,
  /// Raw TCP backend for this service instead of HTTP (experimental).
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Backend health probing for this service (`endpoint`, `interval`,
  /// `timeout`, `threshold`, `wait_for_backend`). Preferred over the flat
  /// `target_health` / `health_*` keys, which still work.
  pub health: Option<HealthConfig>,
  /// Backend health endpoint the client probes to pull itself from rotation
  /// when down. Deprecated spelling of `health.endpoint`.
  #[schemars(extend("examples" = ["/health"]))]
  pub target_health: Option<String>,
  /// Hold this service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `target_health` when that is set).
  pub wait_for_backend: Option<bool>,
  /// Seconds between backend health probes.
  #[schemars(extend("examples" = [10]))]
  pub health_interval: Option<u64>,
  /// Seconds to wait for each health probe before counting it as failed.
  #[schemars(extend("examples" = [5]))]
  pub health_timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy.
  #[schemars(extend("examples" = [3]))]
  pub health_threshold: Option<u32>,
  /// Serve this service without the server's visitor login (needs a token that allows it).
  pub public: Option<bool>,
  /// Gate this service behind your own `user:password` login instead of the server's.
  #[schemars(extend("examples" = ["admin:s3cret"]))]
  pub auth: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach this service (plain IPs or CIDR
  /// ranges); empty/unset = everyone. Enforced by the server before dispatch.
  #[schemars(extend("examples" = [["203.0.113.7", "10.0.0.0/8"]]))]
  pub allowed_ips: Option<Vec<String>>,
  /// Request/response header add-remove rules for this service (replaces the
  /// top-level `headers` when set).
  pub headers: Option<HeaderRules>,
  /// Security response-header preset for this service (`true` or a granular
  /// mapping; replaces the top-level `security_headers` when set).
  pub security_headers: Option<SecurityHeaders>,
  /// Let the server cache this service's GET responses (per their
  /// `Cache-Control`); effective only when the server enables APERIO_CACHE.
  pub cache: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504 (needs `cache: true` and the server-side cache enabled).
  pub resilience: Option<bool>,
  /// Persist inbound POST requests (third-party webhooks) hitting this
  /// service into the server's webhook inbox, for browsing and re-firing.
  pub webhook_inbox: Option<bool>,
  /// Redirect URL for visitors rejected by `allowed_ips` when no candidate
  /// of the route admits them (unset = stealth: the same answer as an
  /// unclaimed route).
  #[schemars(extend("examples" = ["https://example.com/not-for-you"]))]
  pub denied: Option<String>,
}

/// A peer client whose declared tunnels this process binds to local ports.
#[derive(Deserialize, Default, Clone, JsonSchema)]
pub struct BindTunnelEntry {
  /// Token the peer connected with; falls back to this client's server token when unset.
  #[schemars(extend("examples" = ["apr_xxxxxxxxxxxxxxxx"]))]
  pub token: Option<String>,
  /// Map a declared tunnel target to a specific local port instead of reusing the target's.
  #[serde(default, rename = "override")]
  pub overrides: HashMap<String, u16>,
  /// Pre-shared key for this peer's end-to-end encrypted tunnels; must match
  /// the `psk` the declaring client configured. Never sent to the server.
  pub psk: Option<String>,
}

/// The Aperio client configuration file (`aperio.yaml` or `~/.aperio.yaml`).
/// Every key is optional and can equally be set with a CLI flag or an `APERIO_*`
/// environment variable; this file is the lowest-friction way to keep them.
#[derive(Deserialize, Default, JsonSchema)]
pub struct FileConfig {
  /// The Aperio server to reach and the token to authenticate the tunnel with.
  pub server: Option<ServerValue>,
  /// Tunnel token, for when it isn't nested under `server.token`.
  #[schemars(extend("examples" = ["apr_xxxxxxxxxxxxxxxx"]))]
  pub token: Option<String>,
  /// Local backend to expose (single-service mode; use `services` for
  /// several). `h2c://` / `h2://` targets are dialed over HTTP/2 (gRPC).
  #[schemars(extend("examples" = ["http://localhost:3000", "3000", "h2c://127.0.0.1:50051"]))]
  pub target: Option<String>,
  /// Serve a local directory of static files instead of forwarding to a
  /// backend (mutually exclusive with `target`); directories serve their
  /// `index.html`.
  #[schemars(extend("examples" = ["./dist"]))]
  pub serve: Option<String>,
  /// Public hostname(s) to claim for this client's traffic: a single string
  /// or a list.
  #[schemars(extend("examples" = ["app.example.com", ["app.example.com", "www.example.com"]]))]
  pub hostname: Option<Hostnames>,
  /// Public path prefix to claim for this client's traffic.
  #[schemars(extend("examples" = ["/api"]))]
  pub path: Option<String>,
  /// Strip the path prefix before forwarding, so the backend sees `/` not the bind.
  pub trim_bind: Option<bool>,
  /// Forward the visitor's original Host header to the backend instead of the target's.
  pub pass_hostname: Option<bool>,
  /// Most requests handled at once before the server queues the rest.
  #[schemars(extend("examples" = [8]))]
  pub max_concurrent: Option<u32>,
  /// Parallel tunnel connections opened for the exposed service (1–16,
  /// default 1); the server load-balances across them like separate clients,
  /// so a single dropped connection leaves no visitor-facing gap.
  #[schemars(extend("examples" = [2]))]
  pub connections: Option<u32>,
  /// Largest response body, in bytes, the client will relay to a visitor.
  #[schemars(extend("examples" = [10485760]))]
  pub max_response_body: Option<usize>,
  /// Largest request body, in bytes, visitors may upload to this service;
  /// the server rejects bigger uploads with 413 before they enter the tunnel.
  #[schemars(extend("examples" = [1048576]))]
  pub max_request_body: Option<u64>,
  /// Seconds the server should wait for this service to answer a dispatched
  /// request before failing it — a per-service override of the server's global
  /// gateway response timeout (defaults applied per service).
  #[schemars(extend("examples" = [120]))]
  pub response_timeout: Option<u64>,
  /// Seconds to wait for the backend to respond before failing a request.
  #[schemars(extend("examples" = [30]))]
  pub timeout: Option<u64>,
  /// Largest single tunnel frame, in bytes, the client will accept.
  #[schemars(extend("examples" = [33554432]))]
  pub max_message_size: Option<usize>,
  /// Raw TCP backend to expose instead of HTTP (experimental).
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Backend health probing (`endpoint`, `interval`, `timeout`, `threshold`,
  /// `wait_for_backend`). Preferred over the flat `target_health` / `health_*`
  /// keys, which still work; `services:` entries may override it per service.
  pub health: Option<HealthConfig>,
  /// Backend health endpoint to probe; a failing backend leaves rotation
  /// without dropping the tunnel. Deprecated spelling of `health.endpoint`.
  #[schemars(extend("examples" = ["/health"]))]
  pub target_health: Option<String>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `target_health` when that is set).
  pub wait_for_backend: Option<bool>,
  /// Seconds between backend health probes.
  #[schemars(extend("examples" = [10]))]
  pub health_interval: Option<u64>,
  /// Seconds to wait for each health probe before counting it as failed.
  #[schemars(extend("examples" = [5]))]
  pub health_timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy.
  #[schemars(extend("examples" = [3]))]
  pub health_threshold: Option<u32>,
  /// Failover tier for this client (0 = primary, higher numbers are standbys).
  #[schemars(extend("examples" = [0]))]
  pub priority: Option<u32>,
  /// Total link capacity of this client: a budget divided across every service
  /// and every parallel connection, so the sum the server is allowed to push
  /// never exceeds it. Bit suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte
  /// suffixes (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000.
  #[schemars(extend("examples" = ["8mbit", "500kbit", "2MB"]))]
  pub bandwidth: Option<String>,
  /// How many backend redirects to follow transparently before passing one through.
  #[schemars(extend("examples" = [5]))]
  pub max_redirects: Option<usize>,
  /// Expose several backends from one client, each on its own tunnel connection
  /// (replaces the single top-level `target`).
  pub services: Option<Vec<ServiceEntry>>,
  /// Serve without the server's visitor login (needs a token that allows it).
  pub public: Option<bool>,
  /// Gate this client behind your own `user:password` login instead of the server's.
  #[schemars(extend("examples" = ["admin:s3cret"]))]
  pub auth: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach this service (plain IPs or CIDR
  /// ranges); empty/unset = everyone. Enforced by the server before dispatch.
  #[schemars(extend("examples" = [["203.0.113.7", "10.0.0.0/8"]]))]
  pub allowed_ips: Option<Vec<String>>,
  /// Let the server cache GET responses (per their `Cache-Control`);
  /// effective only when the server enables APERIO_CACHE.
  pub cache: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504 (needs `cache: true` and the server-side cache enabled).
  pub resilience: Option<bool>,
  /// Persist inbound POST requests (third-party webhooks) into the server's
  /// webhook inbox, for browsing and re-firing (services may override).
  pub webhook_inbox: Option<bool>,
  /// Redirect URL for visitors rejected by `allowed_ips` when no candidate
  /// of the route admits them (unset = stealth; services may override).
  #[schemars(extend("examples" = ["https://example.com/not-for-you"]))]
  pub denied: Option<String>,
  /// IP family used to dial the tunnel server: `auto` (default, tries both),
  /// `ipv4`, or `ipv6`. Set `ipv4` when the server hostname resolves to an
  /// IPv6 address the host cannot reach (env: APERIO_IP_FAMILY).
  #[schemars(extend("examples" = ["auto", "ipv4"]))]
  pub ip_family: Option<String>,
  /// Fixed instance UUID kept across restarts, so failover and `--bind-tunnels`
  /// can recognize this client; a random one is used when unset.
  #[schemars(extend("examples" = ["3f2504e0-4f89-41d3-9a0c-0305e82c3301"]))]
  pub client_id: Option<String>,
  /// Request/response header add-remove rules applied by this client to
  /// proxied HTTP traffic (services may override with their own `headers`).
  pub headers: Option<HeaderRules>,
  /// Security response-header preset: `true` injects HSTS,
  /// `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and
  /// `Referrer-Policy`; a mapping picks headers individually (services may
  /// override with their own `security_headers`).
  pub security_headers: Option<SecurityHeaders>,
  /// Private local services a peer client may reach via `--bind-tunnels`; never
  /// exposed to the public web.
  pub tunnels: Option<Vec<TunnelDecl>>,
  /// Peer clients whose declared tunnels this process binds to local ports,
  /// keyed by the peer's client id.
  #[serde(rename = "bind-tunnels", alias = "bind_tunnels")]
  pub bind_tunnels: Option<HashMap<String, BindTunnelEntry>>,
  /// Autoscaling: the endpoint the server calls when this client's services
  /// need capacity. Applies to every service this client exposes; each
  /// hostname bind gets its own record on the server.
  pub scaling: Option<ScalingDecl>,
  /// Shut this client down after it has served no request for this long, e.g.
  /// `5m` (unset = never). The scale-in half of `scaling`: the server never
  /// stops anything, an idle client retires itself. The shutdown is graceful
  /// (the server stops routing to it first, in-flight requests finish), and
  /// the timer only starts once the client has served its first request, so a
  /// slow cold start cannot make it exit before it is ever used.
  #[schemars(extend("examples" = ["5m"]))]
  pub idle_timeout: Option<String>,
}

impl FileConfig {
  /// Resolves the server URL from either the nested section or the flat form.
  pub fn server_url(&self) -> Option<String> {
    match &self.server {
      Some(ServerValue::Url(s)) => Some(s.clone()),
      Some(ServerValue::Section { url, .. }) => url.clone(),
      None => None,
    }
  }

  /// Resolves the server token, preferring the nested `server.token` and
  /// falling back to the legacy flat `token:` key.
  pub fn server_token(&self) -> Option<String> {
    match &self.server {
      Some(ServerValue::Section { token: Some(t), .. }) => Some(t.clone()),
      _ => self.token.clone(),
    }
  }

  /// Resolves the admin API key used by the `aperio-client api` commands.
  pub fn server_api_key(&self) -> Option<String> {
    match &self.server {
      Some(ServerValue::Section { api_key, .. }) => api_key.clone(),
      _ => None,
    }
  }
}

/// Renders the `aperio.yaml` JSON Schema as pretty-printed JSON. Used by the
/// aperio-client build script and the release workflow.
pub fn schema_json() -> String {
  let schema = schemars::schema_for!(FileConfig);
  serde_json::to_string_pretty(&schema).unwrap_or_default()
}

/// One `expose:` entry of `aperio-server.yaml` (experimental public TCP port).
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ExposeEntry {
  /// Transport of the exposed port; only `tcp` is supported while experimental.
  #[serde(default = "default_tcp")]
  #[schemars(extend("examples" = ["tcp"]))]
  pub protocol: String,
  /// Public port the server listens on.
  #[schemars(extend("examples" = [2222]))]
  pub port: u16,
  /// Shared secret a client's tunnel declaration must present (`expose: <key>`).
  #[schemars(extend("examples" = ["k5fj2q-expose-secret"]))]
  pub key: String,
}

/// The fixed response of a client-less `respond` route.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RespondRule {
  /// HTTP status to answer with (default 200).
  #[schemars(extend("examples" = [503]))]
  pub status: Option<u16>,
  /// `Content-Type` of the response body.
  #[schemars(extend("examples" = ["text/html; charset=utf-8"]))]
  pub content_type: Option<String>,
  /// Response body.
  #[schemars(extend("examples" = ["<h1>Coming soon</h1>"]))]
  pub body: Option<String>,
}

/// One `routes:` entry of `aperio-server.yaml`: a hostname/path match paired
/// with exactly one action (`redirect` or `respond`), served without a client.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RouteRule {
  /// Hostname to match exactly (unset = any hostname).
  #[schemars(extend("examples" = ["old.example.com"]))]
  pub hostname: Option<String>,
  /// Path prefix to match, with bind semantics (unset = any path).
  #[schemars(extend("examples" = ["/robots.txt"]))]
  pub path: Option<String>,
  /// Redirect target; answers 302 (or 301 with `permanent: true`).
  #[schemars(extend("examples" = ["https://new.example.com"]))]
  pub redirect: Option<String>,
  /// Use a permanent 301 instead of the default 302.
  #[serde(default)]
  pub permanent: bool,
  /// Append the request's path and query to the redirect target.
  #[serde(default)]
  pub preserve_path: bool,
  /// Serve a fixed response instead of redirecting.
  pub respond: Option<RespondRule>,
}

/// One `error_pages:` entry of `aperio-server.yaml`: per-hostname custom
/// 504/503 pages overriding the global `504_page`/`503_page` for that host.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ErrorPageRule {
  /// Hostname the pages apply to (matched exactly, case-insensitive).
  #[schemars(extend("examples" = ["app.example.com"]))]
  pub hostname: String,
  /// Path of the HTML file served on 504 gateway-timeout responses.
  #[serde(rename = "504_page")]
  #[schemars(extend("examples" = ["./pages/app-504.html"]))]
  pub page_504: Option<String>,
  /// Path of the HTML file served on 503 maintenance responses.
  #[serde(rename = "503_page")]
  #[schemars(extend("examples" = ["./pages/app-503.html"]))]
  pub page_503: Option<String>,
}

/// One deprecated flat key found in a config file, and the nested key that
/// replaces it. Reported by the client at load time so an operator can move a
/// file over without reading the changelog.
pub struct DeprecatedKey {
  /// The flat key as written, e.g. `health_interval`.
  pub old: &'static str,
  /// Where it lives now, e.g. `health.interval`.
  pub new: &'static str,
}

/// Folds a nested `health:` block into the flat fields the client resolver
/// already reads, so both spellings are supported by one code path. A value
/// set in the block wins over the flat key of the same meaning: the block is
/// the current form, so the more specific answer to "which did the operator
/// mean" is the one they wrote in the new place.
///
/// Returns the deprecated flat keys that were in use, for the caller to warn
/// about.
macro_rules! fold_health {
  ($self:ident) => {{
    let mut deprecated: Vec<DeprecatedKey> = Vec::new();
    for (present, old, new) in [
      (
        $self.target_health.is_some(),
        "target_health",
        "health.endpoint",
      ),
      (
        $self.wait_for_backend.is_some(),
        "wait_for_backend",
        "health.wait_for_backend",
      ),
      (
        $self.health_interval.is_some(),
        "health_interval",
        "health.interval",
      ),
      (
        $self.health_timeout.is_some(),
        "health_timeout",
        "health.timeout",
      ),
      (
        $self.health_threshold.is_some(),
        "health_threshold",
        "health.threshold",
      ),
    ] {
      if present {
        deprecated.push(DeprecatedKey { old, new });
      }
    }
    if let Some(health) = $self.health.take() {
      if let Some(v) = health.endpoint {
        $self.target_health = Some(v);
      }
      if let Some(v) = health.wait_for_backend {
        $self.wait_for_backend = Some(v);
      }
      if let Some(v) = health.interval {
        $self.health_interval = Some(v);
      }
      if let Some(v) = health.timeout {
        $self.health_timeout = Some(v);
      }
      if let Some(v) = health.threshold {
        $self.health_threshold = Some(v);
      }
    }
    deprecated
  }};
}

impl ServiceEntry {
  /// See [`FileConfig::fold_groups`]; this is the per-service half.
  pub fn fold_groups(&mut self) -> Vec<DeprecatedKey> {
    fold_health!(self)
  }
}

impl FileConfig {
  /// Rewrites every grouped block into the flat fields the resolver reads,
  /// top level and per `services:` entry, and reports the deprecated flat
  /// keys the file still uses. Call once per parse, before resolving.
  pub fn fold_groups(&mut self) -> Vec<DeprecatedKey> {
    let mut deprecated = fold_health!(self);
    for entry in self.services.iter_mut().flat_map(|s| s.iter_mut()) {
      deprecated.extend(entry.fold_groups());
    }
    deprecated
  }
}

/// Alerting thresholds: when the server logs and emits an alert.
///
/// Written as a `alert:` block; the flat `alert_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlertGroup {
  /// Error-rate alert threshold, 0..1.
  #[schemars(extend("examples" = [0.25]))]
  pub error_rate: Option<f64>,
  /// Alert sliding-window seconds.
  pub window: Option<u64>,
  /// Minimum requests in the window before the error-rate alert fires.
  pub min_requests: Option<u64>,
  /// Connected-client floor below which the client-down alert fires.
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
  pub max_size: Option<u64>,
  /// Rotated audit log files kept.
  pub max_files: Option<u64>,
}

/// The server-side GET response cache.
///
/// Written as a `cache:` block; the flat `cache_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheGroup {
  /// Enable the server-side GET response cache.
  pub enabled: Option<bool>,
  /// Response-cache budget in bytes.
  pub max_bytes: Option<u64>,
  /// Serve-stale window in seconds for resilient services.
  pub max_stale: Option<u64>,
}

/// The built-in dashboard.
///
/// Written as a `dashboard:` block; the flat `dashboard_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DashboardGroup {
  /// Serve the admin dashboard.
  pub enabled: Option<bool>,
  /// Dashboard password.
  pub auth: Option<String>,
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
  /// Also publish hostnames a token permits but no client serves yet
  ///.
  pub include_offline: Option<bool>,
}

/// In-flight failover: what happens to a request whose client disappears mid-flight.
///
/// Written as a `failover:` block; the flat `failover_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FailoverGroup {
  /// In-flight failover mode.
  #[schemars(extend("examples" = ["fail", "retry", "wait", "retry-wait"]))]
  pub mode: Option<String>,
  /// Maximum failover re-dispatches per request.
  pub max_jumps: Option<u32>,
  /// Failover window in seconds.
  pub window: Option<u64>,
  /// Allow failover for non-idempotent methods too.
  pub all_methods: Option<bool>,
}

/// Gateway timeouts applied to a proxied request.
///
/// Written as a `gateway:` block; the flat `gateway_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayGroup {
  /// Seconds to wait for a client connection.
  pub timeout: Option<u64>,
  /// Seconds to wait for a client response.
  pub response_timeout: Option<u64>,
}

/// Per-visitor-IP rate limiting (token bucket).
///
/// Written as a `ip_limit:` block; the flat `ip_limit_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IpLimitGroup {
  /// Per-IP rate-limit burst.
  pub max: Option<u64>,
  /// Per-IP rate-limit refill per second.
  pub refill: Option<f64>,
}

/// Dashboard login lockout after repeated failures.
///
/// Written as a `login_lockout:` block; the flat `login_lockout_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginLockoutGroup {
  /// Failed logins per IP before a lockout.
  pub threshold: Option<u32>,
  /// Base lockout seconds, doubled per repeat.
  pub secs: Option<u64>,
}

/// The Prometheus metrics endpoint.
///
/// Written as a `metrics:` block; the flat `metrics_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricsGroup {
  /// Prometheus metrics endpoint toggle.
  pub enabled: Option<bool>,
  /// Bearer token gating the metrics endpoint.
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
  pub issuer: Option<String>,
  /// OIDC client id.
  pub client_id: Option<String>,
  /// Allowed OIDC login emails.
  pub allowed_emails: Option<Vec<String>>,
  /// OIDC scopes.
  pub scopes: Option<Vec<String>>,
  /// OIDC redirect URL override.
  pub redirect_url: Option<String>,
}

/// OpenTelemetry trace export.
///
/// Written as a `otel:` block; the flat `otel_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OtelGroup {
  /// Enable OpenTelemetry OTLP export.
  pub enabled: Option<bool>,
  /// OTLP endpoint.
  #[schemars(extend("examples" = ["http://localhost:4317"]))]
  pub endpoint: Option<String>,
  /// OTLP service name.
  pub service_name: Option<String>,
}

/// How long each kind of recorded data is kept, in days.
///
/// Written as a `retention:` block; the flat `retention_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetentionGroup {
  /// Days to keep inspector captures and webhook inbox entries; 0/unset = forever.
  pub captures: Option<u64>,
  /// Days to keep access-log file lines; 0/unset = forever.
  pub access_log: Option<u64>,
  /// Days to keep audit events; 0/unset = forever.
  pub audit: Option<u64>,
  /// Days to keep day-granularity stats buckets; 0/unset = the built-in caps.
  pub stats: Option<u64>,
}

/// Autoscaling: the server signalling desired capacity to an endpoint you control.
///
/// Written as a `scaling:` block; the flat `scaling_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScalingGroup {
  /// Honor client `scaling:` declarations.
  pub enabled: Option<bool>,
  /// Allow a plain-http autoscaling endpoint.
  pub allow_http: Option<bool>,
  /// Seconds after which an unrefreshed autoscaling record is dropped
  ///.
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
  pub token: Option<String>,
  /// Visitor auth `user:password` gate.
  #[schemars(extend("examples" = ["admin:s3cret"]))]
  pub auth: Option<String>,
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
  pub allowlist: Option<Vec<String>>,
  /// With no allowlist: refuse destinations resolving to internal addresses
  /// (loopback, RFC 1918, link-local/metadata, CGNAT, unique-local).
  pub block_private: Option<bool>,
}

/// Per-stream flow control for streamed data (responses, WebSocket, TCP).
///
/// Written as a `stream:` block; the flat `stream_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamGroup {
  /// Backlog bytes at which the producing client is asked to pause a stream.
  pub pause_bytes: Option<u64>,
  /// Backlog bytes under which a paused producer is asked to resume.
  pub resume_bytes: Option<u64>,
  /// Hard per-stream backlog cap in bytes (drops producers that cannot pause).
  pub backlog_limit: Option<u64>,
}

/// `cache:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum CacheSetting {
  /// `true` turns the cache on with the defaults.
  Enabled(bool),
  /// The full block.
  Group(CacheGroup),
}

/// `dashboard:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum DashboardSetting {
  /// `false` serves no dashboard at all.
  Enabled(bool),
  /// The full block.
  Group(DashboardGroup),
}

/// `metrics:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum MetricsSetting {
  /// `true` exposes the Prometheus endpoint.
  Enabled(bool),
  /// The full block.
  Group(MetricsGroup),
}

/// `otel:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum OtelSetting {
  /// `true` exports traces with the defaults.
  Enabled(bool),
  /// The full block.
  Group(OtelGroup),
}

/// `scaling:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum ScalingSetting {
  /// `true` honors the clients' scaling declarations.
  Enabled(bool),
  /// The full block.
  Group(ScalingGroup),
}

/// `failover:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum FailoverSetting {
  /// The mode alone, e.g. `retry`.
  Mode(String),
  /// The full block.
  Group(FailoverGroup),
}

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
    key: "audit",
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
  pub alert: Option<AlertGroup>,
  /// Audit-log file rotation
  #[serde(default)]
  pub audit: Option<AuditGroup>,
  /// The server-side GET response cache
  #[serde(default)]
  pub cache: Option<CacheSetting>,
  /// The built-in dashboard
  #[serde(default)]
  pub dashboard: Option<DashboardSetting>,
  /// Edge-proxy integration: publishing the served hostnames to a dynamic reverse proxy in front of this server
  #[serde(default)]
  pub edge: Option<EdgeGroup>,
  /// In-flight failover: what happens to a request whose client disappears mid-flight
  #[serde(default)]
  pub failover: Option<FailoverSetting>,
  /// Gateway timeouts applied to a proxied request
  #[serde(default)]
  pub gateway: Option<GatewayGroup>,
  /// Per-visitor-IP rate limiting (token bucket)
  #[serde(default)]
  pub ip_limit: Option<IpLimitGroup>,
  /// Dashboard login lockout after repeated failures
  #[serde(default)]
  pub login_lockout: Option<LoginLockoutGroup>,
  /// The Prometheus metrics endpoint
  #[serde(default)]
  pub metrics: Option<MetricsSetting>,
  /// OIDC single sign-on for the dashboard
  #[serde(default)]
  pub oidc: Option<OidcGroup>,
  /// OpenTelemetry trace export
  #[serde(default)]
  pub otel: Option<OtelSetting>,
  /// Where the server may send outbound callbacks (webhooks, autoscaling hooks)
  #[serde(default)]
  pub outbound: Option<OutboundGroup>,
  /// How long each kind of recorded data is kept, in days
  #[serde(default)]
  pub retention: Option<RetentionGroup>,
  /// Autoscaling: the server signalling desired capacity to an endpoint you control
  #[serde(default)]
  pub scaling: Option<ScalingSetting>,
  /// The server's own credentials
  #[serde(default)]
  pub server: Option<ServerCredentials>,
  /// Per-stream flow control for streamed data (responses, WebSocket, TCP)
  #[serde(default)]
  pub stream: Option<StreamGroup>,
  // --- Core ---
  /// Deprecated spelling of `server.token` (env: APERIO_SERVER_TOKEN).
  pub server_token: Option<String>,
  /// Address to bind (bare env: HOST).
  #[schemars(extend("examples" = ["0.0.0.0"]))]
  pub host: Option<String>,
  /// Port to listen on (bare env: PORT).
  #[schemars(extend("examples" = [8080]))]
  pub port: Option<u16>,
  /// Directory for the SQLite store and logs (env: APERIO_DATA_DIR).
  #[schemars(extend("examples" = ["/app/data"]))]
  pub data_dir: Option<String>,
  /// Log level (bare env: LOG_LEVEL).
  #[schemars(extend("examples" = ["info", "debug"]))]
  pub log_level: Option<String>,

  // --- Routing & load balancing ---
  /// Require every client to carry a hostname bind (env: APERIO_REQUIRE_HOSTNAME_BIND).
  pub require_hostname_bind: Option<bool>,
  /// Wildcard pattern granting each client a random subdomain (env: APERIO_RANDOM_SUBDOMAIN).
  #[schemars(extend("examples" = ["*.example.com"]))]
  pub random_subdomain: Option<String>,
  /// Inject noindex headers for random-subdomain preview services (env: APERIO_PREVIEW_NOINDEX).
  pub preview_noindex: Option<bool>,
  /// Seconds without a heartbeat before a client is considered down (env: APERIO_CLIENT_DOWN_THRESHOLD).
  pub client_down_threshold: Option<u64>,
  /// Load-balancing strategy (env: APERIO_LB_STRATEGY).
  #[schemars(extend("examples" = ["round-robin", "primary-standby", "sticky"]))]
  pub lb_strategy: Option<String>,

  // --- Failover ---
  /// Deprecated spelling of `failover.max_jumps` (env: APERIO_FAILOVER_MAX_JUMPS).
  pub failover_max_jumps: Option<u32>,
  /// Deprecated spelling of `failover.window` (env: APERIO_FAILOVER_WINDOW).
  pub failover_window: Option<u64>,
  /// Deprecated spelling of `failover.all_methods` (env: APERIO_FAILOVER_ALL_METHODS).
  pub failover_all_methods: Option<bool>,

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
  pub max_body_size: Option<u64>,
  /// Concurrent proxied requests limit (env: APERIO_MAX_CONCURRENT_REQUESTS).
  pub max_concurrent_requests: Option<u64>,
  /// Maximum simultaneously connected clients (env: APERIO_MAX_TUNNELS).
  pub max_tunnels: Option<u64>,
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
  pub trust_proxy: Option<bool>,
  /// Trusted proxy IPs/CIDRs (env: APERIO_TRUSTED_PROXIES).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub trusted_proxies: Option<Vec<String>>,
  /// Header carrying the real client IP (env: APERIO_REAL_IP_HEADER).
  #[schemars(extend("examples" = ["CF-Connecting-IP"]))]
  pub real_ip_header: Option<String>,
  /// Trust the Cloudflare client-IP header (env: APERIO_TRUST_CF_HEADER).
  pub trust_cf_header: Option<bool>,
  /// Mark session cookies `Secure` (env: APERIO_SECURE_COOKIES).
  pub secure_cookies: Option<bool>,

  // --- Tunnel & cache ---
  /// zlib-compress tunnel frames (env: APERIO_TUNNEL_COMPRESSION).
  pub tunnel_compression: Option<bool>,
  /// Deprecated spelling of `cache.max_bytes` (env: APERIO_CACHE_MAX_BYTES).
  pub cache_max_bytes: Option<u64>,
  /// Deprecated spelling of `cache.max_stale` (env: APERIO_CACHE_MAX_STALE).
  pub cache_max_stale: Option<u64>,
  /// Flat spelling of `outbound.allowlist` (env: APERIO_OUTBOUND_ALLOWLIST).
  pub outbound_allowlist: Option<Vec<String>>,
  /// Flat spelling of `outbound.block_private` (env: APERIO_OUTBOUND_BLOCK_PRIVATE).
  pub outbound_block_private: Option<bool>,
  /// Flat spelling of `stream.pause_bytes` (env: APERIO_STREAM_PAUSE_BYTES).
  pub stream_pause_bytes: Option<u64>,
  /// Flat spelling of `stream.resume_bytes` (env: APERIO_STREAM_RESUME_BYTES).
  pub stream_resume_bytes: Option<u64>,
  /// Flat spelling of `stream.backlog_limit` (env: APERIO_STREAM_BACKLOG_LIMIT).
  pub stream_backlog_limit: Option<u64>,

  // --- Pages ---
  /// Custom 504 error page path (env: APERIO_504_PAGE).
  #[serde(rename = "504_page")]
  pub error_page_504: Option<String>,
  /// Custom 503 maintenance page path (env: APERIO_503_PAGE).
  #[serde(rename = "503_page")]
  pub error_page_503: Option<String>,

  // --- Logging & telemetry ---
  /// Structured access log path (env: APERIO_ACCESS_LOG).
  pub access_log: Option<String>,
  /// Deprecated spelling of `retention.captures` (env: APERIO_RETENTION_CAPTURES).
  pub retention_captures: Option<u64>,
  /// Deprecated spelling of `retention.access_log` (env: APERIO_RETENTION_ACCESS_LOG).
  pub retention_access_log: Option<u64>,
  /// Deprecated spelling of `retention.audit` (env: APERIO_RETENTION_AUDIT).
  pub retention_audit: Option<u64>,
  /// Deprecated spelling of `retention.stats` (env: APERIO_RETENTION_STATS).
  pub retention_stats: Option<u64>,
  /// Cap on aperio.db (+WAL/SHM) in bytes; nearing it emits a warning, exceeding it auto-prunes low-priority data (env: APERIO_DB_MAX_BYTES).
  pub db_max_bytes: Option<u64>,
  /// Deprecated spelling of `audit.max_size` (env: APERIO_AUDIT_MAX_SIZE).
  pub audit_max_size: Option<u64>,
  /// Deprecated spelling of `audit.max_files` (env: APERIO_AUDIT_MAX_FILES).
  pub audit_max_files: Option<u64>,
  /// Deprecated spelling of `otel.endpoint` (env: APERIO_OTEL_ENDPOINT).
  pub otel_endpoint: Option<String>,
  /// Deprecated spelling of `otel.service_name` (env: APERIO_OTEL_SERVICE_NAME).
  pub otel_service_name: Option<String>,
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
  pub server_auth: Option<String>,
  /// Public dashboard URL enabling passkeys; its domain is the RP ID (env: APERIO_WEBAUTHN_ORIGIN).
  #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
  pub webauthn_origin: Option<String>,
  /// Ignore client-declared visitor passwords (env: APERIO_IGNORE_CLIENT_AUTH).
  pub ignore_client_auth: Option<bool>,
  /// Default dashboard/login UI language (env: APERIO_UI_LANGUAGE).
  #[schemars(extend("examples" = ["en", "tr"]))]
  pub ui_language: Option<String>,
  /// Deprecated spelling of `dashboard.auth` (env: APERIO_DASHBOARD_AUTH).
  pub dashboard_auth: Option<String>,
  /// Days before a token's expiry to start warning (env: APERIO_TOKEN_EXPIRY_WARNING).
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

  // --- Structured sections (read directly, not env-mapped) ---
  /// Server-wide request/response header rewrite rules applied to all traffic.
  pub headers: Option<HeaderRules>,
  /// Client-less routes: bind a hostname/path to a redirect or fixed response.
  pub routes: Option<Vec<RouteRule>>,
  /// Per-hostname custom 504/503 error pages (override the global
  /// `504_page`/`503_page` for that hostname).
  pub error_pages: Option<Vec<ErrorPageRule>>,
  /// Experimental public TCP expose ports.
  pub expose: Option<Vec<ExposeEntry>>,
}

/// Renders a bytes/second rate back into the shorthand `bandwidth:` accepts,
/// so a value the client resolved (a budget share, say) can be shown the way
/// an operator would have written it. Falls back to plain bytes/second when
/// the rate is not a round number of bits.
pub fn format_bandwidth(bps: u64) -> String {
  let bits = bps.saturating_mul(8);
  for (unit, scale) in [
    ("gbit", 1_000_000_000u64),
    ("mbit", 1_000_000),
    ("kbit", 1_000),
  ] {
    if bits >= scale && bits.is_multiple_of(scale) {
      return format!("{}{}", bits / scale, unit);
    }
  }
  format!("{} bytes/s", bps)
}

/// The `aperio-server.yaml` JSON Schema as pretty JSON.
pub fn server_schema_json() -> String {
  let schema = schemars::schema_for!(ServerFileConfig);
  serde_json::to_string_pretty(&schema).unwrap_or_default()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
