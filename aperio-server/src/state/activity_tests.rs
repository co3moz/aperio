//! The activity rings behind the dashboard's traffic chart: that each
//! resolution rolls up from the one below it, that an organization sees only
//! its own, and that a ring is bounded however long the server runs.

use super::*;
use crate::state::*;
use std::time::{Duration, Instant};

// --- The activity ring behind the chart's long view ---

#[test]
fn activity_buckets_by_five_seconds_and_keeps_quiet_slices() {
  let mut activity = Activity::default();
  // Three requests inside one slice, one of them a failure.
  activity.record(None, false, 1000);
  activity.record(None, true, 1002);
  activity.record(None, false, 1004);
  // A slice later, after a gap of two silent slices.
  activity.record(None, false, 1020);

  let series = activity.series(None, ActivityRange::Quarter, 1020);
  assert_eq!(series.len(), ACTIVITY_BUCKETS);
  let series = &series[series.len() - 6..];
  // Oldest first, and the silent slices are present rather than skipped: a
  // quiet minute is an answer, and omitting it draws the traffic on either
  // side as if it were adjacent.
  let totals: Vec<u32> = series.iter().map(|b| b.total).collect();
  assert_eq!(totals, vec![0, 3, 0, 0, 0, 1]);
  assert_eq!(series[1].failed, 1);
  // Every bucket is stamped, five seconds apart, aligned to the slice.
  for pair in series.windows(2) {
    assert_eq!(pair[1].at - pair[0].at, ACTIVITY_BUCKET_SECS);
  }
  assert_eq!(series.last().unwrap().at, 1020);
}

#[test]
fn activity_is_per_organization_and_bounded() {
  let mut activity = Activity::default();
  activity.record(None, false, 100);
  activity.record(Some("acme"), false, 100);
  activity.record(Some("acme"), false, 100);

  let newest = |a: &Activity, org: Option<&str>| {
    *a.series(org, ActivityRange::Quarter, 100)
      .last()
      .expect("the series is never empty")
  };
  assert_eq!(newest(&activity, None).total, 1);
  assert_eq!(newest(&activity, Some("acme")).total, 2);
  // An org that never served anything reads as silence, not as someone else's
  // traffic.
  assert_eq!(newest(&activity, Some("other")).total, 0);

  // The ring is bounded: fifteen minutes of slices, then the oldest goes.
  let mut long = Activity::default();
  for i in 0..(ACTIVITY_BUCKETS as u64 + 50) {
    long.record(None, false, i * ACTIVITY_BUCKET_SECS);
  }
  let last = (ACTIVITY_BUCKETS as u64 + 49) * ACTIVITY_BUCKET_SECS;
  let series = long.series(None, ActivityRange::Quarter, last);
  assert_eq!(series.len(), ACTIVITY_BUCKETS);
  assert!(
    series.iter().all(|b| b.total == 1),
    "the window is full of the most recent slices"
  );
}

#[test]
fn every_range_holds_about_sixty_cells_and_counts_the_same_request() {
  let mut activity = Activity::default();
  // A real wall-clock second: the day ring reaches back 24 hours, and a `now`
  // smaller than that would clamp its oldest buckets to zero.
  let now = 1_700_000_000;
  activity.record(None, true, now);

  // One request, counted once in each resolution: the rings are three views
  // of the same traffic, not three samples of it.
  for range in [
    ActivityRange::Quarter,
    ActivityRange::TwoHours,
    ActivityRange::Day,
  ] {
    let series = activity.series(None, range, now);
    assert_eq!(series.len(), range.buckets());
    assert_eq!(series.last().unwrap().total, 1);
    assert_eq!(series.last().unwrap().failed, 1);
    for pair in series.windows(2) {
      assert_eq!(pair[1].at - pair[0].at, range.width_secs());
    }
    // Roughly sixty cells whatever the span: that is what keeps the chart
    // readable and the payload small.
    assert!((60..=96).contains(&series.len()) || range == ActivityRange::Quarter);
  }

  // The span each range actually covers.
  let span = |r: ActivityRange| r.width_secs() * r.buckets() as u64;
  assert_eq!(span(ActivityRange::Quarter), 15 * 60);
  assert_eq!(span(ActivityRange::TwoHours), 2 * 60 * 60);
  assert_eq!(span(ActivityRange::Day), 24 * 60 * 60);
}

#[test]
fn an_unknown_range_is_the_quarter_hour_that_the_endpoint_always_returned() {
  // The parameter was added after the endpoint shipped, so a caller that does
  // not send it, or sends something this build does not know, gets exactly
  // what it used to get rather than an error or a day of history.
  assert_eq!(ActivityRange::parse(None), ActivityRange::Quarter);
  assert_eq!(ActivityRange::parse(Some("")), ActivityRange::Quarter);
  assert_eq!(ActivityRange::parse(Some("5m")), ActivityRange::Quarter);
  assert_eq!(ActivityRange::parse(Some("2h")), ActivityRange::TwoHours);
  assert_eq!(ActivityRange::parse(Some("1d")), ActivityRange::Day);
}

