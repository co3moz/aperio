//! Tests for the statistics store: counters, per-service rollups, and the
//! pruning that keeps the series bounded.

use super::*;

#[test]
fn test_recent_period_keys() {
  let days = recent_period_keys("day", 7).unwrap();
  assert_eq!(days.len(), 7);
  assert!(days.iter().all(|k| k.starts_with("d:")));
  // Chronological, current period last.
  let [d, _, m, y] = period_keys();
  assert_eq!(days.last().unwrap(), &d);
  let mut sorted = days.clone();
  sorted.sort();
  assert_eq!(sorted, days);

  let months = recent_period_keys("month", 24).unwrap();
  assert_eq!(months.len(), 24);
  assert_eq!(months.last().unwrap(), &m);
  let mut sorted = months.clone();
  sorted.sort();
  assert_eq!(sorted, months);

  let years = recent_period_keys("year", 3).unwrap();
  assert_eq!(years.last().unwrap(), &y);

  let weeks = recent_period_keys("week", 26).unwrap();
  assert_eq!(weeks.len(), 26);
  assert!(weeks.iter().all(|k| k.starts_with("w:")));

  assert!(recent_period_keys("fortnight", 5).is_none());
}

#[test]
fn test_day_keys_between() {
  let keys = day_keys_between("2026-07-01", "2026-07-05").unwrap();
  assert_eq!(
    keys,
    vec![
      "d:2026-07-01",
      "d:2026-07-02",
      "d:2026-07-03",
      "d:2026-07-04",
      "d:2026-07-05"
    ]
  );
  // Single day.
  assert_eq!(
    day_keys_between("2026-07-01", "2026-07-01").unwrap().len(),
    1
  );
  // Capped to the day retention window, keeping the newest buckets.
  let long = day_keys_between("2025-01-01", "2026-01-01").unwrap();
  assert_eq!(long.len(), RETENTION[0].1);
  assert_eq!(long.last().unwrap(), "d:2026-01-01");
  // Invalid input.
  assert!(day_keys_between("2026-07-05", "2026-07-01").is_none());
  assert!(day_keys_between("notadate", "2026-07-01").is_none());
}

