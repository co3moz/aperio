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

  /// Rewrites the admin_keys table. Returns whether the write succeeded.
  fn persist(&mut self) -> bool {
    let rows: Vec<(String, String)> = self
      .keys
      .iter()
      .filter_map(|k| serde_json::to_string(k).ok().map(|j| (k.id.clone(), j)))
      .collect();
    crate::store::replace_all(&mut self.conn, "admin_keys", &rows)
  }

  /// Replaces the stored admin keys with an imported set. The records carry
  /// only hashes, like every other credential in a dump.
  /// Bookkeeping: the dump-restore path, whose caller reports on the whole
  /// import rather than on one row. See `store::replace_all`.
  pub fn import(&mut self, keys: Vec<AdminKey>) -> usize {
    self.keys = keys;
    self.persist();
    self.keys.len()
  }

  /// Creates a new admin key, persists it, and returns the record plus the
  /// plaintext secret (available only at creation time).
  pub fn create(
    &mut self,
    name: String,
    role: Role,
    org_id: Option<String>,
    ttl_seconds: Option<u64>,
  ) -> Option<(AdminKey, String)> {
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
    if !self.persist() {
      // Rolled back for the reason `revoke` below reverts its removal: a key
      // that exists only in memory is handed out, used, and then gone at the
      // next restart, with nothing having said so.
      self.keys.pop();
      return None;
    }
    Some((record, secret))
  }

  /// Revokes an admin key by id. `Ok` only when it was removed *and* durably
  /// persisted; on a write failure the removal is reverted, so a revoked key
  /// cannot silently reappear on restart.
  ///
  /// The two failures are separate answers because of what the caller does
  /// with them, and this is the mutation where it matters most: a revocation
  /// reported as "no such key" reads as "already gone", so an operator pulling
  /// a compromised credential on a full disk was told the job was done by the
  /// same value that meant it had failed, and the key went on authenticating.
  pub fn revoke(&mut self, id: &str) -> Result<(), crate::store::NotWritten> {
    let Some(pos) = self.keys.iter().position(|k| k.id == id) else {
      return Err(crate::store::NotWritten::NoSuchRecord);
    };
    let removed = self.keys.remove(pos);
    if self.persist() {
      Ok(())
    } else {
      self.keys.insert(pos, removed);
      Err(crate::store::NotWritten::NotPersisted)
    }
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
#[path = "admin_keys_tests.rs"]
mod tests;
