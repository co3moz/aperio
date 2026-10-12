//! What the server reads before its database exists, and the first-start
//! settings.
//!
//! The environment holds only what cannot wait for the database: the master
//! token and the data directory here, the listen address and the log level
//! where they are used. Everything else is the settings document, read from
//! the database, or, on a server that has never been configured, from the
//! file `APERIO_INIT_FILE` names.

use aperio_config::server_settings::ServerSettings;

use crate::api::export::ImportDump;

/// The boot environment the startup needs.
pub(crate) struct Boot {
  pub(crate) token: String,
  pub(crate) data_dir: String,
}

impl Boot {
  pub(crate) fn from_env() -> Result<Boot, String> {
    let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
      return Err(
        "APERIO_SERVER_TOKEN must be set: it authenticates tunnel clients and signs in as the built-in admin"
          .to_string(),
      );
    }
    Ok(Boot {
      token,
      data_dir: data_dir(),
    })
  }
}

/// `APERIO_DATA_DIR`, or `./data`.
pub(crate) fn data_dir() -> String {
  std::env::var("APERIO_DATA_DIR")
    .ok()
    .filter(|v| !v.trim().is_empty())
    .unwrap_or_else(|| "./data".to_string())
}

/// Reads an apply document (yaml or json).
pub(crate) fn read_document(path: &str) -> Result<ImportDump, String> {
  let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
  serde_yaml::from_str::<Option<ImportDump>>(&raw)
    .map(Option::unwrap_or_default)
    .map_err(|e| format!("{path}: {e}"))
}

/// The settings this start runs under, and the rest of the init document
/// when this is the first start and `APERIO_INIT_FILE` supplied one.
///
/// A database that has settings uses them, and the init file is not read: it
/// configures a server once, and after that the dashboard and the API own the
/// settings. A database with none takes the init file's, stores them, and
/// hands back the other sections for the caller to apply once the stores
/// exist. With neither, the defaults.
pub(crate) fn load_settings(boot: &Boot) -> Result<(ServerSettings, Option<ImportDump>), String> {
  let mut store = crate::store::settings::SettingsStore::load(&boot.data_dir);
  if let Some(settings) = store.get()? {
    return Ok((settings, None));
  }
  let Some(path) = std::env::var("APERIO_INIT_FILE")
    .ok()
    .filter(|v| !v.trim().is_empty())
  else {
    return Ok((ServerSettings::default(), None));
  };
  let mut document = read_document(&path)?;
  let settings = document.settings.take().unwrap_or_default();
  crate::server::state_build::config::build_config(&settings, &boot.token, &boot.data_dir)
    .map_err(|e| format!("{path}: {e}"))?;
  if !store.put(&settings) {
    return Err(format!(
      "{path}: the settings could not be saved to the database"
    ));
  }
  eprintln!("aperio-server: first start, configured from {path}");
  Ok((settings, Some(document)))
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
