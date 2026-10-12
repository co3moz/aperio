//! That every link in the documentation leads somewhere: each relative link
//! in the Markdown pages names a file that exists and, with an anchor, a
//! heading that file has, and so does every page the dashboard links its
//! help to.
//!
//! The pages were merged and renamed in one sweep, and a link that broke then
//! breaks silently: nothing reads a page's links but a person who has already
//! clicked one. The dashboard's half matters most, since a released dashboard
//! keeps its links for as long as it runs.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, keep: &dyn Fn(&Path) -> bool) {
  let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
  for entry in entries {
    let path = entry.expect("a directory entry").path();
    if path.is_dir() {
      if path.file_name().is_some_and(|n| n == "node_modules") {
        continue;
      }
      collect(&path, out, keep);
    } else if keep(&path) {
      out.push(path);
    }
  }
}

fn read(path: &Path) -> String {
  std::fs::read_to_string(path)
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    .replace("\r\n", "\n")
}

/// The text outside fenced code blocks and inline code, where a link is a
/// link and a heading is a heading.
fn prose(text: &str) -> String {
  let mut out = String::new();
  let mut fenced = false;
  for line in text.lines() {
    if line.trim_start().starts_with("```") {
      fenced = !fenced;
      continue;
    }
    if fenced {
      continue;
    }
    for (i, part) in line.split('`').enumerate() {
      if i % 2 == 0 {
        out.push_str(part);
      }
    }
    out.push('\n');
  }
  out
}

/// `[text](url)` as `text`, and HTML tags gone: what a heading's anchor is
/// made from.
fn heading_text(heading: &str) -> String {
  let mut out = String::new();
  let mut chars = heading.chars().peekable();
  let mut in_tag = false;
  let mut in_url = false;
  while let Some(c) = chars.next() {
    match c {
      '<' => in_tag = true,
      '>' if in_tag => in_tag = false,
      _ if in_tag => {}
      ']' if chars.peek() == Some(&'(') => {
        chars.next();
        in_url = true;
      }
      ')' if in_url => in_url = false,
      _ if in_url => {}
      '[' => {}
      _ => out.push(c),
    }
  }
  out
}

/// The anchors GitHub gives a page's headings: lower case, punctuation
/// dropped, spaces as dashes, and a repeated one numbered.
fn anchors(text: &str) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  let mut seen: Vec<String> = Vec::new();
  let mut fenced = false;
  for line in text.lines() {
    if line.trim_start().starts_with("```") {
      fenced = !fenced;
      continue;
    }
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if fenced || hashes == 0 || hashes > 6 || !line[hashes..].starts_with(' ') {
      continue;
    }
    let slug: String = heading_text(line[hashes..].trim())
      .to_lowercase()
      .chars()
      .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
      .map(|c| if c == ' ' { '-' } else { c })
      .collect();
    let repeats = seen.iter().filter(|s| **s == slug).count();
    seen.push(slug.clone());
    out.push(if repeats == 0 {
      slug
    } else {
      format!("{slug}-{repeats}")
    });
  }
  out
}

/// Whether `url`, relative to `from`, names a file and, when it has an
/// anchor, a heading of it; `None` when it does, the reason when not.
fn broken(from: &Path, url: &str) -> Option<String> {
  if url.contains("://") || url.starts_with("mailto:") {
    return None;
  }
  let (path, anchor) = url.split_once('#').unwrap_or((url, ""));
  let target = if path.is_empty() {
    from.to_path_buf()
  } else {
    from.parent().expect("a page has a directory").join(path)
  };
  if !target.exists() {
    return Some(format!("{url}: no such file"));
  }
  let is_page = target.extension().is_some_and(|e| e == "md");
  if !anchor.is_empty() && is_page && !anchors(&read(&target)).iter().any(|a| a == anchor) {
    return Some(format!("{url}: no such heading"));
  }
  None
}

