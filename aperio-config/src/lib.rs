//! The `aperio.yaml` client configuration schema.
//!
//! These are the exact types `aperio-client` deserializes its config file into.
//! They live in their own crate so the client's build script can emit a JSON
//! Schema (`schemars`) straight from them — the editor schema and the parser can
//! never drift apart. The doc comments below become the `description` of each
//! field in the generated schema, so they double as the `aperio.yaml` reference;
//! keep them to a single purposeful sentence and add `examples` where the value
//! has a specific format.

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
/// in for a name the operator wrote — a suggestion is shown, and whoever is
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
  /// Transport of the tunnel: `tcp` (default), `udp` (best-effort datagram
  /// relay), or `tcp/udp` for a service that is genuinely both (DNS, for
  /// instance). A combined tunnel is one tunnel with one name and one local
  /// port on the binder, answering on both transports.
  #[serde(default = "default_tcp")]
  #[schemars(extend("examples" = ["tcp", "udp", "tcp/udp"]))]
  pub protocol: String,
  /// End-to-end encrypt this tunnel between the two clients (X25519 +
  /// ChaCha20-Poly1305); the server only relays ciphertext. TCP only.
  #[serde(default)]
  pub encrypt: bool,
  /// Pre-shared key mixed into the key derivation of an encrypted tunnel,
  /// protecting against an actively hostile server. Never sent anywhere —
  /// the binder configures the same value in its `bind-tunnels` entry.
  #[serde(default, skip_serializing)]
  #[schemars(extend("examples" = ["a-long-shared-secret-both-sides-hold"]))]
  pub psk: Option<String>,
  /// UDP only: seconds a relay may sit with no datagrams in either direction
  /// before it expires (default 60); binders learn it via tunnel discovery.
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
  /// HSTS `max-age` in seconds (default 63072000 = 2 years).
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
  #[schemars(extend("examples" = ["/health"]))]
  pub endpoint: Option<String>,
  /// Seconds between probes.
  #[schemars(extend("examples" = [10]))]
  pub interval: Option<u64>,
  /// Seconds to wait for each probe before counting it as failed.
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy.
  /// Deprecated spelling of `health.threshold`.
  #[schemars(extend("examples" = [3]))]
  pub threshold: Option<u32>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots
  /// (superseded by `endpoint` when that is set).
  #[schemars(extend("examples" = [true]))]
  pub wait_for_backend: Option<bool>,
}

