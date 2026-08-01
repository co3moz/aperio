//! Tests for the user store: roles, password handling, and the organization
//! a user belongs to, which is what every other permission check reads.

use super::*;

fn temp_dir() -> String {
  let dir =
    crate::test_support::test_temp_root().join(format!("users-test-{}", uuid::Uuid::new_v4()));
  dir.to_string_lossy().to_string()
}

#[test]
fn test_create_verify_update_delete_persist() {
  let dir = temp_dir();
  let mut store = UserStore::load(&dir);
  assert!(store.list().is_empty());

  let user = store
    .create("alice", "correct horse battery", Role::Operator, None)
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
      .create("Alice", "another password", Role::Viewer, None)
      .is_err()
  );
  assert!(
    store
      .create("aperio", "another password", Role::Admin, None)
      .is_err()
  );
  assert!(store.create("bob", "short", Role::Viewer, None).is_err());

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
    .create("mfa-user", "long-password", Role::Operator, None)
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
    .create("passkey-user", "long-password", Role::Viewer, None)
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
