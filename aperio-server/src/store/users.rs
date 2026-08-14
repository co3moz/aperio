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
  /// Read-only: statistics, traffic, audit, every GET.
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
  /// The `webauthn_rs` Passkey, serialized as JSON (public key + counter,
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
  /// Organization this user belongs to; `None` = the master organization.
  #[serde(default)]
  pub org_id: Option<String>,
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
  /// Highest TOTP step counter already accepted for this user; a login code
  /// must match a strictly newer step, so a code observed in transit cannot be
  /// replayed within its ~90s validity window.
  #[serde(default)]
  pub totp_last_step: Option<i64>,
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

/// Why a change to a user did not happen.
///
/// Three cases, kept apart, because they are three different answers: the
/// request was wrong, there is no such user, or the change was undone because
/// it could not be saved. The last one used to be invisible, `create` and the
/// rest ignored what `persist` returned, so an account created on a full disk
/// existed until the process restarted and the operator was told it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserError {
  /// The request itself: a short password, a duplicate name, a reserved one.
  Invalid(String),
  /// No user matched. Nothing was attempted.
  NoSuchUser,
  /// The change was made and then undone, because it could not be saved.
  /// Memory matches disk, and the caller must report a failure.
  NotSaved,
}

impl std::fmt::Display for UserError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      UserError::Invalid(m) => write!(f, "{m}"),
      UserError::NoSuchUser => write!(f, "unknown user id"),
      UserError::NotSaved => write!(
        f,
        "the change could not be saved to the store and was rolled back"
      ),
    }
  }
}

impl UserStore {
  /// Runs `change`, saves, and puts the users back if **either** step failed.
  ///
  /// Both halves matter here, and the second is its own pre-existing bug:
  /// `update` applied the role and the enabled flag before validating the
  /// password, so a rejected password left a role change sitting in memory
  /// that was never saved and never undone. Rolling back on an `Err` from the
  /// change itself makes that impossible to write again.
  fn commit<R>(
    &mut self,
    change: impl FnOnce(&mut Self) -> Result<R, UserError>,
  ) -> Result<R, UserError> {
    let snapshot = self.users.clone();
    match change(self) {
      Ok(out) if self.persist() => Ok(out),
      Ok(_) => {
        self.users = snapshot;
        Err(UserError::NotSaved)
      }
      Err(e) => {
        self.users = snapshot;
        Err(e)
      }
    }
  }

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
  /// Bookkeeping: the dump-restore path, whose caller reports on the whole
  /// import rather than on one row. See `store::replace_all`.
  pub fn import(&mut self, users: Vec<User>) -> usize {
    self.users = users;
    self.persist();
    self.users.len()
  }

