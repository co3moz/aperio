//! The `aperio.yaml` client configuration schema.
//!
//! These are the exact types `aperio-client` deserializes its config file into.
//! They live in their own crate so the client's build script can emit a JSON
//! Schema (`schemars`) straight from them — the editor schema and the parser can
//! never drift apart. Doc comments on the fields below become `description`s in
//! the generated schema, so they double as the reference for `aperio.yaml`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serde default protocol of a declared tunnel.
fn default_tcp() -> String {
  "tcp".to_string()
}

/// One tunnel declared by a client (`tunnels:` list in aperio.yaml): a normally
/// unexposed local service that a peer client may reach through the server with
/// `--bind-tunnels` — same token, explicit client id. Also the wire form sent
/// in the tunnel `Ping`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct TunnelDecl {
  /// Local address the declaring client connects to, e.g. `127.0.0.1:27017`.
  pub target: String,
  /// Transport protocol; only `tcp` is currently supported.
  #[serde(default = "default_tcp")]
  pub protocol: String,
}

/// The `server:` key accepts both the legacy plain URL string and the canonical
/// nested section.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ServerValue {
  /// `server: https://tunnel.example.com` (legacy flat form)
  Url(String),
  /// `server: { url: ..., token: ... }`
  Section {
    url: Option<String>,
    token: Option<String>,
  },
}

/// One entry of the `services:` list — a single exposed target with its own
/// binds and knobs. Unset fields fall back to the top-level / layered value
/// (so shared settings like health cadence live once at the top).
#[derive(Deserialize, Default, Clone, JsonSchema)]
pub struct ServiceEntry {
  /// Display name used in logs and the dashboard.
  pub name: Option<String>,
  /// Local backend to expose. Required per entry.
  pub target: Option<String>,
  /// Hostname bind for this service, e.g. `app.example.com`.
  pub hostname: Option<String>,
  /// Path bind for this service, e.g. `/api`.
  pub path: Option<String>,
  /// Strip the path bind before forwarding to the backend.
  pub trim_bind: Option<bool>,
  /// Forward the original `Host` header instead of the target's.
  pub pass_hostname: Option<bool>,
  /// Local max concurrent requests for this service.
  pub max_concurrent: Option<u32>,
  /// Load-balancing priority tier (0 = primary, higher = standby).
  pub priority: Option<u32>,
  /// Link capacity of this client's network, e.g. `8mbit`, `500kbit`, `2MB`.
  pub bandwidth: Option<String>,
  /// Per-request timeout in seconds.
  pub timeout: Option<u64>,
  /// Cap on response bodies read from the backend, in bytes.
  pub max_response_body: Option<usize>,
  /// Max backend redirects to follow transparently (0 passes them through).
  pub max_redirects: Option<usize>,
  /// Raw TCP target for this service (experimental).
  pub tcp_target: Option<String>,
  /// Health endpoint of the local target (path like `/health` or full URL).
  pub target_health: Option<String>,
  /// Seconds between backend health probes.
  pub health_interval: Option<u64>,
  /// Per-probe timeout in seconds.
  pub health_timeout: Option<u64>,
  /// Consecutive probe failures before the backend is reported unhealthy.
  pub health_threshold: Option<u32>,
  /// Declare this service public (skip the server's visitor auth gate).
  pub public: Option<bool>,
  /// Per-service visitor login as `user:password`: the server gates this
  /// service behind a login with these credentials, overriding its own
  /// visitor password for it (needs the same token permission as `public`).
  pub auth: Option<String>,
}

/// One `bind-tunnels:` entry: how to reach (and locally map) the tunnels of one
/// peer client.
#[derive(Deserialize, Default, Clone, JsonSchema)]
pub struct BindTunnelEntry {
  /// Token the peer client connected with (falls back to the layered server
  /// token when unset).
  pub token: Option<String>,
  /// Local port overrides: declared target → local port. Without an override
  /// the local listener uses the port of the declared target.
  #[serde(default, rename = "override")]
  pub overrides: HashMap<String, u16>,
}

/// Configuration file schema (`aperio.yaml` / `~/.aperio.yaml`). All keys are
/// optional.
///
/// ```yaml
/// server:
///   url: https://tunnel.example.com    # Aperio server URL
///   token: apr_xxxxxxxxxxxxxxxx        # tunnel token (master or dynamic)
/// target: http://localhost:3000        # local backend to expose
/// hostname: a.example.com              # optional hostname bind
/// path: /api                           # optional path bind
/// ```
#[derive(Deserialize, Default, JsonSchema)]
pub struct FileConfig {
  /// Aperio server URL and token (nested section or a bare URL string).
  pub server: Option<ServerValue>,
  /// Legacy flat `token:` key (canonical form is `server.token`).
  pub token: Option<String>,
  /// Single local backend to expose (alternative to a `services:` list).
  pub target: Option<String>,
  /// Hostname bind, e.g. `app.example.com`.
  pub hostname: Option<String>,
  /// Path bind, e.g. `/api`.
  pub path: Option<String>,
  /// Strip the path bind before forwarding to the backend.
  pub trim_bind: Option<bool>,
  /// Forward the original `Host` header instead of the target's.
  pub pass_hostname: Option<bool>,
  /// Local max concurrent requests.
  pub max_concurrent: Option<u32>,
  /// Cap on response bodies read from the backend, in bytes.
  pub max_response_body: Option<usize>,
  /// Per-request timeout in seconds.
  pub timeout: Option<u64>,
  /// Max tunnel message size in bytes.
  pub max_message_size: Option<usize>,
  /// Raw TCP target (experimental).
  pub tcp_target: Option<String>,
  /// Health endpoint of the local target (path like `/health` or full URL).
  pub target_health: Option<String>,
  /// Seconds between backend health probes.
  pub health_interval: Option<u64>,
  /// Per-probe timeout in seconds.
  pub health_timeout: Option<u64>,
  /// Consecutive probe failures before the backend is reported unhealthy.
  pub health_threshold: Option<u32>,
  /// Load-balancing priority tier (0 = primary, higher = standby).
  pub priority: Option<u32>,
  /// Link capacity of this client's network, e.g. `8mbit`, `500kbit`, `2MB`.
  pub bandwidth: Option<String>,
  /// Max backend redirects to follow transparently (same-host scheme upgrades
  /// and same-root-domain hops); 0 passes redirects through.
  pub max_redirects: Option<usize>,
  /// Multiple exposed targets (one tunnel connection each). When non-empty it
  /// replaces the single top-level `target`.
  pub services: Option<Vec<ServiceEntry>>,
  /// Declare the exposed service public: the server skips its visitor password
  /// / OIDC gate for traffic routed here (requires a token that permits
  /// publishing public services).
  pub public: Option<bool>,
  /// Per-service visitor login as `user:password`: the server gates traffic
  /// routed here behind a login with these credentials (top-level default for
  /// single-service mode; per-entry `auth` overrides it).
  pub auth: Option<String>,
  /// Persistent client instance id (a UUID). Defaults to a random UUID per run
  /// when unset.
  pub client_id: Option<String>,
  /// Tunnels declared by this client: normally unexposed local services a peer
  /// client may bind with `--bind-tunnels` (same token, this client's id).
  pub tunnels: Option<Vec<TunnelDecl>>,
  /// Peer clients whose declared tunnels this process binds locally when run
  /// with `--bind-tunnels`. Keys are peer client ids.
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
