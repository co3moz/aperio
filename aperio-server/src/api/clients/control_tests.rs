//! The two operator controls, and mostly what they refuse: an overrule of the
//! binds a client declared, the kill switch over one of its services, and the
//! organization fence that hides both from a caller in another org.

use super::super::clients_tests::*;
use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::Json;
use axum::extract::State;
use axum::extract::{ConnectInfo, Path};
use std::sync::Arc;

fn override_req(hostname: Option<&str>, path: Option<&str>) -> Json<ClientOverrideRequest> {
  Json(ClientOverrideRequest {
    hostname_bind: hostname.map(|s| s.to_string()),
    hostname_binds: None,
    path_bind: path.map(|s| s.to_string()),
  })
}

/// The list form of the payload, as the dashboard's overrule dialog sends it.
fn override_list_req(hostnames: &[&str]) -> Json<ClientOverrideRequest> {
  Json(ClientOverrideRequest {
    hostname_bind: None,
    hostname_binds: Some(hostnames.iter().map(|s| s.to_string()).collect()),
    path_bind: None,
  })
}

#[tokio::test]
async fn override_unknown_client_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = client_override_handler(
    State(state.clone()),
    Path("nope".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some("h.example.com"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn override_set_then_clear() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let headers = admin_headers(&state).await;

  // Set both binds.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_req(Some("New.Example.com"), Some("api/v2")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  {
    let clients = state.clients.read().await;
    let h = clients.get("c1").unwrap();
    assert_eq!(
      h.sole().override_hostname_binds,
      vec!["new.example.com".to_string()]
    );
    assert_eq!(h.sole().override_path_bind.as_deref(), Some("/api/v2"));
  }

  // Clear both (empty string and null).
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some(""), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  {
    let clients = state.clients.read().await;
    let h = clients.get("c1").unwrap();
    assert!(h.sole().override_hostname_binds.is_empty());
    assert!(h.sole().override_path_bind.is_none());
  }
}

#[tokio::test]
async fn override_accepts_a_list_of_hostnames() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let headers = admin_headers(&state).await;

  // The dashboard sends one entry per bind row, so an operator can retarget
  // the name the client declared while keeping the random subdomain. Entries
  // are normalized, blanks dropped, and duplicates collapsed.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_list_req(&[
      "New.Example.com",
      "",
      "wild-fox.tunnel.example.com",
      "new.example.com",
    ]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds,
    vec![
      "new.example.com".to_string(),
      "wild-fox.tunnel.example.com".to_string()
    ]
  );

  // One invalid entry rejects the whole list, leaving the override untouched.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_list_req(&["ok.example.com", "bad host"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  assert_eq!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds
      .len(),
    2
  );

  // An empty list clears it, the same as an empty string in the single form.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    override_list_req(&[]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds
      .is_empty()
  );
}

#[tokio::test]
async fn override_rejects_invalid_values() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let headers = admin_headers(&state).await;

  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_req(Some("bad_host!"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    override_req(None, Some("/foo/../bar")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn override_cross_org_client_is_404() {
  let state = Arc::new(test_state());
  // Client belongs to org "acme"; the master admin's effective org is None.
  insert_client(&state, "c1", |h| {
    h.perms.org_id = Some("acme".to_string());
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some("h.example.com"), None),
  )
  .await;
  assert_eq!(
    resp.status(),
    StatusCode::NOT_FOUND,
    "cross-org client hidden as 404"
  );
  // The override must not have been applied.
  let clients = state.clients.read().await;
  assert!(
    clients
      .get("c1")
      .unwrap()
      .sole()
      .override_hostname_binds
      .is_empty()
  );
}

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
async fn override_refuses_a_hostname_outside_the_org_allowlist() {
  let state = Arc::new(test_state());
  // A fenced org, a client in it, and an admin session that has selected it.
  let org_id = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["*.acme.com".to_string()], None)
    .unwrap()
    .id;
  insert_client(&state, "c1", |h| {
    h.perms.org_id = Some(org_id.clone());
  })
  .await;
  let token = seed_session(&state, Role::Admin, None, Some(org_id.clone())).await;
  let headers = cookie_headers(&token);

  // An overrule is the one bind with no token permission behind it, so the
  // org fence has to hold here too.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_req(Some("evil.example.com"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds
      .is_empty()
  );

  // Inside the fence it applies as before.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::Query(Default::default()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some("app.acme.com"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds,
    vec!["app.acme.com".to_string()]
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
