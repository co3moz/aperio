//! The `aperio.yaml` client configuration schema.
//!
//! These are the exact types `aperio-client` deserializes its config file into.
//! They live in their own crate so the client's build script can emit a JSON
//! Schema (`schemars`) straight from them, the editor schema and the parser can
//! never drift apart. The doc comments below become the `description` of each
//! field in the generated schema, so they double as the `aperio.yaml` reference;
//! keep them to a single purposeful sentence and add `examples` where the value
//! has a specific format.

pub mod authoring;
pub mod compat;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serde default protocol of a declared tunnel.
fn default_tcp() -> String {
  "tcp".to_string()
}

/// The combined declaration: one tunnel reachable over both transports.
pub const PROTOCOL_BOTH: &str = "tcp/udp";

/// Does the topic filter `filter` match the concrete topic `topic`?
///
/// MQTT's filter syntax, because it is the one people already know and the
/// local MQTT face will eventually have to honour it exactly: `+` matches one
/// level, `#` matches the rest and is only legal last. Levels are separated by
/// `/`, and a filter with neither wildcard is an exact match.
///
/// Lives here rather than in the server because both ends need the same
/// answer: the server routes with it, and the client matches deliveries
/// against what each locally attached subscriber asked for.
pub fn topic_matches(filter: &str, topic: &str) -> bool {
  // `$`-prefixed topics are the server's own namespace. A leading wildcard
  // must not sweep them up, the way MQTT keeps `#` away from `$SYS`:
  // subscribing to everything should not silently enroll you in infrastructure
  // events you did not ask to parse.
  if topic.starts_with('$') && !filter.starts_with('$') {
    return false;
  }
  let mut f = filter.split('/');
  let mut t = topic.split('/');
  loop {
    match (f.next(), t.next()) {
      (Some("#"), Some(_)) => return true,
      // `a/#` also matches `a` itself, as MQTT specifies: the parent is part
      // of what the subtree filter selects.
      (Some("#"), None) => return true,
      (Some("+"), Some(_)) => continue,
      (Some(fl), Some(tl)) if fl == tl => continue,
      (None, None) => return true,
      _ => return false,
    }
  }
}

/// Is `filter` a usable topic filter? Rejects the shapes that would otherwise
/// match nothing and look like a typo working.
pub fn validate_topic_filter(filter: &str) -> Result<(), String> {
  if filter.is_empty() {
    return Err("a topic filter cannot be empty".to_string());
  }
  if filter.len() > 512 {
    return Err(format!("topic filter is too long ({} > 512)", filter.len()));
  }
  let levels: Vec<&str> = filter.split('/').collect();
  for (i, level) in levels.iter().enumerate() {
    if level.contains('#') && *level != "#" {
      return Err(format!(
        "`#` must be a level of its own, not part of `{level}`"
      ));
    }
    if level.contains('+') && *level != "+" {
      return Err(format!(
        "`+` must be a level of its own, not part of `{level}`"
      ));
    }
    if *level == "#" && i + 1 != levels.len() {
      return Err("`#` is only allowed as the last level of a filter".to_string());
    }
  }
  Ok(())
}

/// Is `topic` usable as a published topic? Wildcards are for filters only:
/// publishing to `a/#` would otherwise look like a broadcast and reach nobody.
pub fn validate_topic(topic: &str) -> Result<(), String> {
  if topic.is_empty() {
    return Err("a topic cannot be empty".to_string());
  }
  if topic.len() > 512 {
    return Err(format!("topic is too long ({} > 512)", topic.len()));
  }
  if topic.contains('#') || topic.contains('+') {
    return Err("a published topic cannot contain `#` or `+`; those are filter syntax".to_string());
  }
  Ok(())
}

/// The namespace the server publishes its own events under, closed to clients
/// the way MQTT reserves `$SYS`. A client may subscribe to it and may not
/// publish into it, so an infrastructure event always means what it says.
pub const RESERVED_TOPIC_PREFIX: &str = "$aperio/";

/// Largest message payload, before Base64. Shared by both ends so a client
/// can refuse what the server would refuse, instead of answering "accepted"
/// for something that is about to be dropped where nobody is looking.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Is this a topic a *client* may publish on? Rejects a filter, and the
/// server's own namespace, which a client may listen to but never write to.
pub fn validate_publish_topic(topic: &str) -> Result<(), String> {
  validate_topic(topic)?;
  if topic.starts_with(RESERVED_TOPIC_PREFIX) {
    return Err(format!(
      "`{RESERVED_TOPIC_PREFIX}` is the server's own namespace and cannot be published into"
    ));
  }
  Ok(())
}

/// Does a declared `protocol` serve `want` (`tcp` or `udp`)?
///
/// A tunnel may declare `tcp/udp`, which is one tunnel with one name and one
/// local port that answers on both transports. DNS is the reason: port 53 is
/// genuinely both, and writing it as two declarations meant two names and two
/// entries in every binder for what an operator thinks of as one thing.
pub fn protocol_serves(protocol: &str, want: &str) -> bool {
  let protocol = protocol.trim().to_ascii_lowercase();
  protocol == want || protocol == PROTOCOL_BOTH
}

/// True when a string is shaped like the UUID a client id is.
///
/// Tunnel names and client ids share one key space in `bind-tunnels:`, where a
/// key is read as a tunnel name and falls back to a peer's client id. The two
/// shapes cannot collide: a UUID carries `-`, which a name may not, so this
/// only has to recognize the id form rather than defend the name space.
pub fn looks_like_client_id(raw: &str) -> bool {
  let s = raw.trim();
  // 8-4-4-4-12 hex with hyphens, the only form the client emits.
  s.len() == 36
    && s.chars().enumerate().all(|(i, c)| match i {
      8 | 13 | 18 | 23 => c == '-',
      _ => c.is_ascii_hexdigit(),
    })
}

/// Characters a name may contain. Everything else is reserved.
///
/// Lowercase ASCII, digits and `_`, and nothing else, because a name is an
/// **identifier** rather than a label: it is written in one file and read in
/// another, typed into a command line, and joined with other names to form an
/// address (`payments@postgres`). Every character outside this set is a way
/// for two people to write down what they think is the same name and be
/// wrong: `Postgres` and `postgres`, `pg_main` and `pg_main`, an `i` that is
/// actually `ı`.
///
/// What is left out is left out on purpose. `-` and `.` and `*` and `@` carry
/// no meaning in a name today, which is exactly what keeps them available to
/// carry meaning in an *address* tomorrow: `*@postgres`, `acme.*@postgres`.
/// A character that is allowed inside a name can never become syntax around
/// one.
pub const NAME_CHARS: &str = "a-z, 0-9 and _";

/// Longest name accepted. Long enough to be descriptive, short enough to stay
/// readable in a table, a log line and a command.
pub const MAX_NAME_LEN: usize = 64;

/// Rejects a name that cannot be used as an identifier.
///
/// `kind` names what is being validated ("tunnel", "service", "organization")
/// so the message is about the thing the operator wrote rather than about a
/// rule in the abstract.
pub fn validate_name(kind: &str, name: &str) -> Result<(), String> {
  let trimmed = name.trim();
  if trimmed.is_empty() {
    return Err(format!("a {kind} name cannot be empty"));
  }
  if trimmed.chars().count() > MAX_NAME_LEN {
    return Err(format!(
      "{kind} name '{trimmed}' is longer than {MAX_NAME_LEN} characters"
    ));
  }
  if !trimmed
    .chars()
    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
  {
    // The suggestion is the point of the message: almost every rejection is a
    // capital letter, a hyphen or a dot, and the fix is mechanical.
    return Err(format!(
      "{kind} name '{trimmed}' may only contain {NAME_CHARS} (write it as '{}')",
      slug(trimmed)
    ));
  }
  Ok(())
}

/// The ASCII letter a Latin one stands on, for suggesting a handle.
///
/// Only the letters that are one letter wearing a mark, plus the Turkish
/// dotless `ı` and the German `ß`, which are their own letters but have one
/// obvious ASCII reading. Everything else becomes a separator: a suggestion
/// is meant to be recognizable, not to guess at a script it cannot read.
fn fold_latin(ch: char) -> Option<&'static str> {
  Some(match ch {
    'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => "a",
    'ç' | 'ć' | 'č' => "c",
    'é' | 'è' | 'ê' | 'ë' | 'ē' => "e",
    'ğ' | 'ĝ' => "g",
    'í' | 'ì' | 'î' | 'ï' | 'ī' | 'ı' => "i",
    'ñ' | 'ń' => "n",
    'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => "o",
    'ś' | 'š' | 'ş' => "s",
    'ú' | 'ù' | 'û' | 'ü' | 'ū' => "u",
    'ý' | 'ÿ' => "y",
    'ź' | 'ż' | 'ž' => "z",
    'ß' => "ss",
    'æ' => "ae",
    'œ' => "oe",
    _ => return None,
  })
}

/// Turns any string into a valid name, for deriving one and for suggesting a
/// correction. Not a validator: it always succeeds, and never silently stands
/// in for a name the operator wrote, a suggestion is shown, and whoever is
/// naming the thing accepts or replaces it.
pub fn slug(raw: &str) -> String {
  let mut out = String::new();
  for ch in raw.trim().chars() {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    if lower.is_ascii_alphanumeric() {
      out.push(lower);
    } else if let Some(folded) = fold_latin(lower) {
      out.push_str(folded);
    } else if !out.ends_with('_') {
      out.push('_');
    }
  }
  let out = out.trim_matches('_').to_string();
  if out.is_empty() {
    return "unnamed".to_string();
  }
  out.chars().take(MAX_NAME_LEN).collect()
}

/// The name a tunnel is addressed by: what it declared, or one derived from
/// its target and protocol so an unnamed tunnel still has a stable handle
/// (`127.0.0.1:5432` tcp becomes `127_0_0_1_5432_tcp`).
///
/// Derivation lives here rather than in the client so the server, the client
/// and the config builder all spell the same tunnel the same way.
pub fn tunnel_name(decl: &TunnelDecl) -> String {
  if let Some(name) = decl
    .name
    .as_ref()
    .map(|n| n.trim())
    .filter(|n| !n.is_empty())
  {
    return name.to_string();
  }
  // `tcp/udp` would put a slash in the name; the slug folds it, so a combined
  // tunnel stays addressable.
  slug(&format!(
    "{} {}",
    decl.target,
    decl.protocol.trim().to_ascii_lowercase()
  ))
}

/// Rejects a tunnel name that cannot be used as a handle. `Ok(())` when the
/// name is usable (including when none was given).
pub fn validate_tunnel_name(name: &str) -> Result<(), String> {
  validate_name("tunnel", name)
}

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

