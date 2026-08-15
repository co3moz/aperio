//! Tests for the token store, which is the grant table: what a token binds,
//! when it expires, and how a use is recorded.

use super::*;
use crate::store::tokens::{TokenPatch, TokenSpec};

fn temp_dir() -> String {
  let dir =
    crate::test_support::test_temp_root().join(format!("tokens-test-{}", uuid::Uuid::new_v4()));
  dir.to_string_lossy().to_string()
}

#[test]
fn test_create_verify_revoke_persist() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  assert!(store.list().is_empty());

  let (record, secret) = store
    .create(TokenSpec {
      name: "ci-token".to_string(),
      hostnames: vec!["a.example.com".to_string()],
      paths: vec!["*".to_string()],
      ..Default::default()
    })
    .expect("the test store can be written to");
  assert!(secret.starts_with("apr_"));
  assert_eq!(store.verify(&secret).unwrap().id, record.id);
  assert!(store.verify("apr_wrong").is_none());

  // Reload from disk → token persisted
  let store2 = TokenStore::load(&dir);
  assert_eq!(store2.list().len(), 1);
  assert_eq!(store2.verify(&secret).unwrap().name, "ci-token");

  // Revoke
  let mut store3 = TokenStore::load(&dir);
  assert!(store3.revoke(&record.id).is_ok());
  assert!(store3.verify(&secret).is_none());
  let store4 = TokenStore::load(&dir);
  assert!(store4.list().is_empty());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_corrupt_db_is_backed_up_not_discarded() {
  let dir = temp_dir();
  std::fs::create_dir_all(&dir).unwrap();
  let path = std::path::PathBuf::from(&dir).join("aperio.db");
  std::fs::write(&path, "this is not a sqlite database at all").unwrap();

  // Loading a corrupt store starts empty but preserves the bad file.
  let store = TokenStore::load(&dir);
  assert!(store.list().is_empty());

  // The original file was renamed aside as aperio.db.corrupt.<epoch>.
  let backups: Vec<_> = std::fs::read_dir(&dir)
    .unwrap()
    .filter_map(|e| e.ok())
    .filter(|e| {
      e.file_name()
        .to_string_lossy()
        .starts_with("aperio.db.corrupt.")
    })
    .collect();
  assert_eq!(backups.len(), 1, "corrupt file should be preserved");

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_refresh_slides_expiry_by_creation_ttl() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, secret) = store
    .create(TokenSpec {
      name: "ci".to_string(),
      ttl_seconds: Some(3600),
      ..Default::default()
    })
    .expect("the test store can be written to");
  let first_expiry = record.expires_at.unwrap();

  // Refresh answers with a new expiry >= the original (now + same TTL).
  let refreshed = store.refresh(&secret).expect("refresh should succeed");
  assert!(refreshed.expires_at.unwrap() >= first_expiry);
  assert_eq!(refreshed.ttl_seconds, Some(3600));

  // A wrong secret refreshes nothing.
  assert_eq!(
    store.refresh("apr_wrong").err(),
    Some(NotWritten::NoSuchRecord)
  );

  // A never-expiring token has nothing to refresh.
  let (_, forever) = store
    .create(TokenSpec {
      name: "forever".to_string(),
      ..Default::default()
    })
    .expect("the test store can be written to");
  assert_eq!(
    store.refresh(&forever).err(),
    Some(NotWritten::NoSuchRecord)
  );

  // An already-expired token cannot resurrect itself.
  let (_, dead) = store
    .create(TokenSpec {
      name: "dead".to_string(),
      ttl_seconds: Some(0),
      ..Default::default()
    })
    .expect("the test store can be written to");
  assert_eq!(store.refresh(&dead).err(), Some(NotWritten::NoSuchRecord));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_rotate_with_grace_period() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, old_secret) = store
    .create(TokenSpec {
      name: "rotate-me".to_string(),
      ..Default::default()
    })
    .expect("the test store can be written to");

  // Rotation with a grace window: both secrets verify to the same record.
  let (rotated, new_secret) = store.rotate(&record.id, 3600).expect("rotate");
  assert_ne!(new_secret, old_secret);
  assert!(rotated.prev_expires_at.is_some());
  assert_eq!(store.verify(&new_secret).unwrap().id, record.id);
  assert_eq!(store.verify(&old_secret).unwrap().id, record.id);

  // The rotation survives a reload.
  let store2 = TokenStore::load(&dir);
  assert!(store2.verify(&old_secret).is_some());
  assert!(store2.verify(&new_secret).is_some());

  // A second rotation with grace 0 cuts the old secrets off immediately.
  let mut store3 = TokenStore::load(&dir);
  let (_, newest) = store3.rotate(&record.id, 0).expect("rotate");
  assert!(store3.verify(&newest).is_some());
  assert!(store3.verify(&new_secret).is_none());
  assert!(store3.verify(&old_secret).is_none());

  // Unknown ids rotate nothing.
  assert_eq!(
    store3.rotate("nope", 60).err(),
    Some(NotWritten::NoSuchRecord)
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_canary_flag_create_update_persist() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _secret) = store
    .create(TokenSpec {
      name: "decoy".to_string(),
      canary: true,
      ..Default::default()
    })
    .expect("the test store can be written to");
  assert!(record.canary);

  // Survives reload.
  let store2 = TokenStore::load(&dir);
  assert!(store2.list()[0].canary);

  // Can be toggled off in place.
  let mut store3 = TokenStore::load(&dir);
  let updated = store3
    .update(
      &record.id,
      TokenPatch {
        name: None,
        canary: Some(false),
        ..Default::default()
      },
    )
    .unwrap();
  assert!(!updated.canary);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_pin_key_tofu_and_clear_on_rotate() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _secret) = store
    .create(TokenSpec {
      name: "pinned".to_string(),
      ..Default::default()
    })
    .expect("the test store can be written to");

  // First key pins; the same key matches; a different key is a mismatch.
  assert_eq!(store.pin_key(&record.id, "devA"), Ok(PinOutcome::Pinned));
  assert_eq!(store.pin_key(&record.id, "devA"), Ok(PinOutcome::Match));
  assert_eq!(store.pin_key(&record.id, "devB"), Ok(PinOutcome::Mismatch));
  // The pin survives a reload.
  let store2 = TokenStore::load(&dir);
  assert_eq!(store2.list()[0].pinned_key.as_deref(), Some("devA"));

  // Rotating the secret clears the pin so a new device can re-pin.
  let mut store3 = TokenStore::load(&dir);
  store3.rotate(&record.id, 0).unwrap();
  assert!(store3.list()[0].pinned_key.is_none());
  assert_eq!(store3.pin_key(&record.id, "devB"), Ok(PinOutcome::Pinned));

  // Unknown ids pin nothing.
  assert_eq!(store3.pin_key("nope", "x"), Err(NotWritten::NoSuchRecord));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_expired_token_rejected() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (_, secret) = store
    .create(TokenSpec {
      name: "short".to_string(),
      ttl_seconds: Some(0),
      ..Default::default()
    })
    .expect("the test store can be written to");
  // ttl 0 → expires_at == now → already expired
  assert!(store.verify(&secret).is_none());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allow_otel_is_off_for_a_token_created_without_it() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _) = store
    .create(TokenSpec {
      name: "edge".into(),
      ..Default::default()
    })
    .expect("the test store can be written to");
  assert!(!record.allow_otel);

  // And it is editable on its own, without disturbing the rest.
  let updated = store
    .update(
      &record.id,
      TokenPatch {
        name: None,
        allow_otel: Some(true),
        ..Default::default()
      },
    )
    .expect("the token exists");
  assert!(updated.allow_otel);
  assert!(!updated.allow_bind, "the other capabilities are untouched");
  assert!(!updated.allow_public);
}

