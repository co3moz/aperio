//! The views that keep arriving: the request log and its filters, and the
//! event stream the dashboard holds open, including the two ways it ends.

use super::super::clients_tests::*;
use super::*;
use crate::state::RequestLog;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::State;
use std::sync::Arc;

#[tokio::test]
async fn logs_filtered_by_effective_org() {
  let state = Arc::new(test_state());
  {
    let mut logs = state.recent_logs.lock().await;
    logs.push_back(log("a", None));
    logs.push_back(log("b", Some("acme")));
    logs.push_back(log("c", None));
  }
  let headers = admin_headers(&state).await;
  let resp = logs_handler(
    State(state.clone()),
    headers,
    axum::extract::Query(Default::default()),
  )
  .await;
  let entries = resp.0;
  assert_eq!(entries.len(), 2, "only master-org logs visible to admin");
  assert!(entries.iter().all(|l| l.org_id.is_none()));
}

#[tokio::test]
async fn live_stream_emits_stats_traffic_and_ends_on_shutdown() {
  use axum::response::IntoResponse;
  use futures_util::StreamExt;
  use std::time::Duration;
  use tokio::time::timeout;

  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;

  // A viewer session scoped to the master org (None).
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let headers = cookie_headers(&token);

  let sse = live_stream_handler(State(state.clone()), headers).await;
  let resp = sse.into_response();
  let mut body = resp.into_body().into_data_stream();

  // First frame: the immediate `stats` event.
  let first = timeout(Duration::from_secs(2), body.next())
    .await
    .expect("stats frame in time")
    .expect("some frame")
    .expect("ok bytes");
  let text = String::from_utf8_lossy(&first);
  assert!(text.contains("event: stats"), "got: {text}");

  // Publish a mismatched-org log (skipped) then a matching one (streamed).
  let _ = state.traffic_tx.send(log("skip", Some("acme")));
  let _ = state.traffic_tx.send(log("keep", None));

  // Read frames until the matching traffic event arrives.
  let mut saw_traffic = false;
  for _ in 0..4 {
    let frame = timeout(Duration::from_secs(3), body.next())
      .await
      .expect("frame in time");
    let Some(Ok(bytes)) = frame else { break };
    if String::from_utf8_lossy(&bytes).contains("event: traffic") {
      saw_traffic = true;
      break;
    }
  }
  assert!(saw_traffic, "matching traffic event streamed");

  // Signal shutdown → the stream terminates.
  let _ = state.shutdown.send(true);
  let ended = timeout(Duration::from_secs(3), body.next())
    .await
    .expect("stream ends promptly");
  assert!(ended.is_none(), "stream closed on shutdown");
}

#[tokio::test]
async fn live_stream_fences_notifications_to_the_subscriber_org() {
  use axum::response::IntoResponse;
  use futures_util::StreamExt;
  use std::time::Duration;
  use tokio::time::timeout;

  let state = Arc::new(test_state());
  // A viewer session scoped to the master org (None).
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let headers = cookie_headers(&token);

  let sse = live_stream_handler(State(state.clone()), headers).await;
  let resp = sse.into_response();
  let mut body = resp.into_body().into_data_stream();

  // Drain the immediate `stats` frame.
  let _ = timeout(Duration::from_secs(2), body.next())
    .await
    .expect("stats frame in time");

  let ev = |event: &str, org: Option<&str>| crate::state::ServerEvent {
    event: event.to_string(),
    timestamp: "2026-08-06T00:00:00+03:00".to_string(),
    data: serde_json::json!({"id": "t1"}),
    org: org.map(str::to_string),
  };
  // Another org's event first: it must not reach this subscriber, so the
  // frame that does arrive is the master one behind it.
  let _ = state.events_tx.send(ev("token_revoked", Some("acme")));
  let _ = state.events_tx.send(ev("client_disconnected", None));

  let mut seen: Option<String> = None;
  for _ in 0..4 {
    let frame = timeout(Duration::from_secs(3), body.next())
      .await
      .expect("frame in time");
    let Some(Ok(bytes)) = frame else { break };
    let text = String::from_utf8_lossy(&bytes).to_string();
    if text.contains("event: notification") {
      seen = Some(text);
      break;
    }
  }
  let seen = seen.expect("the master-org notification streamed");
  assert!(seen.contains("client_disconnected"), "got: {seen}");
  assert!(
    !seen.contains("token_revoked"),
    "another org's event leaked: {seen}"
  );
}

