//! Tests for the uptime store, where the interesting cases are the edges of
//! a window: an outage that spans two days, and a service that has never
//! been seen at all.

use super::*;

fn live(entries: &[(&str, Availability)]) -> HashMap<String, (Availability, Option<String>)> {
  entries
    .iter()
    .map(|(k, s)| (k.to_string(), (*s, None)))
    .collect()
}

#[test]
fn test_tick_accrues_by_previous_status() {
  let dir =
    crate::test_support::test_temp_root().join(format!("uptime-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();

  let mut store = UptimeStore::load(&dir_str);
  let t0 = 1_700_000_000;
  store.tick(t0, live(&[("web", Availability::Up)]));
  // 60 s later still up: 60 s of uptime accrued.
  store.tick(t0 + 60, live(&[("web", Availability::Up)]));
  // 30 s later the entity is gone: those 30 s still count as up (previous
  // status), and the entity is now marked down.
  store.tick(t0 + 90, live(&[]));
  // 10 more seconds accrue as down.
  store.tick(t0 + 100, live(&[]));

  let snap = store.snapshot();
  let web = snap.get("web").expect("entity tracked");
  assert_eq!(web.status, Availability::Down);
  let total: DayAvailability = web
    .days
    .values()
    .fold(DayAvailability::default(), |mut acc, d| {
      acc.up_secs += d.up_secs;
      acc.degraded_secs += d.degraded_secs;
      acc.down_secs += d.down_secs;
      acc
    });
  assert_eq!(total.up_secs, 90);
  assert_eq!(total.down_secs, 10);
  assert_eq!(total.observed_secs(), 100);

  // Persistence round-trip.
  store.save_if_dirty();
  let reloaded = UptimeStore::load(&dir_str);
  assert_eq!(
    reloaded.snapshot().get("web").unwrap().status,
    Availability::Down
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_accrue_splits_across_midnight() {
  use chrono::TimeZone;
  // 23:59:30 local on an arbitrary day.
  let base = chrono::Local
    .with_ymd_and_hms(2026, 3, 10, 23, 59, 30)
    .single()
    .unwrap()
    .timestamp() as u64;
  let mut days = HashMap::new();
  accrue_days(&mut days, base, base + 60, Availability::Up);
  assert_eq!(days.len(), 2, "span must split across midnight");
  assert_eq!(days.get("2026-03-10").unwrap().up_secs, 30);
  assert_eq!(days.get("2026-03-11").unwrap().up_secs, 30);
}

#[test]
fn test_degraded_and_prune() {
  let dir =
    crate::test_support::test_temp_root().join(format!("uptime-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();

  let mut store = UptimeStore::load(&dir_str);
  let t0 = 1_700_000_000;
  store.tick(t0, live(&[("db", Availability::Degraded)]));
  store.tick(t0 + 10, live(&[("db", Availability::Degraded)]));
  let snap = store.snapshot();
  let total: u64 = snap
    .get("db")
    .unwrap()
    .days
    .values()
    .map(|d| d.degraded_secs)
    .sum();
  assert_eq!(total, 10);

  // An entity unseen for longer than the retention window is dropped.
  let mut store2 = UptimeStore::load(&dir_str);
  store2.tick(t0, live(&[("old", Availability::Up)]));
  store2.tick(t0 + ENTITY_RETENTION_SECS + 10, live(&[]));
  assert!(!store2.snapshot().contains_key("old"));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prune_caps_days_entities_and_total_count() {
  let dir = crate::test_support::test_temp_root().join(format!("up-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let mut store = UptimeStore::load(&dir.to_string_lossy());
  let day = 86_400u64;
  let now = 1_700_000_000u64;

  // An entity with more day buckets than retention keeps only the newest.
  store.tick(
    now - (DAY_RETENTION as u64 + 10) * day,
    live(&[("host:a", Availability::Up)]),
  );
  for i in (0..=(DAY_RETENTION as u64 + 9)).rev() {
    store.tick(now - i * day, live(&[("host:a", Availability::Up)]));
  }
  let snap = store.snapshot();
  assert!(
    snap["host:a"].days.len() <= DAY_RETENTION,
    "{} day buckets survive pruning",
    snap["host:a"].days.len()
  );

  // An entity unseen past the retention window is dropped entirely.
  store.tick(now, live(&[("host:gone", Availability::Up)]));
  store.tick(
    now + ENTITY_RETENTION_SECS + 1,
    live(&[("host:a", Availability::Up)]),
  );
  assert!(!store.snapshot().contains_key("host:gone"));

  // Over the entity cap, the oldest last_seen goes first.
  let mut crowd = HashMap::new();
  for i in 0..(ENTITY_CAP + 20) {
    crowd.insert(format!("host:h{i}"), (Availability::Up, None::<String>));
  }
  store.tick(now + ENTITY_RETENTION_SECS + 2, crowd);
  assert!(store.snapshot().len() <= ENTITY_CAP);

  // And the whole thing survives a save/load round trip.
  store.save_if_dirty();
  let reloaded = UptimeStore::load(&dir.to_string_lossy());
  assert_eq!(reloaded.snapshot().len(), store.snapshot().len());
}

#[test]
fn import_replaces_history_and_persists_it() {
  let dir = crate::test_support::test_temp_root().join(format!("up-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let mut store = UptimeStore::load(&dir.to_string_lossy());
  store.tick(1_700_000_000, live(&[("host:old", Availability::Up)]));

  let mut imported = HashMap::new();
  imported.insert(
    "host:new".to_string(),
    EntityUptime {
      status: Availability::Down,
      last_seen: 1_700_000_000,
      org_id: Some("acme".to_string()),
      days: HashMap::new(),
    },
  );
  assert_eq!(store.import(imported), 1);
  assert!(!store.snapshot().contains_key("host:old"));

  let reloaded = UptimeStore::load(&dir.to_string_lossy());
  assert_eq!(
    reloaded.snapshot()["host:new"].org_id.as_deref(),
    Some("acme")
  );
}
