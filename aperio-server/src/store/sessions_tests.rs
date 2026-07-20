use super::*;
use crate::store::users::Role;

fn temp_dir() -> String {
  let dir = std::env::temp_dir().join(format!("aperio-sessions-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().to_string()
}

fn info(expires_at: u64, username: Option<&str>) -> SessionInfo {
  SessionInfo {
    expires_at,
    created_at: 0,
    ip: None,
    user_agent: None,
    scope_host: None,
    username: username.map(str::to_string),
    role: Role::Admin,
    selected_org: None,
    bound_org: None,
  }
}

#[test]
fn test_sessions_survive_reload_hashed() {
  let dir = temp_dir();
  let now = now_secs();
  {
    let mut store = SessionStore::load(&dir);
    store.insert("token-alive", info(now + 3600, Some("ops")));
    store.insert("token-expired", info(now.saturating_sub(10), None));
  }

  // Reload: the live session is back, the expired one was pruned.
  let store = SessionStore::load(&dir);
  assert_eq!(store.len(), 1);
  let restored = store.get("token-alive").expect("session restored");
  assert_eq!(restored.username.as_deref(), Some("ops"));
  assert!(store.get("token-expired").is_none());

  // Only hashed keys ever reach the database.
  let conn = crate::store::open_db(&dir);
  let ids: Vec<String> = {
    let mut stmt = conn.prepare("SELECT id FROM sessions").unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    rows.flatten().collect()
  };
  assert_eq!(ids.len(), 1);
  assert!(ids[0].len() == 64 && !ids[0].contains("token"));
}

#[test]
fn test_remove_and_retain_persist() {
  let dir = temp_dir();
  let now = now_secs();
  let mut store = SessionStore::load(&dir);
  store.insert("a", info(now + 3600, Some("alice")));
  store.insert("b", info(now + 3600, Some("bob")));
  store.insert("c", info(now + 3600, Some("alice")));

  assert!(store.remove("b").is_some());
  assert!(store.remove("b").is_none());

  // Ending every session of one user (account deletion) persists too.
  store.retain(|s| s.username.as_deref() != Some("alice"));
  assert_eq!(store.len(), 0);

  let reloaded = SessionStore::load(&dir);
  assert_eq!(reloaded.len(), 0);
}

#[test]
fn test_session_user_agent_trims_caps_and_filters() {
  use axum::http::HeaderMap;

  // Missing header -> None.
  assert!(session_user_agent(&HeaderMap::new()).is_none());

  // Present header -> trimmed.
  let mut headers = HeaderMap::new();
  headers.insert("user-agent", "  Mozilla/5.0 (test)  ".parse().unwrap());
  assert_eq!(
    session_user_agent(&headers).as_deref(),
    Some("Mozilla/5.0 (test)")
  );

  // Blank header -> None (filtered).
  let mut headers = HeaderMap::new();
  headers.insert("user-agent", "   ".parse().unwrap());
  assert!(session_user_agent(&headers).is_none());

  // Over-long header -> truncated to 256 bytes.
  let mut headers = HeaderMap::new();
  let long = "a".repeat(500);
  headers.insert("user-agent", long.parse().unwrap());
  assert_eq!(session_user_agent(&headers).unwrap().len(), 256);
}

#[test]
fn test_entries_remove_by_key_and_token_match() {
  let dir = temp_dir();
  let now = now_secs();
  let mut store = SessionStore::load(&dir);
  store.insert("a", info(now + 3600, Some("alice")));
  store.insert("b", info(now + 3600, Some("bob")));

  let entries = store.entries();
  assert_eq!(entries.len(), 2);
  // The management id is the hashed token; token_matches_key confirms it.
  let (key_a, _) = entries
    .iter()
    .find(|(k, _)| SessionStore::token_matches_key("a", k))
    .expect("entry for token a");
  assert!(!SessionStore::token_matches_key("b", key_a));

  let key_a = key_a.clone();
  assert!(store.remove_by_key(&key_a));
  assert!(!store.remove_by_key(&key_a));
  assert_eq!(store.len(), 1);

  // Removal persisted.
  let reloaded = SessionStore::load(&dir);
  assert_eq!(reloaded.len(), 1);
  assert!(reloaded.get("b").is_some());
}

#[test]
fn test_retain_keys_persists() {
  let dir = temp_dir();
  let now = now_secs();
  let mut store = SessionStore::load(&dir);
  store.insert("a", info(now + 3600, Some("alice")));
  store.insert("b", info(now + 3600, Some("bob")));
  store.insert("c", info(now + 3600, Some("carol")));

  let key_a = store
    .entries()
    .into_iter()
    .find(|(k, _)| SessionStore::token_matches_key("a", k))
    .map(|(k, _)| k)
    .unwrap();

  // Keep only session "a"; the rest are dropped and the change persisted.
  store.retain_keys(std::slice::from_ref(&key_a));
  assert_eq!(store.len(), 1);
  assert!(store.get("a").is_some());

  let reloaded = SessionStore::load(&dir);
  assert_eq!(reloaded.len(), 1);
  assert!(reloaded.get("a").is_some());

  // Retaining the identical set is a no-op (no persist path change).
  let mut store = reloaded;
  store.retain_keys(&[key_a]);
  assert_eq!(store.len(), 1);
}

#[test]
fn test_selected_org_set_get_and_persist() {
  let dir = temp_dir();
  let now = now_secs();
  let mut store = SessionStore::load(&dir);
  store.insert("a", info(now + 3600, Some("root")));

  // Unset by default.
  assert_eq!(store.selected_org("a"), Some(None));

  // Switch org and read it back.
  assert!(store.set_selected_org("a", Some("org-1".into())));
  assert_eq!(store.selected_org("a"), Some(Some("org-1".into())));

  // Persisted across a reload.
  let store = SessionStore::load(&dir);
  assert_eq!(store.selected_org("a"), Some(Some("org-1".into())));

  // Unknown sessions: set returns false, get returns None.
  let mut store = store;
  assert!(!store.set_selected_org("unknown", Some("x".into())));
  assert_eq!(store.selected_org("unknown"), None);
}

#[test]
fn test_load_skips_unparseable_session_row() {
  let dir = temp_dir();
  let now = now_secs();
  {
    let mut store = SessionStore::load(&dir);
    store.insert("good", info(now + 3600, Some("ok")));
  }
  // Inject a corrupt row directly into the table.
  {
    let conn = crate::store::open_db(&dir);
    conn
      .execute(
        "INSERT INTO sessions (id, data) VALUES ('corrupt-key', 'not json')",
        [],
      )
      .unwrap();
  }

  // Reload: the bad row is skipped, the good session survives.
  let store = SessionStore::load(&dir);
  assert_eq!(store.len(), 1);
  assert!(store.get("good").is_some());
}
