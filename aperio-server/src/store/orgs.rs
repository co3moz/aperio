//! Organizations (multi-tenancy). Each organization groups the users and
//! tokens created under it; a connected client belongs to the organization of
//! the token it authenticated with.
//!
//! The **master** organization is implicit and is *not* a row here: it is
//! represented by `org_id: None` on users and tokens. The built-in `aperio`
//! admin, the master token, and the dashboard password all act within master
//! and can switch into any child organization. Only the child organizations
//! created through master are stored in this table.

use serde::{Deserialize, Serialize};
use tracing::info;

/// The reserved id the API uses to refer to the implicit master organization
/// (which has no row of its own). Never a valid child-org id (child ids are
/// UUIDs).
pub(crate) const MASTER_ID: &str = "master";

/// One child organization.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct Organization {
  /// Unique id (UUID).
  pub id: String,
  /// Human-readable name.
  pub name: String,
  /// Unix seconds of creation.
  pub created_at: u64,
  /// Max concurrently-connected clients in this org (None = unlimited).
  #[serde(default)]
  pub max_clients: Option<u64>,
  /// Max dynamic tokens in this org (None = unlimited).
  #[serde(default)]
  pub max_tokens: Option<u64>,
  /// Max dashboard users in this org (None = unlimited).
  #[serde(default)]
  pub max_users: Option<u64>,
  /// Max proxied bytes (in + out) this org may serve per calendar month
  /// (None = unlimited). Enforced against the month's per-org stats bucket.
  #[serde(default)]
  pub max_bytes_month: Option<u64>,
}

/// Persistent store of child organizations, backed by the `organizations`
/// table of the shared SQLite store.
pub struct OrgStore {
  conn: rusqlite::Connection,
  orgs: Vec<Organization>,
}

impl OrgStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let orgs: Vec<Organization> = crate::store::load_all(&conn, "organizations");
    if !orgs.is_empty() {
      info!("Loaded {} organization(s) from the store", orgs.len());
    }
    OrgStore { conn, orgs }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .orgs
      .iter()
      .filter_map(|o| serde_json::to_string(o).ok().map(|j| (o.id.clone(), j)))
      .collect();
    crate::store::replace_all(&mut self.conn, "organizations", &rows);
  }

  /// Replaces every org record (dump import) and persists.
  pub fn import(&mut self, orgs: Vec<Organization>) -> usize {
    self.orgs = orgs;
    self.persist();
    self.orgs.len()
  }

  /// Creates a child organization. Names are unique (case-insensitive);
  /// `master` is reserved.
  pub fn create(&mut self, name: &str) -> Result<Organization, String> {
    let name = name.trim();
    if name.is_empty() {
      return Err("organization name is required".into());
    }
    if name.eq_ignore_ascii_case("master") {
      return Err("\"master\" is reserved for the built-in organization".into());
    }
    if self.orgs.iter().any(|o| o.name.eq_ignore_ascii_case(name)) {
      return Err(format!("an organization named \"{name}\" already exists"));
    }
    let org = Organization {
      id: uuid::Uuid::new_v4().to_string(),
      name: name.to_string(),
      created_at: crate::store::tokens::now_secs(),
      max_clients: None,
      max_tokens: None,
      max_users: None,
      max_bytes_month: None,
    };
    self.orgs.push(org.clone());
    self.persist();
    Ok(org)
  }

  /// Removes an org by id. Returns whether one was removed.
  pub fn delete(&mut self, id: &str) -> bool {
    let before = self.orgs.len();
    self.orgs.retain(|o| o.id != id);
    let removed = self.orgs.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  pub fn list(&self) -> &[Organization] {
    &self.orgs
  }

  /// Looks up an org by id.
  pub fn find(&self, id: &str) -> Option<&Organization> {
    self.orgs.iter().find(|o| o.id == id)
  }

  /// Updates an org's quotas in place. `Some(None)` clears a quota, `Some(v)`
  /// sets it, `None` leaves it unchanged. Returns the updated record.
  pub fn set_quota(
    &mut self,
    id: &str,
    max_clients: Option<Option<u64>>,
    max_tokens: Option<Option<u64>>,
    max_users: Option<Option<u64>>,
    max_bytes_month: Option<Option<u64>>,
  ) -> Option<Organization> {
    let org = self.orgs.iter_mut().find(|o| o.id == id)?;
    if let Some(v) = max_clients {
      org.max_clients = v.filter(|n| *n > 0);
    }
    if let Some(v) = max_tokens {
      org.max_tokens = v.filter(|n| *n > 0);
    }
    if let Some(v) = max_users {
      org.max_users = v.filter(|n| *n > 0);
    }
    if let Some(v) = max_bytes_month {
      org.max_bytes_month = v.filter(|n| *n > 0);
    }
    let updated = org.clone();
    self.persist();
    Some(updated)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_dir() -> String {
    let dir = std::env::temp_dir().join(format!("aperio-orgs-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().to_string()
  }

  #[test]
  fn test_create_unique_and_reserved() {
    let dir = temp_dir();
    let mut store = OrgStore::load(&dir);
    let a = store.create("Acme").unwrap();
    assert_eq!(store.list().len(), 1);

    // Case-insensitive uniqueness and the reserved name.
    assert!(store.create("acme").is_err());
    assert!(store.create("master").is_err());
    assert!(store.create("  ").is_err());

    // Survives a reload.
    let reloaded = OrgStore::load(&dir);
    assert_eq!(reloaded.list().len(), 1);

    // Delete.
    let mut store = OrgStore::load(&dir);
    assert!(store.delete(&a.id));
    assert!(!store.delete(&a.id));
    assert!(store.list().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_set_quota_and_persist() {
    let dir = temp_dir();
    let mut store = OrgStore::load(&dir);
    let org = store.create("Acme").unwrap();
    assert!(org.max_tokens.is_none());

    // Set two quotas; leave the others untouched.
    let updated = store
      .set_quota(&org.id, Some(Some(3)), Some(Some(10)), None, None)
      .unwrap();
    assert_eq!(updated.max_clients, Some(3));
    assert_eq!(updated.max_tokens, Some(10));
    assert!(updated.max_users.is_none());

    // Survives reload; 0 clears a quota.
    let mut reloaded = OrgStore::load(&dir);
    assert_eq!(reloaded.find(&org.id).unwrap().max_tokens, Some(10));
    let cleared = reloaded
      .set_quota(&org.id, Some(Some(0)), None, None, None)
      .unwrap();
    assert!(cleared.max_clients.is_none());
    assert_eq!(cleared.max_tokens, Some(10));

    let _ = std::fs::remove_dir_all(&dir);
  }
}
