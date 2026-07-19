//! Inbound webhook capture inbox: persisted copies of third-party webhooks
//! (Stripe, GitHub, ...) that hit a tunnel whose client opted in with
//! `webhook_inbox: true`. Unlike the in-memory inspector ring, entries
//! survive restarts (the `inbox` table of the shared SQLite store) so an
//! event that arrived while the laptop was closed can be re-fired later.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::info;

/// Entries kept (oldest dropped beyond the cap).
const INBOX_MAX_ENTRIES: usize = 500;

/// One captured inbound webhook request.
#[derive(Serialize, Deserialize, Clone)]
pub struct InboxEntry {
  /// Entry UUID.
  pub id: String,
  /// RFC3339 arrival timestamp (with UTC offset, for the dashboard).
  pub timestamp: String,
  pub method: String,
  /// Full request URI including the query string.
  pub uri: String,
  /// Request hostname the webhook was addressed to.
  pub host: Option<String>,
  /// Request headers as forwarded to the tunnel client (raw; redacted at
  /// view time like the inspector, so re-fire stays byte-accurate).
  pub headers: Vec<(String, String)>,
  /// Base64 request body (possibly truncated).
  pub body: Option<String>,
  /// True when the body exceeded the capture limit or was streamed.
  pub body_truncated: bool,
  /// Status the local backend answered with at arrival time.
  pub status: u16,
  /// Service name of the client that served it (dashboard display).
  pub service: Option<String>,
  /// Organization of the serving client (`None` = master); the inbox is
  /// filtered to the caller's effective org on this.
  #[serde(default)]
  pub org_id: Option<String>,
}

/// Persistent inbox, backed by the `inbox` table of the shared SQLite store.
pub struct InboxStore {
  conn: rusqlite::Connection,
  entries: VecDeque<InboxEntry>,
}

impl InboxStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let mut entries: Vec<InboxEntry> = crate::store::load_all(&conn, "inbox");
    // Rows load in arbitrary order; keep the inbox chronological.
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    if !entries.is_empty() {
      info!(
        "Loaded {} webhook inbox entr(ies) from the store",
        entries.len()
      );
    }
    InboxStore {
      conn,
      entries: entries.into(),
    }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .entries
      .iter()
      .filter_map(|e| {
        serde_json::to_string(e)
          .ok()
          .map(|json| (e.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "inbox", &rows);
  }

  /// Appends one captured webhook, dropping the oldest entry past the cap.
  pub fn insert(&mut self, entry: InboxEntry) {
    if self.entries.len() >= INBOX_MAX_ENTRIES {
      self.entries.pop_front();
    }
    self.entries.push_back(entry);
    self.persist();
  }

  /// Newest-first entries of one organization.
  pub fn list(&self, org: &Option<String>) -> Vec<&InboxEntry> {
    self
      .entries
      .iter()
      .rev()
      .filter(|e| e.org_id == *org)
      .collect()
  }

  /// One entry by id, gated to the caller's organization.
  pub fn get(&self, id: &str, org: &Option<String>) -> Option<&InboxEntry> {
    self.entries.iter().find(|e| e.id == id && e.org_id == *org)
  }

  /// Deletes one entry (org-gated). True when something was removed.
  pub fn delete(&mut self, id: &str, org: &Option<String>) -> bool {
    let before = self.entries.len();
    self.entries.retain(|e| !(e.id == id && e.org_id == *org));
    let removed = self.entries.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  /// Retention: drops entries older than `cutoff_ts` (unix seconds), across
  /// all organizations. Returns removed count.
  pub fn prune_older_than(&mut self, cutoff_ts: u64) -> usize {
    let before = self.entries.len();
    self.entries.retain(|e| {
      chrono::DateTime::parse_from_rfc3339(&e.timestamp)
        .map(|dt| dt.timestamp() as u64 >= cutoff_ts)
        // Unparseable timestamps are kept — never silently drop data on a
        // parse quirk.
        .unwrap_or(true)
    });
    let removed = before - self.entries.len();
    if removed > 0 {
      self.persist();
    }
    removed
  }

  /// Disk guard: drops the oldest entries so at most `keep` remain (across
  /// all organizations). Returns removed count.
  pub fn truncate_oldest(&mut self, keep: usize) -> usize {
    let mut removed = 0usize;
    while self.entries.len() > keep {
      self.entries.pop_front();
      removed += 1;
    }
    if removed > 0 {
      self.persist();
    }
    removed
  }

  /// Empties the caller's organization's inbox. Returns removed count.
  pub fn clear(&mut self, org: &Option<String>) -> usize {
    let before = self.entries.len();
    self.entries.retain(|e| e.org_id != *org);
    let removed = before - self.entries.len();
    if removed > 0 {
      self.persist();
    }
    removed
  }
}

#[cfg(test)]
mod tests {
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
    let dir = std::env::temp_dir().join(format!("aperio-inbox-test-{}", uuid::Uuid::new_v4()));
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
    let dir = std::env::temp_dir().join(format!("aperio-inbox-prune-{}", uuid::Uuid::new_v4()));
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
    let dir = std::env::temp_dir().join(format!("aperio-inbox-trunc-{}", uuid::Uuid::new_v4()));
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
}
