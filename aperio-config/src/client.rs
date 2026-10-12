//! The shapes an `aperio.yaml` is written in: a service, its policy, a tunnel,
//! a bind, a subscription. [`crate::file`] is the list of top-level keys that
//! holds them.

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::units::{ByteSize, Span};
use crate::*;

/// A tunnel as it travels to the server: what the declaring client announces
/// for each `tunnels:` entry. The file writes a [`TunnelEntry`] under its name;
/// this is that entry with the name filled in and the secrets left behind.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct TunnelDecl {
  /// Handle this tunnel is bound by, unique within the organization.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub name: Option<String>,
  /// What to call it on screen.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub custom_name: Option<String>,
  /// Local address this client dials when a peer binds the tunnel.
  pub target: String,
  /// `tcp`, `udp` or `tcp/udp`.
  #[serde(default = "default_tcp")]
  pub protocol: String,
  /// End-to-end encrypted between the two clients.
  #[serde(default)]
  pub encrypt: bool,
  /// Pre-shared key of an encrypted tunnel. Never serialized: it stays on
  /// this machine.
  #[serde(default, skip_serializing)]
  pub psk: Option<String>,
  /// Write a PROXY protocol v2 header to the backend first.
  #[serde(default)]
  pub proxy_protocol: bool,
  /// UDP only: seconds a relay may sit idle before it expires.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub idle_timeout: Option<u64>,
}

/// One `tunnels:` entry: a private local service (a database, SSH) a peer
/// client may reach with `aperio-client bind`, never exposed to the public
/// web. The key it is written under is its name.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TunnelEntry {
  /// What to call it on screen. Free text: any language, any punctuation,
  /// spaces. Nothing addresses it, so nothing breaks when it changes.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["Primary Postgres"]))]
  pub custom_name: Option<String>,
  /// Local address this client dials when a peer binds the tunnel.
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub target: String,
  /// Transport of the tunnel: `tcp`, `udp` (best-effort datagram
  /// relay), or `tcp/udp` for a service that is genuinely both (DNS, for
  /// instance). A combined tunnel is one tunnel with one name and one local
  /// port on the binder, answering on both transports. Default: `tcp`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["tcp", "udp", "tcp/udp"]))]
  pub protocol: Option<String>,
  /// End-to-end encrypt this tunnel between the two clients (X25519 +
  /// ChaCha20-Poly1305); the server only relays ciphertext. TCP only.
  /// Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub encrypt: Option<bool>,
  /// Pre-shared key mixed into the key derivation of an encrypted tunnel,
  /// protecting against an actively hostile server. Never sent anywhere;
  /// the binder writes the same value in its `binds:` entry.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["${TUNNEL_PSK}"]))]
  pub psk: Option<String>,
  /// Write a PROXY protocol v2 header to this backend before any payload
  /// byte, announcing the visitor's real address. TCP only. Without it the
  /// backend sees a connection from the client process and the visitor's
  /// address is lost at the last hop. Turn it on only when the backend is
  /// configured to expect the header (nginx `listen ... proxy_protocol`,
  /// HAProxy `accept-proxy`, MySQL `proxy-protocol-networks`): a backend that
  /// is not will read it as protocol garbage and drop the connection.
  /// Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub proxy_protocol: Option<bool>,
  /// UDP only: how long a relay may sit with no datagrams in either
  /// direction before it expires; binders learn it via tunnel discovery.
  /// Default: `60s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["5m"]))]
  pub idle_timeout: Option<Span>,
}

impl TunnelEntry {
  /// The declaration this entry announces, under the name it was written
  /// with.
  pub fn decl(&self, name: &str) -> TunnelDecl {
    TunnelDecl {
      name: Some(name.to_string()),
      custom_name: self
        .custom_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string),
      target: self.target.trim().to_string(),
      protocol: self
        .protocol
        .as_deref()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(default_tcp),
      encrypt: self.encrypt.unwrap_or(false),
      psk: self.psk.clone(),
      proxy_protocol: self.proxy_protocol.unwrap_or(false),
      idle_timeout: self.idle_timeout.map(|s| s.as_secs()),
    }
  }
}

