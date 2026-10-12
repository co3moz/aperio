//! The operator's kill switch over one service of a connected client, and the
//! organization fence that hides it from a caller in another org.

use super::super::clients_tests::*;
use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::Json;
use axum::extract::State;
use axum::extract::{ConnectInfo, Path};
use std::sync::Arc;

#[tokio::test]
async fn enabled_toggle_and_unknown() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| h.sole_mut().admin_enabled = true).await;
  let headers = admin_headers(&state).await;

  // Disable.
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    !state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );

  // Re-enable.
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(ClientEnabledRequest { enabled: true }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );

  // Unknown client.
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("ghost".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    Json(ClientEnabledRequest { enabled: true }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn enabled_cross_org_client_is_404() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.perms.org_id = Some("acme".to_string());
    h.sole_mut().admin_enabled = true;
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
  // Untouched.
  assert!(
    state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );
}

#[tokio::test]
async fn enabling_an_unknown_or_foreign_client_is_not_found() {
  // The kill switch answers 404 both for an id that does not exist and for
  // another organization's client: a tenant must not learn which is which.
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;

  let resp = client_enabled_handler(
    State(state.clone()),
    Path("missing".to_string()),
    axum::extract::Query(Default::default()),
    axum::extract::ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    axum::extract::ConnectInfo(test_peer()),
    cookie_headers(&token),
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
  // And the master client is untouched.
  assert!(
    state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );
}
