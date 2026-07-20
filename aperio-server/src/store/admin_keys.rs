//! Programmatic admin API keys: least-privilege, non-cookie credentials for
//! automation (CI, Terraform, Slack) that call the dashboard API.
//!
//! An admin key authenticates a caller with a fixed **role** (viewer /
//! operator / admin) and a fixed **organization**, presented as
//! `Authorization: Bearer <key>`. Unlike the master token it is scoped and
//! revocable, so automation never needs the all-powerful master credential.
//! Only the SHA-256 hash of the secret is stored; the secret is shown once.

use serde::{Deserialize, Serialize};

use crate::store::tokens::{hash_token, now_secs};
use crate::store::users::Role;

/// A programmatic admin API key record (secret stored only as a hash).
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct AdminKey {
  /// Unique record ID (UUID).
  pub id: String,
  /// Human-readable label chosen at creation time.
  pub name: String,
  /// Hex-encoded SHA-256 hash of the key secret.
  pub key_hash: String,
  /// First characters of the secret, kept for display purposes only.
  pub key_prefix: String,
  /// Role this key authenticates as (its privilege ceiling).
  pub role: Role,
  /// Organization this key acts within; `None` = the master organization.
  #[serde(default)]
  pub org_id: Option<String>,
  /// Unix timestamp (seconds) of creation.
  pub created_at: u64,
  /// Optional unix timestamp (seconds) after which the key is rejected.
  #[serde(default)]
  pub expires_at: Option<u64>,
}

impl AdminKey {
  /// Returns true when the key is past its expiry time.
  pub fn is_expired(&self) -> bool {
    self.expires_at.is_some_and(|exp| now_secs() >= exp)
  }
}

/// Persistent store for programmatic admin API keys, backed by the
/// `admin_keys` table of the shared SQLite store.
pub struct AdminKeyStore {
  conn: rusqlite::Connection,
  keys: Vec<AdminKey>,
}

impl AdminKeyStore {
  /// Opens the shared store and loads all admin-key records.
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let keys: Vec<AdminKey> = crate::store::load_all(&conn, "admin_keys");
    if !keys.is_empty() {
      tracing::info!(
        "Loaded {} programmatic admin key(s) from the store",
        keys.len()
      );
    }
    AdminKeyStore { conn, keys }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .keys
      .iter()
      .filter_map(|k| serde_json::to_string(k).ok().map(|j| (k.id.clone(), j)))
      .collect();
    crate::store::replace_all(&mut self.conn, "admin_keys", &rows);
  }

  /// Creates a new admin key, persists it, and returns the record plus the
  /// plaintext secret (available only at creation time).
  pub fn create(
    &mut self,
    name: String,
    role: Role,
    org_id: Option<String>,
    ttl_seconds: Option<u64>,
  ) -> (AdminKey, String) {
    let secret = format!(
      "apk_{}{}",
      uuid::Uuid::new_v4().simple(),
      uuid::Uuid::new_v4().simple()
    );
    let record = AdminKey {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      key_hash: hash_token(&secret),
      key_prefix: secret.chars().take(12).collect(),
      role,
      org_id,
      created_at: now_secs(),
      expires_at: ttl_seconds.map(|ttl| now_secs().saturating_add(ttl)),
    };
    self.keys.push(record.clone());
    self.persist();
    (record, secret)
  }

  /// Removes a key by ID. Returns true when a key was actually removed.
  pub fn revoke(&mut self, id: &str) -> bool {
    let before = self.keys.len();
    self.keys.retain(|k| k.id != id);
    let removed = self.keys.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  /// All key records (hashes included; strip before exposing).
  pub fn list(&self) -> &[AdminKey] {
    &self.keys
  }

  /// Verifies a presented secret against the store, returning the matching
  /// non-expired key. The hashes are compared in constant time.
  pub fn verify(&self, secret: &str) -> Option<&AdminKey> {
    let hash = hash_token(secret);
    self
      .keys
      .iter()
      .find(|k| !k.is_expired() && crate::auth::constant_time_eq_str(&k.key_hash, &hash))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_dir() -> String {
    std::env::temp_dir()
      .join(format!("aperio-adminkeys-test-{}", uuid::Uuid::new_v4()))
      .to_string_lossy()
      .to_string()
  }

  #[test]
  fn test_create_verify_revoke_scope_persist() {
    let dir = temp_dir();
    let mut store = AdminKeyStore::load(&dir);
    assert!(store.list().is_empty());

    let (rec, secret) = store.create(
      "ci".to_string(),
      Role::Operator,
      Some("org-1".to_string()),
      None,
    );
    assert!(secret.starts_with("apk_"));
    let found = store.verify(&secret).unwrap();
    assert_eq!(found.id, rec.id);
    assert_eq!(found.role, Role::Operator);
    assert_eq!(found.org_id.as_deref(), Some("org-1"));
    assert!(store.verify("apk_wrong").is_none());

    // Persisted across reloads.
    let store2 = AdminKeyStore::load(&dir);
    assert_eq!(store2.verify(&secret).unwrap().name, "ci");

    // Revoked keys stop verifying.
    let mut store3 = AdminKeyStore::load(&dir);
    assert!(store3.revoke(&rec.id));
    assert!(store3.verify(&secret).is_none());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_expired_key_rejected() {
    let dir = temp_dir();
    let mut store = AdminKeyStore::load(&dir);
    let (_, secret) = store.create("short".to_string(), Role::Admin, None, Some(0));
    assert!(store.verify(&secret).is_none());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
