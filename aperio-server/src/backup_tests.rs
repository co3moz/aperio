//! Unit tests for the scheduled DB-backup helpers: the env-driven schedule
//! parser, the filename timestamp codec, the one-shot `VACUUM INTO` snapshot,
//! and the retention prune. The `spawn` task's infinite interval loop is driven
//! for a single startup iteration under a tokio runtime; the perpetual loop
//! itself is not asserted.

use super::*;

/// Serializes the tests that mutate the `APERIO_BACKUP_*` environment.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const BACKUP_KEYS: &[&str] = &[
  "APERIO_BACKUP_INTERVAL",
  "APERIO_BACKUP_DIR",
  "APERIO_BACKUP_KEEP",
];

struct EnvSnapshot {
  saved: Vec<(&'static str, Option<String>)>,
}

impl EnvSnapshot {
  fn take() -> Self {
    let saved = BACKUP_KEYS
      .iter()
      .map(|k| (*k, std::env::var(k).ok()))
      .collect();
    for k in BACKUP_KEYS {
      unsafe { std::env::remove_var(k) };
    }
    Self { saved }
  }
}

impl Drop for EnvSnapshot {
  fn drop(&mut self) {
    for (k, v) in &self.saved {
      match v {
        Some(val) => unsafe { std::env::set_var(k, val) },
        None => unsafe { std::env::remove_var(k) },
      }
    }
  }
}

fn set(k: &str, v: &str) {
  unsafe { std::env::set_var(k, v) };
}

fn temp_dir() -> PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-backup-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

// --------------------------------------------------------------------------
// BackupConfig::from_env
// --------------------------------------------------------------------------

#[test]
fn from_env_disabled_without_interval_or_dir() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();

  // Nothing set at all.
  assert!(BackupConfig::from_env().is_none());

  // Interval set but no directory.
  set("APERIO_BACKUP_INTERVAL", "60");
  assert!(BackupConfig::from_env().is_none());

  // A zero interval is treated as disabled even with a directory.
  set("APERIO_BACKUP_INTERVAL", "0");
  set("APERIO_BACKUP_DIR", "/tmp/aperio-backups");
  assert!(BackupConfig::from_env().is_none());

  // A blank directory is filtered out.
  set("APERIO_BACKUP_INTERVAL", "60");
  set("APERIO_BACKUP_DIR", "   ");
  assert!(BackupConfig::from_env().is_none());
}

#[test]
fn from_env_enabled_with_defaults_and_overrides() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();

  set("APERIO_BACKUP_INTERVAL", "120");
  set("APERIO_BACKUP_DIR", "/tmp/aperio-backups");
  // Keep unset -> default of 7.
  let cfg = BackupConfig::from_env().expect("enabled");
  assert_eq!(cfg.interval, Duration::from_secs(120));
  assert_eq!(cfg.dir, PathBuf::from("/tmp/aperio-backups"));
  assert_eq!(cfg.keep, 7);

  // An explicit keep overrides the default; a garbage keep also falls back.
  set("APERIO_BACKUP_KEEP", "3");
  assert_eq!(BackupConfig::from_env().unwrap().keep, 3);
  set("APERIO_BACKUP_KEEP", "not-a-number");
  assert_eq!(BackupConfig::from_env().unwrap().keep, 7);
}

// --------------------------------------------------------------------------
// snapshot_ts
// --------------------------------------------------------------------------

#[test]
fn snapshot_ts_matches_only_well_formed_names() {
  assert_eq!(snapshot_ts("aperio-1700000000.db"), Some(1_700_000_000));
  assert_eq!(snapshot_ts("aperio-notanumber.db"), None);
  assert_eq!(snapshot_ts("other-100.db"), None);
  assert_eq!(snapshot_ts("aperio-100.txt"), None);
  assert_eq!(snapshot_ts("readme.txt"), None);
}

// --------------------------------------------------------------------------
// write_snapshot
// --------------------------------------------------------------------------

#[test]
fn write_snapshot_produces_self_contained_db() {
  let data_dir = temp_dir();
  // Materialize a real store so aperio.db exists with the schema.
  let _conn = crate::store::open_db(data_dir.to_str().unwrap());
  let db_path = data_dir.join("aperio.db");

  let backup_dir = temp_dir();
  let (snap, size) = write_snapshot(&db_path, &backup_dir).expect("snapshot");
  assert!(snap.exists());
  assert!(size > 0);
  // A VACUUM INTO snapshot has no WAL/SHM sidecars.
  assert!(!backup_dir.join(format!("{}-wal", snap.display())).exists());

  let _ = std::fs::remove_dir_all(&data_dir);
  let _ = std::fs::remove_dir_all(&backup_dir);
}

