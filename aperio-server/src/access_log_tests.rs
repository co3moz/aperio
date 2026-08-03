//! Tests for the per-request logging path.

use super::*;
use crate::test_support::test_state;

#[tokio::test]
async fn the_access_log_json_is_not_built_when_there_is_no_file() {
  // The JSON tree used to be constructed on every request and dropped inside
  // `append_access_line` when no file was configured. There is no way to
  // observe an absence directly, so this asserts the contract it rests on:
  // with no access log, the call does nothing and touches no file.
  let state = Arc::new(test_state());
  assert!(
    state.access_log.is_none(),
    "the fixture configures no access log"
  );

  log_request_success(
    &state,
    "req-1".to_string(),
    "GET",
    "/x",
    200,
    Duration::from_millis(1),
    Some("h.example.com"),
    Some("client-1"),
    None,
    None,
  )
  .await;

  // The dashboard's in-memory ring still gets the entry: that is the part
  // the absence of a file does not turn off.
  assert_eq!(state.recent_logs.lock().await.len(), 1);
}

#[tokio::test]
async fn the_access_event_can_be_silenced_without_silencing_warnings() {
  // `access_events: false` is not `log_level: warn`: it turns off one event
  // per request, and the dashboard's live view keeps working, because that is
  // fed from the ring buffer rather than from the log.
  let mut config = crate::test_support::test_config();
  config.access_events = false;
  let state = Arc::new(crate::test_support::test_state_with(config));

  log_request_success(
    &state,
    "req-2".to_string(),
    "GET",
    "/x",
    200,
    Duration::from_millis(1),
    None,
    None,
    None,
    None,
  )
  .await;

  assert_eq!(
    state.recent_logs.lock().await.len(),
    1,
    "the live view is fed from the ring, not from the log event"
  );
}

// ---------------------------------------------------------------------------
// Sampling (planned_features #55, #66)
// ---------------------------------------------------------------------------

#[test]
fn sampling_off_and_full_are_exact() {
  assert!(sampled_in(1.0, 200));
  assert!(sampled_in(2.0, 200), "a rate above one is still everything");
  assert!(!sampled_in(0.0, 200));
}

#[test]
fn a_server_error_is_never_sampled_out() {
  // The line somebody goes looking for is precisely the one that went wrong.
  for status in [500, 502, 503] {
    for _ in 0..20 {
      assert!(sampled_in(0.0, status), "{status}");
    }
  }
  // A 404 is routine traffic and is sampled like the rest: it is the noise
  // people turn the volume down for.
  assert!(!sampled_in(0.0, 404));
}

#[test]
fn one_in_ten_is_exactly_one_in_ten() {
  // Deterministic, not random: the point of turning the volume down is
  // knowing what the volume now is.
  let kept = (0..100).filter(|_| sampled_in(0.1, 200)).count();
  assert_eq!(kept, 10);
}

#[test]
fn the_relay_log_has_its_own_share() {
  // A separate accumulator, so a busy HTTP surface cannot starve the relay
  // log of its tenth and vice versa: they are different populations.
  let kept = (0..100).filter(|_| relay_sampled_in(0.1)).count();
  assert_eq!(kept, 10);
  assert!(relay_sampled_in(1.0));
  assert!(!relay_sampled_in(0.0));
}
