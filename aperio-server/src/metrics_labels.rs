//! Static Prometheus labels announced by a client (planned_features #53).
//!
//! One Prometheus scraping one Aperio server sees every client's series under
//! the same name, distinguished only by `client_id`. Telling `prod` from
//! `staging`, or `eu-west` from `us-east`, meant writing relabelling rules in
//! Prometheus against ids the client already knows the answer for.
//!
//! Everything here exists because these labels come from *clients*, and label
//! cardinality is how a metrics backend dies. A client cannot be trusted to be
//! careful with a namespace it shares with every other client, so the names
//! and values are validated and capped on arrival rather than on the way out:
//! a series, once scraped, is in the backend whatever the server does later.

use std::collections::BTreeMap;

/// Labels one client may announce. Small on purpose: this is a dimension for
/// grouping deployments, not a place to attach request metadata.
const MAX_LABELS: usize = 8;
/// Longest label name accepted.
const MAX_NAME: usize = 32;
/// Longest label value accepted.
const MAX_VALUE: usize = 64;

/// Names the server writes itself on these series, which a client may not
/// take over. Letting it would produce two labels of the same name in one
/// series, which is not valid exposition, and would let a client relabel
/// itself as another.
const RESERVED: &[&str] = &["client_id", "job", "instance", "token", "hostname", "limit"];

/// Validates and caps what a client announced.
///
/// Returns the labels worth keeping, in name order so a series is byte-stable
/// between scrapes. Anything invalid is dropped rather than rejected as a
/// whole: a typo'd label is not a reason to lose the metrics of a client that
/// is otherwise fine.
pub(crate) fn sanitize(raw: &BTreeMap<String, String>) -> Vec<(String, String)> {
  let mut out: Vec<(String, String)> = Vec::new();
  for (name, value) in raw {
    if out.len() >= MAX_LABELS {
      break;
    }
    let name = name.trim();
    let value = value.trim();
    if !valid_name(name) || value.is_empty() || value.len() > MAX_VALUE {
      continue;
    }
    if RESERVED.contains(&name) {
      continue;
    }
    out.push((name.to_string(), value.to_string()));
  }
  out
}

/// True for a Prometheus label name: `[a-zA-Z_][a-zA-Z0-9_]*`, and not one of
/// the `__`-prefixed names the exposition format reserves for itself.
fn valid_name(name: &str) -> bool {
  if name.is_empty() || name.len() > MAX_NAME || name.starts_with("__") {
    return false;
  }
  let mut chars = name.chars();
  let first = chars.next().unwrap_or('0');
  if !(first.is_ascii_alphabetic() || first == '_') {
    return false;
  }
  chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Renders sanitized labels as the tail of a label set, each one preceded by a
/// comma so it appends after an existing label.
///
/// Values are escaped as the exposition format requires. This is the second
/// line of defence rather than the first: `sanitize` has already dropped the
/// shapes that could break a series, and escaping here means a value that
/// somehow arrives with a quote in it produces a valid line instead of a
/// corrupt scrape.
pub(crate) fn render(labels: &[(String, String)]) -> String {
  let mut out = String::new();
  for (name, value) in labels {
    out.push(',');
    out.push_str(name);
    out.push_str("=\"");
    for c in value.chars() {
      match c {
        '\\' => out.push_str("\\\\"),
        '"' => out.push_str("\\\""),
        '\n' => out.push_str("\\n"),
        other => out.push(other),
      }
    }
    out.push('"');
  }
  out
}

#[cfg(test)]
#[path = "metrics_labels_tests.rs"]
mod tests;
