//! What a service is once the config has been resolved: the drain budget a
//! reload spends, and the pool load an elastic `connections:` range is grown
//! and shrunk from.

use super::*;
use crate::service::tests::test_shared;

// --- reload drain budget (planned_features #33) -----------------------------

#[tokio::test]
async fn a_zero_budget_returns_at_once_even_with_requests_in_flight() {
  // `reload_drain: 0` is the pre-#33 behavior, an immediate drop. It must not
  // wait, and must not depend on the counter reaching zero.
  let shared = test_shared();
  shared.inflight_requests.store(3, Ordering::SeqCst);
  let start = Instant::now();
  drain_inflight_for(&shared, Duration::from_secs(0)).await;
  assert!(start.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn a_drain_returns_as_soon_as_the_last_request_finishes() {
  let shared = test_shared();
  shared.inflight_requests.store(1, Ordering::SeqCst);
  let done = shared.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(150)).await;
    done.inflight_requests.store(0, Ordering::SeqCst);
  });
  let start = Instant::now();
  // A generous budget: what ends the wait is the work finishing, not the cap.
  drain_inflight_for(&shared, Duration::from_secs(30)).await;
  let waited = start.elapsed();
  assert!(
    waited >= Duration::from_millis(140),
    "it waited for the request"
  );
  assert!(
    waited < Duration::from_secs(5),
    "it did not wait out the budget: {waited:?}"
  );
}

#[tokio::test]
async fn a_stalled_request_cannot_hold_a_reload_past_the_budget() {
  let shared = test_shared();
  shared.inflight_requests.store(1, Ordering::SeqCst);
  let start = Instant::now();
  drain_inflight_for(&shared, Duration::from_millis(300)).await;
  let waited = start.elapsed();
  assert!(
    waited >= Duration::from_millis(300),
    "the budget was honored"
  );
  assert!(
    waited < Duration::from_secs(3),
    "and it did give up: {waited:?}"
  );
}

// ---------------------------------------------------------------------------
// PoolLoad (elastic connections, planned_features #48)
// ---------------------------------------------------------------------------

#[test]
fn test_pool_load_reports_the_window_peak() {
  let load = PoolLoad::default();
  load.enter();
  load.enter();
  load.enter();
  load.leave();
  load.leave();
  load.leave();
  // The burst is what the supervisor has to see: by the time it looks, the
  // three requests are long finished, and an instantaneous reading would say
  // the pool was idle through the very moment it was busiest.
  assert_eq!(load.take_peak(), 3);
}

#[test]
fn test_pool_load_carries_running_requests_into_the_next_window() {
  let load = PoolLoad::default();
  load.enter();
  load.enter();
  assert_eq!(load.take_peak(), 2);
  // Both are still running, so the new window starts at two rather than at
  // zero: a slow request occupies the pool across ticks and a window that
  // reset to zero would report an idle pool while it is fully occupied.
  assert_eq!(load.take_peak(), 2);
  load.leave();
  load.leave();
  // They were still running when this window opened, so the window that has
  // just closed did hold two.
  assert_eq!(load.take_peak(), 2);
  // Only now, with nothing carried in, does the pool read as idle.
  assert_eq!(load.take_peak(), 0);
}

#[test]
fn test_pool_load_open_count_is_absent_until_a_supervisor_sets_it() {
  let load = PoolLoad::default();
  // A fixed pool has no supervisor, so the announcement falls back to the
  // configured count rather than claiming zero connections.
  assert_eq!(load.open(), None);
  load.set_open(3);
  assert_eq!(load.open(), Some(3));
}