/// One `expose:` entry of `aperio-server.yaml`: a raw public TCP port the
/// server relays into a client's declared tunnel, with no binder peer.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposeEntry {
  /// Transport of the exposed port; only `tcp` is supported. A public UDP
  /// port is an amplification surface and is a separate decision.
  /// Default: `tcp`.
  #[serde(default = "default_tcp")]
  #[schemars(extend("examples" = ["tcp"]))]
  pub protocol: String,
  /// Public port the server listens on.
  #[schemars(extend("examples" = [2222]))]
  pub port: u16,
  /// Name of the tunnel this port is relayed into. Preferred over `key`:
  /// the claim is settled by identity (which organization declared the
  /// tunnel) rather than by a secret copied into two files. May be written
  /// as `<org>@<name>`, e.g. `payments@postgres`, which says the same thing
  /// as a separate `org:` and reads the way the dashboard shows it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["ssh_bastion", "payments@postgres"]))]
  pub tunnel: Option<String>,
  /// Name of the organization whose client may claim this port. A tunnel name
  /// is unique inside an organization and nowhere else, so this is what makes
  /// the claim unambiguous. Unset (with no `<org>@` prefix and no `token`)
  /// means the master organization.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["payments"]))]
  pub org: Option<String>,
  /// Name of the token whose client may claim this port. Superseded by `org`:
  /// a token name is not unique across organizations, so a rule naming one
  /// can match a client of another organization, and which one gets the port
  /// is not defined. Still honored; write `org` instead.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["bastion-host"], "deprecated" = true))]
  pub token: Option<String>,
  /// Deprecated spelling: a shared secret the client's tunnel declaration
  /// repeats as `expose: <key>`. Still honored, but it names no owner, cannot
  /// be revoked, and lives in plaintext in two files; prefer `tunnel` + `token`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["k5fj2q-expose-secret"]))]
  pub key: Option<String>,
}

/// One `rate_limits:` entry: an aggregate requests-per-second ceiling for a
/// hostname and/or path prefix, independent of the per-visitor IP limit.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RateLimitRule {
  /// Hostname the rule applies to (omit for any host).
  #[schemars(extend("examples" = ["api.example.com"]))]
  pub hostname: Option<String>,
  /// Path prefix the rule applies to (omit for any path).
  #[schemars(extend("examples" = ["/api"]))]
  pub path: Option<String>,
  /// Sustained requests per second allowed to the route.
  #[schemars(extend("examples" = [50.0]))]
  pub rps: f64,
  /// Burst capacity. Default: the `rps` value (one second's worth).
  #[schemars(extend("examples" = [100.0]))]
  pub burst: Option<f64>,
  /// HTTP methods the rule applies to (omit for every method). Lets a write
  /// path be limited without throttling reads of the same route.
  #[schemars(extend("examples" = [["POST", "PUT", "DELETE"]]))]
  pub methods: Option<Vec<String>>,
}

/// One `fallbacks:` entry: where to send visitors of a hostname no client
/// currently serves, instead of answering with a gateway error.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FallbackRule {
  /// Hostname to catch, or `*` for every unserved hostname.
  #[schemars(extend("examples" = ["app.example.com", "*"]))]
  pub hostname: String,
  /// URL visitors are redirected to.
  #[schemars(extend("examples" = ["https://status.example.com"]))]
  pub url: String,
  /// Answer `308` instead of `307`, i.e. let clients cache the redirect.
  /// Default: `false`.
  #[serde(default)]
  pub permanent: bool,
  /// Append the requested path to the target URL. Default: `false`.
  #[serde(default)]
  pub preserve_path: bool,
}

/// A header match inside a `waf:` rule.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WafHeaderMatch {
  /// Header name to inspect (case-insensitive).
  #[schemars(extend("examples" = ["user-agent"]))]
  pub name: String,
  /// Regular expression the header value must match for the rule to fire.
  #[schemars(extend("examples" = ["(?i)sqlmap|nikto"]))]
  pub regex: String,
}

/// One `waf:` entry: a request is denied with `403` when every set condition
/// matches, or answered `413` when `max_body` is exceeded.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WafRule {
  /// Regular expression matched against the request path.
  #[schemars(extend("examples" = ["^/wp-admin"]))]
  pub path: Option<String>,
  /// HTTP methods the rule applies to (omit for any).
  #[schemars(extend("examples" = [["POST", "PUT"]]))]
  pub methods: Option<Vec<String>>,
  /// Header condition the request must match.
  #[schemars(extend("examples" = [{"name": "user-agent", "regex": "(?i)sqlmap|nikto"}]))]
  pub header: Option<WafHeaderMatch>,
  /// Body-size ceiling in bytes for the matched route; makes this a `413`
  /// size rule rather than a `403` deny rule.
  #[schemars(extend("examples" = [1048576]))]
  pub max_body: Option<usize>,
}

/// The fixed response of a client-less `respond` route.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RespondRule {
  /// HTTP status to answer with. Default: `200`.
  #[schemars(extend("examples" = [503]))]
  pub status: Option<u16>,
  /// `Content-Type` of the response body.
  #[schemars(extend("examples" = ["text/html; charset=utf-8"]))]
  pub content_type: Option<String>,
  /// Response body.
  #[schemars(extend("examples" = ["<h1>Coming soon</h1>"]))]
  pub body: Option<String>,
}

/// Retrying a backend request that failed before any response arrived
/// (`retry:` on a service, or at the top level as the default for every
/// entry).
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
  /// Total attempts, including the first. `1` disables retrying.
  /// Default: `1`.
  #[schemars(extend("examples" = [3]))]
  pub attempts: Option<u32>,
  /// Milliseconds to wait before the second attempt, doubled before each
  /// further one. Default: `100`.
  #[schemars(extend("examples" = [100]))]
  pub backoff: Option<u64>,
  /// Retry non-idempotent methods (POST, PATCH) as well. Off by default,
  /// because a retried write may reach the backend twice; the same reasoning
  /// as the server's `failover.all_methods`. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub all_methods: Option<bool>,
}

/// Refusing to dial a backend that keeps failing (`circuit_breaker:` on a
/// service, or at the top level as the default for every entry). Answers 502
/// immediately while open, so a dead backend stops being hammered and the
/// visitor stops waiting for a connection that will not come.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerConfig {
  /// Consecutive failures that open the breaker. `0` disables it.
  /// Default: `0` (off).
  #[schemars(extend("examples" = [5]))]
  pub failures: Option<u32>,
  /// Seconds the breaker stays open before one request is let through to
  /// test the backend again. Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub open_for: Option<u64>,
}

/// Request-id correlation (`request_id:`): the id the server already assigns
/// every proxied request, made visible to the backend and to the visitor.
#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestIdGroup {
  /// Send the id to the backend and echo it on the response. On by default
  /// (env: APERIO_REQUEST_ID).
  #[schemars(extend("examples" = [false]))]
  pub enabled: Option<bool>,
  /// Header carrying it. Default: `x-request-id`
  /// (env: APERIO_REQUEST_ID_HEADER).
  #[schemars(extend("examples" = ["x-correlation-id"]))]
  pub header: Option<String>,
  /// Adopt the visitor's own value when the request already carries one,
  /// instead of ignoring it. Off by default: the header is attacker-supplied,
  /// so trusting it lets a visitor choose what appears in your logs and in
  /// your backend's. Turn it on behind a proxy that sets the header itself.
  /// Default: `false` (env: APERIO_REQUEST_ID_TRUST_INBOUND).
  #[schemars(extend("examples" = [true]))]
  pub trust_inbound: Option<bool>,
}

/// One `alert_rules:` entry: a quantity the server measures, a bound, and how
/// long the condition must hold. A list of mappings, so it has no
/// environment-variable equivalent.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlertRule {
  /// Names the alert. It becomes the event's `kind`, which is what a webhook
  /// receiver switches on, so it has to be unique.
  #[schemars(extend("examples" = ["disk-filling"]))]
  pub name: String,
  /// What to watch: `connected_clients`, `pending_requests`, `store_bytes`
  /// (the SQLite store and its sidecars on disk), or `rss_bytes` (the server
  /// process's resident memory, Linux only).
  #[schemars(extend("examples" = ["store_bytes"]))]
  pub metric: String,
  /// Fire while the value is strictly above this. Set this or `below`.
  #[schemars(extend("examples" = [536870912]))]
  pub above: Option<f64>,
  /// Fire while the value is strictly below this. Set this or `above`.
  #[schemars(extend("examples" = [1]))]
  pub below: Option<f64>,
  /// Seconds the condition must hold before firing, and hold clear before
  /// resolving. Default `0` = react on the first observation. Both directions
  /// use it, so a value sitting on its threshold cannot alert every tick.
  #[serde(rename = "for")]
  #[schemars(extend("examples" = [300]))]
  pub r#for: Option<u64>,
}

/// One `maintenance_windows:` entry: a recurring window during which matching
/// hostnames answer the maintenance page by themselves.
///
/// A list of mappings, so it has no environment-variable equivalent, like
/// `routes:` and `rate_limits:`.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceWindow {
  /// Hostname or pattern the window applies to, in the same shapes the
  /// maintenance API takes (`app.example.com`, `*.example.com`,
  /// `*-pi.example.com`). Omitted or `*` = every hostname on the server.
  #[schemars(extend("examples" = ["*.example.com"]))]
  pub hostname: Option<String>,
  /// Local start time, `HH:MM`.
  #[schemars(extend("examples" = ["02:00"]))]
  pub from: String,
  /// Local end time, `HH:MM`, exclusive. Earlier than `from` means the window
  /// wraps past midnight, and `days` then names the day it *starts*.
  #[schemars(extend("examples" = ["04:00"]))]
  pub to: String,
  /// Weekdays the window runs on (`mon`..`sun`, long names accepted).
  /// Omitted = every day.
  #[schemars(extend("examples" = [["sat", "sun"]]))]
  pub days: Option<Vec<String>>,
  /// IANA time zone the times are local to. Default: `UTC`. Use a named zone
  /// rather than a fixed offset so the window stays put across a
  /// daylight-saving change.
  #[schemars(extend("examples" = ["Europe/Istanbul"]))]
  pub tz: Option<String>,
  /// Shown on the maintenance page and in the dashboard while the window is
  /// running.
  #[schemars(extend("examples" = ["weekly patching"]))]
  pub reason: Option<String>,
}