/// Header edits applied to one direction of proxied traffic (request or
/// response): `add` sets headers (replacing any existing value of the same
/// name), `remove` strips headers by name (case-insensitive).
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
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
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HeaderRules {
  /// Edits applied to forwarded requests before they reach the local backend.
  #[schemars(extend("examples" = [{"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]}]))]
  pub request: Option<HeaderDirectives>,
  /// Edits applied to backend responses before they return to the visitor.
  #[schemars(extend("examples" = [{"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}]))]
  pub response: Option<HeaderDirectives>,
}

/// Security response-header preset: `true` enables the standard set (HSTS,
/// `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`,
/// `Referrer-Policy: strict-origin-when-cross-origin`), a mapping picks
/// headers individually. Explicit `headers:` rules always win over the
/// preset.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum SecurityHeaders {
  /// `true` enables the standard preset, `false` disables it (e.g. for one
  /// service when `defaults:` enables it).
  Flag(bool),
  /// Granular per-header selection.
  Detailed(SecurityHeaderOptions),
}

/// Individually selected security response headers; only the set fields are
/// injected.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityHeaderOptions {
  /// Inject `Strict-Transport-Security` (only meaningful behind HTTPS).
  #[schemars(extend("examples" = [true]))]
  pub hsts: Option<bool>,
  /// HSTS `max-age`. Default: `730d` (two years).
  #[schemars(extend("examples" = ["365d"]))]
  pub hsts_max_age: Option<Span>,
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
          let max_age = opts
            .hsts_max_age
            .map(|s| s.as_secs())
            .unwrap_or(DEFAULT_HSTS_MAX_AGE);
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

/// Backend health probing for a service: the endpoint the client checks and
/// how patient it is before pulling the backend out of routing.
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
  /// Time between probes. Default: `10s`.
  #[schemars(extend("examples" = ["10s"]))]
  pub interval: Option<Span>,
  /// How long to wait for each probe before counting it as failed.
  /// Default: `5s`.
  #[schemars(extend("examples" = ["5s"]))]
  pub timeout: Option<Span>,
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

/// Retrying a backend request that failed before any response arrived.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
  /// Total attempts, including the first. `1` disables retrying.
  /// Default: the profile's, `2` unless `resilience` says otherwise.
  #[schemars(extend("examples" = [3]))]
  pub attempts: Option<u32>,
  /// Wait before the second attempt, doubled before each further one.
  /// Default: the profile's, `100ms` unless `resilience` is `aggressive`.
  #[schemars(extend("examples" = ["100ms", "1s"]))]
  pub delay: Option<Span>,
  /// Retry non-idempotent methods (POST, PATCH) as well. Off by default,
  /// because a retried write may reach the backend twice; the same reasoning
  /// as the server's `resilience.failover.all_methods`. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub all_methods: Option<bool>,
}

/// Refusing to dial a backend that keeps failing. Answers 502 immediately
/// while open, so a dead backend stops being hammered and the visitor stops
/// waiting for a connection that will not come.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerConfig {
  /// Consecutive failures that open the breaker. `0` disables it.
  /// Default: the profile's, off unless `resilience` is `aggressive`.
  #[schemars(extend("examples" = [5]))]
  pub failures: Option<u32>,
  /// How long the breaker stays open before one request is let through to
  /// test the backend again. Default: `30s`.
  #[schemars(extend("examples" = ["30s"]))]
  pub open_for: Option<Span>,
}

