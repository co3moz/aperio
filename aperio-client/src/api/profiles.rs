//! Profiles: the admin keys `aperio-client login` keeps, one per server, so
//! an `api` or `admin` call needs no key on its command line or in a project
//! file. Kept in `~/.aperio/profiles.yaml`, which only the user may read.
//!
//! A profile's key goes to its own server and nowhere else. The current
//! profile stands in for an admin key only when none is configured and the
//! server the call is going to is the profile's, so a project whose
//! `aperio.yaml` names another server is never sent this one's credential.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One server signed in to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
  /// The server's URL, as `login` was given it.
  pub(crate) server: String,
  /// The admin key, sent as `Authorization: Bearer`.
  pub(crate) api_key: String,
  /// The key's id on the server, when `login` minted it: what `logout`
  /// revokes. Absent for a key handed to `login --key`, which belongs to
  /// whoever minted it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) key_id: Option<String>,
}

/// The file: every profile, and which one is in use.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profiles {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) current: Option<String>,
  #[serde(default)]
  pub(crate) profiles: BTreeMap<String, Profile>,
}

/// `~/.aperio/profiles.yaml`, beside the `~/.aperio.yaml` config layer.
pub(crate) fn profiles_path() -> Option<PathBuf> {
  let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
  std::env::var_os(var).map(|home| PathBuf::from(home).join(".aperio").join("profiles.yaml"))
}

impl Profiles {
  /// Reads the file; a missing one is no profiles.
  pub(crate) fn read(path: &Path) -> Result<Profiles, String> {
    match std::fs::read_to_string(path) {
      Ok(text) if text.trim().is_empty() => Ok(Profiles::default()),
      Ok(text) => serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Profiles::default()),
      Err(e) => Err(format!("{}: {e}", path.display())),
    }
  }

  /// Writes the file, readable by its owner alone, through a sibling
  /// temporary file so a crash leaves the old one or the new one.
  pub(crate) fn write(&self, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_yaml::to_string(self).map_err(|e| e.to_string())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    write_private(&tmp, text.as_bytes()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
  }

  /// The profile in use, by name.
  pub(crate) fn current(&self) -> Option<(&str, &Profile)> {
    let name = self.current.as_deref()?;
    self.profiles.get(name).map(|p| (name, p))
  }

  /// The current profile's server and key, for a call going to `server`
  /// (`None` = no server configured, so the profile's is used). A profile
  /// for another server answers nothing.
  pub(crate) fn credential_for(&self, server: Option<&str>) -> Option<(String, String)> {
    let (_, profile) = self.current()?;
    match server {
      Some(server) if !same_server(server, &profile.server) => None,
      _ => Some((profile.server.clone(), profile.api_key.clone())),
    }
  }

  /// Stores `profile` under `name` and makes it the one in use.
  pub(crate) fn insert(&mut self, name: &str, profile: Profile) {
    self.profiles.insert(name.to_string(), profile);
    self.current = Some(name.to_string());
  }

  /// Removes a profile; the one in use falls to the first left, by name.
  pub(crate) fn remove(&mut self, name: &str) -> Option<Profile> {
    let removed = self.profiles.remove(name)?;
    if self.current.as_deref() == Some(name) {
      self.current = self.profiles.keys().next().cloned();
    }
    Some(removed)
  }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  use std::io::Write;
  use std::os::unix::fs::OpenOptionsExt;
  let mut file = std::fs::OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .mode(0o600)
    .open(path)?;
  file.write_all(bytes)
}

/// Windows keeps a file under the user's profile directory to that user by
/// its inherited ACL, which is the whole of what the Unix mode says.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  std::fs::write(path, bytes)
}

/// Whether two spellings name one server: the scheme (`ws`/`http`,
/// `wss`/`https`), host and port, whatever the path or a trailing slash.
pub(crate) fn same_server(a: &str, b: &str) -> bool {
  match (origin(a), origin(b)) {
    (Some(a), Some(b)) => a == b,
    _ => false,
  }
}

fn origin(server: &str) -> Option<String> {
  let url = crate::config::build_http_url(server, "/").ok()?;
  let parsed = url::Url::parse(&url).ok()?;
  Some(format!(
    "{}://{}:{}",
    parsed.scheme(),
    parsed.host_str()?.to_ascii_lowercase(),
    parsed.port_or_known_default()?
  ))
}

/// The name a profile is kept under unless `--name` says: the server's host.
pub(crate) fn default_name(server: &str) -> String {
  crate::config::build_http_url(server, "/")
    .ok()
    .and_then(|url| url::Url::parse(&url).ok())
    .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    .unwrap_or_else(|| server.to_string())
}

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;
