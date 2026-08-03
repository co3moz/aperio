//! Help for a config file that a person types by hand (planned_features #63).
//!
//! Two things live here, and they answer the same complaint from opposite
//! ends: the file is written by hand, and nothing helps.
//!
//! * **Template variables**, so one file can serve several environments
//!   without being copied per environment, which is how two files drift.
//! * **A suggestion for a key nobody recognizes**, because an unknown key is
//!   silently ignored, and a setting that is silently ignored is the most
//!   expensive kind of typo: the file says the thing is configured and the
//!   behavior says it is not.

/// Expands `${NAME}` and `${NAME:-default}` from the environment.
///
/// Only this one spelling is expanded. A bare `$NAME` is left alone on
/// purpose: `$` appears in generated passwords, in regular expressions and in
/// shell snippets inside `run:` commands, and a config loader that rewrites
/// those would corrupt working files to make templating slightly prettier.
///
/// An unset variable with no default is an error rather than an empty string.
/// Substituting nothing produces a config file that parses and means something
/// else, `hostname: .example.com` or an empty token, which fails later and
/// somewhere unrelated.
pub fn expand_vars(text: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String, String> {
  let mut out = String::with_capacity(text.len());
  let bytes = text.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
      let Some(end) = text[i + 2..].find('}').map(|p| i + 2 + p) else {
        return Err(format!(
          "unterminated `${{` in the config file at byte {i}; write `}}` or escape the `$`"
        ));
      };
      let inner = &text[i + 2..end];
      let (name, default) = match inner.split_once(":-") {
        Some((n, d)) => (n.trim(), Some(d)),
        None => (inner.trim(), None),
      };
      if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(format!(
          "`${{{inner}}}` is not a variable name (letters, digits and underscore)"
        ));
      }
      match lookup(name).or_else(|| default.map(str::to_string)) {
        Some(value) => out.push_str(&value),
        None => {
          return Err(format!(
            "`${{{name}}}` is not set in the environment; set it, or write a default as `${{{name}:-value}}`"
          ));
        }
      }
      i = end + 1;
      continue;
    }
    let ch = text[i..].chars().next().unwrap_or('$');
    out.push(ch);
    i += ch.len_utf8();
  }
  Ok(out)
}

/// Edit distance, capped: anything past `max` is not a suggestion anyway, and
/// stopping early keeps this linear in practice over a list of key names.
fn distance_within(a: &str, b: &str, max: usize) -> Option<usize> {
  if a.len().abs_diff(b.len()) > max {
    return None;
  }
  let a: Vec<char> = a.chars().collect();
  let b: Vec<char> = b.chars().collect();
  let mut prev: Vec<usize> = (0..=b.len()).collect();
  let mut cur = vec![0usize; b.len() + 1];
  for (i, ca) in a.iter().enumerate() {
    cur[0] = i + 1;
    for (j, cb) in b.iter().enumerate() {
      let cost = usize::from(ca != cb);
      cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
    }
    if cur.iter().min().copied().unwrap_or(usize::MAX) > max {
      return None;
    }
    std::mem::swap(&mut prev, &mut cur);
  }
  let d = prev[b.len()];
  (d <= max).then_some(d)
}

/// The key `unknown` was most likely meant to be, if any is close enough.
///
/// The threshold scales with the length of the name, because one wrong letter
/// in `tls` and one wrong letter in `security_headers` are not the same kind
/// of mistake: a short name has too few letters for a near miss to mean
/// anything, and suggesting `path` for `math` would be noise.
pub fn suggest<'a>(unknown: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
  let budget = match unknown.chars().count() {
    0..=3 => return None,
    4..=7 => 1,
    _ => 2,
  };
  let lower = unknown.to_ascii_lowercase();
  let mut best: Option<(usize, &str)> = None;
  for candidate in known {
    // An exact match under a different case, or one written with dashes, is a
    // certainty rather than a guess, so it wins outright.
    let normalized = candidate.to_ascii_lowercase();
    if lower.replace('-', "_") == normalized {
      return Some(candidate);
    }
    if let Some(d) = distance_within(&lower, &normalized, budget)
      && best.is_none_or(|(bd, _)| d < bd)
    {
      best = Some((d, candidate));
    }
  }
  best.map(|(_, name)| name)
}

/// Top-level keys `aperio.yaml` accepts, and the keys a `services:` entry
/// accepts, read from the generated schemas.
///
/// Read from the schema rather than listed here: a list would be correct on
/// the day it was written and wrong at the next release, which is exactly the
/// failure the suggestion is trying to prevent.
pub fn known_keys() -> &'static (Vec<String>, Vec<String>) {
  static KEYS: std::sync::OnceLock<(Vec<String>, Vec<String>)> = std::sync::OnceLock::new();
  KEYS.get_or_init(|| {
    let schema: serde_json::Value =
      serde_json::from_str(&crate::schema_json()).unwrap_or(serde_json::Value::Null);
    let names = |node: &serde_json::Value| -> Vec<String> {
      node
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
    };
    let top = names(&schema);
    let service = schema
      .get("$defs")
      .and_then(|d| d.get("ServiceEntry"))
      .map(names)
      .unwrap_or_default();
    (top, service)
  })
}

#[cfg(test)]
#[path = "authoring_tests.rs"]
mod tests;
