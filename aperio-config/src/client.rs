use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::*;

/// A private local service (e.g. a database or SSH) this client makes reachable
/// to a peer running `--bind-tunnels`, without ever exposing it to the public web.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct TunnelDecl {
  /// Handle this tunnel is bound and exposed by, unique within the
  /// organization. Binders name it instead of naming a client id, so the
  /// handle survives reconnects, parallel connections and a `services:` list.
  /// Unset derives one from the target, and a name shaped like a UUID is
  /// rejected so names can never collide with client ids.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["pg_main", "ssh_bastion"]))]
  pub name: Option<String>,
  /// What to call it on screen. Free text: any language, any punctuation,
  /// spaces. Nothing addresses it, so nothing breaks when it changes.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["Primary Postgres"]))]
  pub custom_name: Option<String>,
  /// Local address this client dials when a peer binds the tunnel.
  #[schemars(extend("examples" = ["127.0.0.1:27017"]))]
  pub target: String,
  /// Transport of the tunnel: `tcp`, `udp` (best-effort datagram
  /// relay), or `tcp/udp` for a service that is genuinely both (DNS, for
  /// instance). A combined tunnel is one tunnel with one name and one local
  /// port on the binder, answering on both transports. Default: `tcp`.
  #[serde(default = "default_tcp")]
  #[schemars(extend("examples" = ["tcp", "udp", "tcp/udp"]))]
  pub protocol: String,
  /// End-to-end encrypt this tunnel between the two clients (X25519 +
  /// ChaCha20-Poly1305); the server only relays ciphertext. TCP only.
  /// Default: `false`.
  #[serde(default)]
  pub encrypt: bool,
  /// Pre-shared key mixed into the key derivation of an encrypted tunnel,
  /// protecting against an actively hostile server. Never sent anywhere,
  /// the binder configures the same value in its `bind-tunnels` entry.
  #[serde(default, skip_serializing)]
  #[schemars(extend("examples" = ["a-long-shared-secret-both-sides-hold"]))]
  pub psk: Option<String>,
  /// Write a PROXY protocol v2 header to this backend before any payload
  /// byte, announcing the visitor's real address. TCP only. Without it the
  /// backend sees a connection from the client process and the visitor's
  /// address is lost at the last hop. Turn it on only when the backend is
  /// configured to expect the header (nginx `listen ... proxy_protocol`,
  /// HAProxy `accept-proxy`, MySQL `proxy-protocol-networks`): a backend that
  /// is not will read it as protocol garbage and drop the connection.
  /// Default: `false`.
  #[serde(default)]
  #[schemars(extend("examples" = [true]))]
  pub proxy_protocol: bool,
  /// UDP only: seconds a relay may sit with no datagrams in either direction
  /// before it expires; binders learn it via tunnel discovery. Default: `60`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [300]))]
  pub idle_timeout: Option<u64>,
  /// Deprecated spelling of the public-port claim (TCP only): the value must
  /// equal the `key` of an `expose:` entry in the server's aperio-server.yaml.
  /// Prefer naming the tunnel and letting the server's `expose:` entry point
  /// at it by `tunnel:` + `token:`, which is revocable and names an owner.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["k5fj2q-expose-secret"]))]
  pub expose: Option<String>,
}

