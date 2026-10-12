//! Unit tests for the scheduled DB-backup helpers: the schedule read from
//! the settings, the filename timestamp codec, the one-shot `VACUUM INTO` snapshot,
//! and the retention prune. The `spawn` task's infinite interval loop is driven
//! for a single startup iteration under a tokio runtime; the perpetual loop
//! itself is not asserted.

use super::*;

fn temp_dir() -> PathBuf {
  let dir =
    crate::test_support::test_temp_root().join(format!("backup-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

// --------------------------------------------------------------------------
// BackupConfig::from_settings
// --------------------------------------------------------------------------

#[test]
fn from_settings_disabled_without_interval_or_dir() {
  // Nothing set at all.
  crate::live::reset();
  assert!(BackupConfig::from_settings().is_none());

  // Interval set but no directory.
  crate::live::set_yaml(
    "backup:
  interval: 1m
",
  );
  assert!(BackupConfig::from_settings().is_none());

  // A zero interval is treated as disabled even with a directory.
  crate::live::set_yaml(
    "backup:
  interval: 0s
  dir: /tmp/aperio-backups
",
  );
  assert!(BackupConfig::from_settings().is_none());

  // A blank directory is filtered out.
  crate::live::set_yaml(
    "backup:
  interval: 1m
  dir: \"   \"
",
  );
  assert!(BackupConfig::from_settings().is_none());
}

#[test]
fn from_settings_enabled_with_defaults_and_overrides() {
  crate::live::set_yaml(
    "backup:
  interval: 2m
  dir: /tmp/aperio-backups
",
  );
  // Keep unset -> default of 7.
  let cfg = BackupConfig::from_settings().expect("enabled");
  assert_eq!(cfg.interval, Duration::from_secs(120));
  assert_eq!(cfg.dir, PathBuf::from("/tmp/aperio-backups"));
  assert_eq!(cfg.keep, 7);

  crate::live::set_yaml(
    "backup:
  interval: 2m
  dir: /tmp/aperio-backups
  keep: 3
",
  );
  assert_eq!(BackupConfig::from_settings().unwrap().keep, 3);
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
  let (snap, size) = write_snapshot(&db_path, &backup_dir, None).expect("snapshot");
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

  let err = write_snapshot(&db_path, &bad_dir, None).expect_err("dir creation must fail");
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
  let dir = crate::test_support::test_temp_root().join(format!("missing-{}", uuid::Uuid::new_v4()));
  // read_dir errors -> flattened to empty -> nothing removed.
  assert_eq!(prune_snapshots(&dir, 3), 0);
}

// --------------------------------------------------------------------------
// spawn
// --------------------------------------------------------------------------

#[test]
fn spawn_is_inert_when_unconfigured() {
  crate::live::reset();
  // No backup settings -> from_settings() is None -> spawn returns without
  // spawning.
  let state = std::sync::Arc::new(crate::test_support::test_state());
  spawn(state);
}

#[tokio::test]
async fn spawn_writes_a_snapshot_on_startup() {
  let data_dir = temp_dir();
  // Real store so <data_dir>/aperio.db exists for VACUUM INTO.
  let _conn = crate::store::open_db(data_dir.to_str().unwrap());
  let backup_dir = temp_dir();

  // `spawn` reads the schedule synchronously, before its task is spawned.
  // A large interval so only the immediate first tick fires within the test.
  crate::live::set_yaml(&format!(
    "backup:
  interval: 1h
  dir: {:?}
  keep: 2
",
    backup_dir.to_str().unwrap()
  ));
  let mut st = crate::test_support::test_state();
  // spawn derives the db path from the data directory.
  st.data_dir = data_dir.to_string_lossy().into_owned();
  spawn(std::sync::Arc::new(st));

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

/// An encrypted snapshot lands under a different name, leaves no plaintext
/// behind, and comes back byte for byte.
#[test]
fn an_encrypted_snapshot_replaces_the_plaintext_one() {
  let root = crate::test_support::test_temp_root().join(format!("bkenc-{}", uuid::Uuid::new_v4()));
  let data = root.join("data");
  let backups = root.join("backups");
  std::fs::create_dir_all(&data).unwrap();
  let db_path = data.join("aperio.db");
  let conn = rusqlite::Connection::open(&db_path).unwrap();
  conn
    .execute("CREATE TABLE t (id TEXT PRIMARY KEY, data TEXT)", [])
    .unwrap();
  conn
    .execute("INSERT INTO t VALUES ('1', 'a recognisable row')", [])
    .unwrap();
  drop(conn);

  let key = crate::backup_crypto::BackupKey::parse(&"3c".repeat(32)).unwrap();
  let (snap, size) = write_snapshot(&db_path, &backups, Some(&key)).expect("snapshot");

  assert!(
    snap.to_string_lossy().ends_with(".db.enc"),
    "an encrypted snapshot says so in its name: {snap:?}"
  );
  assert!(size > 0);
  let plaintext_left: Vec<_> = std::fs::read_dir(&backups)
    .unwrap()
    .flatten()
    .map(|e| e.file_name().to_string_lossy().into_owned())
    .filter(|n| n.ends_with(".db"))
    .collect();
  assert!(
    plaintext_left.is_empty(),
    "the intermediate plaintext snapshot is gone: {plaintext_left:?}"
  );
  let bytes = std::fs::read(&snap).unwrap();
  assert!(
    !bytes.windows(18).any(|w| w == b"a recognisable row"),
    "and the row is not readable in the file"
  );

  // And it is still a database when it comes back, which is the only claim
  // that matters about a backup.
  let restored = root.join("restored.db");
  crate::backup_crypto::decrypt_file(&key, &snap, &restored).expect("decrypt");
  let conn = rusqlite::Connection::open(&restored).unwrap();
  let row: String = conn
    .query_row("SELECT data FROM t WHERE id = '1'", [], |r| r.get(0))
    .unwrap();
  assert_eq!(row, "a recognisable row");
  let _ = std::fs::remove_dir_all(&root);
}

/// Pruning has to see both spellings, or a directory written to before
/// encryption was turned on keeps its old snapshots for ever.
#[test]
fn both_snapshot_names_are_pruned() {
  assert_eq!(snapshot_ts("aperio-1700000000.db"), Some(1_700_000_000));
  assert_eq!(snapshot_ts("aperio-1700000000.db.enc"), Some(1_700_000_000));
  assert_eq!(snapshot_ts("aperio-notanumber.db.enc"), None);
  assert_eq!(snapshot_ts("something-else.db"), None);
}
