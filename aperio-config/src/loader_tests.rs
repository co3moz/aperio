//! What the shared loader pins down for both binaries: `${VAR}`s are
//! expanded per file and a missing one names its file, an `include:` merges
//! with the including file winning, a cycle is refused while a diamond is
//! not, and the caller's check sees each file under its own name. The
//! client's own use of it is tested through `parse_config_tree`.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh directory holding `files`.
fn dir_with(files: &[(&str, &str)]) -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "aperio-loader-{}-{}",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  ));
  std::fs::create_dir_all(&dir).unwrap();
  for (name, text) in files {
    std::fs::write(dir.join(name), text).unwrap();
  }
  dir
}

fn no_check() -> impl Fn(&Mapping, &str) -> Result<(), String> {
  |_, _| Ok(())
}

fn get<'a>(doc: &'a Mapping, path: &str) -> Option<&'a Value> {
  let mut node: Option<&Value> = None;
  let mut map = doc;
  for part in path.split('.') {
    node = map.get(part);
    if let Some(Value::Mapping(m)) = node {
      map = m;
    }
  }
  node
}

#[test]
fn a_document_is_read_whole_with_its_includes_and_the_includer_wins() {
  let dir = dir_with(&[
    (
      "init.yaml",
      "include: [secrets.yaml]\nsettings:\n  cache: {enabled: true}\n  oidc: {issuer: https://idp.example.com}\n",
    ),
    (
      "secrets.yaml",
      "settings:\n  oidc: {issuer: https://other.example.com, client_secret: '${APERIO_LOADER_TEST_UNSET:-from-default}'}\n",
    ),
  ]);
  let check = no_check();
  let (doc, files) = read_tree(&dir.join("init.yaml"), &check).unwrap();
  assert_eq!(files.len(), 2, "both files are reported for a watcher");
  // A key only the include sets is kept, with its variable expanded.
  assert_eq!(
    get(&doc, "settings.oidc.client_secret").and_then(Value::as_str),
    Some("from-default")
  );
  // A key both set is the including file's.
  assert_eq!(
    get(&doc, "settings.oidc.issuer").and_then(Value::as_str),
    Some("https://idp.example.com")
  );
  assert_eq!(
    get(&doc, "settings.cache.enabled"),
    Some(&Value::Bool(true))
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_variable_names_the_file_that_wrote_it() {
  let dir = dir_with(&[
    ("init.yaml", "include: [part.yaml]\n"),
    (
      "part.yaml",
      "settings: {oidc: {client_secret: '${APERIO_LOADER_TEST_UNSET}'}}\n",
    ),
  ]);
  let check = no_check();
  let err = read_tree(&dir.join("init.yaml"), &check).unwrap_err();
  assert!(err.contains("part.yaml"), "{err}");
  assert!(err.contains("APERIO_LOADER_TEST_UNSET"), "{err}");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cycle_is_refused_and_a_diamond_is_not() {
  let cycle = dir_with(&[
    ("a.yaml", "include: [b.yaml]\n"),
    ("b.yaml", "include: [a.yaml]\n"),
  ]);
  let check = no_check();
  let err = read_tree(&cycle.join("a.yaml"), &check).unwrap_err();
  assert!(err.contains("cycle"), "{err}");
  let diamond = dir_with(&[
    ("root.yaml", "include: [left.yaml, right.yaml]\n"),
    ("left.yaml", "include: [shared.yaml]\nleft: 1\n"),
    ("right.yaml", "include: [shared.yaml]\nright: 1\n"),
    ("shared.yaml", "shared: 1\n"),
  ]);
  let (doc, files) = read_tree(&diamond.join("root.yaml"), &check).unwrap();
  assert_eq!(files.len(), 4, "a file reached twice is reported once");
  for key in ["left", "right", "shared"] {
    assert!(doc.contains_key(key), "{key}");
  }
  let _ = std::fs::remove_dir_all(&cycle);
  let _ = std::fs::remove_dir_all(&diamond);
}

#[test]
fn the_check_sees_every_file_under_its_own_name() {
  let dir = dir_with(&[
    ("root.yaml", "include: [part.yaml]\nok: 1\n"),
    ("part.yaml", "bad: 1\n"),
  ]);
  let refuse_bad = |doc: &Mapping, origin: &str| {
    if doc.contains_key("bad") {
      Err(format!("{origin}: `bad` is not a setting"))
    } else {
      Ok(())
    }
  };
  let err = read_tree(&dir.join("root.yaml"), &refuse_bad).unwrap_err();
  assert!(err.contains("part.yaml") && err.contains("`bad`"), "{err}");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_include_chain_deeper_than_the_limit_is_refused() {
  let mut files: Vec<(String, String)> = Vec::new();
  for i in 0..=MAX_INCLUDE_DEPTH + 1 {
    files.push((
      format!("f{i}.yaml"),
      format!("include: [f{}.yaml]\n", i + 1),
    ));
  }
  files.push((
    format!("f{}.yaml", MAX_INCLUDE_DEPTH + 2),
    "end: 1\n".to_string(),
  ));
  let refs: Vec<(&str, &str)> = files
    .iter()
    .map(|(n, t)| (n.as_str(), t.as_str()))
    .collect();
  let dir = dir_with(&refs);
  let check = no_check();
  let err = read_tree(&dir.join("f0.yaml"), &check).unwrap_err();
  assert!(err.contains("deeper than"), "{err}");
  let _ = std::fs::remove_dir_all(&dir);
}
