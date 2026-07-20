//! Scheduled physical database backups.
//!
//! Complements the logical export/import (`/api/export`) with periodic
//! point-in-time snapshots of the SQLite store. A background task runs
//! `VACUUM INTO` on `<data_dir>/aperio.db` on a fixed interval, producing a
//! single consolidated `aperio-<epoch>.db` file (no WAL/SHM sidecars) in the
//! backup directory, then prunes the oldest snapshots beyond the keep count.
//!
//! Configuration (backups are inert unless both the interval and directory are
//! set):
//!
//! - `APERIO_BACKUP_INTERVAL` — seconds between snapshots (0/unset = disabled)
//! - `APERIO_BACKUP_DIR`      — directory the snapshots are written to
//! - `APERIO_BACKUP_KEEP`     — snapshots to retain (default 7; 0 = keep all)
//!
//! Each snapshot records a `db_backup` audit event.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::state::AppState;

/// Backup schedule read from the environment.
struct BackupConfig {
  interval: Duration,
  dir: PathBuf,
  /// Snapshots to keep (0 = keep every snapshot).
  keep: usize,
}

impl BackupConfig {
  /// Reads the schedule. `None` (disabled) unless both a positive interval and
  /// a non-empty directory are configured.
  fn from_env() -> Option<Self> {
    let interval_secs = std::env::var("APERIO_BACKUP_INTERVAL")
      .ok()
      .and_then(|v| v.trim().parse::<u64>().ok())
      .filter(|v| *v > 0)?;
    let dir = std::env::var("APERIO_BACKUP_DIR")
      .ok()
      .map(|d| PathBuf::from(d.trim()))
      .filter(|p| !p.as_os_str().is_empty())?;
    let keep = std::env::var("APERIO_BACKUP_KEEP")
      .ok()
      .and_then(|v| v.trim().parse::<usize>().ok())
      .unwrap_or(7);
    Some(BackupConfig {
      interval: Duration::from_secs(interval_secs),
      dir,
      keep,
    })
  }
}

/// Prefix and suffix bounding a snapshot filename (`aperio-<epoch>.db`).
const SNAP_PREFIX: &str = "aperio-";
const SNAP_SUFFIX: &str = ".db";

/// Extracts the epoch timestamp encoded in a snapshot filename, if it matches.
fn snapshot_ts(name: &str) -> Option<u64> {
  name
    .strip_prefix(SNAP_PREFIX)?
    .strip_suffix(SNAP_SUFFIX)?
    .parse::<u64>()
    .ok()
}

/// Writes one consolidated snapshot of `db_path` into `dir` and returns the
/// snapshot path and its size in bytes.
fn write_snapshot(db_path: &Path, dir: &Path) -> Result<(PathBuf, u64), String> {
  std::fs::create_dir_all(dir).map_err(|e| format!("cannot create backup dir: {e}"))?;
  let ts = crate::store::tokens::now_secs();
  let target = dir.join(format!("{SNAP_PREFIX}{ts}{SNAP_SUFFIX}"));
  // `VACUUM INTO` produces a single compacted database with no WAL/SHM
  // sidecars — a clean, self-contained snapshot. A read lock is enough, so it
  // is safe alongside the live connections in WAL mode.
  let conn = rusqlite::Connection::open(db_path).map_err(|e| format!("cannot open store: {e}"))?;
  conn
    .busy_timeout(Duration::from_secs(30))
    .map_err(|e| e.to_string())?;
  conn
    .execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
    .map_err(|e| format!("VACUUM INTO failed: {e}"))?;
  let size = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
  Ok((target, size))
}

/// Deletes the oldest snapshots so at most `keep` remain (0 = keep all).
/// Returns how many were removed.
fn prune_snapshots(dir: &Path, keep: usize) -> usize {
  if keep == 0 {
    return 0;
  }
  let mut snaps: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
    .into_iter()
    .flatten()
    .flatten()
    .filter_map(|e| {
      let name = e.file_name().to_string_lossy().into_owned();
      snapshot_ts(&name).map(|ts| (ts, e.path()))
    })
    .collect();
  if snaps.len() <= keep {
    return 0;
  }
  // Newest first, then drop everything past the keep count.
  snaps.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
  let mut removed = 0;
  for (_, path) in snaps.into_iter().skip(keep) {
    match std::fs::remove_file(&path) {
      Ok(()) => removed += 1,
      Err(e) => warn!("Backup: failed to prune old snapshot {:?}: {}", path, e),
    }
  }
  removed
}

/// Spawns the scheduled-backup task: one snapshot at startup, then on the
/// configured interval. Inert when the schedule is not configured.
pub(crate) fn spawn(state: Arc<AppState>) {
  let Some(cfg) = BackupConfig::from_env() else {
    return;
  };
  let db_path = state
    .settings_path
    .parent()
    .map(|p| p.to_path_buf())
    .unwrap_or_else(|| PathBuf::from("."))
    .join("aperio.db");
  info!(
    "Scheduled DB backups enabled: every {}s into {:?} (keep {})",
    cfg.interval.as_secs(),
    cfg.dir,
    cfg.keep
  );
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(cfg.interval);
    loop {
      interval.tick().await;
      match write_snapshot(&db_path, &cfg.dir) {
        Ok((path, size)) => {
          let pruned = prune_snapshots(&cfg.dir, cfg.keep);
          info!(
            "DB backup written: {:?} ({} bytes), pruned {} old snapshot(s)",
            path, size, pruned
          );
          state
            .audit(
              "db_backup",
              "system",
              "-",
              &format!("path={} size={} pruned={}", path.display(), size, pruned),
            )
            .await;
          state
            .emit_event(
              "db_backup",
              serde_json::json!({
                "path": path.display().to_string(),
                "size_bytes": size,
                "pruned": pruned,
              }),
            )
            .await;
        }
        Err(e) => error!("DB backup failed: {}", e),
      }
    }
  });
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
