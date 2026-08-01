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