/// How much a service takes on: bodies, concurrency, redirects. The same
/// names as the server's own `limits`, at the scope of one service.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceLimits {
  /// Largest request body visitors may upload to this service; the server
  /// rejects bigger uploads with 413 before they enter the tunnel. Unset:
  /// only the server's own `limits.request_body` applies.
  #[schemars(extend("examples" = ["1MB", "10MB"]))]
  pub request_body: Option<ByteSize>,
  /// Largest response body this client relays to a visitor. Default: `50MB`.
  #[schemars(extend("examples" = ["10MB"]))]
  pub response_body: Option<ByteSize>,
  /// Most requests this service handles at once before the server queues the
  /// rest. Unset: no limit of its own.
  #[schemars(extend("examples" = [8]))]
  pub concurrent_requests: Option<u32>,
  /// Lower the announced `concurrent_requests` while requests queue up
  /// waiting for a local permit, and climb back when they stop. The server
  /// already queues rather than dispatching past the announced number, so
  /// this is how a client that has become slow stops being sent work it
  /// cannot do; the server then holds the request, picks another client, or
  /// asks for capacity through autoscaling, all of which beat a refusal.
  /// Needs `concurrent_requests`, since that is the number being moved.
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub adaptive_concurrency: Option<bool>,
  /// How many backend redirects to follow transparently before passing one
  /// through. Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub redirects: Option<usize>,
}

/// The server-side response cache, as far as one service is concerned. The
/// server has to have `cache.enabled` for any of it to do something.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceCache {
  /// Let the server cache this service's GET responses, per their
  /// `Cache-Control`. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504. Needs `enabled`. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub offline: Option<bool>,
}

/// Autoscaling declaration: the URL the *server* calls when this service needs
/// capacity it does not have. Aperio never starts or stops anything itself, it
/// only signals a desired capacity to an endpoint the operator controls.
///
/// The record outlives the client process on purpose: the whole point of
/// `min: 0` is that the server can call this URL when nothing is running.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(deny_unknown_fields)]
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
  /// How long a visitor request may be held while a cold start completes
  /// (`0s` = do not hold, answer immediately). Default: `45s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["45s"]))]
  pub cold_start: Option<Span>,
  /// Pool utilization above which the server asks for one more instance,
  /// between 0 and 1. Default: `0.8`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [0.8]))]
  pub target_utilization: Option<f64>,
  /// How long utilization must stay above the target before scaling out.
  /// Guards against reacting to a single spike. Default: `15s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["15s"]))]
  pub window: Option<Span>,
  /// Minimum gap between two calls for this bind. A new instance needs time
  /// to appear; without this the server would ask again while it is still
  /// starting. Default: `60s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["60s"]))]
  pub cooldown: Option<Span>,
}

/// The Aperio server this client connects to, and how it gets there.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
  /// URL of the Aperio server this client dials out to.
  #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
  pub url: Option<String>,
  /// Further server URLs to fail over to, tried in order when `url` is
  /// unreachable, for a redundant control plane.
  #[schemars(extend("examples" = [["https://tunnel-b.example.com"]]))]
  pub urls: Option<Vec<String>>,
  /// Tunnel token (master or a scoped dynamic token) that authorizes this
  /// client.
  #[schemars(extend("examples" = ["${APERIO_SERVER_TOKEN}"]))]
  pub token: Option<String>,
  /// Admin API key used by `aperio-client api ...` calls, never by the tunnel
  /// itself.
  #[schemars(extend("examples" = ["apk_xxxxxxxxxxxxxxxx"]))]
  pub api_key: Option<String>,
  /// IP family used to dial the server: `auto` (tries both), `ipv4`, or
  /// `ipv6`. Set `ipv4` when the server hostname resolves to an IPv6 address
  /// this host cannot reach. Default: `auto`.
  #[schemars(extend("x-reload" = "restart", "examples" = ["auto", "ipv4"]))]
  pub ip_family: Option<String>,
  /// HTTP proxy to dial the server through, for a network that allows no
  /// direct outbound connection: `host:port`, or `http://host:port`, with an
  /// optional `user:password@` in front. The client sends `CONNECT` and then
  /// runs TLS inside the tunnel the proxy opens, so the proxy sees the
  /// server's hostname and nothing else. Your own backends are never reached
  /// through it. Unset: dial the server directly.
  #[schemars(extend("x-reload" = "restart", "examples" = ["proxy.corp:3128", "http://user:password@proxy.corp:3128"]))]
  pub proxy: Option<String>,
  /// Lowest TLS version offered when dialing the server over `wss://`: `1.2`
  /// or `1.3`. Unset leaves rustls' own set (1.2 and 1.3) in place, which is
  /// the right default; pin it when a policy has to name the floor rather
  /// than inherit it. A value this client cannot offer is refused at startup.
  #[schemars(extend("x-reload" = "restart", "examples" = ["1.3"]))]
  pub min_tls_version: Option<String>,
  /// Exact cipher suites offered when dialing the server, by their IANA
  /// names, comma-separated. Unset leaves rustls' preference order alone,
  /// which is almost always better. An unknown name is refused at startup.
  #[schemars(extend("x-reload" = "restart", "examples" = ["TLS13_AES_256_GCM_SHA384,TLS13_CHACHA20_POLY1305_SHA256"]))]
  pub tls_cipher_suites: Option<String>,
}

