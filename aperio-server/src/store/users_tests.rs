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
  assert!(store3.delete(&user.id).is_ok());
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
  assert!(store.remove_passkey(&user.id, &stored.id).is_ok());
  assert_eq!(
    store.remove_passkey(&user.id, &stored.id).err(),
    Some(UserError::NoSuchUser),
    "a passkey that is not there is not an error about saving"
  );
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

/// A store whose next write will fail, by taking its table away. The real
/// cause is a full disk; see `store/tokens_tests.rs` for why this stands in
/// for it.
fn break_writes(store: &mut UserStore) {
  store
    .conn
    .execute("DROP TABLE users", [])
    .expect("the table exists until this point");
}

/// The six digits a given secret produces for a given moment, the same way the
/// enrollment test above spells it.
fn code_for(secret: &str, now: u64) -> String {
  let decoded = crate::totp::base32_decode(secret).unwrap();
  format!("{:06}", {
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
  })
}

fn a_user(store: &mut UserStore, name: &str) -> User {
  store
    .create(name, "password1", Role::Admin, None)
    .expect("the store works to begin with")
}

/// An account that could not be saved must not be reported as created: the
/// operator would hand out a password for a login that stops existing at the
/// next restart.
#[test]
fn a_user_that_cannot_be_saved_is_refused_rather_than_reported() {
  let mut store = UserStore::load(&temp_dir());
  a_user(&mut store, "alice");

  break_writes(&mut store);
  assert_eq!(
    store.create("bob", "password1", Role::Viewer, None).err(),
    Some(UserError::NotSaved)
  );
  assert_eq!(store.list().len(), 1, "the account was rolled back");
  assert!(store.verify("bob", "password1").is_none());
}

/// **The pre-existing half of this.** `update` applied the role before
/// validating the password, so a rejected password left a role change in
/// memory that was never saved and never undone: the account was an admin
/// until the process restarted, and the API had answered with an error.
#[test]
fn a_rejected_password_does_not_leave_a_role_change_behind() {
  let mut store = UserStore::load(&temp_dir());
  let user = store
    .create("carol", "password1", Role::Viewer, None)
    .expect("created");

  let refused = store.update(&user.id, Some(Role::Admin), None, Some("short"));
  assert!(matches!(refused, Err(UserError::Invalid(_))));
  assert_eq!(
    store.get(&user.id).unwrap().role,
    Role::Viewer,
    "the role is what it was, not what the rejected request asked for"
  );
}

/// A deletion that was not written down did not happen, and the caller has to
/// be told, or the account is gone from the dashboard and back after a restart.
#[test]
fn a_delete_that_cannot_be_saved_keeps_the_account() {
  let mut store = UserStore::load(&temp_dir());
  let user = a_user(&mut store, "dave");

  break_writes(&mut store);
  assert_eq!(store.delete(&user.id).err(), Some(UserError::NotSaved));
  assert_eq!(store.list().len(), 1);
  assert!(store.verify("dave", "password1").is_some());
}

/// The sharp one: a recovery code is single use, and single use is a property
/// of the record. A code the store failed to spend must not sign anyone in,
/// because it still works.
#[test]
fn a_recovery_code_that_cannot_be_spent_is_not_accepted() {
  let mut store = UserStore::load(&temp_dir());
  let user = a_user(&mut store, "erin");
  let secret = store.totp_begin(&user.id).expect("enrollment starts");
  let now = 1_700_000_000u64;
  let codes = store
    .totp_enable(&user.id, &code_for(&secret, now), now)
    .expect("enrollment completes");

  break_writes(&mut store);
  assert!(
    !store.consume_recovery(&user.id, &codes[0]),
    "an unspendable code is refused rather than accepted and forgotten"
  );
  assert_eq!(
    store.get(&user.id).unwrap().recovery_hashes.len(),
    codes.len(),
    "and it is still there, unspent, which is why refusing was right"
  );
}

/// The same argument for the TOTP replay window: accepting a login whose step
/// could not be recorded leaves that code replayable.
#[test]
fn a_totp_step_that_cannot_be_recorded_refuses_the_login() {
  let mut store = UserStore::load(&temp_dir());
  let user = a_user(&mut store, "frank");

  assert!(store.totp_try_advance_step(&user.id, 100));
  break_writes(&mut store);
  assert!(
    !store.totp_try_advance_step(&user.id, 101),
    "a step that cannot be written down is not accepted"
  );
  assert_eq!(
    store.get(&user.id).unwrap().totp_last_step,
    Some(100),
    "and the recorded window is unchanged"
  );
}

// ----- what a previous release wrote -----

/// A password hash written by `argon2` 0.5.3 still verifies.
///
/// This is the check `#134` says has to exist before that upgrade, captured
/// before it rather than after: a fixture generated by the new version only
/// proves the new version agrees with itself. If `argon2` 0.6 changes the PHC
/// string it accepts, or its defaults in a way that stops it parsing what 0.5
/// wrote, the symptom in production is every dashboard user locked out at
/// once, with a correct password and no error that says why.
#[test]
fn a_password_hash_from_the_previous_argon2_still_verifies() {
  let phc = std::fs::read_to_string(crate::store::tests::fixture("password-argon2-0.5.3.phc"))
    .expect("the fixture is checked in");
  let parsed = PasswordHash::new(phc.trim()).expect("a 0.5.3 hash must still parse");
  Argon2::default()
    .verify_password(b"password123", &parsed)
    .expect("the password that made this hash must still verify against it");
  // And the hash is still a hash: the right password is not the only one it
  // accepts.
  assert!(
    Argon2::default()
      .verify_password(b"password124", &parsed)
      .is_err(),
    "a wrong password verified, so this test would pass against anything"
  );
}

/// A passkey credential stored by `webauthn-rs` 0.5.5 still deserializes.
///
/// `StoredPasskey.credential` is the serialized `Passkey`, so its serde shape
/// is a storage format whether or not it was meant to be one. The other half
/// of `#134`: if 0.6 renames a field inside it, every registered passkey stops
/// loading, and the people affected are exactly the ones who took the trouble
/// to set up a second factor.
#[test]
fn a_passkey_stored_by_the_previous_webauthn_rs_still_loads() {
  let json = std::fs::read_to_string(crate::store::tests::fixture(
    "passkey-webauthn-rs-0.5.5.json",
  ))
  .expect("the fixture is checked in");
  let key: webauthn_rs::prelude::Passkey =
    serde_json::from_str(json.trim()).expect("a credential stored by 0.5.5 must still load");
  // Round-trips too, so a change that reads the old shape but writes a new one
  // is caught here rather than on the next upgrade after it.
  let again = serde_json::to_string(&key).expect("serialize");
  let back: webauthn_rs::prelude::Passkey = serde_json::from_str(&again).expect("re-read");
  assert_eq!(
    back.cred_id(),
    key.cred_id(),
    "the credential id did not survive a round trip"
  );
}
