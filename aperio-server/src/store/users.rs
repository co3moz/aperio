use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Dashboard role, ordered by privilege. Every session carries one; the
/// dashboard middleware compares it against the minimum a route requires.
#[derive(
  Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
  /// Read-only: statistics, traffic, audit — every GET.
  Viewer,
  /// Day-to-day operations: clients, tokens, webhooks, maintenance, shares.
  Operator,
  /// Everything, including server settings and user management.
  Admin,
}

impl Role {
  pub fn parse(raw: &str) -> Option<Role> {
    match raw.trim().to_ascii_lowercase().as_str() {
      "viewer" => Some(Role::Viewer),
      "operator" => Some(Role::Operator),
      "admin" => Some(Role::Admin),
      _ => None,
    }
  }

  pub fn as_str(&self) -> &'static str {
    match self {
      Role::Viewer => "viewer",
      Role::Operator => "operator",
      Role::Admin => "admin",
    }
  }
}

/// One registered WebAuthn passkey of a dashboard user.
#[derive(Serialize, Deserialize, Clone)]
pub struct StoredPasskey {
  pub id: String,
  /// User-chosen label ("YubiKey 5", "MacBook Touch ID", ...).
  pub name: String,
  pub created_at: u64,
  /// The `webauthn_rs` Passkey, serialized as JSON (public key + counter —
  /// no secret material; the private key never leaves the authenticator).
  pub credential: String,
  /// The user opted this passkey into usernameless sign-in (discoverable
  /// credential): pressing the passkey button with an empty username may
  /// select it. Off = the passkey works only after typing the username.
  #[serde(default)]
  pub usernameless: bool,
}

/// A dashboard user. The password is stored as an Argon2id PHC string.
#[derive(Serialize, Deserialize, Clone)]
pub struct User {
  pub id: String,
  pub username: String,
  /// Argon2id PHC hash; never exposed through the API.
  pub password_hash: String,
  pub role: Role,
  pub created_at: u64,
  pub enabled: bool,
  /// Base32 TOTP secret; Some = two-factor auth is enabled for this user.
  #[serde(default)]
  pub totp_secret: Option<String>,
  /// Setup-in-progress TOTP secret, promoted to `totp_secret` once the user
  /// proves they enrolled it by entering a valid code.
  #[serde(default)]
  pub totp_pending: Option<String>,
  /// SHA-256 hashes of the unused single-use recovery codes.
  #[serde(default)]
  pub recovery_hashes: Vec<String>,
  /// Registered WebAuthn passkeys (passwordless sign-in).
  #[serde(default)]
  pub passkeys: Vec<StoredPasskey>,
}

/// Persistent store of dashboard users, backed by the `users` table of the
/// shared SQLite store (`<data_dir>/aperio.db`).
pub struct UserStore {
  conn: rusqlite::Connection,
  users: Vec<User>,
}

fn hash_password(password: &str) -> Result<String, String> {
  let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
  Argon2::default()
    .hash_password(password.as_bytes(), &salt)
    .map(|h| h.to_string())
    .map_err(|e| e.to_string())
}