/// One `routes:` entry of `aperio-server.yaml`. Either an *answer* rule, a
/// hostname/path match paired with exactly one action (`redirect` or
/// `respond`) served without a client, or a *policy* rule, which has neither
/// action and instead carries settings (`timeout`, `headers`, `rate_limit`)
/// that apply to proxied traffic matching it. The two kinds are matched
/// independently, each first-match in file order.
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
  /// Use a permanent 301 instead of the default 302. Default: `false`.
  #[serde(default)]
  pub permanent: bool,
  /// Append the request's path and query to the redirect target.
  /// Default: `false`.
  #[serde(default)]
  pub preserve_path: bool,
  /// Serve a fixed response instead of redirecting.
  #[schemars(extend("examples" = [{"status": 503, "body": "Be right back", "content_type": "text/plain"}]))]
  pub respond: Option<RespondRule>,
  /// Seconds to wait for the serving client's answer on this route, overriding
  /// `gateway.response_timeout` and any per-service `response_timeout` the
  /// client declared. Policy field: only valid on an entry that has neither
  /// `redirect` nor `respond`, since those never reach a backend.
  #[schemars(extend("examples" = [120]))]
  pub timeout: Option<u64>,
  /// Header edits for this route only, applied after the server-wide
  /// `headers:` rules so a route can override them. Policy field.
  #[schemars(extend("examples" = [{"response": {"add": {"cache-control": "public, max-age=3600"}}}]))]
  pub headers: Option<HeaderRules>,
  /// Rate limit for this route only, so the hostname and path are written
  /// once instead of repeated under `rate_limits:`. Wins over any
  /// `rate_limits:` entry matching the same request. Policy field.
  #[schemars(extend("examples" = [{"rps": 10, "burst": 20, "methods": ["POST"]}]))]
  pub rate_limit: Option<RouteRateLimit>,
  /// Split this route's traffic between two versions of a service. Policy
  /// field.
  #[schemars(extend("examples" = [{"service": "web-v2", "weight": 20, "header": "x-canary"}]))]
  pub canary: Option<RouteCanary>,
}

/// A `canary:` block inside a `routes:` entry: which service gets the new
/// version's traffic, how much of it, and how somebody opts in by hand.
///
/// Weighted routing and a header-based canary are the same mechanism seen from
/// two angles, which is why they are one block rather than two settings.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteCanary {
  /// Name of the `services:` entry serving the new version. Clients that
  /// announce this service name are the canary side; every other client
  /// serving the route is the stable side.
  #[schemars(extend("examples" = ["web-v2"]))]
  pub service: String,
  /// Percentage of visitors sent there without asking, `0` to `100`. Default:
  /// `0`, which with a `header` set is the opt-in-only shape.
  ///
  /// The split is decided per **visitor**, by hashing their address, not per
  /// request: a per-request coin flip would send one page load's twenty assets
  /// to both versions, which is a mixture rather than a canary and breaks the
  /// thing being tested first. The cost is that the split is only as even as
  /// the addresses are spread, so at low traffic or behind one large NAT
  /// twenty percent may not look like twenty percent.
  #[schemars(extend("examples" = [20]))]
  pub weight: Option<u8>,
  /// Request header that sends this visitor to the canary whatever the weight
  /// says, so a developer can reach the new version on demand.
  #[schemars(extend("examples" = ["x-canary"]))]
  pub header: Option<String>,
  /// Value that header must carry. Unset = any non-empty value.
  #[schemars(extend("examples" = ["on"]))]
  pub value: Option<String>,
}

/// A `rate_limit:` block inside a `routes:` entry: the same token bucket as a
/// `rate_limits:` rule, without repeating the hostname and path.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteRateLimit {
  /// Sustained requests per second allowed to the route.
  #[schemars(extend("examples" = [50.0]))]
  pub rps: f64,
  /// Burst capacity. Default: the `rps` value (one second's worth).
  #[schemars(extend("examples" = [100.0]))]
  pub burst: Option<f64>,
  /// HTTP methods the limit applies to (omit for every method). Lets a write
  /// path be limited without throttling reads of the same route.
  #[schemars(extend("examples" = [["POST", "PUT", "DELETE"]]))]
  pub methods: Option<Vec<String>>,
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

/// The yaml keys that describe a *single* service at the top level.
///
/// They are the file's spelling of the CLI's single-service shorthand, and
/// they are on their way out of the file format: a config file is the place
/// where a deployment is written down, and having two shapes for "what this
/// client exposes", one that only works when the other is absent, is a
/// question nobody should have to answer. `services:` is the one shape.
///
/// The shorthand itself is not going anywhere; it stays where it belongs, on
/// the command line and in the environment, where a one-liner is the point.
pub const SINGLE_SERVICE_KEYS: &[&str] = &[
  "target",
  "serve",
  "hostname",
  "path",
  "tcp_target",
  "target_health",
];

impl FileConfig {
  /// Which single-service keys this file writes, in the order above.
  ///
  /// Call before [`FileConfig::fold_groups`] and before any layering: what is
  /// being reported is what the *file* says, not what the resolved settings
  /// ended up as, and a value that came from the CLI or the environment is
  /// not this file's problem.
  pub fn single_service_keys(&self) -> Vec<&'static str> {
    let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    [
      ("target", set(&self.target)),
      ("serve", set(&self.serve)),
      ("hostname", self.hostname.is_some()),
      ("path", set(&self.path)),
      ("tcp_target", set(&self.tcp_target)),
      // Both spellings, since folding has not run yet and either is the same
      // claim: a probe path for a service named at the top level.
      (
        "target_health",
        set(&self.target_health)
          || self
            .health
            .as_ref()
            .is_some_and(|h| h.endpoint.as_deref().is_some_and(|e| !e.trim().is_empty())),
      ),
    ]
    .into_iter()
    .filter(|(_, present)| *present)
    .map(|(key, _)| key)
    .collect()
  }

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
  /// Error-rate alert threshold, 0..1. Default: off.
  #[schemars(extend("examples" = [0.25]))]
  pub error_rate: Option<f64>,
  /// Alert sliding-window seconds. Default: `300`.
  #[schemars(extend("examples" = [300]))]
  pub window: Option<u64>,
  /// Minimum requests in the window before the error-rate alert fires.
  /// Default: `20`.
  #[schemars(extend("examples" = [20]))]
  pub min_requests: Option<u64>,
  /// Seconds a known service may stay down before the client-down alert
  /// fires; it resolves when the service comes back. `0` = off. Default: off.
  #[schemars(extend("examples" = [60]))]
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
  /// Default: `10485760` (10 MB).
  #[schemars(extend("examples" = [10485760]))]
  pub max_size: Option<u64>,
  /// Rotated audit log files kept. Default: `3`.
  #[schemars(extend("examples" = [3]))]
  pub max_files: Option<u64>,
}

/// The server-side GET response cache.
///
/// Written as a `cache:` block; the flat `cache_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheGroup {
  /// Enable the server-side GET response cache. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Response-cache budget in bytes. Default: `67108864` (64 MB).
  #[schemars(extend("examples" = [67108864]))]
  pub max_bytes: Option<u64>,
  /// Serve-stale window in seconds for resilient services. Default: `3600`.
  #[schemars(extend("examples" = [3600]))]
  pub max_stale: Option<u64>,
  /// Seconds to briefly cache error / negative responses (e.g. `404`), so a
  /// hot missing URL cannot hammer the backend. `0` = disabled.
  /// Default: `0`.
  #[schemars(extend("examples" = [10]))]
  pub negative_ttl: Option<u64>,
}

/// The built-in dashboard.
///
/// Written as a `dashboard:` block; the flat `dashboard_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DashboardGroup {
  /// Serve the admin dashboard. Default: `true`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
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
  #[schemars(extend("examples" = ["change-me-to-a-long-random-string"]))]
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
  /// Also publish hostnames a token permits but no client serves yet.
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub include_offline: Option<bool>,
}

/// In-flight failover: what happens to a request whose client disappears mid-flight.
///
/// Written as a `failover:` block; the flat `failover_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FailoverGroup {
  /// In-flight failover mode. Default: `fail`.
  #[schemars(extend("examples" = ["fail", "retry", "wait", "retry-wait"]))]
  pub mode: Option<String>,
  /// Maximum failover re-dispatches per request. Default: `2`.
  #[schemars(extend("examples" = [2]))]
  pub max_jumps: Option<u32>,
  /// Failover window in seconds. Default: `15`.
  #[schemars(extend("examples" = [300]))]
  pub window: Option<u64>,
  /// Allow failover for non-idempotent methods too. Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub all_methods: Option<bool>,
}

/// Gateway timeouts applied to a proxied request.
///
/// Written as a `gateway:` block; the flat `gateway_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayGroup {
  /// Seconds to wait for a client connection. Default: `10`.
  #[schemars(extend("examples" = [10]))]
  pub timeout: Option<u64>,
  /// Seconds to wait for a client response. Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub response_timeout: Option<u64>,
}

/// Per-visitor-IP rate limiting (token bucket).
///
/// Written as a `ip_limit:` block; the flat `ip_limit_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IpLimitGroup {
  /// Per-IP rate-limit burst. Default: `100`.
  #[schemars(extend("examples" = [120]))]
  pub max: Option<u64>,
  /// Per-IP rate-limit refill per second. Default: `5`.
  #[schemars(extend("examples" = [20.0]))]
  pub refill: Option<f64>,
}

/// Dashboard login lockout after repeated failures.
///
/// Written as a `login_lockout:` block; the flat `login_lockout_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginLockoutGroup {
  /// Failed logins per IP before a lockout. Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub threshold: Option<u32>,
  /// Base lockout seconds, doubled per repeat. Default: `60`.
  #[schemars(extend("examples" = [60]))]
  pub secs: Option<u64>,
}

/// The Prometheus metrics endpoint.
///
/// Written as a `metrics:` block; the flat `metrics_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricsGroup {
  /// Prometheus metrics endpoint toggle. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Bearer token gating the metrics endpoint.
  #[schemars(extend("examples" = ["change-me-to-a-long-random-string"]))]
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
  #[schemars(extend("examples" = ["https://accounts.example.com"]))]
  pub issuer: Option<String>,
  /// OIDC client id.
  #[schemars(extend("examples" = ["aperio"]))]
  pub client_id: Option<String>,
  /// OIDC client secret.
  #[schemars(extend("examples" = ["${OIDC_SECRET}"]))]
  pub client_secret: Option<String>,
  /// Allowed OIDC login emails.
  #[schemars(extend("examples" = [["alice@example.com", "ops@example.com"]]))]
  pub allowed_emails: Option<Vec<String>>,
  /// OIDC scopes. Default: `openid email profile`.
  #[schemars(extend("examples" = [["openid", "email", "profile"]]))]
  pub scopes: Option<Vec<String>>,
  /// OIDC redirect URL override.
  #[schemars(extend("examples" = ["https://tunnel.example.com/aperio/oidc/callback"]))]
  pub redirect_url: Option<String>,
}

