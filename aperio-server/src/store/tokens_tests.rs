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

  let (record, secret) = store.create(TokenSpec {
    name: "ci-token".to_string(),
    hostnames: vec!["a.example.com".to_string()],
    paths: vec!["*".to_string()],
    ..Default::default()
  });
  assert!(secret.starts_with("apr_"));
  assert_eq!(store.verify(&secret).unwrap().id, record.id);
  assert!(store.verify("apr_wrong").is_none());

  // Reload from disk → token persisted
  let store2 = TokenStore::load(&dir);
  assert_eq!(store2.list().len(), 1);
  assert_eq!(store2.verify(&secret).unwrap().name, "ci-token");

  // Revoke
  let mut store3 = TokenStore::load(&dir);
  assert!(store3.revoke(&record.id));
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
  let (record, secret) = store.create(TokenSpec {
    name: "ci".to_string(),
    ttl_seconds: Some(3600),
    ..Default::default()
  });
  let first_expiry = record.expires_at.unwrap();

  // Refresh answers with a new expiry >= the original (now + same TTL).
  let refreshed = store.refresh(&secret).expect("refresh should succeed");
  assert!(refreshed.expires_at.unwrap() >= first_expiry);
  assert_eq!(refreshed.ttl_seconds, Some(3600));

  // A wrong secret refreshes nothing.
  assert!(store.refresh("apr_wrong").is_none());

  // A never-expiring token has nothing to refresh.
  let (_, forever) = store.create(TokenSpec {
    name: "forever".to_string(),
    ..Default::default()
  });
  assert!(store.refresh(&forever).is_none());

  // An already-expired token cannot resurrect itself.
  let (_, dead) = store.create(TokenSpec {
    name: "dead".to_string(),
    ttl_seconds: Some(0),
    ..Default::default()
  });
  assert!(store.refresh(&dead).is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_rotate_with_grace_period() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, old_secret) = store.create(TokenSpec {
    name: "rotate-me".to_string(),
    ..Default::default()
  });

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
  assert!(store3.rotate("nope", 60).is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_canary_flag_create_update_persist() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _secret) = store.create(TokenSpec {
    name: "decoy".to_string(),
    canary: true,
    ..Default::default()
  });
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
  let (record, _secret) = store.create(TokenSpec {
    name: "pinned".to_string(),
    ..Default::default()
  });

  // First key pins; the same key matches; a different key is a mismatch.
  assert_eq!(store.pin_key(&record.id, "devA"), Some(PinOutcome::Pinned));
  assert_eq!(store.pin_key(&record.id, "devA"), Some(PinOutcome::Match));
  assert_eq!(
    store.pin_key(&record.id, "devB"),
    Some(PinOutcome::Mismatch)
  );
  // The pin survives a reload.
  let store2 = TokenStore::load(&dir);
  assert_eq!(store2.list()[0].pinned_key.as_deref(), Some("devA"));

  // Rotating the secret clears the pin so a new device can re-pin.
  let mut store3 = TokenStore::load(&dir);
  store3.rotate(&record.id, 0).unwrap();
  assert!(store3.list()[0].pinned_key.is_none());
  assert_eq!(store3.pin_key(&record.id, "devB"), Some(PinOutcome::Pinned));

  // Unknown ids pin nothing.
  assert_eq!(store3.pin_key("nope", "x"), None);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_expired_token_rejected() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (_, secret) = store.create(TokenSpec {
    name: "short".to_string(),
    ttl_seconds: Some(0),
    ..Default::default()
  });
  // ttl 0 → expires_at == now → already expired
  assert!(store.verify(&secret).is_none());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allow_otel_is_off_for_a_token_created_without_it() {
  let dir = temp_dir();
  let mut store = TokenStore::load(&dir);
  let (record, _) = store.create(TokenSpec {
    name: "edge".into(),
    ..Default::default()
  });
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
