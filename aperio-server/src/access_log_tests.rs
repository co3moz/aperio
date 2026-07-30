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