/// OpenTelemetry trace export.
///
/// Written as a `otel:` block; the flat `otel_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OtelGroup {
  /// Enable OpenTelemetry OTLP export. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// OTLP endpoint. The conventional ports are 4318 for OTLP/HTTP and 4317
  /// for OTLP/gRPC; `protocol` follows from the port unless it is set.
  /// Default: `http://localhost:4318`.
  #[schemars(extend("examples" = ["http://localhost:4318", "http://localhost:4317"]))]
  pub endpoint: Option<String>,
  /// OTLP transport: `http` (protobuf over HTTP) or `grpc`. Unset picks `grpc`
  /// for an endpoint on port 4317 and `http` everywhere else, a collector
  /// answering the wrong protocol drops every span silently, so pin this when
  /// the endpoint runs on a non-standard port.
  #[schemars(extend("examples" = ["http", "grpc"]))]
  pub protocol: Option<String>,
  /// OTLP service name. Default: `aperio-server`.
  #[schemars(extend("examples" = ["aperio"]))]
  pub service_name: Option<String>,
  /// Extra headers for every outgoing OTLP request, as `k=v,k=v`. This is
  /// where a collector's credential goes, and it stays on the server: the
  /// point of the OTel bridge is that an edge host does not hold one.
  #[schemars(extend("examples" = ["authorization=Bearer xxx"]))]
  pub headers: Option<String>,
  /// Fraction of traces to record, 0.0 to 1.0. At `1.0` every request
  /// builds a span tree and hands it to the exporter, which is the setting
  /// that makes tracing show up in a benchmark. `0.01` samples one request in
  /// a hundred, which answers the same questions about latency and error
  /// shape for a hundredth of the cost. The decision is made once per request
  /// and every span of that request follows it. Default: `1.0`.
  #[schemars(extend("examples" = [0.01]))]
  pub sample_rate: Option<f64>,
}

/// How long each kind of recorded data is kept, in days.
///
/// Written as a `retention:` block; the flat `retention_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetentionGroup {
  /// Days to keep inspector captures and webhook inbox entries; `0` =
  /// forever. Default: off (keep).
  #[schemars(extend("examples" = [7]))]
  pub captures: Option<u64>,
  /// Days to keep access-log file lines; `0` = forever. Default: off (keep).
  #[schemars(extend("examples" = [30]))]
  pub access_log: Option<u64>,
  /// Days to keep audit events; `0` = forever. Default: off (keep).
  #[schemars(extend("examples" = [90]))]
  pub audit: Option<u64>,
  /// Days to keep day-granularity stats buckets; `0` = the built-in caps.
  /// Default: off (built-in caps).
  #[schemars(extend("examples" = [365]))]
  pub stats: Option<u64>,
}

/// Autoscaling: the server signalling desired capacity to an endpoint you control.
///
/// Written as a `scaling:` block; the flat `scaling_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScalingGroup {
  /// Honor client `scaling:` declarations. Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Allow a plain-http autoscaling endpoint. Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub allow_http: Option<bool>,
  /// Allow an autoscaling endpoint on a private or loopback address, for a
  /// provider API that genuinely lives on the internal network.
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub allow_private: Option<bool>,
  /// Seconds after which an unrefreshed autoscaling record is dropped.
  /// Default: `2592000` (30 days).
  #[schemars(extend("examples" = [2592000]))]
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
  #[schemars(extend("examples" = ["change-me-to-a-long-random-string"]))]
  pub token: Option<String>,
  /// The server's default visitor gate: a `user:password` scalar, one
  /// `{method: ...}` block, or a list of them, any of which admits a visitor.
  #[schemars(extend("examples" = ["admin:s3cret", [{"method": "basic", "users": ["admin:s3cret"]}]]))]
  pub auth: Option<AuthSetting>,
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
  #[schemars(extend("examples" = [["hooks.example.com", "*.provider.example"]]))]
  pub allowlist: Option<Vec<String>>,
  /// With no allowlist: refuse destinations resolving to internal addresses
  /// (loopback, RFC 1918, link-local/metadata, CGNAT, unique-local).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub block_private: Option<bool>,
}

/// Scheduled snapshots of the SQLite store.
///
/// Written as a `backup:` block; the flat `backup_*` keys mean the same thing
/// and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupGroup {
  /// Seconds between snapshots; `0` disables scheduled backups. Default: off.
  #[schemars(extend("examples" = [86400]))]
  pub interval: Option<u64>,
  /// Directory the snapshots are written to. Required to enable backups:
  /// without it (and a nonzero `interval`) no snapshots are taken.
  #[schemars(extend("examples" = ["/var/backups/aperio"]))]
  pub dir: Option<String>,
  /// Snapshots to keep; older ones are pruned. Default: `7`.
  #[schemars(extend("examples" = [7]))]
  pub keep: Option<u64>,
}

/// Per-stream flow control for streamed data (responses, WebSocket, TCP).
///
/// Written as a `stream:` block; the flat `stream_*` keys mean the same
/// thing and still work, with the block winning per field.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamGroup {
  /// Backlog bytes at which the producing client is asked to pause a stream.
  /// Default: `2097152` (2 MB).
  #[schemars(extend("examples" = [2097152]))]
  pub pause_bytes: Option<u64>,
  /// Backlog bytes under which a paused producer is asked to resume.
  /// Default: `524288` (512 KB).
  #[schemars(extend("examples" = [524288]))]
  pub resume_bytes: Option<u64>,
  /// Hard per-stream backlog cap in bytes (drops producers that cannot pause).
  /// Default: `16777216` (16 MB).
  #[schemars(extend("examples" = [16777216]))]
  pub backlog_limit: Option<u64>,
  /// Bytes per second a streamed response's consumer must take **while data
  /// is waiting for it**, or the stream is ended. `0` (the default) = no
  /// floor.
  ///
  /// The pump already ends a stream whose consumer cannot take a single chunk
  /// within the gateway timeout, so a reader that takes nothing is covered.
  /// This closes the gap in between: a reader that accepts one chunk just
  /// inside the timeout, forever, holding a client concurrency slot and
  /// megabytes of buffer for as long as it likes.
  ///
  /// Only time the consumer kept data waiting counts, so a stream that is
  /// quiet because the *backend* has nothing to send, which is ordinary for
  /// server-sent events and long polling, is never ended for it
  /// (env: APERIO_STREAM_MIN_THROUGHPUT).
  #[schemars(extend("examples" = [1024]))]
  pub min_throughput: Option<u64>,
}

/// The client's `otel_bridge:` block.
///
/// `PartialEq` so a config reload can notice that this block changed: the
/// bridge is the one facility a running client cannot rebuild, so the change
/// is reported rather than silently ignored.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OtelBridge {
  /// Address for the OTLP/HTTP receiver, the one every SDK can reach with
  /// `OTEL_EXPORTER_OTLP_ENDPOINT`. Default: `127.0.0.1:4318`.
  #[schemars(extend("examples" = ["127.0.0.1:4318"]))]
  pub listen: Option<String>,
  /// Address for the OTLP/gRPC receiver, for SDKs pinned to that transport.
  /// Unset = no gRPC listener. Conventionally port 4317.
  #[schemars(extend("examples" = ["127.0.0.1:4317"]))]
  pub listen_grpc: Option<String>,
  /// How exports reach the server: `tunnel` sends them as frames on the
  /// WebSocket this client already holds, `https` posts them to the server's
  /// endpoint as ordinary requests. Default: `tunnel`.
  ///
  /// `tunnel` is the one that keeps the "exactly one outbound connection"
  /// property that makes this worth having; `https` exists because a client
  /// whose telemetry is bursty may prefer it to stay off the tunnel entirely,
  /// where it would share flow control with proxied traffic.
  #[schemars(extend("examples" = ["tunnel", "https"]))]
  pub transport: Option<String>,
  /// Exports to hold when the far end is not keeping up. Past this, the
  /// newest is dropped and counted, never waited on: an exporter that cannot
  /// hand off its batch blocks the application it is instrumenting, so
  /// telemetry must never be the reason a tunnel stalls. Default: `256`.
  #[schemars(extend("examples" = [256]))]
  pub queue: Option<usize>,
}

/// How a visitor gate is written, on either side of the tunnel.
///
/// Three spellings of one thing. The scalar `auth: "user:password"` predates
/// the grammar and keeps working, folding to a single `basic` method; one
/// block names a method with its settings; a list is admitted when **any** of
/// its methods admits the visitor, which is what "a browser signs in, a script
/// presents a key" needs and what a single choice cannot say.
///
/// The variants are ordered for `untagged`: a YAML scalar can only be the
/// first, a mapping only the second, a sequence only the third, so a value is
/// never read as the wrong shape.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum AuthSetting {
  /// `auth: "user:password"`, the spelling that predates the method grammar.
  Credentials(String),
  /// One method: `auth: {method: none}`. Boxed because a method entry grew
  /// fields as methods were added, and an unboxed variant makes every
  /// `AuthSetting`, including the one-word scalar, as large as the largest
  /// method's settings.
  One(Box<AuthMethodSpec>),
  /// Several methods, any one of which admits the visitor.
  Any(Vec<AuthMethodSpec>),
}

