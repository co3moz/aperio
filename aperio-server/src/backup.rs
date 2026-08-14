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
//! - `APERIO_BACKUP_INTERVAL`, seconds between snapshots (0/unset = disabled)
//! - `APERIO_BACKUP_DIR`, directory the snapshots are written to
//! - `APERIO_BACKUP_KEEP`, snapshots to retain (default 7; 0 = keep all)
//! - `APERIO_BACKUP_KEY` / `APERIO_BACKUP_KEY_FILE`, a 32-byte key that turns
//!   the snapshots into `aperio-<epoch>.db.enc` (see `backup_crypto`). Unset
//!   keeps writing them in the clear, exactly as before.
//!
//! Each snapshot records a `db_backup` audit event.
//!
//! **`VACUUM INTO` needs a path**, so an encrypted snapshot is written in the
//! clear first and encrypted after. That intermediate file is created
//! owner-only, lives in the backup directory for the length of one encryption,
//! and is removed whether or not the encryption succeeded. It is worth knowing
//! about rather than being surprised by: for the seconds it exists, the
//! directory holds a plaintext copy.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::state::AppState;

/// Backup schedule read from the environment.
#[derive(Clone)]
struct BackupConfig {
  interval: Duration,
  dir: PathBuf,
  /// Snapshots to keep (0 = keep every snapshot).
  keep: usize,
  /// `None` writes snapshots in the clear.
  key: Option<Arc<crate::backup_crypto::BackupKey>>,
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
    // A key that cannot be used is a refusal, not a fallback to plaintext:
    // an operator who configured encryption and got clear snapshots has the
    // opposite of what they asked for, and no way to notice from the files.
    let key = match crate::backup_crypto::load_key(
      env_nonempty("APERIO_BACKUP_KEY").as_deref(),
      env_nonempty("APERIO_BACKUP_KEY_FILE").as_deref(),
      &dir,
    ) {
      Ok(key) => key.map(Arc::new),
      Err(e) => {
        error!("Scheduled DB backups are disabled: {e}");
        return None;
      }
    };
    Some(BackupConfig {
      interval: Duration::from_secs(interval_secs),
      dir,
      keep,
      key,
    })
  }
}

/// A set, non-blank environment variable.
fn env_nonempty(name: &str) -> Option<String> {
  std::env::var(name)
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
}

/// Prefix and suffix bounding a snapshot filename (`aperio-<epoch>.db`).
const SNAP_PREFIX: &str = "aperio-";
const SNAP_SUFFIX: &str = ".db";

/// Extracts the epoch timestamp encoded in a snapshot filename, if it matches.
fn snapshot_ts(name: &str) -> Option<u64> {
  let rest = name.strip_prefix(SNAP_PREFIX)?;
  // Both spellings, so a directory that was written to before encryption was
  // turned on is still pruned rather than kept forever beside the new files.
  let stamp = rest
    .strip_suffix(crate::backup_crypto::ENCRYPTED_SUFFIX)
    .or_else(|| rest.strip_suffix(SNAP_SUFFIX))?;
  stamp.parse::<u64>().ok()
}

/// Writes one consolidated snapshot of `db_path` into `dir` and returns the
/// snapshot path and its size in bytes.
fn write_snapshot(
  db_path: &Path,
  dir: &Path,
  key: Option<&crate::backup_crypto::BackupKey>,
) -> Result<(PathBuf, u64), String> {
  std::fs::create_dir_all(dir).map_err(|e| format!("cannot create backup dir: {e}"))?;
  let ts = crate::store::tokens::now_secs();
  let target = dir.join(format!("{SNAP_PREFIX}{ts}{SNAP_SUFFIX}"));
  // `VACUUM INTO` produces a single compacted database with no WAL/SHM
  // sidecars, a clean, self-contained snapshot. A read lock is enough, so it
  // is safe alongside the live connections in WAL mode.
  let conn = rusqlite::Connection::open(db_path).map_err(|e| format!("cannot open store: {e}"))?;
  conn
    .busy_timeout(Duration::from_secs(30))
    .map_err(|e| e.to_string())?;
  conn
    .execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
    .map_err(|e| format!("VACUUM INTO failed: {e}"))?;
  let Some(key) = key else {
    let size = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    return Ok((target, size));
  };

  // The plaintext snapshot is the intermediate file the module doc warns
  // about. It goes away whichever way the encryption ends.
  let encrypted = dir.join(format!(
    "{SNAP_PREFIX}{ts}{}",
    crate::backup_crypto::ENCRYPTED_SUFFIX
  ));
  let result = crate::backup_crypto::encrypt_file(key, &target, &encrypted);
  if let Err(e) = std::fs::remove_file(&target) {
    warn!("Backup: could not remove the intermediate snapshot {target:?}: {e}");
  }
  match result {
    Ok(size) => Ok((encrypted, size)),
    Err(e) => {
      let _ = std::fs::remove_file(&encrypted);
      Err(e)
    }
  }
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
    "Scheduled DB backups enabled: every {}s into {:?} (keep {}, {})",
    cfg.interval.as_secs(),
    cfg.dir,
    cfg.keep,
    if cfg.key.is_some() {
      "encrypted"
    } else {
      "not encrypted"
    }
  );
  crate::supervise::spawn_supervised("backup", move || {
    // Cloned per attempt rather than moved: the closure runs again after a
    // panic, so it can only borrow what it can hand out more than once.
    let (cfg, db_path, state) = (cfg.clone(), db_path.clone(), state.clone());
    async move {
      let mut interval = tokio::time::interval(cfg.interval);
      loop {
        interval.tick().await;
        match write_snapshot(&db_path, &cfg.dir, cfg.key.as_deref()) {
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
    }
  });
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
