//! Tests for the inbox store: retention, read state, and the cap that keeps
//! a chatty client from filling it.

use super::*;

fn entry(id: &str, org: Option<&str>) -> InboxEntry {
  InboxEntry {
    id: id.to_string(),
    timestamp: format!("2026-07-19T00:00:0{}+00:00", id.len() % 10),
    method: "POST".to_string(),
    uri: "/webhook".to_string(),
    host: Some("app.example.com".to_string()),
    headers: vec![("content-type".to_string(), "application/json".to_string())],
    body: Some("e30=".to_string()),
    body_truncated: false,
    status: 200,
    service: None,
    org_id: org.map(str::to_string),
  }
}

#[test]
fn test_insert_list_delete_persist() {
  let dir =
    crate::test_support::test_temp_root().join(format!("inbox-test-{}", uuid::Uuid::new_v4()));
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = InboxStore::load(&dir_str);
  store.insert(entry("a", None));
  store.insert(entry("bb", Some("org-1")));

  // Org isolation: each org sees only its own entries.
  assert_eq!(store.list(&None).len(), 1);
  assert_eq!(store.list(&Some("org-1".to_string())).len(), 1);
  assert!(store.get("a", &None).is_some());
  assert!(store.get("a", &Some("org-1".to_string())).is_none());

  // Entries survive a reload.
  let store2 = InboxStore::load(&dir_str);
  assert_eq!(store2.entries.len(), 2);

  // Delete is org-gated too.
  let mut store3 = InboxStore::load(&dir_str);
  assert!(!store3.delete("a", &Some("org-1".to_string())));
  assert!(store3.delete("a", &None));
  assert_eq!(store3.clear(&Some("org-1".to_string())), 1);
  assert!(InboxStore::load(&dir_str).entries.is_empty());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_prune_older_than() {
  let dir =
    crate::test_support::test_temp_root().join(format!("inbox-prune-{}", uuid::Uuid::new_v4()));
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = InboxStore::load(&dir_str);
  let mut old_entry = entry("old", None);
  old_entry.timestamp = "2020-01-01T00:00:00+00:00".to_string();
  let mut fresh = entry("fresh", None);
  fresh.timestamp = chrono::Local::now().to_rfc3339();
  store.insert(old_entry);
  store.insert(fresh);

  let cutoff = crate::store::tokens::now_secs() - 24 * 3600;
  assert_eq!(store.prune_older_than(cutoff), 1);
  assert!(store.get("fresh", &None).is_some());
  assert!(store.get("old", &None).is_none());
  // Persisted: the prune survives a reload.
  assert_eq!(InboxStore::load(&dir_str).entries.len(), 1);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_truncate_oldest() {
  let dir =
    crate::test_support::test_temp_root().join(format!("inbox-trunc-{}", uuid::Uuid::new_v4()));
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = InboxStore::load(&dir_str);
  for i in 0..5 {
    store.insert(entry(&format!("e{i}"), None));
  }
  // The oldest entries go first; the newest survive.
  assert_eq!(store.truncate_oldest(2), 3);
  assert!(store.get("e4", &None).is_some());
  assert!(store.get("e0", &None).is_none());
  assert_eq!(store.truncate_oldest(2), 0);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn insert_and_import_hold_the_cap() {
  let dir = crate::test_support::test_temp_root().join(format!("inbox-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let mut store = InboxStore::load(&dir.to_string_lossy());

  // One over the cap: the oldest live entry gives way.
  for i in 0..=INBOX_MAX_ENTRIES {
    store.insert(entry(&format!("e{i}"), None));
  }
  assert_eq!(store.list_all().len(), INBOX_MAX_ENTRIES);
  assert!(
    store.list_all().iter().all(|e| e.id != "e0"),
    "the oldest went"
  );

  // An oversized import keeps the newest slice, chronologically.
  let mut incoming: Vec<InboxEntry> = (0..INBOX_MAX_ENTRIES + 25)
    .map(|i| {
      let mut e = entry(&format!("i{i}"), None);
      e.timestamp = format!("2026-01-01T00:{:02}:{:02}+00:00", i / 60, i % 60);
      e
    })
    .collect();
  incoming.reverse(); // arrives shuffled; import re-sorts
  assert_eq!(store.import(incoming), INBOX_MAX_ENTRIES);
  assert!(store.list_all().iter().all(|e| e.id != "i0"));
}

/// An entry the operator deleted must not come back at the next restart with
/// nothing having said so, so a delete that could not be saved answers false.
#[test]
fn a_delete_that_cannot_be_saved_reports_false_and_keeps_the_entry() {
  let dir =
    crate::test_support::test_temp_root().join(format!("inbox-full-{}", uuid::Uuid::new_v4()));
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = InboxStore::load(&dir_str);
  store.insert(entry("one", None));
  let id = store.list_all()[0].id.clone();

  store
    .conn
    .execute("DROP TABLE inbox", [])
    .expect("the table exists until this point");

  assert!(!store.delete(&id, &None), "the removal was not saved");
  assert_eq!(store.list_all().len(), 1, "so the entry is still there");
  let _ = std::fs::remove_dir_all(&dir);
}