/// One entry of an `auth:` policy: which method gates the route, and the
/// settings that method needs.
///
/// `method` is deliberately a string here rather than an enum: an unknown one
/// should be refused by name, with the available methods listed, and a serde
/// "unknown variant" error inside an untagged enum says only that nothing
/// matched. The same reason `alert_rules` parses its `metric` by hand.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthMethodSpec {
  /// The gate: `none` (deliberately open) or `basic` (a `user:password`
  /// login). Further methods are their own backlog entries.
  #[schemars(extend("examples" = ["basic", "none"]))]
  pub method: String,
  /// `basic`: the credentials that open this gate, each `user:password`.
  /// A single value or a list.
  #[serde(default)]
  #[schemars(extend("examples" = ["admin:s3cret", ["admin:s3cret", "ops:hunter2"]]))]
  pub users: Option<Credentials>,
  /// `bearer`: the secret a caller presents as `Authorization: Bearer <secret>`.
  /// One or a list, all alternatives on the same gate, so a key can be
  /// rotated by adding the new one before withdrawing the old.
  #[serde(default)]
  #[schemars(extend("examples" = ["${API_SECRET}", ["${API_SECRET}", "${API_SECRET_PREVIOUS}"]]))]
  pub secret: Option<Credentials>,
  /// `bearer`: also accept the secret as `?aperio_token=`, for the callers
  /// that cannot set a header. **Off by default**: a query string reaches the
  /// access log, the `Referer` header, browser history and every proxy in
  /// front, so this is a trade an operator makes on purpose.
  #[serde(default)]
  #[schemars(extend("examples" = [true]))]
  pub query: Option<bool>,
  /// `jwt`: URL of the issuer's JWKS, the public keys its tokens are signed
  /// with. Fetched and cached, and re-fetched when a token names a key id
  /// that is not in the cache. Subject to the server's outbound policy.
  #[serde(default)]
  #[schemars(extend("examples" = ["https://accounts.example.com/.well-known/jwks.json"]))]
  pub jwks_url: Option<String>,
  /// `jwt`: shared secret for HMAC-signed tokens (`HS256`), for an issuer
  /// that is your own service rather than a provider with public keys.
  /// Mutually exclusive with `jwks_url`.
  #[serde(default)]
  #[schemars(extend("examples" = ["${JWT_SECRET}"]))]
  pub hmac_secret: Option<String>,
  /// `jwt`: required `iss` claim. Strongly recommended: without it any issuer
  /// whose key the JWKS happens to carry is accepted.
  #[serde(default)]
  #[schemars(extend("examples" = ["https://accounts.example.com"]))]
  pub issuer: Option<String>,
  /// `jwt`: required `aud` claim, one or a list of accepted values.
  #[serde(default)]
  #[schemars(extend("examples" = ["aperio", ["aperio", "internal-tools"]]))]
  pub audience: Option<Credentials>,
  /// `jwt`: further claims a token must carry, each as an exact value the
  /// claim must equal (`{groups: engineering}`).
  #[serde(default)]
  #[schemars(extend("examples" = [{"groups": "engineering"}]))]
  pub claims: Option<std::collections::BTreeMap<String, String>>,
  /// `jwt`: read the token from this cookie instead of the `Authorization`
  /// header, which is where an identity-aware proxy in front usually puts it
  /// (Cloudflare Access writes `CF_Authorization`).
  #[serde(default)]
  #[schemars(extend("examples" = ["CF_Authorization"]))]
  pub cookie: Option<String>,
  /// `forward`: the endpoint asked about each request. `2xx` admits it;
  /// anything else refuses it, and the endpoint's own answer is what the
  /// visitor gets, so it can redirect to a login of its own.
  #[serde(default)]
  #[schemars(extend("examples" = ["http://127.0.0.1:7070/_authcheck"]))]
  pub url: Option<String>,
  /// `forward`: request headers copied to the subrequest.
  /// Default: `cookie` and `authorization`, the two that carry an identity.
  #[serde(default)]
  #[schemars(extend("examples" = [["cookie", "authorization", "x-api-key"]]))]
  pub request_headers: Option<Vec<String>>,
  /// `forward`: headers of a `2xx` answer copied onto the request that goes
  /// to the backend. This is how the pattern delivers an identity, and an
  /// open list is how it becomes a header injection, so it is named
  /// explicitly and empty by default.
  #[serde(default)]
  #[schemars(extend("examples" = [["x-auth-user", "x-auth-groups"]]))]
  pub response_headers: Option<Vec<String>>,
  /// `forward`: seconds to wait for the endpoint. A timeout **refuses** the
  /// request: an auth gate that opens when its check is unreachable is not a
  /// gate. Default: `5`.
  #[serde(default)]
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// `forward`: seconds to remember a verdict for an identical credential,
  /// so a busy route does not pay a round trip per request. `0` (the default)
  /// asks every time.
  #[serde(default)]
  #[schemars(extend("examples" = [30]))]
  pub cache: Option<u64>,
}

/// A `basic` method's credentials: one `user:password` or a list of them.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(untagged)]
pub enum Credentials {
  /// A single `user:password`.
  One(String),
  /// Several, any of which opens the gate.
  Many(Vec<String>),
}

impl Credentials {
  /// The credentials as a list, whichever way they were written.
  pub fn as_slice(&self) -> &[String] {
    match self {
      Credentials::One(one) => std::slice::from_ref(one),
      Credentials::Many(many) => many,
    }
  }
}

impl AuthSetting {
  /// The policy as a list of method entries, whichever way it was written.
  ///
  /// The scalar spelling becomes exactly what it has always meant: one
  /// `basic` method carrying that one `user:password`.
  pub fn methods(&self) -> Vec<AuthMethodSpec> {
    match self {
      AuthSetting::Credentials(creds) => vec![AuthMethodSpec {
        method: "basic".to_string(),
        users: Some(Credentials::One(creds.clone())),
        ..Default::default()
      }],
      AuthSetting::One(spec) => vec![spec.as_ref().clone()],
      AuthSetting::Any(specs) => specs.clone(),
    }
  }

  /// The single `user:password` this policy is equivalent to, when it is
  /// equivalent to one.
  ///
  /// This is what travels to a server that predates the grammar, and what the
  /// existing scalar surfaces (`APERIO_SERVER_AUTH`, the dashboard's editable
  /// value) still read. `None` means the policy says something the scalar
  /// cannot: no credential at all, several of them, or another method.
  pub fn as_single_credential(&self) -> Option<&str> {
    match self {
      AuthSetting::Credentials(creds) => Some(creds.as_str()),
      AuthSetting::One(spec) => spec.single_credential(),
      AuthSetting::Any(specs) => match specs.as_slice() {
        [only] => only.single_credential(),
        _ => None,
      },
    }
  }
}

impl AuthMethodSpec {
  /// The one `user:password` this entry is equivalent to, if it is a `basic`
  /// method carrying exactly one.
  fn single_credential(&self) -> Option<&str> {
    if !self.method.trim().eq_ignore_ascii_case("basic") {
      return None;
    }
    match self.users.as_ref()?.as_slice() {
      [only] => Some(only.as_str()),
      _ => None,
    }
  }
}

/// The methods a visitor gate may name today, in the order they are listed
/// back to an operator who names one that does not exist.
///
/// Deliberately a closed set: the open version was considered and withdrawn
/// (`planned_features.md` #103). Further methods each arrive as their own
/// entry rather than as a plugin interface.
pub const AUTH_METHODS: &[&str] = &["none", "basic", "bearer", "jwt", "forward"];

/// Shortest `bearer` secret accepted.
///
/// A bearer secret is one opaque string compared verbatim: unlike a
/// `user:password` there is no second half, no login form, and no lockout in
/// front of it, so its length is the whole of its strength. Operators reach
/// for short strings while testing and leave them in, which is the failure
/// this refuses at the point it is written.
pub const MIN_BEARER_SECRET_LEN: usize = 16;

/// Is this a `basic` credential the login path could ever match?
///
/// A value without the separator, or with an empty half, is refused where it
/// is written rather than at the moment a visitor fails to get in: a gate
/// nobody can open looks exactly like a gate that is broken.
fn credential_is_usable(raw: &str) -> bool {
  match raw.split_once(':') {
    Some((user, password)) => !user.is_empty() && !password.is_empty(),
    None => false,
  }
}

/// Validates a visitor-auth policy, naming what is wrong and where.
///
/// Called from both sides: the client refuses to start on its own `auth:`,
/// and the server refuses to start on the file's. A policy that parses but
/// cannot admit anybody is the failure worth catching here, since it presents
/// as "the password does not work" hours later.
pub fn validate_auth_setting(setting: &AuthSetting) -> Result<(), String> {
  let methods = setting.methods();
  if methods.is_empty() {
    return Err("`auth:` is an empty list; remove it, or write `{method: none}` to say the route is deliberately open".to_string());
  }
  if methods.len() > 1
    && methods
      .iter()
      .any(|m| m.method.trim().eq_ignore_ascii_case("none"))
  {
    return Err(
      "`method: none` admits everyone, so listing it beside another method makes that method unreachable; keep one or the other"
        .to_string(),
    );
  }
  for (i, spec) in methods.iter().enumerate() {
    let at = |what: String| format!("`auth:` entry #{}: {}", i + 1, what);
    let name = spec.method.trim().to_ascii_lowercase();
    if !AUTH_METHODS.contains(&name.as_str()) {
      return Err(at(format!(
        "`{}` is not a method ({})",
        spec.method,
        AUTH_METHODS.join(", ")
      )));
    }
    match name.as_str() {
      "none" => {
        if spec.users.is_some()
          || spec.secret.is_some()
          || spec.jwks_url.is_some()
          || spec.hmac_secret.is_some()
          || spec.url.is_some()
        {
          return Err(at(
            "`method: none` is the open gate and takes no credentials".to_string(),
          ));
        }
      }
      "bearer" => {
        if spec.users.is_some() {
          return Err(at(
            "`method: bearer` takes `secret:`, not `users:`; it has no user half".to_string(),
          ));
        }
        let Some(secrets) = spec.secret.as_ref() else {
          return Err(at(
            "`method: bearer` needs `secret:`, the value a caller presents as `Authorization: Bearer`"
              .to_string(),
          ));
        };
        if secrets.as_slice().is_empty() {
          return Err(at("`secret:` is empty".to_string()));
        }
        for secret in secrets.as_slice() {
          if secret.trim().is_empty() {
            return Err(at(
              "a blank `secret:` would be a gate that opens for an empty header".to_string(),
            ));
          }
          if secret.len() < MIN_BEARER_SECRET_LEN {
            return Err(at(format!(
              "this `secret:` is {} characters; a bearer secret is compared verbatim and has no user half to slow a guess down, so it carries the whole of the gate and needs at least {MIN_BEARER_SECRET_LEN}",
              secret.len()
            )));
          }
        }
      }
      "forward" => {
        if spec.users.is_some() || spec.secret.is_some() {
          return Err(at(
            "`method: forward` asks an endpoint; it takes `url:`, not `users:` / `secret:`"
              .to_string(),
          ));
        }
        let Some(url) = spec.url.as_deref() else {
          return Err(at(
            "`method: forward` needs `url:`, the endpoint asked about each request".to_string(),
          ));
        };
        if !url.starts_with("https://") && !url.starts_with("http://") {
          return Err(at(format!("`url:` is not a URL: `{url}`")));
        }
        for (label, list) in [
          ("request_headers", spec.request_headers.as_ref()),
          ("response_headers", spec.response_headers.as_ref()),
        ] {
          for name in list.into_iter().flatten() {
            if name.trim().is_empty() {
              return Err(at(format!("`{label}:` has a blank entry")));
            }
            if name.contains('*') {
              return Err(at(format!(
                "`{label}:` takes header names, not patterns; `{name}` would be a rule nobody can read back"
              )));
            }
          }
        }
      }
      "jwt" => {
        if spec.users.is_some() || spec.secret.is_some() {
          return Err(at(
            "`method: jwt` verifies a signature; it takes `jwks_url:` or `hmac_secret:`, not `users:` / `secret:`".to_string(),
          ));
        }
        match (spec.jwks_url.as_deref(), spec.hmac_secret.as_deref()) {
          (None, None) => {
            return Err(at(
              "`method: jwt` needs `jwks_url:` (the issuer's public keys) or `hmac_secret:` (a shared secret)".to_string(),
            ));
          }
          (Some(_), Some(_)) => {
            return Err(at(
              "`method: jwt` takes `jwks_url:` or `hmac_secret:`, not both: they are two different ways of knowing who signed a token".to_string(),
            ));
          }
          (Some(url), None) => {
            if !url.starts_with("https://") && !url.starts_with("http://") {
              return Err(at(format!("`jwks_url:` is not a URL: `{url}`")));
            }
          }
          (None, Some(secret)) => {
            if secret.len() < MIN_BEARER_SECRET_LEN {
              return Err(at(format!(
                "this `hmac_secret:` is {} characters; it is the whole of what proves a token was not written by its bearer, so it needs at least {MIN_BEARER_SECRET_LEN}",
                secret.len()
              )));
            }
          }
        }
        if spec.issuer.as_deref().is_some_and(|i| i.trim().is_empty()) {
          return Err(at("`issuer:` is blank".to_string()));
        }
        if let Some(claims) = spec.claims.as_ref() {
          for key in claims.keys() {
            if key.trim().is_empty() {
              return Err(at("a claim requirement has a blank name".to_string()));
            }
          }
        }
      }
      "basic" => {
        if spec.secret.is_some() {
          return Err(at(
            "`method: basic` takes `users:`, not `secret:`; `secret:` belongs to `bearer`"
              .to_string(),
          ));
        }
        let Some(users) = spec.users.as_ref() else {
          return Err(at(
            "`method: basic` needs `users:`, one or more `user:password`".to_string(),
          ));
        };
        if users.as_slice().is_empty() {
          return Err(at("`users:` is empty".to_string()));
        }
        for cred in users.as_slice() {
          if !credential_is_usable(cred) {
            return Err(at(format!(
              "`{cred}` is not `user:password`; a value without both halves can never be logged in with"
            )));
          }
        }
      }
      _ => unreachable!("method checked against AUTH_METHODS above"),
    }
  }
  Ok(())
}

