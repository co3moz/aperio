//! Tests for the webhooks + audit dashboard API (audit list/verify, webhook
//! CRUD, delivery log, redeliver).

use super::*;
use crate::store::users::Role;
use crate::store::webhooks::{Delivery, WebhookFormat};
use crate::test_support::{
  admin_headers, cookie_headers, json_body, seed_session, test_peer, test_state,
};
use axum::extract::{ConnectInfo, Path, Query, State};

fn create_req(
  name: &str,
  url: &str,
  events: Vec<&str>,
  secret: Option<&str>,
  format: Option<&str>,
) -> Json<WebhookCreateRequest> {
  Json(WebhookCreateRequest {
    name: name.to_string(),
    url: url.to_string(),
    events: events.into_iter().map(|s| s.to_string()).collect(),
    secret: secret.map(|s| s.to_string()),
    format: format.map(|s| s.to_string()),
  })
}

fn delivery(id: &str, webhook_id: &str, org: Option<&str>) -> Delivery {
  Delivery {
    id: id.to_string(),
    webhook_id: webhook_id.to_string(),
    webhook_name: "hook".to_string(),
    org_id: org.map(|s| s.to_string()),
    event: "client_connected".to_string(),
    timestamp: "2026-07-20T00:00:00+00:00".to_string(),
    success: true,
    status: Some(200),
    error: None,
    attempts: 1,
    duration_ms: 12,
    body: "{}".to_string(),
    created_at: 100,
  }
}

// --- audit ---

#[tokio::test]
async fn audit_handler_returns_org_scoped_events() {
  let state = Arc::new(test_state());
  {
    let mut a = state.audit.lock().await;
    a.record("login", "alice", "127.0.0.1", None, "master event");
    a.record(
      "login",
      "bob",
      "127.0.0.1",
      Some("other".to_string()),
      "foreign",
    );
  }
  let headers = admin_headers(&state).await; // master org (None)
  let resp = audit_handler(State(state.clone()), headers).await;
  let events = resp.0;
  assert_eq!(events.len(), 1);
  assert_eq!(events[0].details, "master event");
}

#[tokio::test]
async fn audit_verify_reports_ok_chain() {
  let state = Arc::new(test_state());
  state
    .audit
    .lock()
    .await
    .record("x", "a", "127.0.0.1", None, "d");
  let resp = audit_verify_handler(State(state.clone())).await;
  assert_eq!(resp.0["ok"], true);
  assert_eq!(resp.0["broken"].as_array().unwrap().len(), 0);
}

// --- webhook CRUD ---

#[tokio::test]
async fn create_list_delete_roundtrip() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let resp = webhooks_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req(
      "ci",
      "https://example.com/hook",
      vec!["client_connected"],
      Some("0123456789abcdef"),
      Some("slack"),
    ),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let id = json_body(resp).await["id"].as_str().unwrap().to_string();

  // List shows it with signed=true and the format, but never the secret.
  let resp = webhooks_list_handler(State(state.clone()), headers.clone()).await;
  let list = resp.0;
  assert_eq!(list.len(), 1);
  assert_eq!(list[0]["signed"], true);
  assert_eq!(list[0]["format"], "slack");
  assert_eq!(list[0]["enabled"], true);
  assert!(list[0].get("secret").is_none());

  // Delete it, then a second delete 404s.
  let resp = webhooks_delete_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let resp = webhooks_delete_handler(
    State(state.clone()),
    Path(id),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_rejects_bad_input() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  // Empty name.
  let resp = webhooks_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("  ", "https://ok", vec![], None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Name too long.
  let long = "x".repeat(65);
  let resp = webhooks_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req(&long, "https://ok", vec![], None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Non-http URL.
  let resp = webhooks_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("k", "ftp://nope", vec![], None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Too-short secret.
  let resp = webhooks_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("k", "https://ok", vec![], Some("short"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Invalid format.
  let resp = webhooks_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    create_req("k", "https://ok", vec![], None, Some("carrier-pigeon")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_and_delete_are_org_scoped() {
  let state = Arc::new(test_state());
  // A webhook in a foreign org.
  let hook = state.webhook_store.lock().await.create(
    "foreign".to_string(),
    "https://example.com".to_string(),
    Vec::new(),
    None,
    WebhookFormat::Generic,
    Some("other".to_string()),
  );
  let headers = admin_headers(&state).await; // master org

  // List excludes the foreign hook.
  let resp = webhooks_list_handler(State(state.clone()), headers.clone()).await;
  assert_eq!(resp.0.len(), 0);

  // Delete of the foreign hook is a 404.
  let resp = webhooks_delete_handler(
    State(state.clone()),
    Path(hook.id),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// --- delivery log + redeliver ---

#[tokio::test]
async fn deliveries_are_org_scoped_and_limited() {
  let state = Arc::new(test_state());
  {
    let mut log = state.webhook_deliveries.lock().await;
    log.record(delivery("d1", "w1", None));
    log.record(delivery("d2", "w1", Some("other")));
  }
  let headers = admin_headers(&state).await;
  let resp = webhook_deliveries_handler(
    State(state.clone()),
    headers,
    Query(DeliveriesQuery {
      webhook_id: None,
      limit: Some(10),
    }),
  )
  .await;
  let rows = resp.0;
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0].id, "d1");
}

#[tokio::test]
async fn redeliver_unknown_delivery_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = webhook_redeliver_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("nope".to_string()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redeliver_foreign_org_is_404() {
  let state = Arc::new(test_state());
  state
    .webhook_deliveries
    .lock()
    .await
    .record(delivery("d1", "w1", Some("other")));
  let headers = admin_headers(&state).await; // master org can't see it
  let resp = webhook_redeliver_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("d1".to_string()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redeliver_missing_webhook_is_404() {
  let state = Arc::new(test_state());
  // Delivery exists in-org, but its webhook definition is gone.
  state
    .webhook_deliveries
    .lock()
    .await
    .record(delivery("d1", "gone", None));
  let headers = admin_headers(&state).await;
  let resp = webhook_redeliver_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("d1".to_string()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redeliver_queues_when_webhook_present() {
  let state = Arc::new(test_state());
  let hook = state.webhook_store.lock().await.create(
    "hook".to_string(),
    "https://127.0.0.1:1/never".to_string(),
    Vec::new(),
    None,
    WebhookFormat::Generic,
    None,
  );
  state
    .webhook_deliveries
    .lock()
    .await
    .record(delivery("d1", &hook.id, None));
  let headers = admin_headers(&state).await;
  let resp = webhook_redeliver_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("d1".to_string()),
  )
  .await;
  // Queued: the actual HTTP retry runs in a spawned task.
  assert_eq!(resp.status(), StatusCode::ACCEPTED);
  let body = json_body(resp).await;
  assert_eq!(body["queued"], true);
}

#[tokio::test]
async fn selected_org_deliveries_scoped() {
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme")
    .unwrap()
    .id
    .clone();
  {
    let mut log = state.webhook_deliveries.lock().await;
    log.record(delivery("mine", "w1", Some(&org)));
    log.record(delivery("theirs", "w1", None));
  }
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let headers = cookie_headers(&token);
  let resp = webhook_deliveries_handler(
    State(state.clone()),
    headers,
    Query(DeliveriesQuery {
      webhook_id: Some("w1".to_string()),
      limit: None,
    }),
  )
  .await;
  assert_eq!(resp.0.len(), 1);
  assert_eq!(resp.0[0].id, "mine");
}
