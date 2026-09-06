//! Tests for the admin key store: a key is a credential, so what matters
//! here is that it is stored hashed, that its scope is what limits it, and
//! that revoking one takes effect at once.

use super::*;
use crate::store::grants::GrantOrg;

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
      GrantOrg::Child("org-1".to_string()),
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
  assert!(store3.revoke(&rec.id).is_ok());
  assert!(store3.verify(&secret).is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_expired_key_rejected() {
  let dir = temp_dir();
  let mut store = AdminKeyStore::load(&dir);
  let (_, secret) = store
    .create("short".to_string(), Role::Admin, GrantOrg::Master, Some(0))
    .expect("the test store can be written to");
  assert!(store.verify(&secret).is_none());
  let _ = std::fs::remove_dir_all(&dir);
}

/// A revocation that could not be saved must never answer as a key that was
/// not there: the two readings are opposites, and the wrong one is the
/// reassuring one.
#[test]
fn a_revoke_that_cannot_be_saved_is_not_reported_as_a_missing_key() {
  let dir = temp_dir();
  let mut store = AdminKeyStore::load(&dir);
  let (rec, secret) = store
    .create("ci".into(), Role::Admin, GrantOrg::Master, None)
    .unwrap();

  store
    .conn
    .execute("DROP TABLE admin_keys", [])
    .expect("the table exists until this point");

  assert_eq!(
    store.revoke(&rec.id),
    Err(crate::store::NotWritten::NotPersisted),
    "a full disk is a 500, and `NoSuchRecord` would be answered with a 404 reading as \"already revoked\""
  );
  assert!(
    store.verify(&secret).is_some(),
    "the key still authenticates, which is the fact the answer has to carry"
  );
  assert_eq!(
    store.revoke("no-such-id"),
    Err(crate::store::NotWritten::NoSuchRecord),
    "and a genuinely absent key is still its own answer"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

/// A key written before `scope` existed reads as it behaved: a master Admin
/// key was the super-admin and becomes `*`, everything else stays in its
/// organization. Converted once, and written back so it stays converted.
#[test]
fn a_pre_scope_key_reads_as_it_behaved_and_only_the_master_admin_widens() {
  let dir = temp_dir();
  {
    let mut store = AdminKeyStore::load(&dir);
    store
      .create("root".into(), Role::Admin, GrantOrg::Master, None)
      .unwrap();
    store
      .create("watcher".into(), Role::Viewer, GrantOrg::Master, None)
      .unwrap();
    store
      .create(
        "acme".into(),
        Role::Admin,
        GrantOrg::Child("org-1".into()),
        None,
      )
      .unwrap();
  }
  {
    let conn = crate::store::open_db(&dir);
    let rows: Vec<(String, String)> = {
      let mut stmt = conn.prepare("SELECT id, data FROM admin_keys").unwrap();
      stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    for (id, data) in rows {
      let mut v: serde_json::Value = serde_json::from_str(&data).unwrap();
      v.as_object_mut().unwrap().remove("scope");
      conn
        .execute(
          "UPDATE admin_keys SET data = ?1 WHERE id = ?2",
          rusqlite::params![v.to_string(), id],
        )
        .unwrap();
    }
  }
  let scope_of = |store: &AdminKeyStore, name: &str| {
    store
      .list()
      .iter()
      .find(|k| k.name == name)
      .unwrap()
      .scope()
  };
  let mut store = AdminKeyStore::load(&dir);
  assert_eq!(store.take_widened(), vec!["root".to_string()]);
  assert_eq!(scope_of(&store, "root"), GrantOrg::All);
  assert_eq!(scope_of(&store, "watcher"), GrantOrg::Master);
  assert_eq!(scope_of(&store, "acme"), GrantOrg::Child("org-1".into()));
  // Written back: the next start converts nothing and widens nobody.
  let mut again = AdminKeyStore::load(&dir);
  assert!(again.take_widened().is_empty());
  assert_eq!(scope_of(&again, "root"), GrantOrg::All);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_star_key_still_writes_org_id_as_the_master_key_it_would_have_been() {
  let dir = temp_dir();
  let mut store = AdminKeyStore::load(&dir);
  let (rec, _) = store
    .create("root".into(), Role::Admin, GrantOrg::All, None)
    .unwrap();
  assert_eq!(rec.org_id, None);
  assert_eq!(rec.scope(), GrantOrg::All);
  let (rec, _) = store
    .create(
      "acme".into(),
      Role::Operator,
      GrantOrg::Child("org-1".into()),
      None,
    )
    .unwrap();
  assert_eq!(rec.org_id.as_deref(), Some("org-1"));
  let _ = std::fs::remove_dir_all(&dir);
}
