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
  /// `h2://` targets are dialed over HTTP/2 (gRPC backends, trailers relayed).
  #[schemars(extend("examples" = ["http://localhost:3000", "3000", "h2c://127.0.0.1:50051"]))]
  pub target: Option<String>,
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
  /// How many backend redirects to follow transparently before passing one through.
  #[schemars(extend("examples" = [5]))]
  pub max_redirects: Option<usize>,
  /// Raw TCP backend for this service instead of HTTP (experimental).
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Backend health endpoint the client probes to pull itself from rotation when down.
  #[schemars(extend("examples" = ["/health"]))]
  pub target_health: Option<String>,
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
  /// Let the server cache this service's GET responses (per their
  /// `Cache-Control`); effective only when the server enables APERIO_CACHE.
  pub cache: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504 (needs `cache: true` and the server-side cache enabled).
  pub resilience: Option<bool>,
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
  /// Fixed instance UUID kept across restarts, so failover and `--bind-tunnels`
  /// can recognize this client; a random one is used when unset.
  #[schemars(extend("examples" = ["3f2504e0-4f89-41d3-9a0c-0305e82c3301"]))]
  pub client_id: Option<String>,
  /// Request/response header add-remove rules applied by this client to
  /// proxied HTTP traffic (services may override with their own `headers`).
  pub headers: Option<HeaderRules>,
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
