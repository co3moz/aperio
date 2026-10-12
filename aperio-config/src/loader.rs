//! Reading a yaml file somebody wrote by hand, with its `${VAR}`s and its
//! `include:`s: the one loader both binaries read such a file with, so a
//! client's `aperio.yaml` and a server's first-start document template the
//! same way and split into fragments the same way.
//!
//! What a document may contain is the caller's question, asked through the
//! `check` it passes: the client holds every file to its schema here, while
//! the server's document is typed in full when it is deserialized.

use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

/// How deep an `include:` chain may go. Deep enough for the layouts anyone
/// actually writes (a root file, a per-environment file, a shared fragment),
/// shallow enough that a mistake is reported instead of being followed.
pub const MAX_INCLUDE_DEPTH: usize = 5;

/// Checks one file's document before it is merged, naming the file in what
/// it reports.
pub type Check<'a> = &'a dyn Fn(&Mapping, &str) -> Result<(), String>;

/// A file and everything it includes, merged into one mapping, with every
/// path that contributed: a hot-reload watcher has to watch all of them, not
/// only the root, since an edit to an included fragment is a change like any
/// other.
pub fn read_tree(path: &Path, check: Check<'_>) -> Result<(Mapping, Vec<PathBuf>), String> {
  let mut chain = Vec::new();
  let mut seen = Vec::new();
  let merged = read_with_includes(path, 0, &mut chain, &mut seen, check)?;
  Ok((merged, seen))
}

/// Reads one file and everything it includes, into a single mapping.
///
/// The merge rule is the one every layer uses: mappings merge key by key, and
/// anything else is replaced. Includes are merged in the order written, so a
/// later one wins over an earlier one, and the including file wins over all
/// of them; a fragment adds a service simply by naming it.
///
/// Paths resolve relative to the file that wrote them, not to the working
/// directory: an included fragment has to mean the same thing whichever
/// directory the binary is started from.
fn read_with_includes(
  path: &Path,
  depth: usize,
  // The files on the path from the root to here. A cycle is a file that
  // includes itself through this chain, so entries are popped on the way back
  // out; keeping them would call a *diamond* a cycle, and a root that includes
  // two per-environment fragments which both include a shared one is the most
  // ordinary layout there is.
  chain: &mut Vec<PathBuf>,
  // Every file that contributed, for the hot-reload watcher. This one only
  // grows: the watcher needs the whole set, and a shared fragment has to be
  // watched even though it is reached twice.
  visited: &mut Vec<PathBuf>,
  check: Check<'_>,
) -> Result<Mapping, String> {
  let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
  if chain.contains(&canonical) {
    return Err(format!(
      "{} is included in a cycle; each file may appear once in a chain",
      path.display()
    ));
  }
  if depth > MAX_INCLUDE_DEPTH {
    return Err(format!(
      "include chain is deeper than {MAX_INCLUDE_DEPTH} files at {}",
      path.display()
    ));
  }
  chain.push(canonical.clone());
  if !visited.contains(&canonical) {
    visited.push(canonical);
  }

  let raw =
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
  // Template variables are expanded before the yaml is parsed, so one file can
  // serve several environments instead of being copied per environment, which
  // is how two files drift. Expanded per file, not after merging, so an
  // included fragment reports its own name when a variable is missing.
  let raw = crate::authoring::expand_vars(&raw, |name| std::env::var(name).ok())
    .map_err(|e| format!("{}: {e}", path.display()))?;
  let mut doc =
    crate::layers::parse_document(&raw).map_err(|e| format!("{}: {e}", path.display()))?;

  let includes = match doc.remove("include") {
    None => Vec::new(),
    Some(Value::String(one)) => vec![one],
    Some(Value::Sequence(items)) => {
      let mut out = Vec::with_capacity(items.len());
      for item in items {
        match item {
          Value::String(p) => out.push(p),
          _ => {
            return Err(format!(
              "{}: every `include:` entry must be a file path",
              path.display()
            ));
          }
        }
      }
      out
    }
    Some(_) => {
      return Err(format!(
        "{}: `include:` must be a file path or a list of them",
        path.display()
      ));
    }
  };
  check(&doc, &path.display().to_string())?;

  let base = path.parent().unwrap_or_else(|| Path::new("."));
  let mut merged = Mapping::new();
  for entry in includes {
    let child = base.join(&entry);
    let child_doc = read_with_includes(&child, depth + 1, chain, visited, check)?;
    crate::layers::merge(&mut merged, child_doc);
  }
  crate::layers::merge(&mut merged, doc);
  chain.pop();
  Ok(merged)
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
