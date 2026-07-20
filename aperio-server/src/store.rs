//! Persistence layer: the SQLite-backed stores for traffic stats, dynamic
//! tokens, and webhook definitions (one shared `<data_dir>/aperio.db`), plus
//! the append-only jsonl audit log.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tracing::{error, warn};

pub(crate) mod admin_keys;
pub(crate) mod audit;
pub(crate) mod inbox;
pub(crate) mod orgs;
pub(crate) mod sessions;
pub(crate) mod stats;
pub(crate) mod tokens;
pub(crate) mod uptime;
pub(crate) mod users;
pub(crate) mod webhooks;

/// Opens (creating if needed) the shared SQLite store `<data_dir>/aperio.db`
/// and ensures the schema exists. Each store holds its own connection; WAL
/// mode plus a busy timeout make concurrent connections safe.
///
/// A file that turns out not to be a usable database is renamed aside as
/// `aperio.db.corrupt.<epoch>` (preserving the bad data for recovery) and a
/// fresh database is created — mirroring the old JSON stores' behavior.
pub(crate) fn open_db(data_dir: &str) -> Connection {
  let dir = PathBuf::from(data_dir);
  if let Err(e) = std::fs::create_dir_all(&dir) {
    warn!("Could not create data directory {:?}: {}", dir, e);
  }
  let path = dir.join("aperio.db");
  match try_open_db(&path) {
    Ok(conn) => conn,
    Err(e) => {
      let backup = backup_corrupt(&path);
      error!(
        "Failed to open store {:?}: {} — backed up to {:?}, starting with a fresh database",
        path, e, backup
      );
      try_open_db(&path).unwrap_or_else(|e| {
        // Nothing sane to do without a store; fall back to an in-memory
        // database so the server still runs (state lost on restart).
        error!(
          "Could not recreate {:?}: {} — using a volatile in-memory store",
          path, e
        );
        Connection::open_in_memory().expect("in-memory SQLite must open")
      })
    }
  }
}

/// Opens one connection and runs the schema/pragma setup.
fn try_open_db(path: &Path) -> rusqlite::Result<Connection> {
  let conn = Connection::open(path)?;
  conn.busy_timeout(std::time::Duration::from_secs(5))?;
  conn.pragma_update(None, "journal_mode", "WAL")?;
  conn.pragma_update(None, "synchronous", "NORMAL")?;
  conn.execute_batch(
    "CREATE TABLE IF NOT EXISTS tokens   (id  TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS webhooks (id  TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS stats    (key TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS users    (id  TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS sessions (id  TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS webhook_deliveries (id TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS organizations (id TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS inbox (id TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS admin_keys (id TEXT PRIMARY KEY, data TEXT NOT NULL);",
  )?;
  Ok(conn)
}

/// Replaces every row of `table` with the given `(id, json)` records in one
/// transaction, so a crash can never leave a half-written store.
/// Atomically replaces every row of `table`. Returns `true` on success; on a
/// write failure it logs and returns `false` so a caller performing a
/// security-relevant mutation (token/session/webhook revoke) can report the
/// failure instead of silently diverging from disk.
pub(crate) fn replace_all(conn: &mut Connection, table: &str, rows: &[(String, String)]) -> bool {
  let res = (|| -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(&format!("DELETE FROM {}", table), [])?;
    {
      let mut stmt = tx.prepare(&format!("INSERT INTO {} (id, data) VALUES (?1, ?2)", table))?;
      for (id, data) in rows {
        stmt.execute(rusqlite::params![id, data])?;
      }
    }
    tx.commit()
  })();
  match res {
    Ok(()) => true,
    Err(e) => {
      error!("Failed to persist {} to the store: {}", table, e);
      false
    }
  }
}

/// Loads every `data` column of `table`, deserialized as `T`. Rows that fail
/// to parse are skipped with a log (never fatal).
pub(crate) fn load_all<T: serde::de::DeserializeOwned>(conn: &Connection, table: &str) -> Vec<T> {
  let mut out = Vec::new();
  let mut stmt = match conn.prepare(&format!("SELECT data FROM {}", table)) {
    Ok(s) => s,
    Err(e) => {
      error!("Failed to read {} from the store: {}", table, e);
      return out;
    }
  };
  let rows = stmt.query_map([], |row| row.get::<_, String>(0));
  if let Ok(rows) = rows {
    for raw in rows.flatten() {
      match serde_json::from_str::<T>(&raw) {
        Ok(v) => out.push(v),
        Err(e) => error!("Skipping unparseable {} row: {}", table, e),
      }
    }
  }
  out
}

/// Renames a file that failed to open/parse aside as `<name>.corrupt.<epoch>`
/// so the bad data is preserved for recovery instead of being overwritten.
/// Returns the backup path on success.
pub(crate) fn backup_corrupt(path: &Path) -> Option<PathBuf> {
  let secs = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
  let backup = path.with_file_name(format!("{name}.corrupt.{secs}"));
  std::fs::rename(path, &backup).ok().map(|_| backup)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
