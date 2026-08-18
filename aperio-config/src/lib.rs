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
pub mod egress;
pub mod pairing;

// The schema itself, split by which file each part describes and by what it
// describes about it. Everything is re-exported, so `aperio_config::Thing`
// resolves exactly where it did: this is a move, not a rename.
//
// - [`client`] and [`server`] are the two documents' service-level shapes;
//   [`file`] is `aperio.yaml`'s own top-level key list.
// - [`rules`] is the server's rule-shaped entries (routes, waf, expose, …),
//   which are lists of small records rather than settings.
// - [`groups`] is its grouped blocks, the ones that flatten into
//   `APERIO_<GROUP>_<CHILD>`.
// - [`auth`] is the `auth:` grammar both ends have to agree on.
// - [`settings`] is the scalar-or-block enums a setting can be written as.
//
// What stays here is what the whole schema is built out of: the name, topic
// and protocol rules every part validates against.
pub mod auth;
pub mod changelog;
pub mod client;
pub mod file;
pub mod groups;
pub mod hop_by_hop;
pub mod rules;
pub mod server;
pub mod settings;
pub mod surfaces;

pub use auth::*;
pub use client::*;
pub use file::*;
pub use groups::*;
pub use rules::*;
pub use server::*;
pub use settings::*;

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

#[cfg(test)]
#[path = "names_tests.rs"]
mod names_tests;

#[cfg(test)]
#[path = "topics_tests.rs"]
mod topics_tests;
