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
  /// The organization the master super-admin is currently viewing (`None` =
  /// master). Ignored for named users, whose organization is fixed. Persisted
  /// so an org switch survives a restart.
  #[serde(default)]
  pub(crate) selected_org: Option<String>,
  /// The organization this session is fixed to (per-org OIDC login). When set,
  /// the session acts within this org and cannot switch or reach master — it
  /// is an org-scoped admin, not the super-admin. `None` for all other logins.
  #[serde(default)]
  pub(crate) bound_org: Option<String>,
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
    let removed = self.sessions.remove(&key)?;
    if let Err(e) = self
      .conn
      .execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![key])
    {
      // The on-disk row survived: re-insert into memory so the two agree
      // (a session logged out here must not silently persist to disk and come
      // back on restart) and report nothing removed.
      error!(
        "Failed to delete a session from the store: {}; keeping it",
        e
      );
      self.sessions.insert(key, removed);
      return None;
    }
    Some(removed)
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
    let Some(removed) = self.sessions.remove(key) else {
      return false;
    };
    if let Err(e) = self
      .conn
      .execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![key])
    {
      // Revert so memory matches the surviving on-disk row; report failure.
      error!(
        "Failed to delete a session from the store: {}; keeping it",
        e
      );
      self.sessions.insert(key.to_string(), removed);
      return false;
    }
    true
  }

  /// Drops every session whose key is NOT in `keep` and persists.
  pub(crate) fn retain_keys(&mut self, keep: &[String]) {
    let before = self.sessions.len();
    self.sessions.retain(|k, _| keep.iter().any(|kk| kk == k));
    if self.sessions.len() != before {
      self.persist_all();
    }
  }

  /// Sets the selected organization on a session (org switch) and persists.
  /// Returns whether the session existed.
  pub(crate) fn set_selected_org(&mut self, token: &str, org: Option<String>) -> bool {
    let key = token_key(token);
    if let Some(info) = self.sessions.get_mut(&key) {
      info.selected_org = org;
      let info = info.clone();
      self.persist_one(&key, &info);
      true
    } else {
      false
    }
  }

  /// The selected organization on a session, if the session exists.
  pub(crate) fn selected_org(&self, token: &str) -> Option<Option<String>> {
    self
      .sessions
      .get(&token_key(token))
      .map(|i| i.selected_org.clone())
  }

  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    self.sessions.len()
  }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