#[test]
fn a_token_record_written_before_allow_otel_existed_loads_without_it() {
  // `#[serde(default)]`: an upgrade must not fail to read the store, and the
  // capability must not appear switched on for tokens that predate it.
  let json = r#"{
    "id": "t1", "name": "old", "token_hash": "h", "token_prefix": "apr_x",
    "hostnames": [], "paths": [], "allowed_ips": [], "created_at": 0,
    "expires_at": null, "ttl_seconds": null, "max_rps": null,
    "daily_max_bytes": null, "allow_public": false, "canary": false,
    "org_id": null
  }"#;
  let token: ApiToken = serde_json::from_str(json).expect("an old record still loads");
  assert!(!token.allow_otel);
}

/// A store whose next write will fail, by taking its table away.
///
/// The real cause is a full disk, which `tests/e2e/specs/chaos/disk.test.ts`
/// exercises against a filesystem it genuinely fills. Here the question is
/// narrower and deserves a narrower instrument: given that the write fails,
/// what does the store return and what does it leave behind. Dropping the
/// table makes that failure happen on demand, in a millisecond, on any
/// platform.
fn break_writes(store: &mut TokenStore) {
  store
    .conn
    .execute("DROP TABLE tokens", [])
    .expect("the table exists until this point");
}

/// **The bug this file's `commit` exists for.** A create whose write failed
/// used to answer with a token: it was in memory, it was not on disk, and it
/// stopped existing at the next restart while its holder had been handed a
/// secret for it.
#[test]
fn a_create_that_cannot_be_saved_is_refused_rather_than_reported() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  store
    .create(TokenSpec {
      name: "first".into(),
      ..Default::default()
    })
    .expect("the store works to begin with");
  assert_eq!(store.list().len(), 1);

  break_writes(&mut store);
  let refused = store.create(TokenSpec {
    name: "second".into(),
    ..Default::default()
  });

  assert_eq!(refused.err(), Some(NotWritten::NotPersisted));
  assert_eq!(
    store.list().len(),
    1,
    "the record is rolled back, so memory agrees with the disk"
  );
  assert!(
    !store.list().iter().any(|t| t.name == "second"),
    "and specifically the one that could not be saved is gone"
  );
}

