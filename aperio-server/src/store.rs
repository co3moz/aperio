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
pub(crate) mod scaling;
pub(crate) mod sessions;
pub(crate) mod stats;
pub(crate) mod tokens;
pub(crate) mod uptime;
pub(crate) mod users;
pub(crate) mod webhooks;

/// Why a change to a stored record did not happen.
///
/// Two reasons, kept apart, because the caller answers them differently and
/// used to be unable to tell them apart at all: a mutation returned one
/// `false` for both "no such record" and "the disk is full", so a 404 and a
/// 500 were the same value. That is the worse way round than it sounds. The
/// 404 reads as "already gone", which is exactly what somebody revoking a
/// credential wants to hear, so the one answer that must never be guessed is
/// the one the conflation produced.
///
/// It lives here rather than in one store because every store has the same
/// two answers, and the first version of this enum was written for tokens
/// alone, which is why the stores beside it kept returning a bare `bool` for
/// another release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotWritten {
  /// No record matched. Nothing was attempted.
  NoSuchRecord,
  /// The change was made and then undone, because it could not be saved.
  /// Memory matches disk, and the caller must report a failure.
  NotPersisted,
}

/// Opens (creating if needed) the shared SQLite store `<data_dir>/aperio.db`
/// and ensures the schema exists. Each store holds its own connection; WAL
/// mode plus a busy timeout make concurrent connections safe.
///
/// A file that turns out not to be a usable database is renamed aside as
/// `aperio.db.corrupt.<epoch>` (preserving the bad data for recovery) and a
/// fresh database is created, mirroring the old JSON stores' behavior.
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
        "Failed to open store {:?}: {}, backed up to {:?}, starting with a fresh database",
        path, e, backup
      );
      try_open_db(&path).unwrap_or_else(|e| {
        // Nothing sane to do without a store; fall back to an in-memory
        // database so the server still runs (state lost on restart).
        error!(
          "Could not recreate {:?}: {}, using a volatile in-memory store",
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
     CREATE TABLE IF NOT EXISTS admin_keys (id TEXT PRIMARY KEY, data TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS scaling (id TEXT PRIMARY KEY, data TEXT NOT NULL);",
  )?;
  Ok(conn)
}

/// Replaces every row of `table` with the given `(id, json)` records in one
/// transaction, so a crash can never leave a half-written store.
/// Writes `contents` to `path` atomically: write to a sibling `<path>.tmp`
/// first, then rename it over the target. A crash or power loss mid-write
/// leaves either the old file or the fully-written new one intact, never a
/// truncated file (which for the tamper-evident audit log would be corruption).
pub(crate) fn atomic_write(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
  let mut tmp = path.as_os_str().to_owned();
  tmp.push(".tmp");
  let tmp = std::path::PathBuf::from(tmp);
  std::fs::write(&tmp, contents)?;
  std::fs::rename(&tmp, path)
}

/// Atomically replaces every row of `table`. Returns `true` on success; on a
/// write failure it logs and returns `false`.
///
/// **What a caller is expected to do with that `false`**, since the stores
/// answer it in two different ways on purpose:
///
/// - **A change somebody asked for** (create a token, delete a user, move an
///   organization's fence, spend a recovery code) is **rolled back**, so
///   memory matches disk, and the failure is **reported**: the endpoint
///   answers 500 rather than a success for a change that stops existing at the
///   next restart. Each store has a `commit` helper for exactly this.
/// - **Bookkeeping the server does to itself** (a retention sweep, a disk-cap
///   truncation, an inbox insert, a dump import, a re-announced autoscaling
///   record) keeps its in-memory result and relies on the log line above.
///   Nobody is waiting for an answer, the next sweep will do it again, and
///   rolling back would mean holding data the operator asked to be rid of, or
///   refusing traffic over a counter.
///
/// The line between them is who is owed an answer, not how important the row
/// looks. `ScalingStore::disown` carries the one case where the two arguments
/// point in different directions, and says so where it is written.
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
