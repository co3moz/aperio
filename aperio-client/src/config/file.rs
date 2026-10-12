//! Reading `aperio.yaml` and `~/.aperio.yaml`, following their `include:`s,
//! and saying what they got wrong.
//!
//! A key nothing reads is an error, naming the key that was probably meant
//! or what replaced it: a setting that is silently ignored is the most
//! expensive kind of typo, because the file says the thing is configured and
//! the behavior says it is not.

use serde_yaml::Mapping;
use std::path::{Path, PathBuf};
use tracing::info;

/// How deep an `include:` chain may go. Deep enough for the layouts anyone
/// actually writes (a root file, a per-environment file, a shared fragment),
/// shallow enough that a mistake is reported instead of being followed.
const MAX_INCLUDE_DEPTH: usize = 5;

/// Reads one config file and everything it includes, into a single mapping.
///
/// The merge rule is the one every layer uses: mappings merge key by key, and
/// anything else is replaced. Includes are merged in the order written, so a
/// later one wins over an earlier one, and the including file wins over all
/// of them; a fragment adds a service simply by naming it.
///
/// Paths resolve relative to the file that wrote them, not to the working
/// directory: an included fragment has to mean the same thing whichever
/// directory the client is started from.
pub(crate) fn read_with_includes(
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
  let raw = aperio_config::authoring::expand_vars(&raw, |name| std::env::var(name).ok())
    .map_err(|e| format!("{}: {e}", path.display()))?;
  let mut doc =
    aperio_config::layers::parse_document(&raw).map_err(|e| format!("{}: {e}", path.display()))?;

  let includes = match doc.remove("include") {
    None => Vec::new(),
    Some(serde_yaml::Value::String(one)) => vec![one],
    Some(serde_yaml::Value::Sequence(items)) => {
      let mut out = Vec::with_capacity(items.len());
      for item in items {
        match item {
          serde_yaml::Value::String(p) => out.push(p),
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
  aperio_config::layers::check_document(&doc, &path.display().to_string())?;

  let base = path.parent().unwrap_or_else(|| Path::new("."));
  let mut merged = Mapping::new();
  for entry in includes {
    let child = base.join(&entry);
    let child_doc = read_with_includes(&child, depth + 1, chain, visited)?;
    aperio_config::layers::merge(&mut merged, child_doc);
  }
  aperio_config::layers::merge(&mut merged, doc);
  chain.pop();
  Ok(merged)
}

/// A config file and its includes, merged, with every path that contributed
/// so the hot-reload watcher can watch all of them, not only the root: an
/// edit to an included fragment is a config change like any other.
pub(crate) fn parse_config_tree(path: &Path) -> Result<(Mapping, Vec<PathBuf>), String> {
  let mut chain = Vec::new();
  let mut seen = Vec::new();
  let merged = read_with_includes(path, 0, &mut chain, &mut seen)?;
  Ok((merged, seen))
}

/// Loads `./aperio.yaml` (or an explicit `--config` path). A missing default
/// file is no file; a missing explicit one is an error, since somebody named
/// it.
pub(crate) fn load_file_config_tree(
  explicit: Option<&str>,
) -> Result<(Mapping, Vec<PathBuf>), String> {
  let path = explicit.unwrap_or("aperio.yaml");
  if !Path::new(path).exists() {
    if explicit.is_some() {
      return Err(format!("cannot read {path}: not found"));
    }
    return Ok((Mapping::new(), Vec::new()));
  }
  let (doc, files) = parse_config_tree(Path::new(path))?;
  if files.len() > 1 {
    info!(
      "Loaded configuration from {} (+{} included)",
      path,
      files.len() - 1
    );
  } else {
    info!("Loaded configuration from {}", path);
  }
  Ok((doc, files))
}

/// Path of the user-level config (`~/.aperio.yaml`).
pub(crate) fn home_config_path() -> Option<PathBuf> {
  let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
  std::env::var_os(var).map(|home| PathBuf::from(home).join(".aperio.yaml"))
}

/// Loads `~/.aperio.yaml`, the lowest file layer, shared across projects
/// (typically holding `server.url` and `server.token`). Missing is fine; a
/// mistake in it is an error like one in the project's own file, since it is
/// read the same way and a setting it seems to give but does not is the same
/// surprise.
pub(crate) fn load_home_config() -> Result<Mapping, String> {
  let Some(path) = home_config_path() else {
    return Ok(Mapping::new());
  };
  if !path.exists() {
    return Ok(Mapping::new());
  }
  let (doc, _) = parse_config_tree(&path)?;
  info!("Loaded user configuration from {:?}", path);
  Ok(doc)
}
