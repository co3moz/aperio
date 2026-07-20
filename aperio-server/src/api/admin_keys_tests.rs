//! Tests for the programmatic admin-keys dashboard API.

use super::*;
use crate::test_support::{admin_headers, json_body, test_peer, test_state};
use axum::extract::{ConnectInfo, Path, State};
use std::sync::Arc;

fn create_req(
  name: &str,
  role: &str,
  org_id: Option<&str>,
  ttl: Option<u64>,
) -> Json<AdminKeyCreateRequest> {
  Json(AdminKeyCreateRequest {
    name: name.to_string(),
    role: role.to_string(),
    org_id: org_id.map(|s| s.to_string()),
    ttl_seconds: ttl,
  })
}

#[tokio::test]
async fn list_requires_master_admin() {
  let state = Arc::new(test_state());
  // No session → 401.
  let resp = admin_keys_list_handler(State(state.clone()), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_list_and_revoke_roundtrip() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  // Create a key.
  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("ci-key", "operator", None, Some(3600)),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["name"], "ci-key");
  assert_eq!(body["role"], "operator");
  assert!(
    body["key"].as_str().unwrap().len() > 8,
    "secret returned once"
  );
  let id = body["id"].as_str().unwrap().to_string();

  // It shows up in the list.
  let resp = admin_keys_list_handler(State(state.clone()), headers.clone()).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let list = json_body(resp).await;
  assert_eq!(list.as_array().unwrap().len(), 1);
  assert_eq!(list[0]["id"], id);
  assert_eq!(list[0]["expired"], false);

  // Revoke it.
  let resp = admin_keys_revoke_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // Now the list is empty and a second revoke is a 404.
  let resp = admin_keys_list_handler(State(state.clone()), headers.clone()).await;
  assert_eq!(json_body(resp).await.as_array().unwrap().len(), 0);

  let resp = admin_keys_revoke_handler(
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
  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("  ", "admin", None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Name too long (>64).
  let long = "x".repeat(65);
  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req(&long, "admin", None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Invalid role.
  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("k", "superuser", None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Unknown organization.
  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    create_req("k", "admin", Some("no-such-org"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_accepts_valid_org() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = state
    .org_store
    .lock()
    .await
    .create("acme")
    .unwrap()
    .id
    .clone();

  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    create_req("scoped", "viewer", Some(&org_id), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["org_id"], org_id);
  assert_eq!(body["expires_at"], serde_json::Value::Null);
}

#[tokio::test]
async fn create_and_revoke_require_master_admin() {
  let state = Arc::new(test_state());
  // Non-admin (viewer) session is forbidden.
  let token = crate::test_support::seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let headers = crate::test_support::cookie_headers(&token);

  let resp = admin_keys_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    create_req("k", "admin", None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);

  let resp = admin_keys_revoke_handler(
    State(state.clone()),
    Path("x".to_string()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