/// Header edits applied to one direction of proxied traffic (request or
/// response): `add` sets headers (replacing any existing value of the same
/// name), `remove` strips headers by name (case-insensitive).
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
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
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
pub struct HeaderRules {
  /// Edits applied to forwarded requests before they reach the local backend.
  #[schemars(extend("examples" = [{"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]}]))]
  pub request: Option<HeaderDirectives>,
  /// Edits applied to backend responses before they return to the visitor.
  #[schemars(extend("examples" = [{"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}]))]
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
  #[schemars(extend("examples" = [true]))]
  pub hsts: Option<bool>,
  /// HSTS `max-age` in seconds. Default: `63072000` (2 years).
  #[schemars(extend("examples" = [31536000]))]
  pub hsts_max_age: Option<u64>,
  /// `X-Frame-Options` value to inject.
  #[schemars(extend("examples" = ["DENY", "SAMEORIGIN"]))]
  pub frame_options: Option<String>,
  /// Inject `X-Content-Type-Options: nosniff`.
  #[schemars(extend("examples" = [true]))]
  pub nosniff: Option<bool>,
  /// `Referrer-Policy` value to inject.
  #[schemars(extend("examples" = ["strict-origin-when-cross-origin"]))]
  pub referrer_policy: Option<String>,
  /// `Content-Security-Policy` value to inject (no default, CSP is
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
        // `X-Frame-Options` has exactly two values a browser acts on. A
        // typo (`DENNY`) is a header that looks like protection and is
        // ignored, so it is corrected to the value it was reaching for
        // rather than emitted as written.
        if let Some(v) = opts.frame_options.as_ref().filter(|v| !v.trim().is_empty()) {
          let value = match v.trim().to_ascii_uppercase().as_str() {
            "SAMEORIGIN" => "SAMEORIGIN",
            _ => "DENY",
          };
          out.push(("X-Frame-Options".to_string(), value.to_string()));
        }
        if opts.nosniff.unwrap_or(false) {
          out.push(("X-Content-Type-Options".to_string(), "nosniff".to_string()));
        }
        // Same reasoning as `X-Frame-Options`: a browser acts on this list and
        // ignores anything else, so a typo is a header that looks like a
        // policy and is not. An unrecognized value falls back to the default
        // rather than going out as written.
        if let Some(v) = opts
          .referrer_policy
          .as_ref()
          .filter(|v| !v.trim().is_empty())
        {
          const KNOWN: &[&str] = &[
            "no-referrer",
            "no-referrer-when-downgrade",
            "origin",
            "origin-when-cross-origin",
            "same-origin",
            "strict-origin",
            "strict-origin-when-cross-origin",
            "unsafe-url",
          ];
          let asked = v.trim().to_ascii_lowercase();
          let value = KNOWN
            .iter()
            .find(|known| **known == asked)
            .copied()
            .unwrap_or("strict-origin-when-cross-origin");
          out.push(("Referrer-Policy".to_string(), value.to_string()));
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
  ///
  /// Against an `h2c://`/`h2://` target this names the **gRPC service** to
  /// health-check instead: the probe calls `grpc.health.v1.Health/Check`,
  /// because a plain GET cannot reach a server that speaks HTTP/2 with prior
  /// knowledge and routes by method name. `/` asks about the server as a
  /// whole, which is the usual answer. An absolute `http(s)://` URL still
  /// means an ordinary HTTP probe, for a backend exposing health on a
  /// separate port.
  #[schemars(extend("examples" = ["/health"]))]
  pub endpoint: Option<String>,
  /// Seconds between probes. Default: `10`.
  #[schemars(extend("examples" = [10]))]
  pub interval: Option<u64>,
  /// Seconds to wait for each probe before counting it as failed.
  /// Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy.
  /// Default: `2`.
  #[schemars(extend("examples" = [3]))]
  pub threshold: Option<u32>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `endpoint` when that is set). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub wait_for_backend: Option<bool>,
}

/// The top level's `health:` block.
///
/// The same fields as [`HealthConfig`], a file written either way parses
/// identically, except that `endpoint` is on its way out here. The other
/// children are real defaults: a `services:` entry that says nothing about
/// its interval, timeout, threshold or boot wait inherits them. `endpoint` is
/// not, and never was: a probe path belongs to the backend it probes, so the
/// resolver reads it strictly per entry and a top-level one is read by
/// nothing at all once a `services:` list exists.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopHealthConfig {
  /// **Not accepted in a config file since 0.9.0; CLI and environment only.** Endpoint to probe.
  /// Write it on the `services:` entry whose backend it probes; at the top
  /// level it is read only by a file that has no `services:` list.
  #[schemars(extend("examples" = ["/health"], "deprecated" = true))]
  pub endpoint: Option<String>,
  /// Seconds between probes. Applies to every `services:` entry that does not
  /// set its own. Default: `10`.
  #[schemars(extend("examples" = [10]))]
  pub interval: Option<u64>,
  /// Seconds to wait for each probe before counting it as failed. Applies to
  /// every `services:` entry that does not set its own. Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy. Applies
  /// to every `services:` entry that does not set its own. Default: `2`.
  #[schemars(extend("examples" = [3]))]
  pub threshold: Option<u32>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots. Applies
  /// to every `services:` entry that does not set its own. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub wait_for_backend: Option<bool>,
}

