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
  /// the server load-balances across them like separate clients.
  #[schemars(extend("examples" = [2]))]
  pub connections: Option<u32>,
  /// Failover tier for this service (0 = primary, higher numbers are standbys).
  #[schemars(extend("examples" = [0]))]
  pub priority: Option<u32>,
  /// Caps how fast the server streams responses so a slow uplink isn't
  /// overwhelmed; bit suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte suffixes
  /// (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000.
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
  /// How many backend redirects to follow transparently before passing one through.
  #[schemars(extend("examples" = [5]))]
  pub max_redirects: Option<usize>,
  /// Raw TCP backend for this service instead of HTTP (experimental).
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Backend health endpoint the client probes to pull itself from rotation when down.
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
  /// default 1); the server load-balances across them like separate clients.
  #[schemars(extend("examples" = [2]))]
  pub connections: Option<u32>,
  /// Largest response body, in bytes, the client will relay to a visitor.
  #[schemars(extend("examples" = [10485760]))]
  pub max_response_body: Option<usize>,
  /// Largest request body, in bytes, visitors may upload to this service;
  /// the server rejects bigger uploads with 413 before they enter the tunnel.
  #[schemars(extend("examples" = [1048576]))]
  pub max_request_body: Option<u64>,
  /// Seconds to wait for the backend to respond before failing a request.
  #[schemars(extend("examples" = [30]))]
  pub timeout: Option<u64>,
  /// Largest single tunnel frame, in bytes, the client will accept.
  #[schemars(extend("examples" = [33554432]))]
  pub max_message_size: Option<usize>,
  /// Raw TCP backend to expose instead of HTTP (experimental).
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Backend health endpoint to probe; a failing backend leaves rotation without dropping the tunnel.
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
  /// Caps how fast the server streams responses so a slow uplink isn't
  /// overwhelmed; bit suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte suffixes
  /// (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000.
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

/// The `aperio-server.yaml` configuration file. The server is environment-
/// driven; every scalar key here is materialized into its `APERIO_*`
/// environment variable at startup (the file takes precedence over the
/// environment). Structured sections (`headers`, `routes`, `expose`) are read
/// directly. Unknown keys are allowed and passed through as env vars.
#[derive(Deserialize, Default, JsonSchema)]
pub struct ServerFileConfig {
  // --- Core ---
  /// Master token; also the fallback dashboard password (env: APERIO_SERVER_TOKEN).
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
  /// In-flight failover mode (env: APERIO_FAILOVER).
  #[schemars(extend("examples" = ["off", "retry", "retry-wait"]))]
  pub failover: Option<String>,
  /// Maximum failover re-dispatches per request (env: APERIO_FAILOVER_MAX_JUMPS).
  pub failover_max_jumps: Option<u32>,
  /// Failover window in seconds (env: APERIO_FAILOVER_WINDOW).
  pub failover_window: Option<u64>,
  /// Allow failover for non-idempotent methods too (env: APERIO_FAILOVER_ALL_METHODS).
  pub failover_all_methods: Option<bool>,

  // --- Alerting ---
  /// Error-rate alert threshold, 0..1 (env: APERIO_ALERT_ERROR_RATE).
  #[schemars(extend("examples" = [0.25]))]
  pub alert_error_rate: Option<f64>,
  /// Alert sliding-window seconds (env: APERIO_ALERT_WINDOW).
  pub alert_window: Option<u64>,
  /// Minimum requests in the window before the error-rate alert fires (env: APERIO_ALERT_MIN_REQUESTS).
  pub alert_min_requests: Option<u64>,
  /// Connected-client floor below which the client-down alert fires (env: APERIO_ALERT_CLIENT_DOWN).
  pub alert_client_down: Option<u64>,

