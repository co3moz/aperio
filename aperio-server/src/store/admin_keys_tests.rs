//! Tests for the admin key store: a key is a credential, so what matters
//! here is that it is stored hashed, that its scope is what limits it, and
//! that revoking one takes effect at once.

use super::*;

fn temp_dir() -> String {
  crate::test_support::test_temp_root()
    .join(format!("adminkeys-test-{}", uuid::Uuid::new_v4()))
    .to_string_lossy()
    .to_string()
}

#[test]
fn test_create_verify_revoke_scope_persist() {
  let dir = temp_dir();
  let mut store = AdminKeyStore::load(&dir);
  assert!(store.list().is_empty());

  let (rec, secret) = store
    .create(
      "ci".to_string(),
      Role::Operator,
      Some("org-1".to_string()),
      None,
    )
    .expect("the test store can be written to");
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
  let (_, secret) = store
    .create("short".to_string(), Role::Admin, None, Some(0))
    .expect("the test store can be written to");
  assert!(store.verify(&secret).is_none());
  let _ = std::fs::remove_dir_all(&dir);
}
