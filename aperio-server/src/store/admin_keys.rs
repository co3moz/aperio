//! Programmatic admin API keys: least-privilege, non-cookie credentials for
//! automation (CI, Terraform, Slack) that call the dashboard API.
//!
//! An admin key authenticates a caller with a fixed **role** (viewer /
//! operator / admin) and a fixed **organization**, presented as
//! `Authorization: Bearer <key>`. Unlike the master token it is scoped and
//! revocable, so automation never needs the all-powerful master credential.
//! Only the SHA-256 hash of the secret is stored; the secret is shown once.

use serde::{Deserialize, Serialize};

use crate::store::grants::GrantOrg;
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
  /// The child organization this key acts within; `None` = master, or every
  /// organization when `scope` says so. Kept as the older binaries wrote it;
  /// `scope()` is what to read.
  #[serde(default)]
  pub org_id: Option<String>,
  /// Where the key acts: one child, master, or `*` (`planned_features.md`
  /// #153). Absent only on a row written before the field existed, which
  /// `load` converts: a master Admin key could reach every organization and
  /// keeps doing so as `*`; any other key meant its own organization.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schema(value_type = Option<String>)]
  pub scope: Option<GrantOrg>,
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

  /// Where this key acts. Rows from before the field existed are read the
  /// way they behaved: a master Admin was the super-admin, everything else
  /// stayed in its organization.
  pub fn scope(&self) -> GrantOrg {
    match &self.scope {
      Some(scope) => scope.clone(),
      None => Self::legacy_scope(self.role, self.org_id.as_deref()),
    }
  }

  fn legacy_scope(role: Role, org_id: Option<&str>) -> GrantOrg {
    match (role, org_id) {
      (Role::Admin, None) => GrantOrg::All,
      (_, org) => GrantOrg::from_org_id(org),
    }
  }
}

/// Persistent store for programmatic admin API keys, backed by the
/// `admin_keys` table of the shared SQLite store.
pub struct AdminKeyStore {
  conn: rusqlite::Connection,
  keys: Vec<AdminKey>,
  /// Names of the keys `load` read as `*` from a pre-scope row, for the
  /// start-up audit event. Taken once by `take_widened`.
  widened_on_load: Vec<String>,
}

impl AdminKeyStore {
  /// Opens the shared store and loads all admin-key records.
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let mut keys: Vec<AdminKey> = crate::store::load_all(&conn, "admin_keys");
    if !keys.is_empty() {
      tracing::info!(
        "Loaded {} programmatic admin key(s) from the store",
        keys.len()
      );
    }
    let converted = keys.iter().filter(|k| k.scope.is_none()).count();
    let widened = Self::convert_legacy_rows(&mut keys);
    let mut store = AdminKeyStore {
      conn,
      keys,
      widened_on_load: widened,
    };
    // Written back at once, so the conversion happens exactly once: a row
    // that carries a scope is never converted again.
    if converted > 0 {
      for name in &store.widened_on_load {
        tracing::warn!(
          "Admin key '{}' was an Admin key of the master organization and now acts in every \
           organization (*), which is what it could already do; revoke and re-create it narrower \
           if it should not",
          name
        );
      }
      store.persist();
    }
    store
  }

  /// Gives every row without a scope the scope it behaved as, and returns the
  /// names of those that came out as `*`. On load and on import alike.
  fn convert_legacy_rows(keys: &mut [AdminKey]) -> Vec<String> {
    let mut widened = Vec::new();
    for k in keys.iter_mut().filter(|k| k.scope.is_none()) {
      let scope = AdminKey::legacy_scope(k.role, k.org_id.as_deref());
      if scope == GrantOrg::All {
        widened.push(k.name.clone());
      }
      k.scope = Some(scope);
    }
    widened
  }

  /// The names the last `load` widened to `*`, once.
  pub fn take_widened(&mut self) -> Vec<String> {
    std::mem::take(&mut self.widened_on_load)
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
  pub fn import(&mut self, mut keys: Vec<AdminKey>) -> usize {
    Self::convert_legacy_rows(&mut keys);
    self.keys = keys;
    self.persist();
    self.keys.len()
  }

  /// Creates a new admin key, persists it, and returns the record plus the
  /// plaintext secret (available only at creation time). `scope` is where it
  /// acts; `org_id` is written alongside for the binaries that read only
  /// that, and a `*` key reads there as the master key it would have been.
  pub fn create(
    &mut self,
    name: String,
    role: Role,
    scope: GrantOrg,
    ttl_seconds: Option<u64>,
  ) -> Option<(AdminKey, String)> {
    let secret = format!(
      "apk_{}{}",
      uuid::Uuid::new_v4().simple(),
      uuid::Uuid::new_v4().simple()
    );
    let org_id = match &scope {
      GrantOrg::Child(id) => Some(id.clone()),
      GrantOrg::Master | GrantOrg::All => None,
    };
    let record = AdminKey {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      key_hash: hash_token(&secret),
      key_prefix: secret.chars().take(12).collect(),
      role,
      org_id,
      scope: Some(scope),
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
