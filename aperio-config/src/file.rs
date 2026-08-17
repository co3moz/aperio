//! The `aperio.yaml` document itself: every top-level key, and the two
//! functions that turn this crate into the editor's schema.
//!
//! Apart from [`FileConfig`] this file is deliberately thin. The *shapes* a
//! key is written in live beside the thing they describe, in [`crate::client`]
//! for a service, [`crate::auth`] for a gate, [`crate::settings`] for the
//! scalar-or-block enums; what is here is the list of keys and the doc comment
//! each one contributes to the generated schema.

use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;

use crate::*;

/// The Aperio client configuration file (`aperio.yaml` or `~/.aperio.yaml`).
/// Every key is optional and can equally be set with a CLI flag or an `APERIO_*`
/// environment variable; this file is the lowest-friction way to keep them.
#[derive(Deserialize, Default, JsonSchema)]
pub struct FileConfig {
  /// The Aperio server to reach and the token to authenticate the tunnel with.
  #[schemars(extend("examples" = [{"url": "https://tunnel.example.com", "token": "apr_xxxxxxxxxxxxxxxx"}]))]
  pub server: Option<ServerValue>,
  /// Deprecated spelling of `server.token`, kept so a token written at the
  /// top level still authenticates rather than being silently ignored.
  #[schemars(extend("examples" = ["apr_xxxxxxxxxxxxxxxx"]))]
  pub token: Option<String>,
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Local backend to
  /// expose. Write it as a `services:` entry instead; single-service mode
  /// stays available as the CLI's positional target and `APERIO_TARGET`.
  /// `h2c://` / `h2://` targets are dialed over HTTP/2 (gRPC).
  #[schemars(extend("examples" = ["http://localhost:3000", "3000", "h2c://127.0.0.1:50051"], "deprecated" = true))]
  pub target: Option<String>,
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Serve a local
  /// directory of static files instead of forwarding to a backend. Write it
  /// as a `services:` entry's `serve:`; the CLI's `--serve` and
  /// `APERIO_SERVE` are unaffected.
  #[schemars(extend("examples" = ["./dist"], "deprecated" = true))]
  pub serve: Option<String>,
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Public hostname(s)
  /// to claim for this client's traffic. A bind belongs to the service it
  /// binds, so write it on the `services:` entry; `--hostname` and
  /// `APERIO_HOSTNAME` are unaffected.
  #[schemars(extend("examples" = ["app.example.com", ["app.example.com", "www.example.com"]], "deprecated" = true))]
  pub hostname: Option<Hostnames>,
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Public path prefix
  /// to claim for this client's traffic. Write it on the `services:` entry
  /// it binds; `--path` and `APERIO_PATH` are unaffected.
  #[schemars(extend("examples" = ["/api"], "deprecated" = true))]
  pub path: Option<String>,
  /// Strip the path prefix before forwarding, so the backend sees `/` not the
  /// bind. Default: `true` when a path bind is set.
  #[schemars(extend("examples" = [true]))]
  pub trim_bind: Option<bool>,
  /// Forward the visitor's original Host header to the backend instead of the
  /// target's. Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub pass_hostname: Option<bool>,
  /// Most requests handled at once before the server queues the rest.
  #[schemars(extend("examples" = [8]))]
  pub max_concurrent: Option<u32>,
  /// Lower the announced `max_concurrent` while requests queue up waiting for
  /// a local permit, and climb back when they stop. The server already queues
  /// rather than dispatching past the announced number, so this is how a
  /// client that has become slow stops being sent work it cannot do; the
  /// server then holds the request, picks another client, or asks for capacity
  /// through autoscaling, all of which beat a refusal. Needs `max_concurrent`
  /// to be set, since that is the number being moved. Default: `false`
  /// (env: APERIO_ADAPTIVE_CONCURRENCY).
  #[schemars(extend("examples" = [true]))]
  pub adaptive_concurrency: Option<bool>,
  /// Accept OpenTelemetry exports from things running next to this client and
  /// carry them to the server, which forwards them to the collector it is
  /// configured for. The point is an edge host that is allowed exactly one
  /// outbound connection, the tunnel: no new firewall rule, no collector
  /// credential at the edge.
  #[schemars(extend("examples" = [{"listen": "127.0.0.1:4318", "transport": "tunnel"}]))]
  pub otel_bridge: Option<OtelBridge>,
  /// Path to write this process's pid to at startup, removed on a clean exit.
  /// For an init system that wants one; a process supervisor usually knows the
  /// pid without being told. Default: unset (env: APERIO_PID_FILE).
  #[schemars(extend("examples" = ["/run/aperio-client.pid"]))]
  pub pid_file: Option<String>,
  /// Parallel tunnel connections opened for the exposed service (the server's
  /// `max_connections_per_service` is the ceiling); the server load-balances
  /// across them like separate clients, so a single dropped connection leaves
  /// no visitor-facing gap.
  /// A number opens exactly that many at startup; `{min: 1, max: 8}` opens the
  /// floor and grows towards the ceiling while requests queue up, then shrinks
  /// back when they stop. Default: `1`.
  #[schemars(extend("examples" = [2, {"min": 1, "max": 8}]))]
  pub connections: Option<Connections>,
  /// Carry every service that sets this on one WebSocket, instead of opening a
  /// connection per service. The default for the whole file, so forty services
  /// turn it on once; an entry may still say `multiplex: false` to keep a
  /// connection of its own.
  ///
  /// What it saves is per service and it is the whole cost of a connection: one
  /// socket, one TLS session, one reader, one writer and one heartbeat, rather
  /// than that many of each on both ends. What it costs is that the services
  /// then share a link, so a large response occupies the writer the others send
  /// through, and a dropped connection takes all of them down together.
  ///
  /// Two services are needed for it to do anything, and they must agree on
  /// `server:` and `token:`, since a connection carries one of each. Every
  /// multiplexed service needs a `name:`, which is what the server keeps its
  /// routing, ejection and statistics under and what addresses it in the
  /// dashboard. Needs a server speaking tunnel protocol 8 or newer (Aperio
  /// 0.10.0); against an older one the services are held back and the client
  /// says so, rather than being quietly served one at a time. `connections:` is
  /// not honored for a multiplexed service, one connection is what multiplexing
  /// means, and the client reports the difference in the dashboard's config
  /// view. Default: `false` (env: APERIO_MULTIPLEX).
  #[schemars(extend("examples" = [true]))]
  pub multiplex: Option<bool>,
  /// Static Prometheus labels attached to this client's own metric series,
  /// e.g. `{env: prod, region: eu-west}`, so one Prometheus can serve several
  /// environments without relabelling rules. At most 8 labels; names must be
  /// `[a-zA-Z_][a-zA-Z0-9_]*` and the server drops anything else, including
  /// the names it writes itself (`client_id`, `job`, `instance`, …).
  #[schemars(extend("examples" = [{"env": "prod", "region": "eu-west"}]))]
  pub metrics_labels: Option<std::collections::BTreeMap<String, String>>,
  /// Seconds to wait for the TCP connection to this backend before giving
  /// up, separate from `timeout`, which covers the whole request. A backend
  /// across a VPN needs longer than one on loopback, and one number for both
  /// means either slow failure detection everywhere or spurious failures for
  /// the far one. Default: unset (the whole-request `timeout` applies)
  /// (env: APERIO_CONNECT_TIMEOUT).
  #[schemars(extend("examples" = [2]))]
  pub connect_timeout: Option<u64>,
  /// Lowest TLS version accepted from an `https://` backend: `1.2` or `1.3`.
  /// Per service because a fleet with one legacy backend should not have to
  /// lower the floor for all of them. Default: rustls's own floor
  /// (env: APERIO_MIN_TLS_VERSION).
  #[schemars(extend("examples" = ["1.3"]))]
  pub min_tls_version: Option<String>,
  /// Seconds to wait before this service opens its tunnel. For a backend that
  /// is starting alongside the client and is not ready to answer the moment
  /// the process is. Default: `0` (env: APERIO_STARTUP_DELAY).
  #[schemars(extend("examples" = [5]))]
  pub startup_delay: Option<u64>,
  /// Names of services in this same file that must have a live tunnel before
  /// this one opens its own. Waits for them, then proceeds regardless after a
  /// bounded grace period, because a dependency that never arrives must not
  /// keep a service that could serve traffic off the air forever.
  #[schemars(extend("examples" = [["api"]]))]
  pub depends_on: Option<Vec<String>>,
  /// Largest response body, in bytes, the client will relay to a visitor.
  /// Default: `52428800` (50 MB).
  #[schemars(extend("examples" = [10485760]))]
  pub max_response_body: Option<usize>,
  /// Largest request body, in bytes, visitors may upload to this service;
  /// the server rejects bigger uploads with 413 before they enter the tunnel.
  #[schemars(extend("examples" = [1048576]))]
  pub max_request_body: Option<u64>,
  /// Seconds the server should wait for this service to answer a dispatched
  /// request before failing it, a per-service override of the server's global
  /// gateway response timeout (defaults applied per service).
  #[schemars(extend("examples" = [120]))]
  pub response_timeout: Option<u64>,
  /// Seconds to wait for the backend to respond before failing a request.
  /// Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub timeout: Option<u64>,
  /// Largest single tunnel frame, in bytes, the client will accept.
  /// Default: `33554432` (32 MB).
  #[schemars(extend("examples" = [33554432]))]
  pub max_message_size: Option<usize>,
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Raw TCP backend to
  /// expose instead of HTTP. Write it as a `services:` entry's `tcp_target:`;
  /// `--tcp-target` and `APERIO_TCP_TARGET` are unaffected.
  #[schemars(extend("examples" = ["127.0.0.1:5432"], "deprecated" = true))]
  pub tcp_target: Option<String>,
  /// What to call the service on screen when one is named on the command line
  /// or in the environment (`APERIO_CUSTOM_NAME`). A `services:` entry carries
  /// its own `custom_name:`, which is where a config file says it.
  #[schemars(extend("examples" = ["Public Web"]))]
  pub custom_name: Option<String>,
  /// Backend health probing (`endpoint`, `interval`, `timeout`, `threshold`,
  /// `wait_for_backend`). Preferred over the flat `target_health` / `health_*`
  /// keys, which still work; `services:` entries may override it per service.
  #[schemars(extend("examples" = [{"interval": 10, "timeout": 5, "threshold": 2}]))]
  pub health: Option<TopHealthConfig>,
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Backend health
  /// endpoint to probe; also the old flat spelling of `health.endpoint`.
  /// Write it on the `services:` entry whose backend it probes.
  #[schemars(extend("examples" = ["/health"], "deprecated" = true))]
  pub target_health: Option<String>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `target_health` when that is set). Deprecated spelling of
  /// `health.wait_for_backend`.
  #[schemars(extend("examples" = [true]))]
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
  /// Default: `0`.
  #[schemars(extend("examples" = [0]))]
  pub priority: Option<u32>,
  /// Total link capacity of this client: a budget divided across every service
  /// and every parallel connection, so the sum the server is allowed to push
  /// never exceeds it. Bit suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte
  /// suffixes (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000.
  #[schemars(extend("examples" = ["8mbit", "500kbit", "2MB"]))]
  pub bandwidth: Option<String>,
  /// How many backend redirects to follow transparently before passing one
  /// through. Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub max_redirects: Option<usize>,
  /// Other config files to read before this one, each path relative to the
  /// file that names it. Their keys are used unless this file sets them, and
  /// sequences of mappings (`services:`, `subscribe:`, `expose:`) concatenate
  /// with the includes first, so a fragment adds services rather than
  /// replacing them. Later includes win over earlier ones, and this file wins
  /// over all of them. Chains may nest up to five deep; a cycle is an error.
  #[schemars(extend("examples" = [["services/prod.yaml", "shared/health.yaml"]]))]
  pub include: Option<Vec<String>>,
  /// Seconds to let in-flight requests finish when a configuration reload
  /// stops a service, before its tunnel connection is dropped. The client
  /// announces `Draining` first, so the server sends it nothing new while it
  /// finishes. `0` drops the connection immediately, which is what happened
  /// before this existed. Default: `10`
  /// (env: APERIO_RELOAD_DRAIN).
  #[schemars(extend("examples" = [10]))]
  pub reload_drain: Option<u64>,
  /// Retry policy for backend requests that fail before a response arrives.
  /// The default for every `services:` entry that does not set its own.
  #[schemars(extend("examples" = [{"attempts": 3, "backoff": 100}]))]
  pub retry: Option<RetryConfig>,
  /// Circuit breaker for the backend. The default for every `services:` entry
  /// that does not set its own.
  #[schemars(extend("examples" = [{"failures": 5, "open_for": 30}]))]
  pub circuit_breaker: Option<CircuitBreakerConfig>,
  /// Topic filters this client subscribes to, for messages from the other
  /// clients of its organization. MQTT filter syntax: `+` is one level, `#`
  /// is the rest. `$aperio/...` carries the server's own events.
  ///
  /// An entry is a bare filter, or an object that also names a command to run
  /// when a message arrives.
  #[schemars(extend("examples" = [["deploy/web", {"topic": "deploy/api", "run": "./deploy.sh", "timeout": 120}]]))]
  pub subscribe: Option<Vec<SubscribeValue>>,
  /// Local address the message face listens on, so an application on this
  /// machine can subscribe (SSE) and publish (POST) without speaking the
  /// tunnel protocol. Unset = no local listener.
  #[schemars(extend("examples" = ["127.0.0.1:1888"]))]
  pub messages_listen: Option<String>,
  /// Local address an MQTT listener answers on, for an application that would
  /// rather use the MQTT client library it already has. MQTT 3.1.1, QoS 0;
  /// the protocol never leaves this machine. Unset = no MQTT listener.
  #[schemars(extend("examples" = ["127.0.0.1:1883"]))]
  pub messages_mqtt_listen: Option<String>,
  /// Expose several backends from one client, each on its own tunnel connection
  /// (replaces the single top-level `target`).
  #[schemars(extend("examples" = [[
    {"name": "web", "target": "http://localhost:3000", "hostname": "app.example.com"},
    {"name": "api", "target": "http://localhost:4000", "hostname": "api.example.com"}
  ]]))]
  pub services: Option<Vec<ServiceEntry>>,
  /// Serve without the server's visitor login (needs a token that allows it).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub public: Option<bool>,
  /// Gate this client behind your own visitor login instead of the server's.
  /// A `user:password` scalar, one `{method: ...}` block, or a list of them.
  #[schemars(extend("examples" = ["admin:s3cret", {"method": "none"}]))]
  pub auth: Option<AuthSetting>,
  /// Visitor IPs/CIDRs allowed to reach this service (plain IPs or CIDR
  /// ranges); empty/unset = everyone. Enforced by the server before dispatch.
  #[schemars(extend("examples" = [["203.0.113.7", "10.0.0.0/8"]]))]
  pub allowed_ips: Option<Vec<String>>,
  /// Let the server cache GET responses (per their `Cache-Control`);
  /// effective only when the server enables APERIO_CACHE. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub cache: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504 (needs `cache: true` and the server-side cache enabled).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub resilience: Option<bool>,
  /// Record transactions for the dashboard's request inspector (services may
  /// override). Default: `true`.
  #[schemars(extend("examples" = [false]))]
  pub capture: Option<bool>,
  /// Persist inbound POST requests (third-party webhooks) into the server's
  /// webhook inbox, for browsing and re-firing (services may override).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub webhook_inbox: Option<bool>,
  /// Redirect URL for visitors rejected by `allowed_ips` when no candidate
  /// of the route admits them (unset = stealth; services may override).
  #[schemars(extend("examples" = ["https://example.com/not-for-you"]))]
  pub denied: Option<String>,
  /// IP family used to dial the tunnel server: `auto` (tries both), `ipv4`,
  /// or `ipv6`. Set `ipv4` when the server hostname resolves to an IPv6
  /// address the host cannot reach (env: APERIO_IP_FAMILY). Default: `auto`.
  #[schemars(extend("examples" = ["auto", "ipv4"]))]
  pub ip_family: Option<String>,
  /// HTTP proxy to dial the tunnel server through, for a network that allows
  /// no direct outbound connection: `host:port`, or `http://host:port`, with
  /// an optional `user:password@` in front. The client sends `CONNECT` and
  /// then runs TLS inside the tunnel the proxy opens, so the proxy sees the
  /// server's hostname and nothing else. Applies to the tunnel connection
  /// only; requests to your own backend never go through it
  /// (env: APERIO_EGRESS_PROXY). Unset: dial the server directly.
  #[schemars(extend("examples" = ["proxy.corp:3128", "http://user:password@proxy.corp:3128"]))]
  pub egress_proxy: Option<String>,
  /// Lowest TLS version offered when dialing the tunnel server over `wss://`:
  /// `1.2` or `1.3`. Unset leaves rustls' own set (1.2 and 1.3) in place,
  /// which is the right default; pin it when a policy has to name the floor
  /// rather than inherit it. A value this client cannot offer is refused at
  /// startup rather than ignored (env: APERIO_TLS_MIN_VERSION).
  #[schemars(extend("examples" = ["1.3"]))]
  pub tls_min_version: Option<String>,
  /// Exact cipher suites offered when dialing the tunnel server, by their
  /// IANA names, comma-separated. Unset leaves rustls'
  /// preference order alone, which is almost always better; name them only
  /// when something external requires it. An unknown name is refused at
  /// startup (env: APERIO_TLS_CIPHER_SUITES).
  #[schemars(extend("examples" = ["TLS13_AES_256_GCM_SHA384,TLS13_CHACHA20_POLY1305_SHA256"]))]
  pub tls_cipher_suites: Option<String>,
  /// The Aperio version this file was written for, e.g. `0.5.0`. On startup
  /// the client compares it against its own build and reports every recorded
  /// change to the configuration format that landed in between, refusing to
  /// start when one of them has security consequences. Unset disables the
  /// check (env: APERIO_VERSION).
  #[schemars(extend("examples" = ["0.5.0"]))]
  pub version: Option<String>,
  /// Fixed instance UUID kept across restarts, so failover and `--bind-tunnels`
  /// can recognize this client; a random one is used when unset.
  #[schemars(extend("examples" = ["3f2504e0-4f89-41d3-9a0c-0305e82c3301"]))]
  pub client_id: Option<String>,
  /// Request/response header add-remove rules applied by this client to
  /// proxied HTTP traffic (services may override with their own `headers`).
  #[schemars(extend("examples" = [{
    "request": {"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]},
    "response": {"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}
  }]))]
  pub headers: Option<HeaderRules>,
  /// Security response-header preset: `true` injects HSTS,
  /// `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and
  /// `Referrer-Policy`; a mapping picks headers individually (services may
  /// override with their own `security_headers`).
  #[schemars(extend("examples" = [true, {"hsts": true, "frame_options": "SAMEORIGIN"}]))]
  pub security_headers: Option<SecurityHeaders>,
  /// Private local services a peer client may reach via `--bind-tunnels`; never
  /// exposed to the public web.
  #[schemars(extend("examples" = [[
    {"name": "pg_main", "target": "127.0.0.1:5432"},
    {"name": "dns", "target": "127.0.0.1:53", "protocol": "tcp/udp"}
  ]]))]
  pub tunnels: Option<Vec<TunnelDecl>>,
  /// Tunnels this process binds to local ports, keyed by the tunnel's name.
  /// A key naming a peer's client id instead binds every tunnel that peer
  /// declares, which is the older spelling and still works.
  #[serde(rename = "bind-tunnels", alias = "bind_tunnels")]
  #[schemars(extend("examples" = [{
    "pg_main": 15432,
    "dns": {"port": 15353, "token": "apr_binder_token"}
  }]))]
  pub bind_tunnels: Option<HashMap<String, BindTunnelValue>>,
  /// Autoscaling: the endpoint the server calls when this client's services
  /// need capacity. Applies to every service this client exposes; each
  /// hostname bind gets its own record on the server.
  #[schemars(extend("examples" = [{
    "url": "https://api.provider.example/apps/web/scale",
    "min": 1,
    "max": 8,
    "cold_start": "45s"
  }]))]
  pub scaling: Option<ScalingDecl>,
  /// Shut this client down after it has served no request for this long, e.g.
  /// `5m` (unset = never). The scale-in half of `scaling`: the server never
  /// stops anything, an idle client retires itself. The shutdown is graceful
  /// (the server stops routing to it first, in-flight requests finish), and
  /// the timer only starts once the client has served its first request, so a
  /// slow cold start cannot make it exit before it is ever used.
  #[schemars(extend("examples" = ["5m"]))]
  pub idle_timeout: Option<String>,
  /// Static-file mode: answer a navigation (`Accept: text/html`) that matches
  /// no file with the root `index.html` and status 200, so a client-side
  /// router owns its routes. A missing asset still 404s
  /// (env: APERIO_SERVE_SPA). Process-wide: it applies to every served
  /// directory of this client. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub serve_spa: Option<bool>,
  /// Static-file mode: HTML file served with status 404 for misses the SPA
  /// fallback does not cover (env: APERIO_SERVE_404). Process-wide, like
  /// `serve_spa`.
  #[schemars(extend("examples" = ["./dist/404.html"]))]
  pub serve_404: Option<String>,
  /// Trust-on-first-use device key announced with the tunnel token, so a
  /// server with token pinning on can bind that token to this machine
  /// (env: APERIO_DEVICE_KEY).
  #[schemars(extend("examples" = ["9f8c1d2e3a4b5c6d7e8f9a0b1c2d3e4f9f8c1d2e3a4b5c6d7e8f9a0b1c2d3e4f"]))]
  pub device_key: Option<String>,
  /// File holding the device key; a random one is generated and persisted
  /// there on first run. Ignored when `device_key` is set directly
  /// (env: APERIO_DEVICE_KEY_FILE).
  #[schemars(extend("examples" = ["/var/lib/aperio/device.key"]))]
  pub device_key_file: Option<String>,
  /// Log verbosity of this client (env: LOG_LEVEL; `RUST_LOG` overrides
  /// both). Default: `info`.
  #[schemars(extend("examples" = ["info", "debug"]))]
  pub log_level: Option<String>,
  /// Log output format: `json`, or `pretty`/`text` for the human-readable
  /// form. Unset auto-detects, a TTY gets `pretty` and a pipe gets `json`
  /// (env: APERIO_LOG_FORMAT).
  #[schemars(extend("examples" = ["json", "pretty"]))]
  pub log_format: Option<String>,
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

  /// Additional server URLs to fail over to, from the nested section.
  pub fn server_urls(&self) -> Option<Vec<String>> {
    match &self.server {
      Some(ServerValue::Section { urls, .. }) => urls.clone(),
      _ => None,
    }
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
