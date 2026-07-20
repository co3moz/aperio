//! Tests for the webhook inbox dashboard API (list, detail, clear, delete,
//! re-fire).

use super::*;
use crate::state::{ClientHandle, PendingRequest};
use crate::store::inbox::InboxEntry;
use crate::test_support::ok_tunnel_response;
use crate::test_support::{
  admin_headers, cookie_headers, json_body, mock_client, seed_session, test_peer, test_state,
};
use axum::extract::ws::Message;
use axum::extract::{ConnectInfo, Path, State};
use base64::prelude::*;
use std::time::Duration;
use tokio::sync::mpsc;

/// A mock client whose receiver stays alive (so a dispatch `send` succeeds),
/// returned alongside that receiver which the caller must keep in scope.
fn live_client() -> (ClientHandle, mpsc::Receiver<Message>) {
  let (tx, rx) = mpsc::channel::<Message>(4);
  let mut client = mock_client(None, None, None, None);
  client.tx = tx;
  (client, rx)
}

/// Spawns a task that completes the first pending request with `resp` (or, when
/// `None`, drops its sender to simulate a lost client connection).
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

/// Builds an inbox entry with the given id/org, a small base64 body, and a
/// route (host/uri) usable by the re-fire routing.
fn entry(id: &str, org: Option<&str>, truncated: bool) -> InboxEntry {
  let body = BASE64_STANDARD.encode(b"hello world");
  InboxEntry {
    id: id.to_string(),
    timestamp: "2026-07-20T00:00:00+00:00".to_string(),
    method: "POST".to_string(),
    uri: "/hook?x=1".to_string(),
    host: None,
    headers: vec![("content-type".to_string(), "application/json".to_string())],
    body: Some(body),
    body_truncated: truncated,
    status: 200,
    service: Some("svc".to_string()),
    org_id: org.map(|s| s.to_string()),
  }
}

async fn seed(state: &AppState, e: InboxEntry) {
  state.inbox_store.lock().await.insert(e);
}

#[tokio::test]
async fn list_is_org_scoped_and_reports_body_bytes() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  seed(&state, entry("b", Some("other"), false)).await;
  let headers = admin_headers(&state).await; // master org (None)

  let resp = inbox_list_handler(State(state.clone()), headers).await;
  let body = serde_json::to_value(&resp.0).unwrap();
  let arr = body.as_array().unwrap();
  assert_eq!(arr.len(), 1, "only the master-org entry is visible");
  assert_eq!(arr[0]["id"], "a");
  assert_eq!(arr[0]["method"], "POST");
  // 11 bytes of "hello world" -> base64 len 16 -> 16*3/4 = 12 reported.
  assert!(arr[0]["body_bytes"].as_u64().unwrap() > 0);
  assert_eq!(arr[0]["body_truncated"], false);
}

#[tokio::test]
async fn detail_returns_entry_and_404_for_unknown_or_foreign() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  seed(&state, entry("b", Some("other"), false)).await;
  let headers = admin_headers(&state).await;

  let resp =
    inbox_detail_handler(State(state.clone()), Path("a".to_string()), headers.clone()).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["id"], "a");
  assert_eq!(body["status"], 200);
  assert!(body["headers"].is_array());

  // Unknown id.
  let resp = inbox_detail_handler(
    State(state.clone()),
    Path("nope".to_string()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // Foreign org entry is invisible (404).
  let resp = inbox_detail_handler(State(state.clone()), Path("b".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clear_empties_only_callers_org() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  seed(&state, entry("b", None, false)).await;
  seed(&state, entry("c", Some("other"), false)).await;
  let headers = admin_headers(&state).await;

  let resp = inbox_clear_handler(State(state.clone()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["removed"], 2);
  // The foreign-org entry survives.
  let remaining = state
    .inbox_store
    .lock()
    .await
    .list(&Some("other".to_string()))
    .len();
  assert_eq!(remaining, 1);
}

#[tokio::test]
async fn delete_removes_entry_then_404() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  let headers = admin_headers(&state).await;

  let resp =
    inbox_delete_handler(State(state.clone()), Path("a".to_string()), headers.clone()).await;
  assert_eq!(resp.status(), StatusCode::OK);

  // Second delete is a 404.
  let resp = inbox_delete_handler(State(state.clone()), Path("a".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refire_unknown_id_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("nope".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refire_truncated_body_is_400() {
  let state = Arc::new(test_state());
  seed(&state, entry("t", None, true)).await;
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("t".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn refire_without_client_is_504() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn refire_with_client_reaches_dispatch() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  // An unbound client matches the host-less "/hook" route. Its receiver is
  // dropped inside mock_client, so the socket send fails -> 502 (a reachable
  // status without a live backend).
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn refire_success_returns_backend_status() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  respond_to_pending(state.clone(), Some(ok_tunnel_response()));
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["refired_id"], "a");
  assert_eq!(body["status"], 200);
}

#[tokio::test]
async fn refire_connection_lost_is_502() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  // Drop the pending sender without answering -> RecvError -> 502.
  respond_to_pending(state.clone(), None);
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn refire_response_timeout_is_504() {
  let state = Arc::new(test_state());
  seed(&state, entry("a", None, false)).await;
  let (client, _rx) = live_client();
  state.clients.lock().await.insert("c1".to_string(), client);
  // No responder: the send succeeds but the wait times out (1s in test config).
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn refire_respects_org_scope() {
  let state = Arc::new(test_state());
  // Entry belongs to "other"; a master-admin viewing the master org can't see it.
  seed(&state, entry("a", Some("other"), false)).await;
  let headers = admin_headers(&state).await;
  let resp = inbox_refire_handler(
    State(state.clone()),
    Path("a".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn selected_org_scopes_the_view() {
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme")
    .unwrap()
    .id
    .clone();
  seed(&state, entry("mine", Some(&org), false)).await;
  seed(&state, entry("theirs", None, false)).await;
  // A master-admin whose session has an org selected sees only that org.
  let token = seed_session(
    &state,
    crate::store::users::Role::Admin,
    None,
    Some(org.clone()),
  )
  .await;
  let headers = cookie_headers(&token);

  let resp = inbox_list_handler(State(state.clone()), headers).await;
  let body = serde_json::to_value(&resp.0).unwrap();
  let arr = body.as_array().unwrap();
  assert_eq!(arr.len(), 1);
  assert_eq!(arr[0]["id"], "mine");
}
