use super::*;

fn temp_dir() -> String {
  let dir = std::env::temp_dir().join(format!("aperio-orgs-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().to_string()
}

#[test]
fn test_create_unique_and_reserved() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let a = store.create("Acme").unwrap();
  assert_eq!(store.list().len(), 1);

  // Case-insensitive uniqueness and the reserved name.
  assert!(store.create("acme").is_err());
  assert!(store.create("master").is_err());
  assert!(store.create("  ").is_err());

  // Survives a reload.
  let reloaded = OrgStore::load(&dir);
  assert_eq!(reloaded.list().len(), 1);

  // Delete.
  let mut store = OrgStore::load(&dir);
  assert!(store.delete(&a.id));
  assert!(!store.delete(&a.id));
  assert!(store.list().is_empty());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_quota_and_persist() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store.create("Acme").unwrap();
  assert!(org.max_tokens.is_none());

  // Set two quotas; leave the others untouched.
  let updated = store
    .set_quota(&org.id, Some(Some(3)), Some(Some(10)), None, None)
    .unwrap();
  assert_eq!(updated.max_clients, Some(3));
  assert_eq!(updated.max_tokens, Some(10));
  assert!(updated.max_users.is_none());

  // Survives reload; 0 clears a quota.
  let mut reloaded = OrgStore::load(&dir);
  assert_eq!(reloaded.find(&org.id).unwrap().max_tokens, Some(10));
  let cleared = reloaded
    .set_quota(&org.id, Some(Some(0)), None, None, None)
    .unwrap();
  assert!(cleared.max_clients.is_none());
  assert_eq!(cleared.max_tokens, Some(10));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_quota_all_fields_and_users_bytes() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store.create("Acme").unwrap();

  // Exercise the max_users and max_bytes_month branches too.
  let updated = store
    .set_quota(
      &org.id,
      Some(Some(1)),
      Some(Some(2)),
      Some(Some(3)),
      Some(Some(4096)),
    )
    .unwrap();
  assert_eq!(updated.max_users, Some(3));
  assert_eq!(updated.max_bytes_month, Some(4096));

  // Clearing max_bytes_month with 0 and leaving the rest unchanged.
  let cleared = store
    .set_quota(&org.id, None, None, None, Some(Some(0)))
    .unwrap();
  assert!(cleared.max_bytes_month.is_none());
  assert_eq!(cleared.max_users, Some(3));

  // Persisted across reload.
  let reloaded = OrgStore::load(&dir);
  let got = reloaded.find(&org.id).unwrap();
  assert_eq!(got.max_users, Some(3));
  assert!(got.max_bytes_month.is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_import_replaces_and_persists() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  store.create("Existing").unwrap();

  let now = crate::store::tokens::now_secs();
  let mk = |name: &str| Organization {
    id: uuid::Uuid::new_v4().to_string(),
    name: name.to_string(),
    created_at: now,
    max_clients: None,
    max_tokens: None,
    max_users: None,
    max_bytes_month: None,
    oidc: None,
  };
  let count = store.import(vec![mk("One"), mk("Two"), mk("Three")]);
  assert_eq!(count, 3);
  assert_eq!(store.list().len(), 3);
  // The pre-import org is gone (import replaces wholesale).
  assert!(!store.list().iter().any(|o| o.name == "Existing"));

  // Import result survives a reload.
  let reloaded = OrgStore::load(&dir);
  assert_eq!(reloaded.list().len(), 3);
  assert!(reloaded.list().iter().any(|o| o.name == "Two"));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_oidc_set_and_clear() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store.create("Acme").unwrap();
  assert!(org.oidc.is_none());

  let oidc = OrgOidc {
    issuer: "https://issuer.example".into(),
    client_id: "cid".into(),
    client_secret: "secret".into(),
    allowed_emails: vec!["*@example.com".into()],
  };
  let updated = store.set_oidc(&org.id, Some(oidc)).unwrap();
  assert_eq!(
    updated.oidc.as_ref().unwrap().issuer,
    "https://issuer.example"
  );

  // Persisted across reload.
  let mut reloaded = OrgStore::load(&dir);
  assert_eq!(
    reloaded
      .find(&org.id)
      .unwrap()
      .oidc
      .as_ref()
      .unwrap()
      .client_id,
    "cid"
  );

  // Clearing it removes the override.
  let cleared = reloaded.set_oidc(&org.id, None).unwrap();
  assert!(cleared.oidc.is_none());
  let reloaded = OrgStore::load(&dir);
  assert!(reloaded.find(&org.id).unwrap().oidc.is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_lookups_on_missing_org_are_none() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  assert!(store.find("does-not-exist").is_none());
  assert!(
    store
      .set_quota("does-not-exist", Some(Some(5)), None, None, None)
      .is_none()
  );
  assert!(store.set_oidc("does-not-exist", None).is_none());
  assert!(!store.delete("does-not-exist"));
  let _ = std::fs::remove_dir_all(&dir);
}