/// The changelog's newest section: `[Unreleased]`, or the version about to
/// be tagged, whichever is on top. That is the one still being written; an
/// older section links to the pages as they were at its tag, which is where
/// a reader of it finds them.
fn newest_section(changelog: &str) -> String {
  let Some(start) = changelog.find("\n## [") else {
    return String::new();
  };
  let rest = &changelog[start + 1..];
  let end = rest[3..].find("\n## [").map_or(rest.len(), |i| i + 3);
  rest[..end].to_string()
}

/// Every link in `text` that leads nowhere, as `page: why`.
fn broken_links(page: &Path, text: &str, problems: &mut Vec<String>) {
  let text = prose(text);
  let mut rest = text.as_str();
  while let Some(at) = rest.find("](") {
    let after = &rest[at + 2..];
    let end = after.find([')', ' ', '\n']).unwrap_or(after.len());
    if let Some(why) = broken(page, &after[..end]) {
      problems.push(format!("{}: {why}", page.display()));
    }
    rest = &after[end..];
  }
}

#[test]
fn every_link_in_the_documentation_leads_somewhere() {
  let root = root();
  // Every Markdown file in the repository, the crates' and the tests' own
  // READMEs included: those were left out once, and a dozen of their links
  // pointed at pages the merge had removed.
  let mut pages = Vec::new();
  collect(&root, &mut pages, &|p| {
    p.extension().is_some_and(|e| e == "md")
      && !p
        .components()
        .any(|c| c.as_os_str() == "target" || c.as_os_str() == ".git")
      && p.file_name().is_some_and(|n| n != "CHANGELOG.md")
  });
  let mut problems = Vec::new();
  for page in &pages {
    broken_links(page, &read(page), &mut problems);
  }
  let changelog = root.join("CHANGELOG.md");
  broken_links(
    &changelog,
    &newest_section(&read(&changelog)),
    &mut problems,
  );
  assert!(
    problems.is_empty(),
    "links that lead nowhere:\n  {}",
    problems.join("\n  ")
  );
}

#[test]
fn every_help_link_of_the_dashboard_leads_somewhere() {
  let root = root();
  let docs_index = root.join("docs/README.md");
  let mut sources = Vec::new();
  collect(&root.join("aperio-dashboard/src"), &mut sources, &|p| {
    p.extension().is_some_and(|e| e == "ts" || e == "tsx")
  });
  let mut found = 0;
  let mut problems = Vec::new();
  for source in &sources {
    let text = read(source);
    let mut links: Vec<&str> = Vec::new();
    for marker in ["docs: '", "${DOCS_BASE}"] {
      let mut rest = text.as_str();
      while let Some(at) = rest.find(marker) {
        let after = &rest[at + marker.len()..];
        let end = after.find(['\'', '`']).unwrap_or(after.len());
        // `${DOCS_BASE}${entry.docs}` is the glossary reading its own table,
        // whose entries the first marker already finds.
        if !after.starts_with('$') {
          links.push(&after[..end]);
        }
        rest = &after[end..];
      }
    }
    for link in links {
      found += 1;
      if let Some(why) = broken(&docs_index, link) {
        problems.push(format!("{}: {why}", source.display()));
      }
    }
  }
  assert!(
    found > 10,
    "found only {found} help links; the scan has stopped finding them"
  );
  assert!(
    problems.is_empty(),
    "dashboard help links that lead nowhere:\n  {}",
    problems.join("\n  ")
  );
}

#[test]
fn an_anchor_is_spelled_the_way_github_spells_it() {
  let page =
    "# Title\n## Static files (`serve:`)\n### `limits`\n## Title\n```\n# not a heading\n```\n";
  assert_eq!(
    anchors(page),
    vec!["title", "static-files-serve", "limits", "title-1"]
  );
  assert_eq!(
    heading_text("See [Sites](x.md#sites) <b>now</b>"),
    "See Sites now"
  );
}