#[test]
fn the_long_ranges_survive_a_restart_and_the_fine_one_does_not() {
  let dir = crate::test_support::test_temp_root()
    .join(format!("activity-restart-{}", uuid::Uuid::new_v4()));
  let path = dir.to_string_lossy().to_string();
  let now = 1_700_000_000;

  let mut activity = Activity::load(&path, now);
  activity.record(None, false, now);
  activity.record(Some("acme"), true, now);
  activity.save_if_dirty();
  drop(activity);

  let restarted = Activity::load(&path, now + 60);
  // A view covering a day that empties on every deploy answers "what happened
  // overnight" with a shrug, which is worse than not offering the range.
  for range in [ActivityRange::TwoHours, ActivityRange::Day] {
    let series = restarted.series(None, range, now + 60);
    assert_eq!(
      series.iter().map(|b| b.total).sum::<u32>(),
      1,
      "the coarse rings come back"
    );
    assert_eq!(
      restarted
        .series(Some("acme"), range, now + 60)
        .iter()
        .map(|b| b.failed)
        .sum::<u32>(),
      1,
      "and they come back per organization"
    );
  }
  // The fine ring is deliberately not restored: fifteen minutes of five-second
  // slices is the view of "right now", and a restart is exactly the moment
  // when "right now" changed.
  assert_eq!(
    restarted
      .series(None, ActivityRange::Quarter, now + 60)
      .iter()
      .map(|b| b.total)
      .sum::<u32>(),
    0
  );

  // Aged out: a file written a day ago must not redraw yesterday as today.
  let stale = Activity::load(&path, now + 2 * 24 * 60 * 60);
  assert_eq!(
    stale
      .series(None, ActivityRange::Day, now + 2 * 24 * 60 * 60)
      .iter()
      .map(|b| b.total)
      .sum::<u32>(),
    0
  );
  let _ = std::fs::remove_dir_all(&dir);
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn a_route_rate_limit_spends_its_burst_and_then_refuses() {
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
    }
  }
  let _cleanup = Cleanup;
  std::fs::write(
    "aperio-server.yaml",
    "rate_limits:\n  - hostname: api.example.com\n    path: /login\n    rps: 1\n    burst: 2\n",
  )
  .unwrap();
  crate::config_file::reload().unwrap();

  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let mut cfg = crate::test_support::test_config();
    cfg.route_limits = crate::route_limits::from_config_file();
    let state = crate::test_support::test_state_with(cfg);

    // A path outside every rule never pays.
    assert!(
      state
        .check_route_rate_limit(Some("api.example.com"), "/free", "GET")
        .await
    );
    // The burst is two; the third request inside the same instant is refused.
    assert!(
      state
        .check_route_rate_limit(Some("api.example.com"), "/login", "GET")
        .await
    );
    assert!(
      state
        .check_route_rate_limit(Some("api.example.com"), "/login", "GET")
        .await
    );
    assert!(
      !state
        .check_route_rate_limit(Some("api.example.com"), "/login", "GET")
        .await
    );
  });
}

#[tokio::test]
async fn one_gc_beat_sweeps_stale_buckets_and_expired_sessions() {
  let state = crate::test_support::test_state_with(crate::test_support::test_config());
  // A stale and a fresh rate bucket, dated by hand.
  let old = Instant::now() - Duration::from_secs(700);
  state.rate_limiter.lock().await.insert(
    "203.0.113.9".parse().unwrap(),
    RateLimitState {
      tokens: 1.0,
      last_updated: old,
    },
  );
  assert!(
    state
      .check_rate_limit("198.51.100.7".parse().unwrap())
      .await
  );
  state.route_rate.lock().await.insert(
    "stale-route".to_string(),
    RateLimitState {
      tokens: 1.0,
      last_updated: old,
    },
  );
  let session = |expires_at: u64| crate::store::sessions::SessionInfo {
    plane: crate::store::sessions::Plane::Admin,
    expires_at,
    created_at: 0,
    ip: None,
    user_agent: None,
    scope_host: None,
    username: None,
    role: crate::store::users::Role::Admin,
    selected_org: None,
    bound_org: None,
  };
  {
    let mut sessions = state.sessions.lock().await;
    sessions.insert("expired", session(1));
    sessions.insert("live", session(crate::store::tokens::now_secs() + 3600));
  }

  state.gc_tick_once(Instant::now()).await;

  let rates = state.rate_limiter.lock().await;
  assert!(
    !rates.contains_key(&"203.0.113.9".parse().unwrap()),
    "stale IP swept"
  );
  assert!(
    rates.contains_key(&"198.51.100.7".parse().unwrap()),
    "fresh IP kept"
  );
  drop(rates);
  assert!(
    state.route_rate.lock().await.is_empty(),
    "stale route swept"
  );
  let sessions = state.sessions.lock().await;
  assert!(sessions.get("expired").is_none());
  assert!(sessions.get("live").is_some());
}
