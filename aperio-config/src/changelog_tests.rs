//! That every version in `CHANGELOG.md` groups its entries once.
//!
//! The failure this catches is quiet by construction: a repeated heading loses
//! nothing and reads correctly line by line, and it is only visible to
//! somebody who scrolls the whole version looking for a second list they have
//! no reason to expect. It happened four times before anything checked.

use super::*;

fn changelog() -> String {
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../CHANGELOG.md");
  std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// One version heading and the section headings under it, in file order.
fn versions(text: &str) -> Vec<(String, Vec<String>)> {
  let mut out: Vec<(String, Vec<String>)> = Vec::new();
  for line in text.lines() {
    if let Some(rest) = line.strip_prefix("## ") {
      out.push((rest.trim().to_string(), Vec::new()));
    } else if let Some(rest) = line.strip_prefix("### ")
      && let Some(last) = out.last_mut()
    {
      last.1.push(rest.trim().to_string());
    }
  }
  out
}

fn excused(version: &str) -> bool {
  EXCUSED.iter().any(|e| version.starts_with(e.version))
}

/// No version carries the same section twice.
#[test]
fn a_version_groups_its_entries_once() {
  let text = changelog();
  let mut bad = Vec::new();
  for (version, sections) in versions(&text) {
    if excused(&version) {
      continue;
    }
    let mut seen = Vec::new();
    for s in &sections {
      if seen.contains(s) {
        bad.push(format!("  {version} repeats `### {s}`"));
      }
      seen.push(s.clone());
    }
  }
  assert!(
    bad.is_empty(),
    "a repeated section splits one group in two, and a reader who finds the \
     first list has no way to know the second exists:\n{}",
    bad.join("\n")
  );
}

/// Sections appear in the order the file uses, and no other section exists.
#[test]
fn the_sections_are_the_known_ones_in_the_files_order() {
  let text = changelog();
  let mut bad = Vec::new();
  for (version, sections) in versions(&text) {
    if excused(&version) {
      continue;
    }
    for s in &sections {
      if !SECTION_ORDER.contains(&s.as_str()) {
        bad.push(format!(
          "  {version} has `### {s}`, which is not one of {SECTION_ORDER:?}"
        ));
      }
    }
    let ranks: Vec<usize> = sections
      .iter()
      .filter_map(|s| SECTION_ORDER.iter().position(|k| k == s))
      .collect();
    if ranks.windows(2).any(|w| w[0] > w[1]) {
      bad.push(format!("  {version} lists {sections:?}, out of order"));
    }
  }
  assert!(bad.is_empty(), "changelog sections:\n{}", bad.join("\n"));
}

/// Every entry sits under a section rather than loose under the version.
#[test]
fn no_entry_floats_outside_a_section() {
  let text = changelog();
  let mut version: Option<String> = None;
  let mut section = false;
  let mut bad = Vec::new();
  for line in text.lines() {
    if let Some(rest) = line.strip_prefix("## ") {
      version = Some(rest.trim().to_string());
      section = false;
    } else if line.starts_with("### ") {
      section = true;
    } else if line.starts_with("- **")
      && let Some(v) = &version
      && !section
      && v.starts_with('[')
    {
      bad.push(format!("  {v}: {}", &line[..line.len().min(70)]));
    }
  }
  assert!(
    bad.is_empty(),
    "an entry under no section is invisible to anyone reading by section:\n{}",
    bad.join("\n")
  );
}

/// Every excused version exists and still needs excusing.
///
/// The same shape as the exemption checks in `surfaces.rs`: a list of things
/// deliberately left wrong is only safe while it is accurate. An entry for a
/// version that has since been repaired would quietly stop the check from
/// covering it.
#[test]
fn every_excused_version_exists_and_is_still_malformed() {
  let text = changelog();
  let all = versions(&text);
  for e in EXCUSED {
    assert!(
      !e.reason.trim().is_empty(),
      "excused {} without saying why",
      e.version
    );
    let found = all
      .iter()
      .find(|(v, _)| v.starts_with(e.version))
      .unwrap_or_else(|| panic!("excused {} is not in the changelog", e.version));
    assert!(
      malformed(&found.1),
      "{} now follows the shape, so its exemption should be removed",
      e.version
    );
  }
}

/// True when a section list breaks any of the three rules the tests enforce.
fn malformed(sections: &[String]) -> bool {
  let mut seen: Vec<&String> = Vec::new();
  for s in sections {
    if seen.contains(&s) {
      return true;
    }
    seen.push(s);
  }
  if sections
    .iter()
    .any(|s| !SECTION_ORDER.contains(&s.as_str()))
  {
    return true;
  }
  let ranks: Vec<usize> = sections
    .iter()
    .filter_map(|s| SECTION_ORDER.iter().position(|k| k == s))
    .collect();
  ranks.windows(2).any(|w| w[0] > w[1])
}
