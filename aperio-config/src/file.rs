//! The `aperio.yaml` document itself: every top-level key, and the function
//! that turns this crate into the editor's schema.
//!
//! The top level holds what belongs to the client as a whole: how to reach
//! the server, what to call this process, its tunnels, binds and message
//! faces. Everything that belongs to a service is written in `services:`,
//! keyed by the service's name, and what every service shares is written once
//! under `defaults:`. The shapes themselves live in [`crate::client`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::units::{ByteSize, Span};
use crate::*;

/// The Aperio client configuration file (`aperio.yaml` or `~/.aperio.yaml`).
///
/// Layers, lowest first: built-in defaults, `~/.aperio.yaml`, `./aperio.yaml`
/// (or `--config`), the environment, the command line. A later layer wins,
/// key by key. Every scalar key has an environment variable, and a key nothing
/// reads is an error that names the key it was probably meant to be.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
  /// The Aperio server to reach, the token to authenticate the tunnel with,
  /// and how to get there.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"url": "https://tunnel.example.com", "token": "${APERIO_SERVER_TOKEN}"}]))]
  pub server: Option<ServerSection>,
  /// What to call this client on screen and in the server's logs, e.g.
  /// `eu_server_1`. Lowercase letters, digits and `_`, like a service name.
  ///
  /// A label, not an address: `client_id` stays the identity that failover
  /// recognizes this process by, because that needs something unique and a
  /// name is not. Unset shows the id.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["eu_server_1"]))]
  pub name: Option<String>,
  /// This client's instance id, a UUID kept across restarts so failover can
  /// recognize the process when it comes back. Unset, one is generated on
  /// the first run and kept in `client_id_file`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["3f2504e0-4f89-41d3-9a0c-0305e82c3301"]))]
  pub client_id: Option<String>,
  /// Where a generated `client_id` is kept. Default: a file under
  /// `~/.aperio/clients/` named after the config file this client reads, so
  /// two projects on one machine never share an id and the file never lands
  /// in a repository. A client with no config file, or one that cannot
  /// write here, runs with a fresh id each time.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["/var/lib/aperio-client/myapp/client-id"]))]
  pub client_id_file: Option<String>,
  /// Other config files to read before this one, each path relative to the
  /// file that names it. Mappings merge key by key and anything else is
  /// replaced, the same rule as every other layer: a fragment can add a
  /// service, and this file wins over all of its includes. Later includes
  /// win over earlier ones. Chains may nest up to five deep; a cycle is an
  /// error.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["services/prod.yaml", "shared/health.yaml"]]))]
  pub include: Option<Vec<String>>,
  /// What every service gets unless its own entry says otherwise: the same
  /// keys a `services:` entry takes, apart from where its traffic comes from
  /// and goes to. The environment and the command line write here too, so
  /// `APERIO_TIMEOUT=60s` is `defaults.timeout`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"access": "public", "timeout": "60s", "health": {"interval": "5s"}}]))]
  pub defaults: Option<ServicePolicy>,
  /// The backends this client exposes, keyed by name. Each opens its own
  /// tunnel connection (or shares one, with `multiplex`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{
    "web": {"target": "http://localhost:3000", "hostname": "app.example.com"},
    "api": {"target": "http://localhost:4000", "hostname": "api.example.com", "timeout": "2m"}
  }]))]
  pub services: Option<Named<ServiceEntry>>,
  /// Private local services a peer client may reach with `aperio-client
  /// bind`, keyed by the name binders use. Never exposed to the public web.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{
    "pg_main": {"target": "127.0.0.1:5432"},
    "dns": {"target": "127.0.0.1:53", "protocol": "tcp/udp"}
  }]))]
  pub tunnels: Option<Named<TunnelEntry>>,
  /// Tunnels `aperio-client bind` listens for on this machine, keyed by the
  /// tunnel's name: a local port, or `{port, address, token, psk}`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{
    "pg_main": 15432,
    "dns": {"port": 15353, "token": "${BINDER_TOKEN}"}
  }]))]
  pub binds: Option<Named<BindValue>>,
  /// Topic filters this client subscribes to, for messages from the other
  /// clients of its organization. MQTT filter syntax: `+` is one level, `#`
  /// is the rest. `$aperio/...` carries the server's own events.
  ///
  /// An entry is a bare filter, or an object that also names a command to run
  /// when a message arrives.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["deploy/web", {"topic": "deploy/api", "run": "./deploy.sh", "timeout": "2m"}]]))]
  pub subscribe: Option<Vec<SubscribeValue>>,
  /// The local message faces, for an application on this machine.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"listen": "127.0.0.1:1888"}]))]
  pub messages: Option<MessagesSection>,
  /// Accept OpenTelemetry exports from things running next to this client and
  /// carry them to the server, which forwards them to the collector it is
  /// configured for. The point is an edge host that is allowed exactly one
  /// outbound connection, the tunnel: no new firewall rule, no collector
  /// credential at the edge. Applied at startup; a change needs a restart.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = [{"listen": "127.0.0.1:4318", "transport": "tunnel"}]))]
  pub otel_bridge: Option<OtelBridge>,
  /// Autoscaling: the endpoint the server calls when this client's services
  /// need capacity. Applies to every service this client exposes; each
  /// hostname bind gets its own record on the server.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{
    "url": "https://api.provider.example/apps/web/scale",
    "min": 1,
    "max": 8,
    "cold_start": "45s"
  }]))]
  pub scaling: Option<ScalingDecl>,
  /// Shut this client down after it has served no request for this long
  /// (unset = never). The scale-in half of `scaling`: the server never stops
  /// anything, an idle client retires itself. The shutdown is graceful (the
  /// server stops routing to it first, in-flight requests finish), and the
  /// timer only starts once the client has served its first request, so a
  /// slow cold start cannot make it exit before it is ever used.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["5m"]))]
  pub idle_timeout: Option<Span>,
  /// How long in-flight requests may finish when a configuration reload
  /// stops a service, before its tunnel connection is dropped. The client
  /// announces `Draining` first, so the server sends it nothing new while it
  /// finishes. `0s` drops the connection immediately. Default: `10s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["10s"]))]
  pub reload_drain: Option<Span>,
  /// Total link capacity of this client: a budget divided across every
  /// service and every parallel connection, so the sum the server is allowed
  /// to push never exceeds it. Bit suffixes (`kbit`/`mbit`/`gbit`) count as
  /// /8, byte suffixes (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["8mbit", "500kbit", "2MB"]))]
  pub bandwidth: Option<String>,
  /// Largest single tunnel frame this client accepts. Default: `32MB`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["32MB"]))]
  pub max_message_size: Option<ByteSize>,
  /// Answer a `forward` check (`via: client`) whose endpoint the *server*
  /// wrote, in its own visitor gate, rather than one of this client's
  /// services. Such a check makes this client call a URL the server chose,
  /// from this client's network, so it is refused unless this is set; the
  /// checks this client's own `access:` declares are always answered.
  /// Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub allow_server_forward: Option<bool>,
  /// Static-file services: answer a navigation (`Accept: text/html`) that
  /// matches no file with the root `index.html` and status 200, so a
  /// client-side router owns its routes. A missing asset still 404s. Applies
  /// to every `serve:` directory of this client. Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub serve_spa: Option<bool>,
  /// Static-file services: HTML file served with status 404 for misses the
  /// SPA fallback does not cover. Applies to every `serve:` directory of
  /// this client.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["./dist/404.html"]))]
  pub serve_404: Option<String>,
  /// Trust-on-first-use device key announced with the tunnel token, so a
  /// server with token pinning on can bind that token to this machine.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["9f8c1d2e3a4b5c6d7e8f9a0b1c2d3e4f9f8c1d2e3a4b5c6d7e8f9a0b1c2d3e4f"]))]
  pub device_key: Option<String>,
  /// File holding the device key; a random one is generated and kept there
  /// on the first run. Ignored when `device_key` is set directly.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["/var/lib/aperio/device.key"]))]
  pub device_key_file: Option<String>,
  /// Path to write this process's pid to at startup, removed on a clean exit.
  /// For an init system that wants one; a process supervisor usually knows the
  /// pid without being told.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["/run/aperio-client.pid"]))]
  pub pid_file: Option<String>,
  /// Log verbosity of this client (`RUST_LOG` overrides it). Default: `info`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["info", "debug"]))]
  pub log_level: Option<String>,
  /// Log output format: `json`, or `pretty`/`text` for the human-readable
  /// form. Unset auto-detects, a TTY gets `pretty` and a pipe gets `json`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("x-reload" = "restart", "examples" = ["json", "pretty"]))]
  pub log_format: Option<String>,
}

impl FileConfig {
  /// The `server:` block, or an empty one.
  pub fn server(&self) -> ServerSection {
    self.server.clone().unwrap_or_default()
  }
}

/// Renders the `aperio.yaml` JSON Schema as pretty-printed JSON. Used by the
/// aperio-client build script and the release workflow.
pub fn schema_json() -> String {
  let schema = schemars::schema_for!(FileConfig);
  serde_json::to_string_pretty(&schema).expect("the config schema must serialize")
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