/// `shutdown_drain:` as a number of seconds, or the word `auto`.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum ShutdownDrain {
  /// Wait this many seconds for in-flight requests.
  Seconds(u64),
  /// `auto`: take the longest drain budget connected clients announce,
  /// capped, so the number follows the deployment instead of being a constant
  /// nobody revisits.
  Named(String),
}

/// `cache:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum CacheSetting {
  /// `true` turns the cache on with the defaults.
  Enabled(bool),
  /// The full block.
  Group(CacheGroup),
}

/// `dashboard:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `true`
/// (enabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum DashboardSetting {
  /// `false` serves no dashboard at all.
  Enabled(bool),
  /// The full block.
  Group(DashboardGroup),
}

/// `metrics:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum MetricsSetting {
  /// `true` exposes the Prometheus endpoint.
  Enabled(bool),
  /// The full block.
  Group(MetricsGroup),
}

/// `otel:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum OtelSetting {
  /// `true` exports traces with the defaults.
  Enabled(bool),
  /// The full block.
  Group(OtelGroup),
}

/// `scaling:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum ScalingSetting {
  /// `true` honors the clients' scaling declarations.
  Enabled(bool),
  /// The full block.
  Group(ScalingGroup),
}

/// `failover:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `fail`.
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
    key: "request_id",
    self_key: Some("enabled"),
  },
  ServerGroup {
    key: "audit",
    self_key: None,
  },
  ServerGroup {
    key: "backup",
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
  #[schemars(extend("examples" = [{"error_rate": 0.25, "window": 300, "min_requests": 20, "client_down": 60}]))]
  pub alert: Option<AlertGroup>,
  /// Audit-log file rotation
  #[serde(default)]
  #[schemars(extend("examples" = [{"max_size": 10485760, "max_files": 3}]))]
  pub audit: Option<AuditGroup>,
  /// Scheduled snapshots of the SQLite store
  #[serde(default)]
  #[schemars(extend("examples" = [{"dir": "/app/data/backups", "interval": 86400, "keep": 7}]))]
  pub backup: Option<BackupGroup>,
  /// The server-side GET response cache
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "max_bytes": 67108864, "max_stale": 3600}]))]
  pub cache: Option<CacheSetting>,
  /// The built-in dashboard
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true}]))]
  pub dashboard: Option<DashboardSetting>,
  /// Edge-proxy integration: publishing the served hostnames to a dynamic reverse proxy in front of this server
  #[serde(default)]
  #[schemars(extend("examples" = [{"token": "edge_xxxxxxxx", "service_url": "http://aperio:8080", "entrypoints": "websecure"}]))]
  pub edge: Option<EdgeGroup>,
  /// In-flight failover: what happens to a request whose client disappears mid-flight
  #[serde(default)]
  #[schemars(extend("examples" = ["retry-wait", {"mode": "retry", "max_jumps": 2, "window": 30}]))]
  pub failover: Option<FailoverSetting>,
  /// Gateway timeouts applied to a proxied request
  #[serde(default)]
  #[schemars(extend("examples" = [{"timeout": 10, "response_timeout": 30}]))]
  pub gateway: Option<GatewayGroup>,
  /// Per-visitor-IP rate limiting (token bucket)
  #[serde(default)]
  #[schemars(extend("examples" = [{"max": 120, "refill": 20.0}]))]
  pub ip_limit: Option<IpLimitGroup>,
  /// Dashboard login lockout after repeated failures
  #[serde(default)]
  #[schemars(extend("examples" = [{"threshold": 5, "secs": 60}]))]
  pub login_lockout: Option<LoginLockoutGroup>,
  /// The Prometheus metrics endpoint
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "token": "scrape_xxxxxxxx"}]))]
  pub metrics: Option<MetricsSetting>,
  /// OIDC single sign-on for the dashboard
  #[serde(default)]
  #[schemars(extend("examples" = [{"issuer": "https://accounts.example.com", "client_id": "aperio", "client_secret": "${OIDC_SECRET}", "redirect_url": "https://tunnel.example.com/aperio/oidc/callback"}]))]
  pub oidc: Option<OidcGroup>,
  /// OpenTelemetry trace export
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "endpoint": "http://collector:4317", "protocol": "grpc", "service_name": "aperio"}]))]
  pub otel: Option<OtelSetting>,
  /// Where the server may send outbound callbacks (webhooks, autoscaling hooks)
  #[serde(default)]
  #[schemars(extend("examples" = [{"block_private": true, "allowlist": ["hooks.example.com", "*.provider.example"]}]))]
  pub outbound: Option<OutboundGroup>,
  /// How long each kind of recorded data is kept, in days
  #[serde(default)]
  #[schemars(extend("examples" = [{"stats": 365, "audit": 90, "captures": 7, "access_log": 30}]))]
  pub retention: Option<RetentionGroup>,
  /// Autoscaling: the server signalling desired capacity to an endpoint you control
  #[serde(default)]
  #[schemars(extend("examples" = [true, {"enabled": true, "allow_http": false, "record_ttl": 2592000}]))]
  pub scaling: Option<ScalingSetting>,
  /// The server's own credentials
  #[serde(default)]
  #[schemars(extend("examples" = [{"token": "change-me-to-a-long-random-string"}]))]
  pub server: Option<ServerCredentials>,
  /// Request-id correlation: the id sent to the backend and echoed to the visitor
  #[serde(default)]
  #[schemars(extend("examples" = [{"enabled": true, "trust_inbound": false}]))]
  pub request_id: Option<RequestIdGroup>,
  /// Flat spelling of `request_id.header` (env: APERIO_REQUEST_ID_HEADER).
  #[schemars(extend("examples" = ["x-correlation-id"]))]
  pub request_id_header: Option<String>,
  /// Flat spelling of `request_id.trust_inbound`
  /// (env: APERIO_REQUEST_ID_TRUST_INBOUND).
  #[schemars(extend("examples" = [true]))]
  pub request_id_trust_inbound: Option<bool>,
  /// Per-stream flow control for streamed data (responses, WebSocket, TCP)
  #[serde(default)]
  #[schemars(extend("examples" = [{"pause_bytes": 2097152, "resume_bytes": 524288, "backlog_limit": 16777216}]))]
  pub stream: Option<StreamGroup>,
  // --- Core ---
  /// The Aperio version this file was written for, e.g. `0.5.0`. On startup
  /// the server compares it against its own build and reports every recorded
  /// change to the configuration format that landed in between, refusing to
  /// start when one of them has security consequences. Unset disables the
  /// check (env: APERIO_VERSION).
  #[schemars(extend("examples" = ["0.5.0"]))]
  pub version: Option<String>,
  /// Deprecated spelling of `server.token` (env: APERIO_SERVER_TOKEN).
  pub server_token: Option<String>,
  /// Address to bind (bare env: HOST). Default: `0.0.0.0`.
  #[schemars(extend("examples" = ["0.0.0.0"]))]
  pub host: Option<String>,
  /// Port to listen on (bare env: PORT). Default: `8080`.
  #[schemars(extend("examples" = [8080]))]
  pub port: Option<u16>,
  /// Directory for the SQLite store and logs (env: APERIO_DATA_DIR).
  /// Default: `./data`.
  #[schemars(extend("examples" = ["/app/data"]))]
  pub data_dir: Option<String>,
  /// Log level (bare env: LOG_LEVEL). Default: `info`.
  #[schemars(extend("examples" = ["info", "debug"]))]
  pub log_level: Option<String>,

  /// What a route nobody gated means: `allow` (today's behaviour, and the
  /// default) or `deny`. With `deny` a route is reachable because something
  /// said so, an `auth:` policy that admits the visitor or an explicit
  /// `method: none` / `public: true`, rather than because nothing said
  /// otherwise (env: APERIO_DEFAULT_ACCESS). Default: `allow`.
  #[schemars(extend("examples" = ["deny"]))]
  pub default_access: Option<String>,

  // --- Routing & load balancing ---
  /// Require every client to carry a hostname bind
  /// (env: APERIO_REQUIRE_HOSTNAME_BIND). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub require_hostname_bind: Option<bool>,
  /// Wildcard pattern granting each client a random subdomain (env: APERIO_RANDOM_SUBDOMAIN).
  #[schemars(extend("examples" = ["*.example.com"]))]
  pub random_subdomain: Option<String>,
  /// Inject noindex headers for random-subdomain preview services
  /// (env: APERIO_PREVIEW_NOINDEX). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub preview_noindex: Option<bool>,
  /// Seconds without a heartbeat before a client is considered down
  /// (env: APERIO_CLIENT_DOWN_THRESHOLD). Default: `15`.
  #[schemars(extend("examples" = [15]))]
  pub client_down_threshold: Option<u64>,
  /// Load-balancing strategy (env: APERIO_LB_STRATEGY).
  /// Default: `round-robin`.
  #[schemars(extend("examples" = ["round-robin", "primary-standby", "sticky"]))]
  pub lb_strategy: Option<String>,
  /// Passive outlier ejection: temporarily drop a client from the pool when it
  /// returns too many errors in a window (env: APERIO_OUTLIER_EJECTION).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub outlier_ejection: Option<bool>,
  /// Failures inside the window before a client is ejected
  /// (env: APERIO_OUTLIER_MAX_FAILURES). Default: `5`.
  #[schemars(extend("examples" = [5]))]
  pub outlier_max_failures: Option<u32>,
  /// Sliding window in seconds the failures are counted over
  /// (env: APERIO_OUTLIER_WINDOW). Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub outlier_window: Option<u64>,
  /// Seconds an ejected client stays out before re-admission
  /// (env: APERIO_OUTLIER_EJECT_SECS). Default: `30`.
  #[schemars(extend("examples" = [30]))]
  pub outlier_eject_secs: Option<u64>,

  // --- Failover ---
  /// Deprecated spelling of `failover.max_jumps` (env: APERIO_FAILOVER_MAX_JUMPS).
  pub failover_max_jumps: Option<u32>,
  /// Deprecated spelling of `failover.window` (env: APERIO_FAILOVER_WINDOW).
  pub failover_window: Option<u64>,
  /// Deprecated spelling of `failover.all_methods` (env: APERIO_FAILOVER_ALL_METHODS).
  pub failover_all_methods: Option<bool>,
  /// Re-dispatch a buffered response whose status is a retryable server error
  /// to another client instead of returning it (env: APERIO_RETRY_ON_5XX).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub retry_on_5xx: Option<bool>,
  /// Status codes that trigger that retry; empty = every 5xx
  /// (env: APERIO_RETRY_STATUSES).
  #[schemars(extend("examples" = [[502, 503]]))]
  pub retry_statuses: Option<Vec<u16>>,

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
  /// Default: `10485760` (10 MB).
  #[schemars(extend("examples" = [10485760]))]
  pub max_body_size: Option<u64>,
  /// Concurrent proxied requests limit (env: APERIO_MAX_CONCURRENT_REQUESTS).
  /// Default: `100`.
  #[schemars(extend("examples" = [512]))]
  pub max_concurrent_requests: Option<u64>,
  /// Maximum simultaneously connected clients (env: APERIO_MAX_TUNNELS).
  /// Default: `10`.
  #[schemars(extend("examples" = [10]))]
  pub max_tunnels: Option<u64>,
  /// Parallel tunnel connections one client may open for a single service
  /// (its `connections:`). A token may lower this for its own holder, never
  /// raise it (env: APERIO_MAX_CONNECTIONS_PER_SERVICE). Default: `16`.
  #[schemars(extend("examples" = [16]))]
  pub max_connections_per_service: Option<u64>,
  /// Record every proxied transaction for the dashboard's request inspector.
  /// `false` gives back a mutex, two header clones and a
  /// capture entry per request, and nothing can be inspected or replayed
  /// (env: APERIO_INSPECTOR). Default: `true`.
  #[schemars(extend("examples" = [false]))]
  pub inspector: Option<bool>,
  /// Emit the per-request structured access event for a successful request.
  /// Distinct from `log_level`: `false` silences that
  /// one-per-request line and leaves warnings and errors alone, so a refused
  /// or failed request still logs at `warn` (env: APERIO_ACCESS_EVENTS).
  /// Default: `true`.
  #[schemars(extend("examples" = [false]))]
  pub access_events: Option<bool>,
  /// Maximum concurrently-live proxied public WebSockets; they are long-lived,
  /// so they get their own ceiling separate from `max_concurrent_requests`.
  /// `0` = uncapped (env: APERIO_MAX_WS_CONNECTIONS). Default: `10000`.
  #[schemars(extend("examples" = [1000]))]
  pub max_ws_connections: Option<u64>,
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
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub trust_proxy: Option<bool>,
  /// Trusted proxy IPs/CIDRs (env: APERIO_TRUSTED_PROXIES).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub trusted_proxies: Option<Vec<String>>,
  /// Header carrying the real client IP (env: APERIO_REAL_IP_HEADER).
  #[schemars(extend("examples" = ["CF-Connecting-IP"]))]
  pub real_ip_header: Option<String>,
  /// Trust the Cloudflare client-IP header (env: APERIO_TRUST_CF_HEADER).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub trust_cf_header: Option<bool>,
  /// Mark session cookies `Secure` (env: APERIO_SECURE_COOKIES).
  /// Default: the `trust_proxy` value.
  #[schemars(extend("examples" = [true]))]
  pub secure_cookies: Option<bool>,
  /// Source IPs/CIDRs allowed to reach the authenticated admin surface (the
  /// dashboard and `/aperio/api/*`); empty = no restriction. An invalid entry
  /// refuses startup rather than applying a partial allowlist
  /// (env: APERIO_ADMIN_ALLOWED_IPS).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub admin_allowed_ips: Option<Vec<String>>,
  /// Source IPs/CIDRs refused everything, checked before every other rule:
  /// proxied traffic, the dashboard and its API, and the tunnel endpoints
  /// alike. The inverse of `allowed_ips`, for blocking an abusive address
  /// without turning on an allowlist that would lock out everyone unnamed.
  /// Answers `403`. Hot-reloadable, so an address can be blocked without a
  /// restart. An invalid entry refuses startup rather than applying a partial
  /// deny list (env: APERIO_DENIED_IPS).
  #[schemars(extend("examples" = [["203.0.113.7", "198.51.100.0/24"]]))]
  pub denied_ips: Option<Vec<String>>,
  /// Tell the backend which client, organization and token served the
  /// request, as `x-aperio-client-id`, `x-aperio-org` and `x-aperio-token`.
  /// Off by default: they are new trust surface, and a backend that starts
  /// believing them should do so deliberately. Inbound `x-aperio-*` headers
  /// are stripped from every proxied request whatever this is set to, so a
  /// visitor can never forge one (env: APERIO_IDENTITY_HEADERS).
  #[schemars(extend("examples" = [true]))]
  pub identity_headers: Option<bool>,
  /// Announce to the backend who the *visitor* is, as `x-aperio-visitor-how`
  /// (`session` / `bearer` / `share`) and `x-aperio-visitor-id` (the email or
  /// username behind a session, where there is one). Off by default and the
  /// same trust surface as `identity_headers`; an ungated or deliberately
  /// open route identifies nobody and sends neither header
  /// (env: APERIO_VISITOR_IDENTITY_HEADERS).
  #[schemars(extend("examples" = [true]))]
  pub visitor_identity_headers: Option<bool>,
  /// Fraction of *successful* requests that produce an access line, 0.0 to
  /// 1.0. Default `1.0` = every request, which is what this always did.
  /// Failures are never sampled out: a sampled-away error is the one line
  /// somebody needed. Applies to the `aperio_access` event and the
  /// `access_log` file alike (env: APERIO_ACCESS_LOG_SAMPLE_RATE).
  #[schemars(extend("examples" = [0.1]))]
  pub access_log_sample_rate: Option<f64>,
  /// Seconds to let in-flight proxied requests finish before shutdown ends
  /// the connections carrying them. Behind a load balancer this is the number
  /// that decides whether a deploy is invisible or shows up as a handful of
  /// 502s. `auto` sizes it from the drain budgets connected clients announce,
  /// capped at 30 seconds. Default: `0` (do not wait)
  /// (env: APERIO_SHUTDOWN_DRAIN).
  #[schemars(extend("examples" = [10, "auto"]))]
  pub shutdown_drain: Option<ShutdownDrain>,
  /// Other Aperio servers a client of this one may fall back to, announced in
  /// the handshake. A planned migration or a regional failover otherwise means
  /// editing every client's config; announce the new server here and clients
  /// learn it on their next connection.
  ///
  /// Advice, not instruction: a client appends these *after* the servers its
  /// own config names, so the operator's list still decides the order. A
  /// client that has never reached this server learns nothing, which is why
  /// this is for a migration announced in advance rather than a rescue
  /// (env: APERIO_ALTERNATE_SERVERS, comma-separated).
  #[schemars(extend("examples" = [["wss://eu.tunnel.example.com/tunnel"]]))]
  pub alternate_servers: Option<Vec<String>>,
  /// Streamed responses one visitor address may hold open at once. `0` (the
  /// default) = no limit. Saturating a service's concurrency budget otherwise
  /// takes one host holding many slow streams; this makes it take a botnet.
  ///
  /// No default value is chosen for you on purpose: a NAT or a carrier-grade
  /// NAT puts many real people behind one address, so any number here is a
  /// guess with a queue of users behind it. Set it from what your own traffic
  /// looks like, and make sure `trust_proxy` is right first, or every visitor
  /// behind your CDN shares one address (env: APERIO_MAX_STREAMS_PER_IP).
  #[schemars(extend("examples" = [8]))]
  pub max_streams_per_ip: Option<u32>,
  /// Accept OpenTelemetry exports from tunnel clients and forward them to the
  /// collector this server exports its own spans to (`otel.endpoint`). Off by
  /// default: it is an outbound path a client can drive, so it is a decision
  /// rather than a consequence of having `otel` on. While it is on, any client
  /// with a valid tunnel token may export; every export is attributed to the
  /// token that sent it and both size and rate are capped
  /// (env: APERIO_OTEL_BRIDGE).
  #[schemars(extend("examples" = [true]))]
  pub otel_bridge: Option<bool>,
  /// Seconds after which shutdown stops waiting for anything still holding a
  /// connection open (a proxied WebSocket, a TCP relay, a stalled peer) and
  /// exits. Default: `10` (env: APERIO_SHUTDOWN_TIMEOUT).
  #[schemars(extend("examples" = [30]))]
  pub shutdown_timeout: Option<u64>,

  // --- Tunnel & cache ---
  /// zlib-compress tunnel frames (env: APERIO_TUNNEL_COMPRESSION).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub tunnel_compression: Option<bool>,
  /// Deprecated spelling of `cache.max_bytes` (env: APERIO_CACHE_MAX_BYTES).
  pub cache_max_bytes: Option<u64>,
  /// Deprecated spelling of `cache.max_stale` (env: APERIO_CACHE_MAX_STALE).
  pub cache_max_stale: Option<u64>,
  /// Flat spelling of `outbound.allowlist` (env: APERIO_OUTBOUND_ALLOWLIST).
  #[schemars(extend("examples" = [["hooks.example.com", "*.provider.example"]]))]
  pub outbound_allowlist: Option<Vec<String>>,
  /// Flat spelling of `outbound.block_private` (env: APERIO_OUTBOUND_BLOCK_PRIVATE).
  #[schemars(extend("examples" = [true]))]
  pub outbound_block_private: Option<bool>,
  /// Flat spelling of `stream.pause_bytes` (env: APERIO_STREAM_PAUSE_BYTES).
  #[schemars(extend("examples" = [2097152]))]
  pub stream_pause_bytes: Option<u64>,
  /// Flat spelling of `stream.resume_bytes` (env: APERIO_STREAM_RESUME_BYTES).
  #[schemars(extend("examples" = [524288]))]
  pub stream_resume_bytes: Option<u64>,
  /// Flat spelling of `stream.backlog_limit` (env: APERIO_STREAM_BACKLOG_LIMIT).
  #[schemars(extend("examples" = [16777216]))]
  pub stream_backlog_limit: Option<u64>,
  /// Flat spelling of `stream.min_throughput` (env: APERIO_STREAM_MIN_THROUGHPUT).
  #[schemars(extend("examples" = [1024]))]
  pub stream_min_throughput: Option<u64>,
  /// Flat spelling of `cache.negative_ttl` (env: APERIO_CACHE_NEGATIVE_TTL).
  #[schemars(extend("examples" = [10]))]
  pub cache_negative_ttl: Option<u64>,
  /// Flat spelling of `scaling.allow_private` (env: APERIO_SCALING_ALLOW_PRIVATE).
  #[schemars(extend("examples" = [false]))]
  pub scaling_allow_private: Option<bool>,
  /// Flat spelling of `backup.interval` (env: APERIO_BACKUP_INTERVAL).
  #[schemars(extend("examples" = [86400]))]
  pub backup_interval: Option<u64>,
  /// Flat spelling of `backup.dir` (env: APERIO_BACKUP_DIR).
  #[schemars(extend("examples" = ["/app/data/backups"]))]
  pub backup_dir: Option<String>,
  /// Flat spelling of `backup.keep` (env: APERIO_BACKUP_KEEP).
  #[schemars(extend("examples" = [7]))]
  pub backup_keep: Option<u64>,

  // --- Process & startup ---
  /// Watch the config file and apply live-editable changes without a restart
  /// (env: APERIO_CONFIG_HOT_RELOAD). Default: `true`.
  #[schemars(extend("examples" = [true]))]
  pub config_hot_reload: Option<bool>,
  /// Bind the listener with SO_REUSEPORT, so a second process can take over
  /// the port for a zero-downtime handover (env: APERIO_REUSEPORT).
  /// Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub reuseport: Option<bool>,

  // --- Pages ---
  /// Custom 504 error page path (env: APERIO_504_PAGE).
  #[serde(rename = "504_page")]
  #[schemars(extend("examples" = ["./pages/504.html"]))]
  pub error_page_504: Option<String>,
  /// Custom 503 maintenance page path (env: APERIO_503_PAGE).
  #[serde(rename = "503_page")]
  #[schemars(extend("examples" = ["./pages/503.html"]))]
  pub error_page_503: Option<String>,

  // --- Logging & telemetry ---
  /// Structured access log path (env: APERIO_ACCESS_LOG).
  #[schemars(extend("examples" = ["/app/data/access.jsonl"]))]
  pub access_log: Option<String>,
  /// Mask credential headers and secret-looking body fields in the request
  /// inspector, the cURL copy and the HAR export
  /// (env: APERIO_INSPECTOR_REDACT). Default: `true`.
  #[schemars(extend("examples" = [true]))]
  pub inspector_redact: Option<bool>,
  /// Seconds between webhook delivery retries; empty = no retries
  /// (env: APERIO_WEBHOOK_RETRY_SCHEDULE). Default: `1,5,25,60`.
  #[schemars(extend("examples" = [[1, 5, 25, 60]]))]
  pub webhook_retry_schedule: Option<Vec<u64>>,
  /// Seconds between availability-history ticks for the dashboard's Uptime
  /// panel, minimum 1 (env: APERIO_UPTIME_TICK_SECS). Default: `10`.
  #[schemars(extend("examples" = [60]))]
  pub uptime_tick_secs: Option<u64>,
  /// Deprecated spelling of `retention.captures` (env: APERIO_RETENTION_CAPTURES).
  pub retention_captures: Option<u64>,
  /// Deprecated spelling of `retention.access_log` (env: APERIO_RETENTION_ACCESS_LOG).
  pub retention_access_log: Option<u64>,
  /// Deprecated spelling of `retention.audit` (env: APERIO_RETENTION_AUDIT).
  pub retention_audit: Option<u64>,
  /// Deprecated spelling of `retention.stats` (env: APERIO_RETENTION_STATS).
  pub retention_stats: Option<u64>,
  /// Cap on aperio.db (+WAL/SHM) in bytes; nearing it emits a warning, exceeding it auto-prunes low-priority data (env: APERIO_DB_MAX_BYTES).
  #[schemars(extend("examples" = [1073741824]))]
  pub db_max_bytes: Option<u64>,
  /// Deprecated spelling of `audit.max_size` (env: APERIO_AUDIT_MAX_SIZE).
  pub audit_max_size: Option<u64>,
  /// Deprecated spelling of `audit.max_files` (env: APERIO_AUDIT_MAX_FILES).
  pub audit_max_files: Option<u64>,
  /// Deprecated spelling of `otel.endpoint` (env: APERIO_OTEL_ENDPOINT).
  pub otel_endpoint: Option<String>,
  /// Deprecated spelling of `otel.protocol` (env: APERIO_OTEL_PROTOCOL).
  pub otel_protocol: Option<String>,
  /// Deprecated spelling of `otel.service_name` (env: APERIO_OTEL_SERVICE_NAME).
  pub otel_service_name: Option<String>,
  /// Flat spelling of `otel.headers` (env: APERIO_OTEL_HEADERS).
  #[schemars(extend("examples" = ["authorization=Bearer xxx"]))]
  pub otel_headers: Option<String>,
  /// Deprecated spelling of `otel.sample_rate` (env: APERIO_OTEL_SAMPLE_RATE).
  pub otel_sample_rate: Option<f64>,
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
  pub server_auth: Option<AuthSetting>,
  /// Public dashboard URL enabling passkeys; its domain is the RP ID (env: APERIO_WEBAUTHN_ORIGIN).
  #[schemars(extend("examples" = ["https://tunnel.example.com"]))]
  pub webauthn_origin: Option<String>,
  /// Passkey relying-party ID, when it must differ from the origin's domain
  /// (env: APERIO_WEBAUTHN_RP_ID).
  #[schemars(extend("examples" = ["example.com"]))]
  pub webauthn_rp_id: Option<String>,
  /// Pin a dynamic token to the first device key that presents it; a later
  /// connection with a different (or missing) key is rejected
  /// (env: APERIO_TOKEN_PINNING). Default: `false`.
  #[schemars(extend("examples" = [true]))]
  pub token_pinning: Option<bool>,
  /// Ignore client-declared visitor passwords (env: APERIO_IGNORE_CLIENT_AUTH).
  /// Default: `false`.
  #[schemars(extend("examples" = [false]))]
  pub ignore_client_auth: Option<bool>,
  /// Default dashboard/login UI language (env: APERIO_UI_LANGUAGE).
  /// Default: `en`.
  #[schemars(extend("examples" = ["en", "tr"]))]
  pub ui_language: Option<String>,
  /// Seconds before a token's expiry to start warning
  /// (env: APERIO_TOKEN_EXPIRY_WARNING). Default: `86400` (24 hours).
  #[schemars(extend("examples" = [7]))]
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
  /// Deprecated spelling of `oidc.client_secret` (env: APERIO_OIDC_CLIENT_SECRET).
  pub oidc_client_secret: Option<String>,

  // --- Structured sections (read directly, not env-mapped) ---
  /// Server-wide request/response header rewrite rules applied to all traffic.
  #[schemars(extend("examples" = [{
    "request": {"add": {"X-Forwarded-Env": "staging"}, "remove": ["X-Internal-Debug"]},
    "response": {"add": {"X-Served-By": "aperio"}, "remove": ["X-Powered-By"]}
  }]))]
  pub headers: Option<HeaderRules>,
  /// Client-less routes: bind a hostname/path to a redirect or fixed response.
  #[schemars(extend("examples" = [[{"hostname": "old.example.com", "redirect": "https://new.example.com", "permanent": true}]]))]
  pub routes: Option<Vec<RouteRule>>,
  /// Operator-defined alert rules over what the server measures
  #[schemars(extend("examples" = [[{"name": "disk-filling", "metric": "store_bytes", "above": 536870912, "for": 300}]]))]
  pub alert_rules: Option<Vec<AlertRule>>,
  /// Recurring maintenance windows, evaluated in their own time zone
  #[schemars(extend("examples" = [[{"hostname": "*.example.com", "from": "02:00", "to": "04:00", "days": ["sat"], "tz": "Europe/Istanbul"}]]))]
  pub maintenance_windows: Option<Vec<MaintenanceWindow>>,
  /// Per-hostname custom 504/503 error pages (override the global
  /// `504_page`/`503_page` for that hostname).
  #[schemars(extend("examples" = [[{"hostname": "app.example.com", "504_page": "./pages/app-504.html"}]]))]
  pub error_pages: Option<Vec<ErrorPageRule>>,
  /// Experimental public TCP expose ports.
  #[schemars(extend("examples" = [[{"port": 5432, "tunnel": "pg_main", "protocol": "tcp"}]]))]
  pub expose: Option<Vec<ExposeEntry>>,
  /// Per-route request rate limits, capping aggregate rps to a host+path.
  #[schemars(extend("examples" = [[{"hostname": "api.example.com", "path": "/api/login", "rps": 5.0, "burst": 10.0}]]))]
  pub rate_limits: Option<Vec<RateLimitRule>>,
  /// Per-hostname fallback URLs answered when no client serves the route.
  #[schemars(extend("examples" = [[{"hostname": "app.example.com", "url": "https://status.example.com", "preserve_path": true}]]))]
  pub fallbacks: Option<Vec<FallbackRule>>,
  /// WAF-lite deny/size rules evaluated before a request is proxied.
  #[schemars(extend("examples" = [[{"path": "^/wp-admin"}, {"header": {"name": "user-agent", "regex": "(?i)sqlmap|nikto"}}, {"path": "^/upload", "max_body": 1048576}]]))]
  pub waf: Option<Vec<WafRule>>,
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
  serde_json::to_string_pretty(&schema).expect("the config schema must serialize")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
