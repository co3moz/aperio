//! The per-route latency windows behind the dashboard's slow-endpoint and
//! trend views: what they average, what they refuse to average from too few
//! samples, and the caps that keep them bounded on a long-lived server.

use super::*;

// ----- EndpointStats / EndpointWindow -----

#[test]
fn test_endpoint_stats_record_summary_and_overflow() {
  use crate::state::{ENDPOINT_MIN_SAMPLES, EndpointStats};
  let mut stats = EndpointStats::default();
  // A spread of durations plus one 5xx to bump the error counter.
  for ms in [10u64, 20, 30, 40, 500] {
    let status = if ms == 500 { 503 } else { 200 };
    stats.record(Some("a.local"), "/api", status, ms, None);
  }
  let w = stats.endpoints.get("a.local|/api").expect("endpoint");
  assert_eq!(w.count, 5);
  assert_eq!(w.errors, 1);
  assert!(w.samples() >= ENDPOINT_MIN_SAMPLES.min(5));
  let (avg, p50, p95, max) = w.summary();
  assert!(avg > 0.0);
  assert_eq!(max, 500);
  assert!(p50 <= p95 && p95 <= max);

  // An empty window summarizes to zeros.
  let empty = EndpointStats::default();
  assert!(empty.endpoints.is_empty());
}

#[test]
fn test_endpoint_stats_key_cap_folds_into_other() {
  use crate::state::EndpointStats;
  let mut stats = EndpointStats::default();
  // Overflow the distinct-endpoint cap; extra keys fold into __other.
  for i in 0..400 {
    stats.record(Some(&format!("h{i}.local")), "/p", 200, 5, None);
  }
  assert!(
    stats.endpoints.contains_key("__other|__other"),
    "overflow endpoint folds into __other"
  );
}

// ----- RouteTrends / RouteTrend -----

#[test]
fn test_route_trends_record_and_series() {
  let mut trends = RouteTrends::default();
  let now = 1_000_000u64; // seconds
  let minute = now / 60;
  // One of each status class into the same minute bucket.
  trends.record(Some("app.local"), 204, None, now);
  trends.record(Some("app.local"), 301, None, now);
  trends.record(Some("app.local"), 404, None, now);
  trends.record(Some("app.local"), 500, None, now);
  // A later minute rolls a new bucket.
  trends.record(Some("app.local"), 200, None, now + 60);

  let series = trends
    .routes
    .get("app.local")
    .unwrap()
    .series(3, minute + 1);
  assert_eq!(series.len(), 3);
  // The first minute holds the four class counts.
  let first = series.iter().find(|b| b.minute == minute).unwrap();
  assert_eq!(first.total, 4);
  assert_eq!(first.s2xx, 1);
  assert_eq!(first.s3xx, 1);
  assert_eq!(first.s4xx, 1);
  assert_eq!(first.s5xx, 1);
  // The next minute holds the single 2xx.
  let second = series.iter().find(|b| b.minute == minute + 1).unwrap();
  assert_eq!(second.total, 1);
  assert_eq!(second.s2xx, 1);
}

#[test]
fn test_route_trends_cap_ignores_overflow() {
  let mut trends = RouteTrends::default();
  for i in 0..100 {
    trends.record(Some(&format!("h{i}.local")), 200, None, 0);
  }
  let len = trends.routes.len();
  // A brand-new route past the cap is simply not trended.
  trends.record(Some("overflow.local"), 200, None, 0);
  assert_eq!(trends.routes.len(), len);
  assert!(!trends.routes.contains_key("overflow.local"));
}