/// The top level's `health:` block.
///
/// The same fields as [`HealthConfig`] — a file written either way parses
/// identically — except that `endpoint` is on its way out here. The other
/// children are real defaults: a `services:` entry that says nothing about
/// its interval, timeout, threshold or boot wait inherits them. `endpoint` is
/// not, and never was: a probe path belongs to the backend it probes, so the
/// resolver reads it strictly per entry and a top-level one is read by
/// nothing at all once a `services:` list exists.
#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopHealthConfig {
  /// **Deprecated in a config file, removed in 0.7.0.** Endpoint to probe.
  /// Write it on the `services:` entry whose backend it probes; at the top
  /// level it is read only by a file that has no `services:` list.
  #[schemars(extend("examples" = ["/health"], "deprecated" = true))]
  pub endpoint: Option<String>,
  /// Seconds between probes. Applies to every `services:` entry that does not
  /// set its own.
  #[schemars(extend("examples" = [10]))]
  pub interval: Option<u64>,
  /// Seconds to wait for each probe before counting it as failed. Applies to
  /// every `services:` entry that does not set its own.
  #[schemars(extend("examples" = [5]))]
  pub timeout: Option<u64>,
  /// Failed probes in a row before the backend is reported unhealthy. Applies
  /// to every `services:` entry that does not set its own.
  #[schemars(extend("examples" = [3]))]
  pub threshold: Option<u32>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection, avoiding connection-refused errors while it boots. Applies
  /// to every `services:` entry that does not set its own.
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
  /// Gate this service behind your own `user:password` login instead of the server's.
  #[schemars(extend("examples" = ["admin:s3cret"]))]
  pub auth: Option<String>,
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
  /// `- deploy/web` — listen and deliver, nothing else.
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
  /// Seconds a run may take before it is killed (default 60). A command that
  /// hangs must not hold the subscription's one slot forever.
  #[schemars(extend("examples" = [60]))]
  pub timeout: Option<u64>,
  /// Runs allowed at once for this subscription (default 1). Messages that
  /// arrive while the cap is reached are dropped with a warning rather than
  /// queued: a queue for a command that cannot keep up is the same problem
  /// one step later.
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
  /// `pg_main: 15432` — the local port, everything else defaulted.
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
  /// Local address the listener binds. Defaults to `127.0.0.1`; anything else
  /// puts a deliberately unexposed service on the network and is warned about.
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
  /// **Deprecated in a config file, removed in 0.7.0.** Local backend to
  /// expose. Write it as a `services:` entry instead; single-service mode
  /// stays available as the CLI's positional target and `APERIO_TARGET`.
  /// `h2c://` / `h2://` targets are dialed over HTTP/2 (gRPC).
  #[schemars(extend("examples" = ["http://localhost:3000", "3000", "h2c://127.0.0.1:50051"], "deprecated" = true))]
  pub target: Option<String>,
  /// **Deprecated in a config file, removed in 0.7.0.** Serve a local
  /// directory of static files instead of forwarding to a backend. Write it
  /// as a `services:` entry's `serve:`; the CLI's `--serve` and
  /// `APERIO_SERVE` are unaffected.
  #[schemars(extend("examples" = ["./dist"], "deprecated" = true))]
  pub serve: Option<String>,
  /// **Deprecated in a config file, removed in 0.7.0.** Public hostname(s)
  /// to claim for this client's traffic. A bind belongs to the service it
  /// binds, so write it on the `services:` entry; `--hostname` and
  /// `APERIO_HOSTNAME` are unaffected.
  #[schemars(extend("examples" = ["app.example.com", ["app.example.com", "www.example.com"]], "deprecated" = true))]
  pub hostname: Option<Hostnames>,
  /// **Deprecated in a config file, removed in 0.7.0.** Public path prefix
  /// to claim for this client's traffic. Write it on the `services:` entry
  /// it binds; `--path` and `APERIO_PATH` are unaffected.
  #[schemars(extend("examples" = ["/api"], "deprecated" = true))]
  pub path: Option<String>,
  /// Strip the path prefix before forwarding, so the backend sees `/` not the bind.
  #[schemars(extend("examples" = [true]))]
  pub trim_bind: Option<bool>,
  /// Forward the visitor's original Host header to the backend instead of the target's.
  #[schemars(extend("examples" = [false]))]
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
  /// **Deprecated in a config file, removed in 0.7.0.** Raw TCP backend to
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
  /// **Deprecated in a config file, removed in 0.7.0.** Backend health
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
  #[schemars(extend("examples" = [true]))]
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
  #[schemars(extend("examples" = [true]))]
  pub cache: Option<bool>,
  /// Keep serving this service's cached responses (marked, even past their
  /// lifetime) while no healthy client is connected, instead of failing with
  /// 504 (needs `cache: true` and the server-side cache enabled).
  #[schemars(extend("examples" = [true]))]
  pub resilience: Option<bool>,
  /// Persist inbound POST requests (third-party webhooks) into the server's
  /// webhook inbox, for browsing and re-firing (services may override).
  #[schemars(extend("examples" = [true]))]
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
  /// directory of this client.
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
  /// Log verbosity of this client (env: LOG_LEVEL; `RUST_LOG` overrides both).
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
  /// Burst capacity; defaults to one second's worth of `rps`.
  #[schemars(extend("examples" = [100.0]))]
  pub burst: Option<f64>,
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
  #[serde(default)]
  pub permanent: bool,
  /// Append the requested path to the target URL.
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
  #[schemars(extend("examples" = [{"status": 503, "body": "Be right back", "content_type": "text/plain"}]))]
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