/// The same for an edit, where the rollback has to restore a value rather than
/// remove a row: a half-applied change that only exists in memory is the worst
/// of the three possible outcomes.
#[test]
fn an_update_that_cannot_be_saved_leaves_the_record_as_it_was() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _) = store
    .create(TokenSpec {
      name: "before".into(),
      hostnames: vec!["a.test".into()],
      ..Default::default()
    })
    .expect("the store works to begin with");

  break_writes(&mut store);
  let refused = store.update(
    &record.id,
    TokenPatch {
      name: Some("after".into()),
      hostnames: Some(vec!["b.test".into()]),
      ..Default::default()
    },
  );

  assert_eq!(refused.err(), Some(NotWritten::NotPersisted));
  let held = &store.list()[0];
  assert_eq!(held.name, "before", "the name is what it was");
  assert_eq!(held.hostnames, vec!["a.test".to_string()], "and the binds");
}

/// The two failures are told apart, which they were not before: `revoke`
/// answered one `false` for "no such token" and for "the disk is full", so a
/// caller could not tell a 404 from a 500.
#[test]
fn a_missing_token_and_an_unwritable_store_are_different_answers() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _) = store
    .create(TokenSpec {
      name: "doomed".into(),
      ..Default::default()
    })
    .expect("the store works to begin with");

  assert_eq!(
    store.revoke("no-such-id").err(),
    Some(NotWritten::NoSuchRecord)
  );

  break_writes(&mut store);
  assert_eq!(
    store.revoke(&record.id).err(),
    Some(NotWritten::NotPersisted)
  );
  assert_eq!(
    store.list().len(),
    1,
    "a revocation that was not written down did not happen, so the token still works"
  );
}
