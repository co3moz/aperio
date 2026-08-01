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

  /// Every entry, oldest first, across organizations. For a dump; the
  /// dashboard's list is org-scoped.
  pub fn list_all(&self) -> Vec<&InboxEntry> {
    self.entries.iter().collect()
  }

  /// Replaces the inbox with an imported set, keeping it chronological and
  /// within the cap. Returns how many entries were kept.
  pub fn import(&mut self, entries: Vec<InboxEntry>) -> usize {
    let mut entries = entries;
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    if entries.len() > INBOX_MAX_ENTRIES {
      entries.drain(..entries.len() - INBOX_MAX_ENTRIES);
    }
    self.entries = entries.into();
    self.persist();
    self.entries.len()
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
        // Unparseable timestamps are kept, never silently drop data on a
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
#[path = "inbox_tests.rs"]
mod tests;