impl TopHealthConfig {
  /// True when nothing in the block was set (an empty `health:` mapping).
  pub fn is_empty(&self) -> bool {
    self.endpoint.is_none()
      && self.interval.is_none()
      && self.timeout.is_none()
      && self.threshold.is_none()
      && self.wait_for_backend.is_none()
  }
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
  #[schemars(extend("examples" = ["${SCALE_SECRET}"]))]
  pub secret: Option<String>,
  /// Instances that should always be running. `0` opts into scale-to-zero:
  /// a request for an unserved hostname triggers a cold start instead of a
  /// 504. Default: `0`.
  #[serde(default)]
  pub min: u32,
  /// Ceiling the server will never ask to exceed (0 = only cold starts, no
  /// scale-out). Default: `0`.
  #[serde(default)]
  pub max: u32,
  /// How long a visitor request may be held while a cold start completes,
  /// e.g. `45s` (0 = do not hold, answer immediately). Default: `45s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["45s"]))]
  pub cold_start: Option<String>,
  /// Pool utilization above which the server asks for one more instance,
  /// between 0 and 1. Default: `0.8`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [0.8]))]
  pub target_utilization: Option<f64>,
  /// How long utilization must stay above the target before scaling out,
  /// e.g. `15s`. Guards against reacting to a single spike. Default: `15s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["15s"]))]
  pub window: Option<String>,
  /// Minimum gap between two calls for this bind, e.g. `60s`.
  /// A new instance needs time to appear; without this the server would ask
  /// again while it is still starting. Default: `60s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["60s"]))]
  pub cooldown: Option<String>,
}

/// The Aperio server this client connects to: either a bare URL string, or a
/// `{ url, token }` section that also carries the tunnel token.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ServerValue {
  /// Server URL only, the token then comes from `token:` or the environment.
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
    /// Additional server URLs to fail over to, tried in order when `url` is
    /// unreachable, for a redundant control plane.
    #[schemars(extend("examples" = [["https://tunnel-b.example.com"]]))]
    urls: Option<Vec<String>>,
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

/// `connections:` as written in the file: a fixed count, or an elastic pool
/// with a floor and a ceiling.
///
/// The scalar keeps meaning exactly what it always did, N connections opened
/// at startup and kept, so no existing file changes behavior. Elasticity is
/// something an operator opts into by writing a range, which is the honest
/// default: our own measurements have a peak, past which more connections cost
/// more than they return, so growing a pool without being asked would not be
/// a safe assumption.
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum Connections {
  /// `connections: 4`, opened at startup and kept.
  Fixed(u32),
  /// `connections: {min: 1, max: 8}`, grown and shrunk with load.
  Range(ConnectionRange),
}

/// The `{min, max}` spelling of `connections:`.
#[derive(Deserialize, Clone, Debug, Default, JsonSchema)]
pub struct ConnectionRange {
  /// Connections opened at startup and never dropped. Default: `1`.
  #[schemars(extend("examples" = [1]))]
  pub min: Option<u32>,
  /// Most connections the pool may grow to under load. Default: `min`.
  #[schemars(extend("examples" = [8]))]
  pub max: Option<u32>,
}

impl Connections {
  /// Connections opened at startup.
  pub fn min(&self) -> u32 {
    match self {
      Connections::Fixed(n) => (*n).max(1),
      Connections::Range(r) => r.min.unwrap_or(1).max(1),
    }
  }

  /// Ceiling the pool may grow to. Never below the floor: a range written the
  /// wrong way round is a typo, and honoring it literally would mean opening
  /// fewer connections than the file's own `min` promises.
  pub fn max(&self) -> u32 {
    match self {
      Connections::Fixed(n) => (*n).max(1),
      Connections::Range(r) => r.max.unwrap_or_else(|| self.min()).max(self.min()),
    }
  }

  /// True when this pool grows and shrinks rather than being a fixed size.
  pub fn is_elastic(&self) -> bool {
    self.max() > self.min()
  }
}