#[test]
fn write_snapshot_errors_when_backup_dir_cannot_be_created() {
  let data_dir = temp_dir();
  let _conn = crate::store::open_db(data_dir.to_str().unwrap());
  let db_path = data_dir.join("aperio.db");

  // Use an existing regular file as a directory component: create_dir_all fails.
  let blocker = data_dir.join("not-a-dir");
  std::fs::write(&blocker, b"x").unwrap();
  let bad_dir = blocker.join("sub");

  let err = write_snapshot(&db_path, &bad_dir).expect_err("dir creation must fail");
  assert!(err.contains("cannot create backup dir"), "got: {err}");

  let _ = std::fs::remove_dir_all(&data_dir);
}

// --------------------------------------------------------------------------
// prune_snapshots
// --------------------------------------------------------------------------

#[test]
fn prune_keeps_newest_and_respects_keep_zero() {
  let dir = temp_dir();
  for ts in [100u64, 200, 300, 400] {
    std::fs::write(dir.join(format!("aperio-{ts}.db")), b"x").unwrap();
  }
  // An unrelated file must be left untouched.
  std::fs::write(dir.join("readme.txt"), b"x").unwrap();

  let removed = prune_snapshots(&dir, 2);
  assert_eq!(removed, 2);
  assert!(dir.join("aperio-400.db").exists());
  assert!(dir.join("aperio-300.db").exists());
  assert!(!dir.join("aperio-200.db").exists());
  assert!(!dir.join("aperio-100.db").exists());
  assert!(dir.join("readme.txt").exists());

  // keep = 0 means keep everything.
  assert_eq!(prune_snapshots(&dir, 0), 0);
  // keep >= remaining count is a no-op.
  assert_eq!(prune_snapshots(&dir, 10), 0);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prune_on_missing_dir_is_a_noop() {
  let dir = std::env::temp_dir().join(format!("aperio-missing-{}", uuid::Uuid::new_v4()));
  // read_dir errors -> flattened to empty -> nothing removed.
  assert_eq!(prune_snapshots(&dir, 3), 0);
}

// --------------------------------------------------------------------------
// spawn
// --------------------------------------------------------------------------

#[test]
fn spawn_is_inert_when_unconfigured() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // No backup env -> from_env() is None -> spawn returns without spawning.
  let state = std::sync::Arc::new(crate::test_support::test_state());
  spawn(state);
}

#[tokio::test]
async fn spawn_writes_a_snapshot_on_startup() {
  let data_dir = temp_dir();
  // Real store so <data_dir>/aperio.db exists for VACUUM INTO.
  let _conn = crate::store::open_db(data_dir.to_str().unwrap());
  let backup_dir = temp_dir();

  // `spawn` reads the schedule synchronously (before its task is spawned), so
  // the env only needs to be held around that call. Scope the lock/snapshot so
  // no non-Send guard is held across the awaits below.
  {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvSnapshot::take();
    // A large interval so only the immediate first tick fires within the test.
    set("APERIO_BACKUP_INTERVAL", "3600");
    set("APERIO_BACKUP_DIR", backup_dir.to_str().unwrap());
    set("APERIO_BACKUP_KEEP", "2");

    let mut st = crate::test_support::test_state();
    // spawn derives the db path from settings_path.parent()/aperio.db.
    st.settings_path = data_dir.join("settings.json");
    spawn(std::sync::Arc::new(st));
  }

  // Wait for the first (immediate) interval tick to produce a snapshot.
  let mut wrote = false;
  for _ in 0..50 {
    tokio::time::sleep(Duration::from_millis(20)).await;
    let any = std::fs::read_dir(&backup_dir)
      .map(|rd| {
        rd.flatten()
          .any(|e| snapshot_ts(&e.file_name().to_string_lossy()).is_some())
      })
      .unwrap_or(false);
    if any {
      wrote = true;
      break;
    }
  }
  assert!(wrote, "spawn should write a snapshot on startup");

  let _ = std::fs::remove_dir_all(&data_dir);
  let _ = std::fs::remove_dir_all(&backup_dir);
}