  // --- Capacity & limits ---
  /// Largest request body in bytes (env: APERIO_MAX_BODY_SIZE).
  pub max_body_size: Option<u64>,
  /// Concurrent proxied requests limit (env: APERIO_MAX_CONCURRENT_REQUESTS).
  pub max_concurrent_requests: Option<u64>,
  /// Maximum simultaneously connected clients (env: APERIO_MAX_TUNNELS).
  pub max_tunnels: Option<u64>,
  /// Per-IP rate-limit burst (env: APERIO_IP_LIMIT_MAX).
  pub ip_limit_max: Option<u64>,
  /// Per-IP rate-limit refill per second (env: APERIO_IP_LIMIT_REFILL).
  pub ip_limit_refill: Option<f64>,
  /// Failed logins per IP before a lockout (env: APERIO_LOGIN_LOCKOUT_THRESHOLD).
  pub login_lockout_threshold: Option<u32>,
  /// Base lockout seconds, doubled per repeat (env: APERIO_LOGIN_LOCKOUT_SECS).
  pub login_lockout_secs: Option<u64>,
  /// Seconds to wait for a client connection (env: APERIO_SERVER_GATEWAY_TIMEOUT).
  pub server_gateway_timeout: Option<u64>,
  /// Seconds to wait for a client response (env: APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT).
  pub server_gateway_response_timeout: Option<u64>,

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
  /// Enable the server-side GET response cache (env: APERIO_CACHE).
  pub cache: Option<bool>,
  /// Response-cache budget in bytes (env: APERIO_CACHE_MAX_BYTES).
  pub cache_max_bytes: Option<u64>,
  /// Serve-stale window in seconds for resilient services (env: APERIO_CACHE_MAX_STALE).
  pub cache_max_stale: Option<u64>,

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
  /// Audit log rotation size in bytes, 0 disables (env: APERIO_AUDIT_MAX_SIZE).
  pub audit_max_size: Option<u64>,
  /// Rotated audit log files kept (env: APERIO_AUDIT_MAX_FILES).
  pub audit_max_files: Option<u64>,
  /// Enable OpenTelemetry OTLP export (env: APERIO_OTEL).
  pub otel: Option<bool>,
  /// OTLP endpoint (env: APERIO_OTEL_ENDPOINT).
  #[schemars(extend("examples" = ["http://localhost:4317"]))]
  pub otel_endpoint: Option<String>,
  /// OTLP service name (env: APERIO_OTEL_SERVICE_NAME).
  pub otel_service_name: Option<String>,
  /// Prometheus metrics endpoint toggle (env: APERIO_METRICS).
  pub metrics: Option<bool>,
  /// Bearer token gating the metrics endpoint (env: APERIO_METRICS_TOKEN).
  pub metrics_token: Option<String>,

  // --- Auth, dashboard & SSO ---
  /// Visitor auth `user:password` gate (env: APERIO_SERVER_AUTH).
  #[schemars(extend("examples" = ["admin:s3cret"]))]
  pub server_auth: Option<String>,
  /// Public dashboard URL enabling passkeys; its domain is the RP ID (env: APERIO_WEBAUTHN_ORIGIN).
  #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
  pub webauthn_origin: Option<String>,
  /// Ignore client-declared visitor passwords (env: APERIO_IGNORE_CLIENT_AUTH).
  pub ignore_client_auth: Option<bool>,
  /// Serve the admin dashboard (env: APERIO_DASHBOARD).
  pub dashboard: Option<bool>,
  /// Default dashboard/login UI language (env: APERIO_UI_LANGUAGE).
  #[schemars(extend("examples" = ["en", "tr"]))]
  pub ui_language: Option<String>,
  /// Dashboard password (env: APERIO_DASHBOARD_AUTH).
  pub dashboard_auth: Option<String>,
  /// Days before a token's expiry to start warning (env: APERIO_TOKEN_EXPIRY_WARNING).
  pub token_expiry_warning: Option<u64>,
  /// OIDC issuer URL (env: APERIO_OIDC_ISSUER).
  pub oidc_issuer: Option<String>,
  /// OIDC client id (env: APERIO_OIDC_CLIENT_ID).
  pub oidc_client_id: Option<String>,
  /// Allowed OIDC login emails (env: APERIO_OIDC_ALLOWED_EMAILS).
  pub oidc_allowed_emails: Option<Vec<String>>,
  /// OIDC scopes (env: APERIO_OIDC_SCOPES).
  pub oidc_scopes: Option<Vec<String>>,
  /// OIDC redirect URL override (env: APERIO_OIDC_REDIRECT_URL).
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

/// The `aperio-server.yaml` JSON Schema as pretty JSON.
pub fn server_schema_json() -> String {
  let schema = schemars::schema_for!(ServerFileConfig);
  serde_json::to_string_pretty(&schema).unwrap_or_default()
}
