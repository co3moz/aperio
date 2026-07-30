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