#[test]
fn test_record_and_reload() {
  let dir =
    crate::test_support::test_temp_root().join(format!("stats-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();

  let mut store = StatsStore::load(&dir_str);
  // First request served by the master org, second by child org "acme".
  store.record_request_labeled(
    true,
    100,
    2000,
    40,
    Some("master"),
    Some("a.example.com"),
    None,
  );
  store.record_request_labeled(
    false,
    50,
    0,
    60,
    Some("tenant-a"),
    Some("a.example.com"),
    Some("acme"),
  );
  store.record_bytes_sent(500, None);
  store.save_if_dirty();

  let snap = store.snapshot();
  assert_eq!(snap.total_requests, 2);
  assert_eq!(snap.total_success, 1);
  assert_eq!(snap.total_failed, 1);
  assert_eq!(snap.total_bytes_received, 150);
  assert_eq!(snap.total_bytes_sent, 2500);
  assert_eq!(snap.total_request_duration_ms, 100);
  assert!((snap.avg_response_ms() - 50.0).abs() < f64::EPSILON);

  // Per-org slices: the master org saw only its own request (+ the streamed
  // bytes), the "acme" org only its own; neither sees the other's traffic.
  let master = store.snapshot_for_org(None);
  assert_eq!(master.total_requests, 1);
  assert_eq!(master.total_success, 1);
  assert_eq!(master.total_bytes_sent, 2500);
  let acme = store.snapshot_for_org(Some("acme"));
  assert_eq!(acme.total_requests, 1);
  assert_eq!(acme.total_failed, 1);
  assert_eq!(acme.total_bytes_sent, 0);
  assert!(store.snapshot_for_org(Some("unknown")).total_requests == 0);

  // Period buckets exist for the current day/week/month/year.
  let [d, w, m, y] = period_keys();
  for key in [d, w, m, y] {
    let p = snap.periods.get(&key).expect("period bucket");
    assert_eq!(p.requests, 2);
    assert_eq!(p.bytes_sent, 2500);
  }

  // Label breakdowns are attributed per token and hostname.
  assert_eq!(snap.by_token.get("master").unwrap().requests, 1);
  assert_eq!(snap.by_token.get("tenant-a").unwrap().failed, 1);
  let host = snap.by_hostname.get("a.example.com").unwrap();
  assert_eq!(host.requests, 2);
  assert_eq!(host.bytes_sent, 2000);

  // Reload from disk → counters survive.
  let store2 = StatsStore::load(&dir_str);
  assert_eq!(store2.snapshot().total_requests, 2);
  assert_eq!(
    store2
      .snapshot()
      .by_hostname
      .get("a.example.com")
      .unwrap()
      .requests,
    2
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_purge_hostname_and_token() {
  let dir =
    crate::test_support::test_temp_root().join(format!("stats-purge-{}", uuid::Uuid::new_v4()));
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = StatsStore::load(&dir_str);
  store.record_request_labeled(
    true,
    100,
    1000,
    50,
    Some("tenant-a"),
    Some("a.example.com"),
    Some("org-1"),
  );
  store.record_request_labeled(
    true,
    100,
    1000,
    50,
    Some("master"),
    Some("b.example.com"),
    None,
  );
  store.save_if_dirty();

  // Hostname purge removes the global row and the org breakdown row.
  assert!(store.purge_hostname("a.example.com") >= 1);
  assert!(!store.snapshot().by_hostname.contains_key("a.example.com"));
  // Other hostnames and totals are untouched.
  assert!(store.snapshot().by_hostname.contains_key("b.example.com"));
  assert_eq!(store.snapshot().total_requests, 2);

  // Token purge removes the label rows.
  assert!(store.purge_token("tenant-a") >= 1);
  assert!(!store.snapshot().by_token.contains_key("tenant-a"));

  // Purges persist across a reload.
  let store2 = StatsStore::load(&dir_str);
  assert!(!store2.snapshot().by_hostname.contains_key("a.example.com"));
  assert!(!store2.snapshot().by_token.contains_key("tenant-a"));

  // Unknown selectors remove nothing.
  assert_eq!(store.purge_hostname("nope.example.com"), 0);
  assert_eq!(store.purge_token("nope"), 0);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_bandwidth_period_labels() {
  let dir =
    crate::test_support::test_temp_root().join(format!("stats-bw-{}", uuid::Uuid::new_v4()));
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = StatsStore::load(&dir_str);
  store.record_request_labeled(
    true,
    100,
    900,
    5,
    Some("master"),
    Some("a.example.com"),
    None,
  );
  store.record_request_labeled(
    true,
    50,
    450,
    5,
    Some("master"),
    Some("a.example.com"),
    None,
  );
  store.save_if_dirty();

  let [day_key, _, month_key, _] = period_keys();
  let snap = store.snapshot();
  // Day and month buckets carry the label's bytes; weeks/years stay lifetime-only.
  let day = snap
    .by_token_periods
    .get(&day_key)
    .unwrap()
    .get("master")
    .unwrap();
  assert_eq!(day.requests, 2);
  assert_eq!(day.bytes_sent, 1350);
  assert_eq!(day.bytes_received, 150);
  let month = snap
    .by_hostname_periods
    .get(&month_key)
    .unwrap()
    .get("a.example.com")
    .unwrap();
  assert_eq!(month.requests, 2);
  assert!(
    snap
      .by_token_periods
      .keys()
      .all(|k| k.starts_with("d:") || k.starts_with("m:"))
  );

  // Survives a reload.
  let snap2 = StatsStore::load(&dir_str).snapshot();
  assert!(snap2.by_token_periods.contains_key(&day_key));

  let _ = std::fs::remove_dir_all(&dir);
}