  fn persist(&mut self) -> bool {
    let rows: Vec<(String, String)> = self
      .users
      .iter()
      .filter_map(|u| {
        serde_json::to_string(u)
          .ok()
          .map(|json| (u.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "users", &rows)
  }

  pub fn list(&self) -> &[User] {
    &self.users
  }

  /// Creates a user. Fails when the (case-insensitive) username is taken,
  /// reserved, or the password hash cannot be computed.
  pub fn create(
    &mut self,
    username: &str,
    password: &str,
    role: Role,
    org_id: Option<String>,
  ) -> Result<User, UserError> {
    let name = username.trim();
    if name.is_empty() {
      return Err(UserError::Invalid("username is required".into()));
    }
    // "aperio" is the fixed username of the master/dashboard credentials.
    if name.eq_ignore_ascii_case("aperio") {
      return Err(UserError::Invalid("username 'aperio' is reserved".into()));
    }
    if self
      .users
      .iter()
      .any(|u| u.username.eq_ignore_ascii_case(name))
    {
      return Err(UserError::Invalid(format!(
        "username '{}' already exists",
        name
      )));
    }
    if password.len() < 8 {
      return Err(UserError::Invalid(
        "password must be at least 8 characters".into(),
      ));
    }
    let user = User {
      id: uuid::Uuid::new_v4().to_string(),
      username: name.to_string(),
      password_hash: hash_password(password).map_err(UserError::Invalid)?,
      role,
      org_id,
      created_at: crate::store::tokens::now_secs(),
      enabled: true,
      totp_secret: None,
      totp_pending: None,
      recovery_hashes: Vec::new(),
      totp_last_step: None,
      passkeys: Vec::new(),
    };
    self.commit(|store| {
      store.users.push(user.clone());
      Ok(user)
    })
  }

  /// Updates role/enabled/password in place. `None` keeps the current value.
  pub fn update(
    &mut self,
    id: &str,
    role: Option<Role>,
    enabled: Option<bool>,
    password: Option<&str>,
  ) -> Result<User, UserError> {
    self.commit(|store| {
      let user = store
        .users
        .iter_mut()
        .find(|u| u.id == id)
        .ok_or(UserError::NoSuchUser)?;
      if let Some(r) = role {
        user.role = r;
      }
      if let Some(e) = enabled {
        user.enabled = e;
      }
      if let Some(p) = password {
        if p.len() < 8 {
          return Err(UserError::Invalid(
            "password must be at least 8 characters".into(),
          ));
        }
        user.password_hash = hash_password(p).map_err(UserError::Invalid)?;
      }
      Ok(user.clone())
    })
  }

  /// Removes a user by id.
  pub fn delete(&mut self, id: &str) -> Result<(), UserError> {
    if !self.users.iter().any(|u| u.id == id) {
      return Err(UserError::NoSuchUser);
    }
    self.commit(|store| {
      store.users.retain(|u| u.id != id);
      Ok(())
    })
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

  /// True when a user row with this (case-insensitive) username exists and is
  /// disabled. Used to strip authority from a disabled account's live sessions;
  /// a username with no row at all is an OIDC identity, which is not affected.
  pub fn is_disabled_username(&self, username: &str) -> bool {
    self
      .users
      .iter()
      .any(|u| !u.enabled && u.username.eq_ignore_ascii_case(username.trim()))
  }

  /// Starts TOTP enrollment: stores a fresh pending secret (replacing any
  /// earlier unfinished one) and returns it. Enrollment only takes effect
  /// after [`totp_enable`] verifies a code against it.
  pub fn totp_begin(&mut self, id: &str) -> Result<String, UserError> {
    self.commit(|store| {
      let user = store
        .users
        .iter_mut()
        .find(|u| u.id == id)
        .ok_or(UserError::NoSuchUser)?;
      let secret = crate::totp::generate_secret();
      user.totp_pending = Some(secret.clone());
      Ok(secret)
    })
  }

  /// Completes TOTP enrollment: the code must match the pending secret.
  /// Returns the freshly generated single-use recovery codes (shown once).
  pub fn totp_enable(
    &mut self,
    id: &str,
    code: &str,
    now_secs: u64,
  ) -> Result<Vec<String>, UserError> {
    self.commit(|store| {
      let user = store
        .users
        .iter_mut()
        .find(|u| u.id == id)
        .ok_or(UserError::NoSuchUser)?;
      let pending = user
        .totp_pending
        .clone()
        .ok_or_else(|| UserError::Invalid("no TOTP enrollment in progress".into()))?;
      if !crate::totp::verify(&pending, code, now_secs) {
        return Err(UserError::Invalid("invalid code".into()));
      }
      let (codes, hashes) = crate::totp::generate_recovery_codes(8);
      user.totp_secret = Some(pending);
      user.totp_pending = None;
      user.recovery_hashes = hashes;
      // The replay window is seeded by the first real login, not enrollment:
      // seeding here would reject a legitimate login made within the same 30s
      // step as enrollment (a very common flow).
      Ok(codes)
    })
  }

  /// Records a freshly accepted TOTP login step for replay prevention. Returns
  /// true (and persists) when `step` is strictly newer than the last accepted
  /// one; false when the same or an older step was already used, the caller
  /// treats that as an invalid code so an intercepted code can't be replayed.
  ///
  /// **False when the step could not be written down, too**, which refuses a
  /// login that would otherwise have succeeded. That is the right way round:
  /// this record is the only thing standing between an intercepted code and
  /// its reuse, so accepting the login while failing to remember the step
  /// leaves the code replayable for as long as the store stays unwritable, and
  /// past a restart. A refused login is visible and recoverable; a silent
  /// replay window is neither.
  pub fn totp_try_advance_step(&mut self, id: &str, step: i64) -> bool {
    let Some(user) = self.users.iter().find(|u| u.id == id) else {
      return false;
    };
    if user.totp_last_step.is_some_and(|last| step <= last) {
      return false;
    }
    self
      .commit(|store| {
        if let Some(user) = store.users.iter_mut().find(|u| u.id == id) {
          user.totp_last_step = Some(step);
        }
        Ok(())
      })
      .is_ok()
  }

  /// Disables TOTP for a user, clearing the secret and recovery codes.
  pub fn totp_disable(&mut self, id: &str) -> Result<(), UserError> {
    self.commit(|store| {
      let user = store
        .users
        .iter_mut()
        .find(|u| u.id == id)
        .ok_or(UserError::NoSuchUser)?;
      user.totp_secret = None;
      user.totp_pending = None;
      user.recovery_hashes = Vec::new();
      Ok(())
    })
  }

  /// Registers a passkey on a user (capped at 10 per user).
  pub fn add_passkey(
    &mut self,
    id: &str,
    name: &str,
    credential_json: &str,
    usernameless: bool,
  ) -> Result<StoredPasskey, UserError> {
    self.commit(|store| {
      let user = store
        .users
        .iter_mut()
        .find(|u| u.id == id)
        .ok_or(UserError::NoSuchUser)?;
      if user.passkeys.len() >= 10 {
        return Err(UserError::Invalid("at most 10 passkeys per user".into()));
      }
      let stored = StoredPasskey {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        created_at: crate::store::tokens::now_secs(),
        credential: credential_json.to_string(),
        usernameless,
      };
      user.passkeys.push(stored.clone());
      Ok(stored)
    })
  }

  /// Removes a passkey by id.
  pub fn remove_passkey(&mut self, user_id: &str, passkey_id: &str) -> Result<(), UserError> {
    let present = self
      .users
      .iter()
      .find(|u| u.id == user_id)
      .is_some_and(|u| u.passkeys.iter().any(|p| p.id == passkey_id));
    if !present {
      return Err(UserError::NoSuchUser);
    }
    self.commit(|store| {
      if let Some(user) = store.users.iter_mut().find(|u| u.id == user_id) {
        user.passkeys.retain(|p| p.id != passkey_id);
      }
      Ok(())
    })
  }

  /// Applies post-authentication credential updates (signature counter) so
  /// webauthn-rs clone detection keeps working across sign-ins.
  pub fn update_passkey_after_auth(
    &mut self,
    user_id: &str,
    result: &webauthn_rs::prelude::AuthenticationResult,
  ) {
    let snapshot = self.users.clone();
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
    // Bookkeeping, not a decision: the authentication has already succeeded,
    // and this is the clone-detection counter catching up. So a failed write
    // is rolled back to keep memory and disk agreeing, and is *not* turned
    // into a refused login: counters only have to be monotonic, and the
    // authenticator's own will still be ahead next time. `replace_all` has
    // already logged the failure.
    if changed && !self.persist() {
      self.users = snapshot;
    }
  }

  /// Consumes a single-use recovery code: true (and the code is spent) when
  /// it matches an unused one.
  ///
  /// **A code that could not be marked as spent is not accepted**, for the
  /// same reason as `totp_try_advance_step`: single use is a property of the
  /// record, not of the check. Signing someone in on a code the store failed
  /// to spend hands them a code that still works, which is the one thing a
  /// recovery code must not be.
  pub fn consume_recovery(&mut self, id: &str, code: &str) -> bool {
    let hash = crate::totp::hash_recovery_code(code);
    let Some(user) = self.users.iter().find(|u| u.id == id) else {
      return false;
    };
    if !user.recovery_hashes.contains(&hash) {
      return false;
    }
    self
      .commit(|store| {
        if let Some(user) = store.users.iter_mut().find(|u| u.id == id) {
          user.recovery_hashes.retain(|h| *h != hash);
        }
        Ok(())
      })
      .is_ok()
  }
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
