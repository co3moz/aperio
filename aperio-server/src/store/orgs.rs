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
}