/// One exposed backend when a single client serves several at once; any unset
/// field falls back to the top-level value.
#[derive(Deserialize, Default, Clone, JsonSchema)]
pub struct ServiceEntry {
  /// Handle for this service in client logs and the dashboard clients table.
  /// An identifier: a-z, 0-9 and `_`. Use `custom_name` for something to read.
  #[schemars(extend("examples" = ["web"]))]
  pub name: Option<String>,
  /// What to call this service on screen. Free text: any language, any
  /// punctuation, spaces. Nothing addresses it, so nothing breaks when it
  /// changes.
  #[serde(default)]
  #[schemars(extend("examples" = ["Public Web"]))]
  pub custom_name: Option<String>,
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
  #[schemars(extend("examples" = [true]))]
  pub trim_bind: Option<bool>,
  /// Forward the visitor's original Host header instead of the target's.
  #[schemars(extend("examples" = [false]))]
  pub pass_hostname: Option<bool>,
  /// Most requests this service handles at once before the server queues the rest.
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
  /// Parallel tunnel connections opened for this service; the
  /// server's `max_connections_per_service` is the ceiling, announced on
  /// connect, and a token may lower it further;
  /// the server load-balances across them like separate clients, so a single
  /// dropped connection leaves no visitor-facing gap.
  /// A number opens exactly that many at startup; `{min: 1, max: 8}` opens the
  /// floor and grows towards the ceiling while requests queue up, then shrinks
  /// back when they stop. Default: `1`.
  #[schemars(extend("examples" = [2, {"min": 1, "max": 8}]))]
  pub connections: Option<Connections>,
  /// Carry this service on a WebSocket it shares with the other services that
  /// set it, instead of opening one of its own. Overrides the top-level
  /// `multiplex:`, so a file that turns it on for everything can still keep a
  /// single service on a connection of its own with `multiplex: false`.
  /// Default: the top-level `multiplex:`.
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
  /// request before failing it, a per-service override of the server's global
  /// gateway response timeout, for slow report/upload endpoints.
  #[schemars(extend("examples" = [120]))]
  pub response_timeout: Option<u64>,
  /// How many backend redirects to follow transparently before passing one through.
  #[schemars(extend("examples" = [5]))]
  pub max_redirects: Option<usize>,
  /// Retry policy for this service's backend requests; falls back to the
  /// top-level `retry:`.
  #[schemars(extend("examples" = [{"attempts": 3, "backoff": 100}]))]
  pub retry: Option<RetryConfig>,
  /// Circuit breaker for this service's backend; falls back to the top-level
  /// `circuit_breaker:`.
  #[schemars(extend("examples" = [{"failures": 5, "open_for": 30}]))]
  pub circuit_breaker: Option<CircuitBreakerConfig>,
  /// Raw TCP backend for this service instead of HTTP (experimental).
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Backend health probing for this service (`endpoint`, `interval`,
  /// `timeout`, `threshold`, `wait_for_backend`). Preferred over the flat
  /// `target_health` / `health_*` keys, which still work.
  #[schemars(extend("examples" = [{"endpoint": "/health", "interval": 10, "threshold": 2}]))]
  pub health: Option<HealthConfig>,
  /// Backend health endpoint the client probes to pull itself from rotation
  /// when down. Deprecated spelling of `health.endpoint`.
  #[schemars(extend("examples" = ["/health"]))]
  pub target_health: Option<String>,
  /// Hold this service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `target_health` when that is set).
  #[schemars(extend("examples" = [true]))]
  pub wait_for_backend: Option<bool>,
  /// Seconds between backend health probes. Deprecated spelling of
  /// `health.interval`.
  #[schemars(extend("examples" = [10]))]
  pub health_interval: Option<u64>,
  /// Seconds to wait for each health probe before counting it as failed.
  /// Deprecated spelling of `health.timeout`.
  #[schemars(extend("examples" = [5]))]
  pub health_timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy.
  #[schemars(extend("examples" = [3]))]
  pub health_threshold: Option<u32>,
  /// Serve this service without the server's visitor login (needs a token that allows it).
  #[schemars(extend("examples" = [true]))]
  pub public: Option<bool>,
  /// Let the server reach `target` itself instead of relaying through this
  /// client, when the server's `server_side_targets:` permits that address
  /// (needs a token that allows it). Saves the two hops a relayed request
  /// makes, the one to this client and the one from here to the target, and is
  /// only useful when the target is somewhere the server can already reach.
  /// The service is still declared, gated and routed here; only the last hop
  /// moves. Refused rather than ignored when the target is not permitted, and
  /// cannot be combined with `serve:`, whose files are on this machine.
  /// Default: false.
  #[schemars(extend("examples" = [true]))]
  pub server_side: Option<bool>,
  /// Gate this service behind your own visitor login instead of the server's.
  /// A `user:password` scalar, one `{method: ...}` block, or a list of them.
  #[schemars(extend("examples" = ["admin:s3cret", {"method": "none"}]))]
  pub auth: Option<AuthSetting>,
  /// Visitor IPs/CIDRs allowed to reach this service (plain IPs or CIDR
  /// ranges); empty/unset = everyone. Enforced by the server before dispatch.
  #[schemars(extend("examples" = [["203.0.113.7", "10.0.0.0/8"]]))]
  pub allowed_ips: Option<Vec<String>>,
  /// Request/response header add-remove rules for this service (replaces the
  /// top-level `headers` when set).
  #[schemars(extend("examples" = [{
    "request": {"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]},
    "response": {"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}
  }]))]
  pub headers: Option<HeaderRules>,
  /// Security response-header preset for this service (`true` or a granular
  /// mapping; replaces the top-level `security_headers` when set).
  #[schemars(extend("examples" = [true, {"hsts": true, "frame_options": "SAMEORIGIN"}]))]
  pub security_headers: Option<SecurityHeaders>,
  /// Let the server cache this service's GET responses (per their
  /// `Cache-Control`); effective only when the server enables APERIO_CACHE.
  #[schemars(extend("examples" = [true]))]
  pub cache: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504 (needs `cache: true` and the server-side cache enabled).
  #[schemars(extend("examples" = [true]))]
  pub resilience: Option<bool>,
  /// Record this service's transactions for the dashboard's request
  /// inspector. Turning it off for a service that carries
  /// heavy traffic buys back a mutex, two header clones and a capture entry
  /// per request, at the cost of not being able to inspect or replay its
  /// requests afterwards. Default: `true`.
  #[schemars(extend("examples" = [false]))]
  pub capture: Option<bool>,
  /// Persist inbound POST requests (third-party webhooks) hitting this
  /// service into the server's webhook inbox, for browsing and re-firing.
  #[schemars(extend("examples" = [true]))]
  pub webhook_inbox: Option<bool>,
  /// Redirect URL for visitors rejected by `allowed_ips` when no candidate
  /// of the route admits them (unset = stealth: the same answer as an
  /// unclaimed route).
  #[schemars(extend("examples" = ["https://example.com/not-for-you"]))]
  pub denied: Option<String>,
}