#[tokio::test]
async fn live_stream_ends_when_the_session_it_opened_with_is_revoked() {
  // The session middleware runs once, when the stream is opened, and the
  // stream then lives for hours: signing out (or "sign out everywhere", or an
  // expiry, or a user being disabled) left it emitting traffic and statistics
  // to a caller with no session at all.
  use axum::response::IntoResponse;
  use futures_util::StreamExt;
  use std::time::Duration;
  use tokio::time::timeout;

  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;

  let sse = live_stream_handler(State(state.clone()), cookie_headers(&token)).await;
  let mut body = sse.into_response().into_body().into_data_stream();

  let first = timeout(Duration::from_secs(2), body.next())
    .await
    .expect("stats frame in time")
    .expect("some frame")
    .expect("ok bytes");
  assert!(String::from_utf8_lossy(&first).contains("event: stats"));

  // Sign out, then let a tick come round.
  state.sessions.lock().await.remove(&token);
  let mut ended = false;
  for _ in 0..3 {
    match timeout(Duration::from_secs(4), body.next()).await {
      Ok(None) => {
        ended = true;
        break;
      }
      Ok(Some(_)) => continue,
      Err(_) => break,
    }
  }
  assert!(
    ended,
    "the stream must close within a tick of the session going away"
  );
}

/// A log entry with the fields the filters look at.
fn log_of(
  id: &str,
  method: &str,
  uri: &str,
  status: Option<u16>,
  error: Option<&str>,
) -> RequestLog {
  RequestLog {
    id: id.to_string(),
    timestamp: "2026-08-02T00:00:00Z".to_string(),
    method: method.to_string(),
    uri: uri.to_string(),
    status,
    duration_ms: 5,
    error: error.map(|e| e.to_string()),
    host: Some("svc.example.com".to_string()),
    org_id: None,
  }
}

async fn query(state: &Arc<AppState>, pairs: &[(&str, &str)]) -> Vec<RequestLog> {
  let params: std::collections::HashMap<String, String> = pairs
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
  let headers = admin_headers(state).await;
  logs_handler(State(state.clone()), headers, axum::extract::Query(params))
    .await
    .0
}

async fn seeded_state() -> Arc<AppState> {
  let state = Arc::new(test_state());
  {
    let mut logs = state.recent_logs.lock().await;
    logs.push_back(log_of("a", "GET", "/api/users", Some(200), None));
    logs.push_back(log_of("b", "POST", "/api/login", Some(401), None));
    logs.push_back(log_of("c", "GET", "/api/login", Some(500), None));
    logs.push_back(log_of(
      "d",
      "DELETE",
      "/things/9",
      None,
      Some("client gone"),
    ));
  }
  state
}

#[tokio::test]
async fn status_accepts_an_exact_code_and_a_class() {
  let state = seeded_state().await;
  let exact = query(&state, &[("status", "401")]).await;
  assert_eq!(exact.len(), 1);
  assert_eq!(exact[0].id, "b");

  let class = query(&state, &[("status", "4xx")]).await;
  assert_eq!(class.len(), 1, "401 is the only 4xx");

  // A failed request with no status is a 5xx, as the dashboard buckets it,
  // so it must not vanish from the query an operator runs to find failures.
  let server_errors = query(&state, &[("status", "5xx")]).await;
  let ids: Vec<&str> = server_errors.iter().map(|l| l.id.as_str()).collect();
  assert_eq!(ids, vec!["c", "d"]);
}

#[tokio::test]
async fn method_and_path_filter_and_combine() {
  let state = seeded_state().await;
  assert_eq!(
    query(&state, &[("method", "get")]).await.len(),
    2,
    "case-insensitive"
  );
  assert_eq!(
    query(&state, &[("path", "LOGIN")]).await.len(),
    2,
    "substring, case-insensitive"
  );
  // Predicates are AND, not OR.
  let both = query(&state, &[("method", "GET"), ("path", "/api/login")]).await;
  assert_eq!(both.len(), 1);
  assert_eq!(both[0].id, "c");
}

#[tokio::test]
async fn an_empty_parameter_is_not_a_filter() {
  // A form that submits `?method=` must not mean "no method matches".
  let state = seeded_state().await;
  assert_eq!(query(&state, &[("method", "")]).await.len(), 4);
  assert_eq!(query(&state, &[("status", "  ")]).await.len(), 4);
}

#[tokio::test]
async fn a_limit_returns_the_newest_matches() {
  let state = seeded_state().await;
  let capped = query(&state, &[("limit", "2")]).await;
  let ids: Vec<&str> = capped.iter().map(|l| l.id.as_str()).collect();
  assert_eq!(
    ids,
    vec!["d", "c"],
    "newest first, so a cap keeps the recent ones"
  );
  // Without a limit the order is unchanged, which the live view relies on.
  let all = query(&state, &[]).await;
  assert_eq!(all.first().unwrap().id, "a");
}
