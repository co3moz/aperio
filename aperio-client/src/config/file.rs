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

/// A config file and its includes, merged, with every path that contributed
/// so the hot-reload watcher can watch all of them, not only the root. The
/// loader is the one the server reads its first-start document with; what a
/// client file may contain is checked here, against the client's schema.
pub(crate) fn parse_config_tree(path: &Path) -> Result<(Mapping, Vec<PathBuf>), String> {
  aperio_config::loader::read_tree(path, &|doc, origin| {
    aperio_config::layers::check_document(doc, origin)
  })
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
