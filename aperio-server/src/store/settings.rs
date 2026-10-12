//! The server's settings document, stored in the shared database.
//!
//! One row. It holds the document in its minimal form, only what differs
//! from the defaults, so a default that improves in a later release reaches
//! every setting the operator never chose. A database with no row is a
//! server that has never been configured, which is what decides whether
//! `APERIO_INIT_FILE` is applied.

use aperio_config::server_settings::ServerSettings;
use rusqlite::Connection;

/// The row id the document is kept under.
const ROW: &str = "server";

pub(crate) struct SettingsStore {
  conn: Connection,
}

impl SettingsStore {
  pub(crate) fn load(data_dir: &str) -> Self {
    SettingsStore {
      conn: super::open_db(data_dir),
    }
  }

  /// The stored document, `Ok(None)` when none was ever written. An `Err`
  /// is a stored document this build cannot read, which names the key: a
  /// server must not start under settings it would half apply.
  pub(crate) fn get(&self) -> Result<Option<ServerSettings>, String> {
    let raw: Option<String> = self
      .conn
      .query_row("SELECT data FROM settings WHERE id = ?1", [ROW], |row| {
        row.get(0)
      })
      .map(Some)
      .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
      })
      .map_err(|e| format!("cannot read the stored settings: {e}"))?;
    match raw {
      None => Ok(None),
      Some(raw) => serde_json::from_str::<ServerSettings>(&raw)
        .map(Some)
        .map_err(|e| format!("the stored settings do not read under this build: {e}")),
    }
  }

  /// Replaces the stored document. `false` when it could not be written,
  /// which the caller reports.
  pub(crate) fn put(&mut self, settings: &ServerSettings) -> bool {
    let data = settings.minimal().to_string();
    super::replace_all(&mut self.conn, "settings", &[(ROW.to_string(), data)])
  }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