/// One `subscribe:` entry.
///
/// A bare filter is the short form: listen, and let whatever is attached to
/// the local face receive it. The object form is for a client that should
/// *act* on the message itself.
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum SubscribeValue {
  /// `- deploy/web`, listen and deliver, nothing else.
  Filter(String),
  /// The full entry, for a subscription that runs something.
  Entry(SubscribeEntry),
}

impl SubscribeValue {
  /// The entry form, so callers do not branch on the spelling.
  pub fn entry(&self) -> SubscribeEntry {
    match self {
      SubscribeValue::Filter(topic) => SubscribeEntry {
        topic: topic.clone(),
        ..SubscribeEntry::default()
      },
      SubscribeValue::Entry(entry) => entry.clone(),
    }
  }
}

/// A subscription that may also run a command when a message arrives.
///
/// `run:` is a remote-execution primitive by design: a message from another
/// client of the organization causes a command to run here. Everything about
/// its shape follows from that. The payload never reaches the command line,
/// only stdin and the environment, so a message can never become part of the
/// command. Concurrency is capped and the run is timed, so a publisher in a
/// loop cannot fork a thousand processes or leave one wedged forever. And it
/// is per topic, opt-in, in a file the operator wrote.
#[derive(Deserialize, Clone, Debug, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct SubscribeEntry {
  /// The topic filter to subscribe to.
  #[schemars(extend("examples" = ["deploy/web", "$aperio/client/#"]))]
  pub topic: String,
  /// Command to run for each message, through the shell. The message body
  /// arrives on **stdin**, never as an argument; `APERIO_MESSAGE_TOPIC` and
  /// `APERIO_MESSAGE_ID` are set in the environment. Unset = deliver only.
  #[schemars(extend("examples" = ["./deploy.sh", "systemctl reload nginx"]))]
  pub run: Option<String>,
  /// Extra environment variables for `run:`, on top of what the client sets.
  /// The command inherits the client's own environment as well; this is for
  /// the values that belong to *this* subscription. `APERIO_MESSAGE_TOPIC`
  /// and `APERIO_MESSAGE_ID` are set after these and cannot be overridden,
  /// since a command reading them must be able to trust what it finds.
  #[schemars(extend("examples" = [{"DEPLOY_ENV": "staging", "SLACK_CHANNEL": "#ops"}]))]
  #[serde(default)]
  pub env: std::collections::HashMap<String, String>,
  /// Seconds a run may take before it is killed. A command that hangs must
  /// not hold the subscription's one slot forever. Default: `60`.
  #[schemars(extend("examples" = [60]))]
  pub timeout: Option<u64>,
  /// Runs allowed at once for this subscription. Messages that
  /// arrive while the cap is reached are dropped with a warning rather than
  /// queued: a queue for a command that cannot keep up is the same problem
  /// one step later. Default: `1`.
  #[schemars(extend("examples" = [1]))]
  pub max_concurrent: Option<u32>,
}