/// The yaml keys that describe a *single* service at the top level.
///
/// They are the file's spelling of the CLI's single-service shorthand, and
/// they are on their way out of the file format: a config file is the place
/// where a deployment is written down, and having two shapes for "what this
/// client exposes" — one that only works when the other is absent — is a
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
  /// Error-rate alert threshold, 0..1.
  #[schemars(extend("examples" = [0.25]))]
  pub error_rate: Option<f64>,
  /// Alert sliding-window seconds.
  #[schemars(extend("examples" = [300]))]
  pub window: Option<u64>,
  /// Minimum requests in the window before the error-rate alert fires.
  #[schemars(extend("examples" = [20]))]
  pub min_requests: Option<u64>,
  /// Connected-client floor below which the client-down alert fires.
  #[schemars(extend("examples" = [1]))]
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
  #[schemars(extend("examples" = [10485760]))]
  pub max_size: Option<u64>,
  /// Rotated audit log files kept.
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
  /// Enable the server-side GET response cache.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Response-cache budget in bytes.
  #[schemars(extend("examples" = [67108864]))]
  pub max_bytes: Option<u64>,
  /// Serve-stale window in seconds for resilient services.
  #[schemars(extend("examples" = [3600]))]
  pub max_stale: Option<u64>,
  /// Seconds to briefly cache error / negative responses (e.g. `404`), so a
  /// hot missing URL cannot hammer the backend. `0` = disabled.
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
  /// Serve the admin dashboard.
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
  /// Also publish hostnames a token permits but no client serves yet
  ///.
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
  /// In-flight failover mode.
  #[schemars(extend("examples" = ["fail", "retry", "wait", "retry-wait"]))]
  pub mode: Option<String>,
  /// Maximum failover re-dispatches per request.
  #[schemars(extend("examples" = [2]))]
  pub max_jumps: Option<u32>,
  /// Failover window in seconds.
  #[schemars(extend("examples" = [300]))]
  pub window: Option<u64>,
  /// Allow failover for non-idempotent methods too.
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
  /// Seconds to wait for a client connection.
  #[schemars(extend("examples" = [10]))]
  pub timeout: Option<u64>,
  /// Seconds to wait for a client response.
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
  /// Per-IP rate-limit burst.
  #[schemars(extend("examples" = [120]))]
  pub max: Option<u64>,
  /// Per-IP rate-limit refill per second.
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
  /// Failed logins per IP before a lockout.
  #[schemars(extend("examples" = [5]))]
  pub threshold: Option<u32>,
  /// Base lockout seconds, doubled per repeat.
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
  /// Prometheus metrics endpoint toggle.
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
  /// OIDC scopes.
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
  /// Enable OpenTelemetry OTLP export.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// OTLP endpoint. The conventional ports are 4318 for OTLP/HTTP and 4317
  /// for OTLP/gRPC; `protocol` follows from the port unless it is set.
  #[schemars(extend("examples" = ["http://localhost:4318", "http://localhost:4317"]))]
  pub endpoint: Option<String>,
  /// OTLP transport: `http` (protobuf over HTTP) or `grpc`. Unset picks `grpc`
  /// for an endpoint on port 4317 and `http` everywhere else — a collector
  /// answering the wrong protocol drops every span silently, so pin this when
  /// the endpoint runs on a non-standard port.
  #[schemars(extend("examples" = ["http", "grpc"]))]
  pub protocol: Option<String>,
  /// OTLP service name.
  #[schemars(extend("examples" = ["aperio"]))]
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
  #[schemars(extend("examples" = [7]))]
  pub captures: Option<u64>,
  /// Days to keep access-log file lines; 0/unset = forever.
  #[schemars(extend("examples" = [30]))]
  pub access_log: Option<u64>,
  /// Days to keep audit events; 0/unset = forever.
  #[schemars(extend("examples" = [90]))]
  pub audit: Option<u64>,
  /// Days to keep day-granularity stats buckets; 0/unset = the built-in caps.
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
  /// Honor client `scaling:` declarations.
  #[schemars(extend("examples" = [true]))]
  pub enabled: Option<bool>,
  /// Allow a plain-http autoscaling endpoint.
  #[schemars(extend("examples" = [false]))]
  pub allow_http: Option<bool>,
  /// Allow an autoscaling endpoint on a private or loopback address, for a
  /// provider API that genuinely lives on the internal network.
  #[schemars(extend("examples" = [false]))]
  pub allow_private: Option<bool>,
  /// Seconds after which an unrefreshed autoscaling record is dropped
  ///.
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
  #[schemars(extend("examples" = [["hooks.example.com", "*.provider.example"]]))]
  pub allowlist: Option<Vec<String>>,
  /// With no allowlist: refuse destinations resolving to internal addresses
  /// (loopback, RFC 1918, link-local/metadata, CGNAT, unique-local).
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
  /// Seconds between snapshots; unset or `0` disables scheduled backups.
  #[schemars(extend("examples" = [86400]))]
  pub interval: Option<u64>,
  /// Directory the snapshots are written to (default `<data_dir>/backups`).
  #[schemars(extend("examples" = ["/var/backups/aperio"]))]
  pub dir: Option<String>,
  /// Snapshots to keep; older ones are pruned.
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
  #[schemars(extend("examples" = [2097152]))]
  pub pause_bytes: Option<u64>,
  /// Backlog bytes under which a paused producer is asked to resume.
  #[schemars(extend("examples" = [524288]))]
  pub resume_bytes: Option<u64>,
  /// Hard per-stream backlog cap in bytes (drops producers that cannot pause).
  #[schemars(extend("examples" = [16777216]))]
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
  #[schemars(extend("examples" = [{"error_rate": 0.25, "window": 300, "min_requests": 20, "client_down": 1}]))]
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
  #[schemars(extend("examples" = [true, {"enabled": true, "auth": "ops:s3cret"}]))]
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
  #[schemars(extend("examples" = [true]))]
  pub require_hostname_bind: Option<bool>,
  /// Wildcard pattern granting each client a random subdomain (env: APERIO_RANDOM_SUBDOMAIN).
  #[schemars(extend("examples" = ["*.example.com"]))]
  pub random_subdomain: Option<String>,
  /// Inject noindex headers for random-subdomain preview services (env: APERIO_PREVIEW_NOINDEX).
  #[schemars(extend("examples" = [true]))]
  pub preview_noindex: Option<bool>,
  /// Seconds without a heartbeat before a client is considered down (env: APERIO_CLIENT_DOWN_THRESHOLD).
  #[schemars(extend("examples" = [15]))]
  pub client_down_threshold: Option<u64>,
  /// Load-balancing strategy (env: APERIO_LB_STRATEGY).
  #[schemars(extend("examples" = ["round-robin", "primary-standby", "sticky"]))]
  pub lb_strategy: Option<String>,
  /// Passive outlier ejection: temporarily drop a client from the pool when it
  /// returns too many errors in a window (env: APERIO_OUTLIER_EJECTION).
  #[schemars(extend("examples" = [true]))]
  pub outlier_ejection: Option<bool>,
  /// Failures inside the window before a client is ejected
  /// (env: APERIO_OUTLIER_MAX_FAILURES).
  #[schemars(extend("examples" = [5]))]
  pub outlier_max_failures: Option<u32>,
  /// Sliding window in seconds the failures are counted over
  /// (env: APERIO_OUTLIER_WINDOW).
  #[schemars(extend("examples" = [30]))]
  pub outlier_window: Option<u64>,
  /// Seconds an ejected client stays out before re-admission
  /// (env: APERIO_OUTLIER_EJECT_SECS).
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
  #[schemars(extend("examples" = [10485760]))]
  pub max_body_size: Option<u64>,
  /// Concurrent proxied requests limit (env: APERIO_MAX_CONCURRENT_REQUESTS).
  #[schemars(extend("examples" = [512]))]
  pub max_concurrent_requests: Option<u64>,
  /// Maximum simultaneously connected clients (env: APERIO_MAX_TUNNELS).
  #[schemars(extend("examples" = [10]))]
  pub max_tunnels: Option<u64>,
  /// Maximum concurrently-live proxied public WebSockets; they are long-lived,
  /// so they get their own ceiling separate from `max_concurrent_requests`.
  /// `0` = uncapped (env: APERIO_MAX_WS_CONNECTIONS).
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
  #[schemars(extend("examples" = [true]))]
  pub trust_proxy: Option<bool>,
  /// Trusted proxy IPs/CIDRs (env: APERIO_TRUSTED_PROXIES).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub trusted_proxies: Option<Vec<String>>,
  /// Header carrying the real client IP (env: APERIO_REAL_IP_HEADER).
  #[schemars(extend("examples" = ["CF-Connecting-IP"]))]
  pub real_ip_header: Option<String>,
  /// Trust the Cloudflare client-IP header (env: APERIO_TRUST_CF_HEADER).
  #[schemars(extend("examples" = [true]))]
  pub trust_cf_header: Option<bool>,
  /// Mark session cookies `Secure` (env: APERIO_SECURE_COOKIES).
  #[schemars(extend("examples" = [true]))]
  pub secure_cookies: Option<bool>,
  /// Source IPs/CIDRs allowed to reach the authenticated admin surface (the
  /// dashboard and `/aperio/api/*`); empty = no restriction. An invalid entry
  /// refuses startup rather than applying a partial allowlist
  /// (env: APERIO_ADMIN_ALLOWED_IPS).
  #[schemars(extend("examples" = [["10.0.0.0/8"]]))]
  pub admin_allowed_ips: Option<Vec<String>>,

  // --- Tunnel & cache ---
  /// zlib-compress tunnel frames (env: APERIO_TUNNEL_COMPRESSION).
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
  /// Watch the config file and apply live-editable changes without a restart.
  /// On by default (env: APERIO_CONFIG_HOT_RELOAD).
  #[schemars(extend("examples" = [true]))]
  pub config_hot_reload: Option<bool>,
  /// Bind the listener with SO_REUSEPORT, so a second process can take over
  /// the port for a zero-downtime handover (env: APERIO_REUSEPORT).
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
  /// inspector, the cURL copy and the HAR export. On by default
  /// (env: APERIO_INSPECTOR_REDACT).
  #[schemars(extend("examples" = [true]))]
  pub inspector_redact: Option<bool>,
  /// Seconds between webhook delivery retries; empty = no retries
  /// (env: APERIO_WEBHOOK_RETRY_SCHEDULE).
  #[schemars(extend("examples" = [[1, 5, 25, 60]]))]
  pub webhook_retry_schedule: Option<Vec<u64>>,
  /// Seconds between availability-history ticks for the dashboard's Uptime
  /// panel, minimum 1 (env: APERIO_UPTIME_TICK_SECS).
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
  /// Passkey relying-party ID, when it must differ from the origin's domain
  /// (env: APERIO_WEBAUTHN_RP_ID).
  #[schemars(extend("examples" = ["example.com"]))]
  pub webauthn_rp_id: Option<String>,
  /// Pin a dynamic token to the first device key that presents it; a later
  /// connection with a different (or missing) key is rejected
  /// (env: APERIO_TOKEN_PINNING).
  #[schemars(extend("examples" = [true]))]
  pub token_pinning: Option<bool>,
  /// Ignore client-declared visitor passwords (env: APERIO_IGNORE_CLIENT_AUTH).
  #[schemars(extend("examples" = [false]))]
  pub ignore_client_auth: Option<bool>,
  /// Default dashboard/login UI language (env: APERIO_UI_LANGUAGE).
  #[schemars(extend("examples" = ["en", "tr"]))]
  pub ui_language: Option<String>,
  /// Days before a token's expiry to start warning (env: APERIO_TOKEN_EXPIRY_WARNING).
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
