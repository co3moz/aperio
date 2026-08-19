//! That every `APERIO_` variable the repository's test harnesses set is one a
//! binary actually reads.
//!
//! This is here rather than beside the harnesses because of what breaks it.
//! The harnesses are JavaScript and nothing type-checks a string they put in an
//! environment; the names they use are correct or not according to what the
//! Rust reads, so a rename here is what makes one of them wrong, and this is
//! the suite that a rename here runs (project rule 25).
//!
//! Two were found by hand and neither had ever failed anything. `tests/soak`
//! set `APERIO_RATE_LIMIT: '0'` believing it turned the per-visitor limiter
//! off, and there is no such setting, so the soak measured memory with the
//! limiter on. `tests/conformance/h2spec` set `APERIO_PATH_BIND`, whose real
//! name is `APERIO_PATH`, and passed anyway because a client with no bind
//! serves everything. That is the shape of this bug: the harness does not
//! fail, it quietly tests something else.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Names a harness legitimately sets that no binary reads.
///
/// Two kinds only, and both are about the harness rather than the product:
/// where a binary is, and what a setting used to be called. The older
/// spellings are set on purpose by the compatibility suite, which drives
/// releases old enough to know only those.
const HARNESS_OWN: &[&str] = &[
  "APERIO_SERVER_BIN",
  "APERIO_CLIENT_BIN",
  "APERIO_CLIENT_TARGET",
  "APERIO_HOSTNAME_BIND",
];

fn repo_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .expect("the crate sits in the workspace")
    .to_path_buf()
}

/// Every `APERIO_...` token appearing in a file tree, by shallow recursion
/// over the extensions the harnesses are written in.
fn names_in(dir: &Path, out: &mut BTreeSet<String>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    let name = path
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
      .to_string();
    if name == "node_modules" || name == "reports" || name.starts_with('.') {
      continue;
    }
    if path.is_dir() {
      names_in(&path, out);
      continue;
    }
    let is_source = matches!(
      path.extension().and_then(|e| e.to_str()),
      Some("mjs" | "js" | "ts" | "tsx")
    );
    if !is_source {
      continue;
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
      continue;
    };
    // Comments stripped first, for the reason the `known` side takes literals
    // only: prose naming a variable is not a harness setting one, and a note
    // explaining a name that was wrong would otherwise report itself.
    let text = strip_comments(&raw);
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(found) = text[i..].find("APERIO_") {
      let start = i + found;
      let mut end = start + "APERIO_".len();
      while end < bytes.len()
        && (bytes[end].is_ascii_uppercase() || bytes[end].is_ascii_digit() || bytes[end] == b'_')
      {
        end += 1;
      }
      // A trailing underscore is a prefix being built, not a name.
      let token = text[start..end].trim_end_matches('_').to_string();
      if token.len() > "APERIO_".len() {
        out.insert(token);
      }
      i = end;
    }
  }
}

/// The source with `//` and `/* */` comments blanked out.
///
/// Crude on purpose: a `//` inside a string literal (a URL, say) costs this a
/// few names it would have found, and the `used` set is only ever compared
/// against what the product declares, so the failure mode is a missed warning
/// rather than a false one.
fn strip_comments(src: &str) -> String {
  let mut out = String::with_capacity(src.len());
  let bytes = src.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i..].starts_with(b"//") {
      while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
      }
    } else if bytes[i..].starts_with(b"/*") {
      i += 2;
      while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
        i += 1;
      }
      i = (i + 2).min(bytes.len());
    } else {
      out.push(bytes[i] as char);
      i += 1;
    }
  }
  out
}

#[test]
fn every_harness_env_var_is_one_a_binary_reads() {
  let root = repo_root();

  let mut used = BTreeSet::new();
  names_in(&root.join("tests"), &mut used);
  names_in(&root.join("aperio-dashboard/scripts"), &mut used);
  assert!(
    used.len() > 20,
    "the scan found {} names, which means it stopped finding the harnesses \
     rather than that they stopped setting anything",
    used.len()
  );

  // What the product knows: every name read as a literal in the crates, plus
  // the documented table, which project rule 17 requires a setting to reach.
  let mut known = BTreeSet::new();
  for crate_dir in [
    "aperio-server/src",
    "aperio-client/src",
    "aperio-config/src",
  ] {
    collect_literals(&root.join(crate_dir), &mut known);
  }
  let documented = std::fs::read_to_string(root.join("docs/configuration.md")).unwrap_or_default();

  let unknown: Vec<&String> = used
    .iter()
    .filter(|n| !HARNESS_OWN.contains(&n.as_str()))
    .filter(|n| !known.contains(*n))
    .filter(|n| !documented.contains(n.as_str()))
    .collect();

  assert!(
    unknown.is_empty(),
    "a test harness sets {unknown:?}, which no binary reads and no \
     documentation describes. Either the name is wrong, in which case the \
     harness has been testing something other than what it says, or the \
     setting was renamed and the harness was not. Add it to HARNESS_OWN only \
     if it configures the harness itself rather than the product."
  );
}

/// Every `"APERIO_..."` *string literal* under `dir`.
///
/// Literals rather than the whole text, because an env var is read by naming
/// it in quotes and a comment naming one proves nothing. The first version of
/// this test collected the text and passed while the bug was reintroduced: the
/// doc comment above mentions `APERIO_RATE_LIMIT`, so the file made its own
/// counter-example look like a setting.
fn collect_literals(dir: &Path, out: &mut BTreeSet<String>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_dir() {
      collect_literals(&path, out);
      continue;
    }
    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
      continue;
    }
    // This file's own allowlist is not evidence about the product.
    if path
      .file_name()
      .is_some_and(|n| n == "harness_env_tests.rs")
    {
      continue;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
      continue;
    };
    for piece in text.split('"').skip(1).step_by(2) {
      if piece.starts_with("APERIO_")
        && piece
          .chars()
          .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
      {
        out.insert(piece.to_string());
      }
    }
  }
}