/// The local message faces: where an application on this machine can
/// publish and subscribe without speaking the tunnel protocol.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessagesSection {
  /// Local address the HTTP face listens on: subscribe with SSE, publish
  /// with POST. Unset = no local listener.
  #[schemars(extend("examples" = ["127.0.0.1:1888"]))]
  pub listen: Option<String>,
  /// Local address an MQTT listener answers on, for an application that would
  /// rather use the MQTT client library it already has. MQTT 3.1.1, QoS 0;
  /// the protocol never leaves this machine. Unset = no MQTT listener.
  #[schemars(extend("examples" = ["127.0.0.1:1883"]))]
  pub mqtt_listen: Option<String>,
}

/// A service's public hostname(s): one, or a list routing to the same
/// service. Each must be permitted by the client's token.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum Hostnames {
  /// A single hostname.
  One(String),
  /// Several hostnames routing to the same service.
  Many(Vec<String>),
}

impl Hostnames {
  /// Flattens to a list of trimmed, lowercased, non-empty hostnames.
  pub fn into_vec(self) -> Vec<String> {
    let raw = match self {
      Hostnames::One(h) => vec![h],
      Hostnames::Many(hs) => hs,
    };
    raw
      .into_iter()
      .map(|h| h.trim().to_ascii_lowercase())
      .filter(|h| !h.is_empty())
      .collect()
  }
}

/// `connections:` as written in the file: a fixed count, or an elastic pool
/// with a floor and a ceiling.
///
/// A number opens exactly that many connections at startup and keeps them. A
/// range opens the floor and grows towards the ceiling under load. Unwritten,
/// a service on a connection of its own runs a small range, one to four: our
/// own measurements have a peak past which more connections cost more than
/// they return, and four is on the near side of it.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum Connections {
  /// `connections: 4`, opened at startup and kept.
  Fixed(u32),
  /// `connections: {min: 1, max: 8}`, grown and shrunk with load.
  Range(ConnectionRange),
}

/// The `{min, max}` spelling of `connections:`.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
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

