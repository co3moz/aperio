//! Persistent dashboard sessions: the `sessions` table of the shared SQLite
//! store, so signed-in users (password, TOTP, passkey, OIDC) survive a server
//! restart instead of being bounced to the login page.
//!
//! Rows are keyed by the SHA-256 of the session cookie token — someone who
//! can read `aperio.db` must not be able to lift live session cookies out of
//! it. Every mutation persists immediately: sessions change on login/logout,
//! not per request, so the write volume is negligible.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{error, info};

/// Server-side state of one `aperio_session` cookie.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct SessionInfo {
  /// Unix seconds after which this session is invalid.
  pub(crate) expires_at: u64,
  /// Unix seconds when the session was created (0 for rows predating the
  /// session-management feature).
  #[serde(default)]
  pub(crate) created_at: u64,
  /// IP the session was created from, for the sessions list.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) ip: Option<String>,
  /// User-Agent of the signing-in browser, for the sessions list.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) user_agent: Option<String>,
  /// When `Some(host)`, the session only authorizes proxied traffic for that
  /// exact request host — a login against a client-set visitor password. It
  /// never authorizes the dashboard or other hosts. `None` = a full/global
  /// session (server password, dashboard password, master token, or OIDC).
  pub(crate) scope_host: Option<String>,
  /// Dashboard identity: the user this session belongs to (None = master
  /// token / dashboard password / visitor session).
  pub(crate) username: Option<String>,
  /// Dashboard role checked by the authorization middleware.
  pub(crate) role: crate::store::users::Role,
}

/// Current unix time in seconds.
pub(crate) fn now_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

/// Trimmed User-Agent of the signing-in browser, capped for storage.
pub(crate) fn session_user_agent(headers: &axum::http::HeaderMap) -> Option<String> {
  headers
    .get("user-agent")
    .and_then(|v| v.to_str().ok())
    .map(|v| {
      let mut ua = v.trim().to_string();
      ua.truncate(256);
      ua
    })
    .filter(|v| !v.is_empty())
}

/// Hex SHA-256 of a session token — the only form ever written to disk.
fn token_key(token: &str) -> String {
  use sha2::{Digest, Sha256};
  let mut hasher = Sha256::new();
  hasher.update(token.as_bytes());
  hasher
    .finalize()
    .iter()
    .map(|b| format!("{:02x}", b))
    .collect()
}

/// Reads every non-expired session row; unparseable rows are skipped.
fn read_live_sessions(conn: &rusqlite::Connection) -> HashMap<String, SessionInfo> {
  let mut sessions = HashMap::new();
  let mut stmt = match conn.prepare("SELECT id, data FROM sessions") {
    Ok(s) => s,
    Err(e) => {
      error!("Failed to read sessions from the store: {}", e);
      return sessions;
    }
  };
  let rows = stmt.query_map([], |row| {
    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
  });
  if let Ok(rows) = rows {
    let now = now_secs();
    for (key, raw) in rows.flatten() {
      match serde_json::from_str::<SessionInfo>(&raw) {
        Ok(info) if info.expires_at > now => {
          sessions.insert(key, info);
        }
        Ok(_) => {} // expired while the server was down
        Err(e) => error!("Skipping unparseable session row: {}", e),
      }
    }
  }
  sessions
}

/// Disk-backed session store (the `sessions` table of `<data_dir>/aperio.db`).
pub(crate) struct SessionStore {
  conn: rusqlite::Connection,
  /// Live sessions keyed by the hashed token.
  sessions: HashMap<String, SessionInfo>,
}

impl SessionStore {
  pub(crate) fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let sessions = read_live_sessions(&conn);
    let mut store = SessionStore { conn, sessions };
    // Expired rows loaded above were dropped from memory; drop them on disk
    // too so the table doesn't accumulate.
    store.persist_all();
    if !store.sessions.is_empty() {
      info!(
        "Restored {} dashboard session(s) from the store",
        store.sessions.len()
      );
    }
    store
  }

  fn persist_one(&self, key: &str, info: &SessionInfo) {
    if let Ok(json) = serde_json::to_string(info)
      && let Err(e) = self.conn.execute(
        "INSERT INTO sessions (id, data) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        rusqlite::params![key, json],
      )
    {
      error!("Failed to persist a session to the store: {}", e);
    }
  }

  fn persist_all(&mut self) {
    let rows: Vec<(String, String)> = self
      .sessions
      .iter()
      .filter_map(|(k, v)| serde_json::to_string(v).ok().map(|json| (k.clone(), json)))
      .collect();
    crate::store::replace_all(&mut self.conn, "sessions", &rows);
  }

  pub(crate) fn insert(&mut self, token: &str, info: SessionInfo) {
    let key = token_key(token);
    self.persist_one(&key, &info);
    self.sessions.insert(key, info);
  }

  pub(crate) fn get(&self, token: &str) -> Option<&SessionInfo> {
    self.sessions.get(&token_key(token))
  }

  pub(crate) fn remove(&mut self, token: &str) -> Option<SessionInfo> {
    let key = token_key(token);
    let removed = self.sessions.remove(&key);
    if removed.is_some()
      && let Err(e) = self
        .conn
        .execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![key])
    {
      error!("Failed to delete a session from the store: {}", e);
    }
    removed
  }

  /// Drops every session that fails the predicate (bulk: expiry GC, user
  /// deletion) and rewrites the table when anything changed.
  pub(crate) fn retain<F: FnMut(&SessionInfo) -> bool>(&mut self, mut keep: F) {
    let before = self.sessions.len();
    self.sessions.retain(|_, info| keep(info));
    if self.sessions.len() != before {
      self.persist_all();
    }
  }

  /// All live sessions with their (hashed-token) management ids, for the
  /// dashboard sessions list. The hash cannot be turned back into a cookie,
  /// so exposing it as an id is safe.
  pub(crate) fn entries(&self) -> Vec<(String, SessionInfo)> {
    self
      .sessions
      .iter()
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect()
  }

  /// True when `token` hashes to `key` — used to mark the caller's own
  /// session in the list.
  pub(crate) fn token_matches_key(token: &str, key: &str) -> bool {
    token_key(token) == key
  }

  /// Removes a session by its management id (hashed token). Returns whether
  /// anything was removed.
  pub(crate) fn remove_by_key(&mut self, key: &str) -> bool {
    let removed = self.sessions.remove(key).is_some();
    if removed
      && let Err(e) = self
        .conn
        .execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![key])
    {
      error!("Failed to delete a session from the store: {}", e);
    }
    removed
  }

  /// Drops every session whose key is NOT in `keep` and persists.
  pub(crate) fn retain_keys(&mut self, keep: &[String]) {
    let before = self.sessions.len();
    self.sessions.retain(|k, _| keep.iter().any(|kk| kk == k));
    if self.sessions.len() != before {
      self.persist_all();
    }
  }

  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    self.sessions.len()
  }
}

#[cfg(test)]
mod tests {
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
}
