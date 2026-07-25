//! Tests for the autoscaling record store: keying, idempotent upserts,
//! ownership, and pruning.

use super::*;

fn temp_dir() -> String {
  let dir = std::env::temp_dir().join(format!("aperio-scaling-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().to_string()
}

fn record(hostname: &str, url: &str) -> ScalingRecord {
  ScalingRecord {
    id: ScalingRecord::key(None, hostname, None),
    org_id: None,
    hostname: hostname.to_string(),
    path: None,
    url: url.to_string(),
    secret: None,
    min: 0,
    max: 4,
    cold_start_secs: DEFAULT_COLD_START_SECS,
    target_utilization: DEFAULT_TARGET_UTILIZATION,
    window_secs: DEFAULT_WINDOW_SECS,
    cooldown_secs: DEFAULT_COOLDOWN_SECS,
    owners: Vec::new(),
    config_hash: String::new(),
    created_at: 0,
    last_seen: 0,
  }
}

#[test]
fn test_key_is_derived_from_the_bind() {
  assert_eq!(
    ScalingRecord::key(None, "app.example.com", None),
    "master|app.example.com|"
  );
  assert_eq!(
    ScalingRecord::key(Some("acme"), "app.example.com", Some("/api")),
    "acme|app.example.com|/api"
  );
  // The master org and an org literally named "master" would collide, but org
  // ids are UUIDs, so the reserved word can never be a child id.
  assert_ne!(
    ScalingRecord::key(None, "a.example.com", None),
    ScalingRecord::key(None, "b.example.com", None)
  );
}

#[test]
fn test_identical_replicas_converge_on_one_record() {
  let dir = temp_dir();
  let mut store = ScalingStore::load(&dir);

  // Eight replicas of the same service, each with its own token, all
  // announcing the same block: one record, eight owners, no flapping.
  for i in 0..8 {
    let outcome = store.upsert(
      record("app.example.com", "https://api.example/scale"),
      Some(&format!("token-{i}")),
      100 + i as u64,
    );
    assert_eq!(
      outcome,
      if i == 0 {
        Upsert::Created
      } else {
        Upsert::Unchanged
      },
      "replica {i}"
    );
  }
  assert_eq!(store.list().len(), 1);
  assert_eq!(store.list()[0].owners.len(), 8);
  // A refresh still moves last_seen, which the TTL sweep reads.
  assert_eq!(store.list()[0].last_seen, 107);
  // ... but not created_at.
  assert_eq!(store.list()[0].created_at, 100);

  // A genuinely different config is an update, not a no-op.
  let mut changed = record("app.example.com", "https://api.example/scale");
  changed.max = 16;
  assert_eq!(store.upsert(changed, Some("token-0"), 200), Upsert::Updated);
  assert_eq!(store.list().len(), 1);
  assert_eq!(store.list()[0].max, 16);
  // The owner set survives a config change.
  assert_eq!(store.list()[0].owners.len(), 8);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_hash_ignores_ownership_and_timestamps() {
  let mut a = record("app.example.com", "https://api.example/scale");
  let mut b = a.clone();
  b.owners = vec!["someone".to_string()];
  b.created_at = 999;
  b.last_seen = 999;
  b.id = "different".to_string();
  assert_eq!(a.compute_hash(), b.compute_hash());

  // Anything behavioral does change it.
  a.secret = Some("s".to_string());
  assert_ne!(a.compute_hash(), b.compute_hash());
}

#[test]
fn test_disown_removes_records_whose_last_owner_is_gone() {
  let dir = temp_dir();
  let mut store = ScalingStore::load(&dir);
  store.upsert(record("a.example.com", "https://x/1"), Some("t1"), 1);
  store.upsert(record("a.example.com", "https://x/1"), Some("t2"), 1);
  store.upsert(record("b.example.com", "https://x/2"), Some("t1"), 1);
  // Armed without an owner (e.g. by an operator, not a client).
  store.upsert(record("c.example.com", "https://x/3"), None, 1);

  // t1 goes away: b (its only owner) is dropped, a survives on t2, and the
  // ownerless record is untouched.
  assert_eq!(store.disown("t1"), 1);
  let hosts: Vec<&str> = store.list().iter().map(|r| r.hostname.as_str()).collect();
  assert_eq!(hosts, vec!["a.example.com", "c.example.com"]);

  assert_eq!(store.disown("t2"), 1);
  assert_eq!(store.list().len(), 1);
  assert_eq!(store.list()[0].hostname, "c.example.com");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_prune_drops_records_nothing_reannounced() {
  let dir = temp_dir();
  let mut store = ScalingStore::load(&dir);
  store.upsert(record("old.example.com", "https://x/1"), None, 1_000);
  store.upsert(record("fresh.example.com", "https://x/2"), None, 9_000);

  assert_eq!(store.prune(3_600, 10_000), 1);
  assert_eq!(store.list().len(), 1);
  assert_eq!(store.list()[0].hostname, "fresh.example.com");

  // Survives a reload.
  let reloaded = ScalingStore::load(&dir);
  assert_eq!(reloaded.list().len(), 1);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_find_and_delete() {
  let dir = temp_dir();
  let mut store = ScalingStore::load(&dir);
  store.upsert(record("app.example.com", "https://x/1"), None, 1);
  assert!(store.find(None, "app.example.com", None).is_some());
  assert!(store.find(None, "other.example.com", None).is_none());
  assert!(store.find(Some("acme"), "app.example.com", None).is_none());

  let id = ScalingRecord::key(None, "app.example.com", None);
  assert!(store.delete(&id));
  assert!(!store.delete(&id));
  assert!(store.list().is_empty());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_cold_start_enabled_requires_min_zero_and_a_budget() {
  let mut r = record("app.example.com", "https://x/1");
  assert!(r.cold_start_enabled());
  r.min = 1;
  assert!(!r.cold_start_enabled());
  r.min = 0;
  r.cold_start_secs = 0;
  assert!(!r.cold_start_enabled());
}
