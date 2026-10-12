//! What this pins down: a fresh database has no settings, a written document
//! reads back the same, it is stored in its minimal form, and a stored
//! document this build cannot read is an error rather than the defaults.

use super::*;

fn temp_dir() -> String {
  let dir =
    crate::test_support::test_temp_root().join(format!("settings-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().into_owned()
}

#[test]
fn a_fresh_database_has_no_settings() {
  let store = SettingsStore::load(&temp_dir());
  assert_eq!(store.get().unwrap(), None);
}

#[test]
fn a_written_document_reads_back_and_is_stored_minimal() {
  let dir = temp_dir();
  let mut settings = ServerSettings::default();
  settings.cache.enabled = true;
  settings.limits.tunnels = 7;
  assert!(SettingsStore::load(&dir).put(&settings));

  let store = SettingsStore::load(&dir);
  assert_eq!(store.get().unwrap(), Some(settings));
  let raw: String = store
    .conn
    .query_row("SELECT data FROM settings WHERE id = 'server'", [], |r| {
      r.get(0)
    })
    .unwrap();
  assert_eq!(
    serde_json::from_str::<serde_json::Value>(&raw).unwrap(),
    serde_json::json!({"cache": {"enabled": true}, "limits": {"tunnels": 7}})
  );
}

#[test]
fn a_stored_document_this_build_cannot_read_is_an_error() {
  let dir = temp_dir();
  let mut store = SettingsStore::load(&dir);
  assert!(super::super::replace_all(
    &mut store.conn,
    "settings",
    &[(
      "server".to_string(),
      r#"{"no_such_section":{}}"#.to_string()
    )],
  ));
  let err = store
    .get()
    .expect_err("an unknown key must not read as defaults");
  assert!(err.contains("no_such_section"), "{err}");
}