/// What a service does with its traffic, as opposed to where the traffic
/// comes from and goes to. This is the shape of `defaults:`, and every
/// `services:` entry carries the same keys, so a value written once under
/// `defaults:` reaches every service that does not write its own.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
pub struct ServicePolicy {
  /// Who may reach the service: `public`, one method block, or a list of
  /// them (any one admits). Unset, the server's own `access` decides, and a
  /// site's `access` on the server wins over this either way.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["public", {"method": "basic", "users": "admin:${ADMIN_PASSWORD}"}]))]
  pub access: Option<Access>,
  /// Visitor IPs/CIDRs allowed to reach the service; unset = everyone.
  /// Enforced by the server before dispatch.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["203.0.113.7", "10.0.0.0/8"]]))]
  pub allowed_ips: Option<Vec<String>>,
  /// Redirect URL for visitors refused by `allowed_ips` when no other
  /// client of the route admits them. Unset = stealth: the same answer as
  /// an unclaimed route.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["https://example.com/not-for-you"]))]
  pub denied: Option<String>,
  /// How long to wait for the backend to answer before failing the request.
  /// Default: `30s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["30s"]))]
  pub timeout: Option<Span>,
  /// How long to wait for the TCP connection to the backend, separate from
  /// `timeout`, which covers the whole request. A backend across a VPN needs
  /// longer than one on loopback. Unset: only `timeout` applies.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["2s"]))]
  pub connect_timeout: Option<Span>,
  /// How long the server waits for this service to answer a dispatched
  /// request before failing it: the service's own `gateway.response_timeout`,
  /// for slow report and upload endpoints. Unset: the server's applies.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["2m"]))]
  pub response_timeout: Option<Span>,
  /// Bodies, concurrency and redirects.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"request_body": "10MB", "concurrent_requests": 8}]))]
  pub limits: Option<ServiceLimits>,
  /// Backend health probing.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"endpoint": "/health", "interval": "10s", "threshold": 2}]))]
  pub health: Option<HealthConfig>,
  /// How hard this service works to answer when its backend fails: `off`,
  /// `safe` (the default: an idempotent request that fails before any
  /// response is tried once more) or `aggressive` (three attempts, and a
  /// circuit breaker). `retry` and `circuit_breaker`, where written, win
  /// over the profile's values.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["safe", "aggressive", "off"]))]
  pub resilience: Option<ResilienceProfile>,
  /// Retrying backend requests that fail before a response arrives.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"attempts": 3, "delay": "100ms"}]))]
  pub retry: Option<RetryConfig>,
  /// Refusing to dial a backend that keeps failing.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"failures": 5, "open_for": "30s"}]))]
  pub circuit_breaker: Option<CircuitBreakerConfig>,
  /// Request and response header edits.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{
    "request": {"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]},
    "response": {"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}
  }]))]
  pub headers: Option<HeaderRules>,
  /// Security response-header preset: `true` injects HSTS,
  /// `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and
  /// `Referrer-Policy`; a mapping picks headers individually.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true, {"hsts": true, "frame_options": "SAMEORIGIN"}]))]
  pub security_headers: Option<SecurityHeaders>,
  /// The server-side response cache, for this service.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"enabled": true, "offline": true}]))]
  pub cache: Option<ServiceCache>,
  /// Record this service's transactions for the server's request
  /// inspector. Turning it off for a service that carries heavy traffic buys
  /// back a mutex, two header clones and a capture entry per request, at the
  /// cost of not being able to inspect or replay its requests afterwards.
  /// Default: `true`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [false]))]
  pub inspector: Option<bool>,
  /// Persist inbound POST requests (third-party webhooks) into the server's
  /// webhook inbox, for browsing and re-firing. Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub webhook_inbox: Option<bool>,
  /// Forward the visitor's original Host header to the backend instead of
  /// the target's. Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub pass_hostname: Option<bool>,
  /// Strip the `path` prefix before forwarding, so the backend sees `/` and
  /// not the bind. Default: `true` when `path` is set.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [false]))]
  pub trim_bind: Option<bool>,
  /// Failover tier (0 = primary, higher numbers are standbys). Default: `0`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [0, 1]))]
  pub priority: Option<u32>,
  /// Parallel tunnel connections; the server's
  /// `limits.connections_per_service` is the ceiling, announced on connect,
  /// and a token may lower it further. The server load-balances across them
  /// like separate clients, so a single dropped connection leaves no
  /// visitor-facing gap. A number opens exactly that many at startup;
  /// `{min: 1, max: 8}` opens the floor and grows towards the ceiling while
  /// requests queue up, then shrinks back when they stop. Default:
  /// `{min: 1, max: 4}` for a service on a connection of its own, and `1` for
  /// one that shares a connection or whose bandwidth is paced, since the
  /// rate is divided across the pool's ceiling. A connection the pool grew
  /// that the server refuses as full stands down rather than waiting for room.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [2, {"min": 1, "max": 8}]))]
  pub connections: Option<Connections>,
  /// Carry the service on one WebSocket shared with the client's other
  /// services, instead of a connection of its own.
  ///
  /// What it saves is the whole cost of a connection per service: one
  /// socket, one TLS session, one reader, one writer and one heartbeat. What
  /// it costs is that the services then share a link, so a large response
  /// occupies the writer the others send through, and a dropped connection
  /// takes all of them down together. Two services are needed for it to do
  /// anything, and `connections:` is not honored for a service that asks for
  /// it, one connection is what multiplexing means. Default: shared, for one
  /// of several services that writes nothing a shared socket would change (a
  /// `connections:` above one, a `bandwidth:` of its own, `startup_delay:`,
  /// `depends_on:` or `tcp_target:`); `false` keeps a connection of its own.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [false]))]
  pub multiplex: Option<bool>,
  /// Static Prometheus labels on the service's metric series, e.g.
  /// `{env: prod, region: eu-west}`, so one Prometheus can serve several
  /// environments without relabelling rules. At most 8 labels; names must be
  /// `[a-zA-Z_][a-zA-Z0-9_]*` and the server drops anything else, including
  /// the names it writes itself (`client_id`, `job`, `instance`, …).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"env": "prod", "region": "eu-west"}]))]
  pub metrics_labels: Option<BTreeMap<String, String>>,
  /// Lowest TLS version accepted from an `https://` backend: `1.2` or `1.3`.
  /// Default: rustls' own floor.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["1.3"]))]
  pub min_tls_version: Option<String>,
  /// Wait this long before the service opens its tunnel, for a backend
  /// starting alongside the client that is not ready the moment the process
  /// is. Default: `0s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["5s"]))]
  pub startup_delay: Option<Span>,
  /// Let the server reach `target` itself instead of relaying through this
  /// client, when the server's `security.server_side_targets` permits that
  /// address (needs a token that allows it). Saves the two hops a relayed
  /// request makes, and is only useful when the target is somewhere the
  /// server can already reach. Refused rather than ignored when the target
  /// is not permitted, and cannot be combined with `serve:`, whose files are
  /// on this machine. Default: `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [true]))]
  pub server_side: Option<bool>,
}