/// One `bind-tunnels:` entry, keyed by the tunnel's name (or, in the older
/// spelling, by a peer client's id).
///
/// A bare port number is the short form of `{ port: <n> }`, since naming the
/// local port is the only thing most entries do.
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum BindTunnelValue {
  /// `pg_main: 15432`, the local port, everything else defaulted.
  Port(u16),
  /// The full entry.
  Entry(BindTunnelEntry),
}

impl BindTunnelValue {
  /// The entry form, so callers do not branch on the spelling.
  pub fn entry(&self) -> BindTunnelEntry {
    match self {
      BindTunnelValue::Port(port) => BindTunnelEntry {
        port: Some(*port),
        ..BindTunnelEntry::default()
      },
      BindTunnelValue::Entry(entry) => entry.clone(),
    }
  }
}

/// What to bind, and where to bind it: one named tunnel (or, in the older
/// spelling, every tunnel of one peer client).
#[derive(Deserialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindTunnelEntry {
  /// Local port for this tunnel. Unset reuses the declared target's port when
  /// it is free and unprivileged, and otherwise picks a stable port derived
  /// from the tunnel's name (logged at startup).
  #[schemars(extend("examples" = [15432]))]
  pub port: Option<u16>,
  /// Local address the listener binds; anything but the default puts a
  /// deliberately unexposed service on the network and is warned about.
  /// Default: `127.0.0.1`.
  #[schemars(extend("examples" = ["127.0.0.1"]))]
  pub address: Option<String>,
  /// Token to authenticate the binding with; falls back to this client's
  /// server token. Only needed when the tunnel is reached with a different
  /// credential than the one this client connects with.
  #[schemars(extend("examples" = ["apr_xxxxxxxxxxxxxxxx"]))]
  pub token: Option<String>,
  /// Map a declared tunnel target to a specific local port instead of reusing
  /// the target's. Only meaningful for an entry keyed by a peer's client id,
  /// which binds every tunnel that peer declares; a name-keyed entry is one
  /// tunnel and uses `port`.
  #[serde(default, rename = "override")]
  #[schemars(extend("examples" = [{"pg_main": 15432, "redis": 16379}]))]
  pub overrides: HashMap<String, u16>,
  /// Pre-shared key for this peer's end-to-end encrypted tunnels; must match
  /// the `psk` the declaring client configured. Never sent to the server.
  #[schemars(extend("examples" = ["a-long-shared-secret-both-sides-hold"]))]
  pub psk: Option<String>,
}
