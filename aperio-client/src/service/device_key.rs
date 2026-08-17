use tracing::{info, warn};

/// Resolves this client's trust-on-first-use device key for token pinning,
/// announced in the Ping. Opt-in: an explicit `key` is used as given;
/// otherwise `file` names a path whose contents are used, generating and
/// persisting a fresh random key there on first run. `None` (nothing
/// announced) when neither is set. Both come from the layered configuration
/// (yaml `device_key` / `device_key_file`, or their `APERIO_*` spellings).
fn resolve_device_key(key: Option<String>, file: Option<String>) -> Option<String> {
  if let Some(v) = key {
    let v = v.trim().to_string();
    if !v.is_empty() {
      return Some(v);
    }
  }
  let path = file
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty())?;
  match std::fs::read_to_string(&path) {
    Ok(k) if !k.trim().is_empty() => Some(k.trim().to_string()),
    _ => {
      let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
      );
      // The device key is a pinning secret; persist it owner-only (0600) on
      // Unix so a local user cannot read it and replay a leaked token.
      let write_res = {
        use std::io::Write;
        #[cfg(unix)]
        let opened = {
          use std::os::unix::fs::OpenOptionsExt;
          std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        };
        #[cfg(not(unix))]
        let opened = std::fs::OpenOptions::new()
          .write(true)
          .create(true)
          .truncate(true)
          .open(&path);
        opened.and_then(|mut f| f.write_all(key.as_bytes()))
      };
      // `mode` on the open only applies to a file this call *creates*. A path
      // that already existed, an empty one left by a failed write, or one an
      // operator touched, keeps whatever mode it had, which is 0644 under the
      // usual umask: the secret would be written world-readable into a file
      // that looks like it was written owner-only. Tightening after the fact
      // covers both paths with one rule.
      #[cfg(unix)]
      if write_res.is_ok() {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
          warn!("Could not restrict the device key file {path} to 0600: {e}");
        }
      }
      match write_res {
        Ok(()) => info!("Generated a new device key at {path} for token pinning"),
        Err(e) => warn!(
          "Could not persist the device key to {path}: {e}. Running with an in-memory key that changes on every restart, if the server enforces token pinning it will reject this client after a restart. On a read-only or ephemeral filesystem, set a stable key via the APERIO_DEVICE_KEY environment variable instead of a file."
        ),
      }
      Some(key)
    }
  }
}

/// Where the device key comes from, resolved from the full configuration
/// layering (yaml `device_key`/`device_key_file`, or `APERIO_DEVICE_KEY` /
/// `APERIO_DEVICE_KEY_FILE`) and installed once at startup.
static DEVICE_KEY_SOURCES: std::sync::OnceLock<(Option<String>, Option<String>)> =
  std::sync::OnceLock::new();

/// Installs the device-key sources. Called once from `main` before any
/// service connects; a later call is ignored, so a config reload cannot swap
/// the identity of a running process out from under the server's pin.
pub(crate) fn set_device_key_sources(key: Option<String>, file: Option<String>) {
  let _ = DEVICE_KEY_SOURCES.set((key, file));
}

/// The process-wide device key, resolved once.
pub(crate) fn device_key() -> Option<String> {
  static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
  KEY
    .get_or_init(|| {
      let (key, file) = DEVICE_KEY_SOURCES.get().cloned().unwrap_or_default();
      resolve_device_key(key, file)
    })
    .clone()
}

#[cfg(test)]
#[path = "device_key_tests.rs"]
mod tests;
