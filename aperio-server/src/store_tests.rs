use super::*;

/// A unique, freshly-created temp directory returned as (PathBuf, String).
fn temp_dir() -> (PathBuf, String) {
  let dir = std::env::temp_dir().join(format!("aperio-store-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let s = dir.to_string_lossy().to_string();
  (dir, s)
}

#[test]
fn test_replace_all_and_load_all_round_trip() {
  let (dir, dir_str) = temp_dir();
  let mut conn = open_db(&dir_str);

  replace_all(
    &mut conn,
    "tokens",
    &[
      ("1".into(), "\"hello\"".into()),
      ("2".into(), "\"world\"".into()),
    ],
  );
  let mut vals: Vec<String> = load_all(&conn, "tokens");
  vals.sort();
  assert_eq!(vals, vec!["hello".to_string(), "world".to_string()]);

  // A second replace_all fully supersedes the previous contents.
  replace_all(&mut conn, "tokens", &[("3".into(), "\"only\"".into())]);
  let vals: Vec<String> = load_all(&conn, "tokens");
  assert_eq!(vals, vec!["only".to_string()]);
  drop(conn);

  // Reload from disk: the persisted row is visible on a fresh connection.
  let conn2 = open_db(&dir_str);
  let vals2: Vec<String> = load_all(&conn2, "tokens");
  assert_eq!(vals2, vec!["only".to_string()]);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_load_all_skips_unparseable_and_missing_table() {
  let (dir, dir_str) = temp_dir();
  let conn = open_db(&dir_str);
  conn
    .execute(
      "INSERT INTO tokens (id, data) VALUES ('x', 'not valid json')",
      [],
    )
    .unwrap();
  conn
    .execute("INSERT INTO tokens (id, data) VALUES ('y', '\"ok\"')", [])
    .unwrap();

  // The unparseable row is skipped (logged), the valid one is returned.
  let vals: Vec<String> = load_all(&conn, "tokens");
  assert_eq!(vals, vec!["ok".to_string()]);

  // A missing table makes prepare fail -> an empty vec, never a panic.
  let empty: Vec<String> = load_all(&conn, "no_such_table");
  assert!(empty.is_empty());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_all_bad_table_is_non_fatal() {
  let (dir, dir_str) = temp_dir();
  let mut conn = open_db(&dir_str);
  // DELETE FROM <missing table> errors inside the transaction; replace_all
  // logs and returns without panicking.
  replace_all(&mut conn, "no_such_table", &[("1".into(), "\"x\"".into())]);
  // The real tables are still usable afterwards.
  replace_all(&mut conn, "tokens", &[("1".into(), "\"live\"".into())]);
  let vals: Vec<String> = load_all(&conn, "tokens");
  assert_eq!(vals, vec!["live".to_string()]);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_corrupt_db_is_backed_up_and_recreated() {
  let (dir, dir_str) = temp_dir();
  let db = dir.join("aperio.db");
  // A file that is not a valid SQLite database: open_db must back it up and
  // start fresh instead of failing.
  std::fs::write(&db, b"this is definitely not a sqlite database header").unwrap();

  let mut conn = open_db(&dir_str);

  // The bad file was preserved as aperio.db.corrupt.<epoch>.
  let has_backup = std::fs::read_dir(&dir).unwrap().any(|e| {
    e.unwrap()
      .file_name()
      .to_string_lossy()
      .starts_with("aperio.db.corrupt.")
  });
  assert!(has_backup, "corrupt database should be backed up aside");

  // The freshly created database is fully usable.
  replace_all(&mut conn, "tokens", &[("1".into(), "\"fresh\"".into())]);
  let vals: Vec<String> = load_all(&conn, "tokens");
  assert_eq!(vals, vec!["fresh".to_string()]);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_backup_corrupt_returns_none_when_missing() {
  // Renaming a non-existent path cannot succeed -> None (not a panic).
  let (dir, _dir_str) = temp_dir();
  let missing = dir.join("nope.db");
  assert!(backup_corrupt(&missing).is_none());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_data_dir_is_a_file_falls_back_to_memory() {
  // When data_dir is actually a regular file, create_dir_all fails and the
  // database can neither be opened nor renamed aside, so open_db falls back
  // to a volatile in-memory connection rather than crashing the server.
  let (dir, _dir_str) = temp_dir();
  let file_path = dir.join("iam-a-file");
  std::fs::write(&file_path, b"regular file, not a directory").unwrap();

  let conn = open_db(&file_path.to_string_lossy());
  // The fallback connection is a live SQLite handle (schema-less, in-memory).
  let one: i64 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
  assert_eq!(one, 1);

  let _ = std::fs::remove_dir_all(&dir);
}