/// One exposed backend. The key it is written under is its name: a-z, 0-9
/// and `_`, the handle the dashboard and the logs use. Every key of
/// `defaults:` is accepted here as well and wins over it.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
pub struct ServiceEntry {
  /// Local backend to expose: a port (`3000`), a host, or a URL. `h2c://` /
  /// `h2://` targets are dialed over HTTP/2 (gRPC backends, trailers
  /// relayed); `unix://` targets forward over a Unix domain socket.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["http://localhost:3000", "3000", "h2c://127.0.0.1:50051", "unix:///var/run/app.sock"]))]
  pub target: Option<String>,
  /// Serve a local directory of static files instead of forwarding to a
  /// backend (instead of `target`); directories serve their `index.html`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["./dist"]))]
  pub serve: Option<String>,
  /// Raw TCP backend instead of HTTP (instead of `target`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["127.0.0.1:5432"]))]
  pub tcp_target: Option<String>,
  /// Public hostname(s) that route to the service: one, or a list. Each must
  /// be permitted by the client's token. `*` serves every hostname no other
  /// service claims, and needs a token that is not limited to particular
  /// hostnames. A service naming none is reached only by a hostname its token
  /// grants, or a random subdomain the server hands out.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["app.example.com", ["app.example.com", "www.example.com"], "*"]))]
  pub hostname: Option<Hostnames>,
  /// Public path prefix that routes to the service.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["/api"]))]
  pub path: Option<String>,
  /// What to call the service on screen. Free text: any language, any
  /// punctuation, spaces. Nothing addresses it, so nothing breaks when it
  /// changes.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["Public Web"]))]
  pub custom_name: Option<String>,
  /// Services in this file that must have a live tunnel before this one
  /// opens its own. Waits for them, then proceeds regardless after a bounded
  /// grace period, because a dependency that never arrives must not keep a
  /// service that could serve traffic off the air forever.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["api"]]))]
  pub depends_on: Option<Vec<String>>,
  /// The service's share of the link: the server never pushes it faster
  /// than this, and the share is split across its `connections`. Settled
  /// against the top-level `bandwidth` budget when there is one. Bit
  /// suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte suffixes
  /// (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["8mbit", "500kbit"]))]
  pub bandwidth: Option<String>,
  /// Everything `defaults:` can say, for this service alone.
  #[serde(flatten)]
  pub policy: ServicePolicy,
}

