//! That every file the release's package manifests name is actually there.
//!
//! `tools/packaging/nfpm-*.yaml` is read by nfpm during a release and by
//! nothing else, so a path that stops resolving is discovered while the
//! release is running. One did: `chore: the root holds the product, tools/
//! holds what supports it` moved `packaging/` under `tools/` and left the
//! `scripts:` paths inside these files pointing at the old location, so `.deb`
//! and `.rpm` builds failed with "Open packaging/scripts/postinstall-server.sh:
//! no such file or directory" on the next release and every one after it.
//!
//! Here rather than in CI-only shell because a `cargo test` run is the thing
//! that always happens: the edit that breaks this is a file move, which
//! belongs to no language and would otherwise be checked by nothing.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .expect("the crate sits in the workspace")
    .to_path_buf()
}

/// Every `key: path` line whose value looks like a repository path.
///
/// Deliberately a line scan rather than a yaml parse: what is being checked is
/// that a string in this file names a file that exists, and the shapes that
/// carry one (`postinstall:`, `src:`) are all `key: value`. A parser would buy
/// nothing and would have to be taught nfpm's schema.
fn referenced_paths(text: &str) -> Vec<String> {
  text
    .lines()
    .filter_map(|line| {
      let line = line.trim().trim_start_matches("- ");
      let (_, value) = line.split_once(": ")?;
      let value = value.trim().trim_matches('"');
      // `dist/` is staged by the release job itself, and `${...}` is resolved
      // by nfpm at run time; neither is in the tree now.
      let staged = value.starts_with("dist/") || value.contains("${");
      // A homepage is not a file.
      let is_url = value.contains("://");
      let looks_like_a_path = value.contains('/') && !value.starts_with('/') && !staged && !is_url;
      looks_like_a_path.then(|| value.to_string())
    })
    .collect()
}

#[test]
fn every_path_the_package_manifests_name_exists() {
  let root = repo_root();
  let dir = root.join("tools/packaging");
  let manifests: Vec<PathBuf> = std::fs::read_dir(&dir)
    .expect("tools/packaging exists")
    .flatten()
    .map(|e| e.path())
    .filter(|p| {
      p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("nfpm-") && n.ends_with(".yaml"))
    })
    .collect();
  assert!(
    manifests.len() >= 2,
    "found {} nfpm manifests, so this test is looking in the wrong place rather \
     than finding nothing wrong",
    manifests.len()
  );

  let mut missing = Vec::new();
  for manifest in &manifests {
    let text = std::fs::read_to_string(manifest).expect("a readable manifest");
    for path in referenced_paths(&text) {
      if !root.join(&path).exists() {
        missing.push(format!(
          "{}: {path}",
          manifest.file_name().unwrap_or_default().to_string_lossy()
        ));
      }
    }
  }

  assert!(
    missing.is_empty(),
    "the release's package manifests name files that are not in the tree: {missing:?}.\n\n\
     nfpm reads these during a release and nowhere else, so a path left behind by a \
     move fails while the release is running. Fix the path, or stage the file the way \
     `dist/` entries are staged."
  );
}