impl UserStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let users: Vec<User> = crate::store::load_all(&conn, "users");
    if !users.is_empty() {
      info!("Loaded {} dashboard user(s) from the store", users.len());
    }
    UserStore { conn, users }
  }

  /// Replaces every user record with the given list (dump import) and
  /// persists. Returns how many records are now stored.
  pub fn import(&mut self, users: Vec<User>) -> usize {
    self.users = users;
    self.persist();
    self.users.len()
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .users
      .iter()
      .filter_map(|u| {
        serde_json::to_string(u)
          .ok()
          .map(|json| (u.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "users", &rows);
  }

  pub fn list(&self) -> &[User] {
    &self.users
  }

  /// Creates a user. Fails when the (case-insensitive) username is taken,
  /// reserved, or the password hash cannot be computed.
  pub fn create(&mut self, username: &str, password: &str, role: Role) -> Result<User, String> {
    let name = username.trim();
    if name.is_empty() {
      return Err("username is required".into());
    }
    // "aperio" is the fixed username of the master/dashboard credentials.
    if name.eq_ignore_ascii_case("aperio") {
      return Err("username 'aperio' is reserved".into());
    }
    if self
      .users
      .iter()
      .any(|u| u.username.eq_ignore_ascii_case(name))
    {
      return Err(format!("username '{}' already exists", name));
    }
    if password.len() < 8 {
      return Err("password must be at least 8 characters".into());
    }
    let user = User {
      id: uuid::Uuid::new_v4().to_string(),
      username: name.to_string(),
      password_hash: hash_password(password)?,
      role,
      created_at: crate::store::tokens::now_secs(),
      enabled: true,
      totp_secret: None,
      totp_pending: None,
      recovery_hashes: Vec::new(),
      passkeys: Vec::new(),
    };
    self.users.push(user.clone());
    self.persist();
    Ok(user)
  }

  /// Updates role/enabled/password in place. `None` keeps the current value.
  pub fn update(
    &mut self,
    id: &str,
    role: Option<Role>,
    enabled: Option<bool>,
    password: Option<&str>,
  ) -> Result<User, String> {
    let user = self
      .users
      .iter_mut()
      .find(|u| u.id == id)
      .ok_or_else(|| "unknown user id".to_string())?;
    if let Some(r) = role {
      user.role = r;
    }
    if let Some(e) = enabled {
      user.enabled = e;
    }
    if let Some(p) = password {
      if p.len() < 8 {
        return Err("password must be at least 8 characters".into());
      }
      user.password_hash = hash_password(p)?;
    }
    let updated = user.clone();
    self.persist();
    Ok(updated)
  }

  /// Removes a user by id. Returns true when one was actually removed.
  pub fn delete(&mut self, id: &str) -> bool {
    let before = self.users.len();
    self.users.retain(|u| u.id != id);
    let removed = self.users.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  /// Verifies a username/password pair against the store. Returns the
  /// matching enabled user, if any. Argon2 verification is constant-time by
  /// construction.
  pub fn verify(&self, username: &str, password: &str) -> Option<&User> {
    let user = self
      .users
      .iter()
      .find(|u| u.enabled && u.username.eq_ignore_ascii_case(username.trim()))?;
    let parsed = PasswordHash::new(&user.password_hash).ok()?;
    Argon2::default()
      .verify_password(password.as_bytes(), &parsed)
      .ok()
      .map(|_| user)
  }

  /// Looks a user up by id.
  pub fn get(&self, id: &str) -> Option<&User> {
    self.users.iter().find(|u| u.id == id)
  }

  /// Looks an enabled user up by (case-insensitive) username.
  pub fn find_by_username(&self, username: &str) -> Option<&User> {
    self
      .users
      .iter()
      .find(|u| u.enabled && u.username.eq_ignore_ascii_case(username.trim()))
  }

  /// Starts TOTP enrollment: stores a fresh pending secret (replacing any
  /// earlier unfinished one) and returns it. Enrollment only takes effect
  /// after [`totp_enable`] verifies a code against it.
  pub fn totp_begin(&mut self, id: &str) -> Result<String, String> {
    let user = self
      .users
      .iter_mut()
      .find(|u| u.id == id)
      .ok_or_else(|| "unknown user id".to_string())?;
    let secret = crate::totp::generate_secret();
    user.totp_pending = Some(secret.clone());
    self.persist();
    Ok(secret)
  }

  /// Completes TOTP enrollment: the code must match the pending secret.
  /// Returns the freshly generated single-use recovery codes (shown once).
  pub fn totp_enable(
    &mut self,
    id: &str,
    code: &str,
    now_secs: u64,
  ) -> Result<Vec<String>, String> {
    let user = self
      .users
      .iter_mut()
      .find(|u| u.id == id)
      .ok_or_else(|| "unknown user id".to_string())?;
    let pending = user
      .totp_pending
      .clone()
      .ok_or_else(|| "no TOTP enrollment in progress".to_string())?;
    if !crate::totp::verify(&pending, code, now_secs) {
      return Err("invalid code".into());
    }
    let (codes, hashes) = crate::totp::generate_recovery_codes(8);
    user.totp_secret = Some(pending);
    user.totp_pending = None;
    user.recovery_hashes = hashes;
    self.persist();
    Ok(codes)
  }

  /// Disables TOTP for a user, clearing the secret and recovery codes.
  pub fn totp_disable(&mut self, id: &str) -> Result<(), String> {
    let user = self
      .users
      .iter_mut()
      .find(|u| u.id == id)
      .ok_or_else(|| "unknown user id".to_string())?;
    user.totp_secret = None;
    user.totp_pending = None;
    user.recovery_hashes = Vec::new();
    self.persist();
    Ok(())
  }

  /// Registers a passkey on a user (capped at 10 per user).
  pub fn add_passkey(
    &mut self,
    id: &str,
    name: &str,
    credential_json: &str,
    usernameless: bool,
  ) -> Result<StoredPasskey, String> {
    let user = self
      .users
      .iter_mut()
      .find(|u| u.id == id)
      .ok_or_else(|| "unknown user id".to_string())?;
    if user.passkeys.len() >= 10 {
      return Err("at most 10 passkeys per user".into());
    }
    let stored = StoredPasskey {
      id: uuid::Uuid::new_v4().to_string(),
      name: name.to_string(),
      created_at: crate::store::tokens::now_secs(),
      credential: credential_json.to_string(),
      usernameless,
    };
    user.passkeys.push(stored.clone());
    self.persist();
    Ok(stored)
  }

  /// Removes a passkey by id; true when one was removed.
  pub fn remove_passkey(&mut self, user_id: &str, passkey_id: &str) -> bool {
    let Some(user) = self.users.iter_mut().find(|u| u.id == user_id) else {
      return false;
    };
    let before = user.passkeys.len();
    user.passkeys.retain(|p| p.id != passkey_id);
    let removed = user.passkeys.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  /// Applies post-authentication credential updates (signature counter) so
  /// webauthn-rs clone detection keeps working across sign-ins.
  pub fn update_passkey_after_auth(
    &mut self,
    user_id: &str,
    result: &webauthn_rs::prelude::AuthenticationResult,
  ) {
    let Some(user) = self.users.iter_mut().find(|u| u.id == user_id) else {
      return;
    };
    let mut changed = false;
    for stored in user.passkeys.iter_mut() {
      if let Ok(mut passkey) =
        serde_json::from_str::<webauthn_rs::prelude::Passkey>(&stored.credential)
        && passkey.update_credential(result) == Some(true)
        && let Ok(json) = serde_json::to_string(&passkey)
      {
        stored.credential = json;
        changed = true;
      }
    }
    if changed {
      self.persist();
    }
  }

  /// Consumes a single-use recovery code: true (and the code is spent) when
  /// it matches an unused one.
  pub fn consume_recovery(&mut self, id: &str, code: &str) -> bool {
    let hash = crate::totp::hash_recovery_code(code);
    let Some(user) = self.users.iter_mut().find(|u| u.id == id) else {
      return false;
    };
    let before = user.recovery_hashes.len();
    user.recovery_hashes.retain(|h| *h != hash);
    let consumed = user.recovery_hashes.len() != before;
    if consumed {
      self.persist();
    }
    consumed
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_dir() -> String {
    let dir = std::env::temp_dir().join(format!("aperio-users-test-{}", uuid::Uuid::new_v4()));
    dir.to_string_lossy().to_string()
  }

  #[test]
  fn test_create_verify_update_delete_persist() {
    let dir = temp_dir();
    let mut store = UserStore::load(&dir);
    assert!(store.list().is_empty());

    let user = store
      .create("alice", "correct horse battery", Role::Operator)
      .unwrap();
    assert_eq!(
      store.verify("alice", "correct horse battery").unwrap().id,
      user.id
    );
    // Case-insensitive username, wrong password rejected.
    assert!(store.verify("ALICE", "correct horse battery").is_some());
    assert!(store.verify("alice", "wrong").is_none());

    // Duplicates, the reserved name, and short passwords are refused.
    assert!(
      store
        .create("Alice", "another password", Role::Viewer)
        .is_err()
    );
    assert!(
      store
        .create("aperio", "another password", Role::Admin)
        .is_err()
    );
    assert!(store.create("bob", "short", Role::Viewer).is_err());

    // Reload from disk → user persisted with its role.
    let store2 = UserStore::load(&dir);
    assert_eq!(store2.list().len(), 1);
    assert_eq!(store2.list()[0].role, Role::Operator);

    // Disable → verify fails; re-enable + password change → new password works.
    let mut store3 = UserStore::load(&dir);
    store3.update(&user.id, None, Some(false), None).unwrap();
    assert!(store3.verify("alice", "correct horse battery").is_none());
    store3
      .update(
        &user.id,
        Some(Role::Admin),
        Some(true),
        Some("new password!"),
      )
      .unwrap();
    assert!(store3.verify("alice", "correct horse battery").is_none());
    let verified = store3.verify("alice", "new password!").unwrap();
    assert_eq!(verified.role, Role::Admin);

    // Delete.
    assert!(store3.delete(&user.id));
    assert!(store3.verify("alice", "new password!").is_none());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_totp_enrollment_lifecycle() {
    let dir = temp_dir();
    let mut store = UserStore::load(&dir);
    let user = store
      .create("mfa-user", "long-password", Role::Operator)
      .unwrap();

    // Setup produces a pending secret; login-relevant totp_secret stays off.
    let secret = store.totp_begin(&user.id).unwrap();
    assert!(store.get(&user.id).unwrap().totp_secret.is_none());

    // A wrong code does not enable; a correct one does and yields recovery codes.
    let now = 1_700_000_000u64;
    assert!(store.totp_enable(&user.id, "000000", now).is_err());
    let decoded = crate::totp::base32_decode(&secret).unwrap();
    let code = format!("{:06}", {
      use hmac::{Hmac, Mac};
      let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&decoded).unwrap();
      mac.update(&(now / 30).to_be_bytes());
      let d = mac.finalize().into_bytes();
      let o = (d[19] & 0x0f) as usize;
      ((u32::from(d[o]) & 0x7f) << 24
        | u32::from(d[o + 1]) << 16
        | u32::from(d[o + 2]) << 8
        | u32::from(d[o + 3]))
        % 1_000_000
    });
    let recovery = store.totp_enable(&user.id, &code, now).unwrap();
    assert_eq!(recovery.len(), 8);
    assert_eq!(
      store.get(&user.id).unwrap().totp_secret.as_deref(),
      Some(secret.as_str())
    );

    // Recovery codes are single-use.
    assert!(store.consume_recovery(&user.id, &recovery[0]));
    assert!(!store.consume_recovery(&user.id, &recovery[0]));
    assert!(!store.consume_recovery(&user.id, "not-a-code"));

    // Enrollment state survives a reload.
    let reloaded = UserStore::load(&dir);
    assert!(reloaded.get(&user.id).unwrap().totp_secret.is_some());
    assert_eq!(reloaded.get(&user.id).unwrap().recovery_hashes.len(), 7);

    // Disable clears everything.
    store.totp_disable(&user.id).unwrap();
    let u = store.get(&user.id).unwrap();
    assert!(u.totp_secret.is_none() && u.recovery_hashes.is_empty());
  }

  #[test]
  fn test_passkey_storage_lifecycle() {
    let dir = temp_dir();
    let mut store = UserStore::load(&dir);
    let user = store
      .create("passkey-user", "long-password", Role::Viewer)
      .unwrap();

    let stored = store
      .add_passkey(&user.id, "YubiKey 5", r#"{"fake":"credential"}"#, false)
      .unwrap();
    assert_eq!(stored.name, "YubiKey 5");
    assert_eq!(store.get(&user.id).unwrap().passkeys.len(), 1);

    // Survives a reload; the credential JSON is stored verbatim.
    let reloaded = UserStore::load(&dir);
    assert_eq!(
      reloaded.get(&user.id).unwrap().passkeys[0].credential,
      r#"{"fake":"credential"}"#
    );

    // Cap: at most 10 per user.
    for i in 0..9 {
      store
        .add_passkey(&user.id, &format!("k{i}"), "{}", false)
        .unwrap();
    }
    assert!(
      store
        .add_passkey(&user.id, "overflow", "{}", false)
        .is_err()
    );

    // Removal by id; unknown ids are a no-op.
    assert!(store.remove_passkey(&user.id, &stored.id));
    assert!(!store.remove_passkey(&user.id, &stored.id));
    assert_eq!(store.get(&user.id).unwrap().passkeys.len(), 9);
  }

  #[test]
  fn test_role_ordering_and_parse() {
    assert!(Role::Admin > Role::Operator);
    assert!(Role::Operator > Role::Viewer);
    assert_eq!(Role::parse("ADMIN"), Some(Role::Admin));
    assert_eq!(Role::parse("operator"), Some(Role::Operator));
    assert_eq!(Role::parse("bogus"), None);
  }
}