/// One `subscribe:` entry.
///
/// A bare filter is the short form: listen, and let whatever is attached to
/// the local face receive it. The object form is for a client that should
/// *act* on the message itself.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
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
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct SubscribeEntry {
  /// The topic filter to subscribe to.
  #[schemars(extend("examples" = ["deploy/web", "$aperio/client/#"]))]
  pub topic: String,
  /// Command to run for each message, through the shell. The message body
  /// arrives on **stdin**, never as an argument; `APERIO_MESSAGE_TOPIC` and
  /// `APERIO_MESSAGE_ID` are set in the environment. Unset = deliver only.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["./deploy.sh", "systemctl reload nginx"]))]
  pub run: Option<String>,
  /// Extra environment variables for `run:`, on top of what the client sets.
  /// The command inherits the client's own environment as well; this is for
  /// the values that belong to *this* subscription. `APERIO_MESSAGE_TOPIC`
  /// and `APERIO_MESSAGE_ID` are set after these and cannot be overridden,
  /// since a command reading them must be able to trust what it finds.
  #[schemars(extend("examples" = [{"DEPLOY_ENV": "staging", "SLACK_CHANNEL": "#ops"}]))]
  #[serde(default, skip_serializing_if = "HashMap::is_empty")]
  pub env: HashMap<String, String>,
  /// How long a run may take before it is killed. A command that hangs must
  /// not hold the subscription's one slot forever. Default: `60s`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["2m"]))]
  pub timeout: Option<Span>,
  /// Runs allowed at once for this subscription. Messages that
  /// arrive while the cap is reached are dropped with a warning rather than
  /// queued: a queue for a command that cannot keep up is the same problem
  /// one step later. Default: `1`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [1]))]
  pub max_concurrent: Option<u32>,
}

/// One `binds:` entry, keyed by the tunnel's name (`pg_main`, or
/// `acme@pg_main` for another organization's).
///
/// A bare port number is the short form of `{port: <n>}`, since naming the
/// local port is the only thing most entries do.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum BindValue {
  /// `pg_main: 15432`, the local port, everything else defaulted.
  Port(u16),
  /// The full entry.
  Entry(BindEntry),
}

impl BindValue {
  /// The entry form, so callers do not branch on the spelling.
  pub fn entry(&self) -> BindEntry {
    match self {
      BindValue::Port(port) => BindEntry {
        port: Some(*port),
        ..BindEntry::default()
      },
      BindValue::Entry(entry) => entry.clone(),
    }
  }
}

/// Where and how to bind one named tunnel.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindEntry {
  /// Local port for this tunnel. Unset reuses the declared target's port when
  /// it is free and unprivileged, and otherwise picks a stable port derived
  /// from the tunnel's name (logged at startup).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [15432]))]
  pub port: Option<u16>,
  /// Local address the listener binds; anything but the default puts a
  /// deliberately unexposed service on the network and is warned about.
  /// Default: `127.0.0.1`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["127.0.0.1"]))]
  pub address: Option<String>,
  /// Token to bind with, when the tunnel is reached with a different
  /// credential than `server.token`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["${BINDER_TOKEN}"]))]
  pub token: Option<String>,
  /// Pre-shared key of an end-to-end encrypted tunnel; must match the `psk`
  /// the declaring client wrote. Never sent to the server.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["${TUNNEL_PSK}"]))]
  pub psk: Option<String>,
}

/// Services, tunnels and binds are maps keyed by name, kept in file order.
pub type Named<T> = IndexMap<String, T>;
