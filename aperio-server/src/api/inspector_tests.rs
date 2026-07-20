//! Tests for the request inspector dashboard API (detail + replay).

use super::*;
use crate::state::CapturedRequest;
use crate::state::{ClientHandle, PendingRequest};
use crate::test_support::ok_tunnel_response;
use crate::test_support::{
  admin_headers, cookie_headers, json_body, mock_client, seed_session, test_peer, test_state,
};
use axum::extract::ws::Message;
use axum::extract::{ConnectInfo, Path, State};
use base64::prelude::*;
use std::time::Duration;
use tokio::sync::mpsc;

/// A mock client whose receiver stays alive (so a dispatch `send` succeeds).
fn live_client() -> (ClientHandle, mpsc::Receiver<Message>) {
  let (tx, rx) = mpsc::channel::<Message>(4);
  let mut client = mock_client(None, None, None, None);
  client.tx = tx;
  (client, rx)
}

/// Completes the first pending request with `resp` (or drops its sender when
/// `None` to simulate a lost client connection).
fn respond_to_pending(state: Arc<AppState>, resp: Option<crate::state::TunnelResponse>) {
  tokio::spawn(async move {
    for _ in 0..200 {
      let pr: Option<PendingRequest> = {
        let mut pending = state.pending_requests.lock().await;
        pending
          .keys()
          .next()
          .cloned()
          .and_then(|k| pending.remove(&k))
      };
      if let Some(pr) = pr {
        if let Some(r) = resp {
          let _ = pr.tx.send(r);
        }
        return;
      }
      tokio::time::sleep(Duration::from_millis(2)).await;
    }
  });
}

/// Builds a captured request with the given id/org and an unbound "/echo"
/// route (no Host header) so replay routing picks an unbound mock client.
fn captured(id: &str, org: Option<&str>, truncated: bool) -> CapturedRequest {
  CapturedRequest {
    id: id.to_string(),
    timestamp: "2026-07-20T00:00:00+00:00".to_string(),
    method: "POST".to_string(),
    uri: "/echo?q=1".to_string(),
    req_headers: vec![("content-type".to_string(), "application/json".to_string())],
    req_body: Some(BASE64_STANDARD.encode(b"payload")),
    req_body_truncated: truncated,
    status: 200,
    resp_headers: Vec::new(),
    resp_body: None,
    resp_body_truncated: false,
    resp_streamed: false,
    duration_ms: 5,
    timeline: None,
    org_id: org.map(|s| s.to_string()),
  }
}

async fn seed(state: &AppState, c: CapturedRequest) {
  state.captured_requests.lock().await.push_back(c);
}

#[tokio::test]
async fn detail_returns_capture_and_404() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  seed(&state, captured("b", Some("other"), false)).await;
  let headers = admin_headers(&state).await;

  let resp =
    request_detail_handler(State(state.clone()), Path("a".to_string()), headers.clone()).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["id"], "a");
  assert_eq!(body["method"], "POST");

  // Unknown id.
  let resp = request_detail_handler(
    State(state.clone()),
    Path("nope".to_string()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // Foreign-org capture is invisible.
  let resp = request_detail_handler(State(state.clone()), Path("b".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replay_unknown_id_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("nope".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replay_foreign_org_is_404() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", Some("other"), false)).await;
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replay_truncated_body_is_400() {
  let state = Arc::new(test_state());
  seed(&state, captured("t", None, true)).await;
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("t".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn replay_without_client_is_504() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn replay_with_client_reaches_dispatch() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  // Receiver dropped in mock_client -> socket send fails -> 502.
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn replay_routes_by_host_header() {
  let state = Arc::new(test_state());
  // Capture carries a Host header; a hostname-bound client serves that route.
  let mut c = captured("h", None, false);
  c.req_headers
    .push(("host".to_string(), "svc.example.com:443".to_string()));
  seed(&state, c).await;
  state.clients.lock().await.insert(
    "c1".to_string(),
    mock_client(Some("svc.example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("h".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn replay_success_counts_2xx() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  respond_to_pending(state.clone(), Some(ok_tunnel_response()));
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["replayed_id"], "a");
  assert_eq!(body["status"], 200);
  let stats = state.stats.lock().await;
  assert_eq!(stats.total_requests, 1);
  assert_eq!(stats.successful_requests, 1);
}

#[tokio::test]
async fn replay_success_counts_5xx_as_failure() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  let mut res = ok_tunnel_response();
  res.status = 503;
  respond_to_pending(state.clone(), Some(res));
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["status"], 503);
  assert_eq!(state.stats.lock().await.failed_requests, 1);
}

#[tokio::test]
async fn replay_connection_lost_is_502() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  respond_to_pending(state.clone(), None);
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn replay_response_timeout_is_504() {
  let state = Arc::new(test_state());
  seed(&state, captured("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  let headers = admin_headers(&state).await;
  let resp = request_replay_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn selected_org_replay_is_scoped() {
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme")
    .unwrap()
    .id
    .clone();
  seed(&state, captured("mine", Some(&org), false)).await;
  let token = seed_session(&state, crate::store::users::Role::Admin, None, Some(org)).await;
  let headers = cookie_headers(&token);
  // Own-org capture, no client -> 504 (passed the org gate).
  let resp = request_replay_handler(
    State(state.clone()),
    Path("mine".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}
