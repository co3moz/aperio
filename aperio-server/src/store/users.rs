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
  fn test_role_ordering_and_parse() {
    assert!(Role::Admin > Role::Operator);
    assert!(Role::Operator > Role::Viewer);
    assert_eq!(Role::parse("ADMIN"), Some(Role::Admin));
    assert_eq!(Role::parse("operator"), Some(Role::Operator));
    assert_eq!(Role::parse("bogus"), None);
  }
}
