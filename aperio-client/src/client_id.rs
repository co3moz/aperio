//! The client's instance id: given, kept on disk, or made up for this run.
//!
//! Failover's `wait` mode recognizes a client that comes back by its id, so
//! an id that changes with every restart defeats it. The id is therefore kept
//! in a file when there is somewhere sensible to keep it: next to nothing in
//! the project (a file committed with the project would give every checkout
//! the same id), under the user's home, named after the config file so two
//! projects on one machine never share one.

use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Where an id is kept by default for a client reading `config`: under
/// `~/.aperio/clients/`, named after the config file's absolute path.
pub(crate) fn default_file(home: &Path, config: &Path) -> PathBuf {
  let absolute = std::fs::canonicalize(config).unwrap_or_else(|_| config.to_path_buf());
  home.join(".aperio").join("clients").join(format!(
    "{:016x}.id",
    fnv1a(absolute.to_string_lossy().as_bytes())
  ))
}

/// Where the device key is kept by default: beside the id, under the same
/// name, so the two halves of this client's identity live and go together.
pub(crate) fn default_key_file(home: &Path, config: &Path) -> PathBuf {
  default_file(home, config).with_extension("key")
}

/// FNV-1a, 64 bits: a stable name for a path, with no dependency to add and
/// no promise beyond telling two paths apart.
fn fnv1a(bytes: &[u8]) -> u64 {
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
  for b in bytes {
    hash ^= u64::from(*b);
    hash = hash.wrapping_mul(0x0100_0000_01b3);
  }
  hash
}

/// The id kept in `path`, or a new one written there. `None` when the file
/// can be neither read nor written, which the caller answers with an id for
/// this run only.
pub(crate) fn kept_in(path: &Path) -> Option<String> {
  if let Ok(text) = std::fs::read_to_string(path) {
    let text = text.trim();
    if let Ok(id) = uuid::Uuid::parse_str(text) {
      return Some(id.to_string());
    }
    debug!(
      "{} does not hold a client id; writing a new one",
      path.display()
    );
  }
  let id = uuid::Uuid::new_v4().to_string();
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).ok()?;
  }
  std::fs::write(path, format!("{id}\n")).ok()?;
  info!("Client id {id} kept in {}", path.display());
  Some(id)
}

/// Resolves the id this process runs under: the configured one (which must
/// be a UUID), the one kept in `file` or the default file for `config`, or a
/// fresh one.
pub(crate) fn resolve(
  explicit: Option<&str>,
  file: Option<&str>,
  config: Option<&Path>,
  home: Option<&Path>,
) -> Result<String, String> {
  if let Some(raw) = explicit {
    return uuid::Uuid::parse_str(raw.trim())
      .map(|u| u.to_string())
      .map_err(|_| {
        format!(
          "client_id '{raw}' is not a UUID (--client-id / APERIO_CLIENT_ID / yaml: client_id)"
        )
      });
  }
  let path = match (file, config, home) {
    (Some(file), _, _) => Some(PathBuf::from(file)),
    (None, Some(config), Some(home)) => Some(default_file(home, config)),
    _ => None,
  };
  if let Some(path) = path {
    if let Some(id) = kept_in(&path) {
      return Ok(id);
    }
    info!(
      "Cannot keep a client id in {}; using one for this run only",
      path.display()
    );
  }
  Ok(uuid::Uuid::new_v4().to_string())
}

#[cfg(test)]
#[path = "client_id_tests.rs"]
mod tests;
