//! Tests for the maintenance-mode dashboard API.

use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;

fn req(hostname: &str, enabled: bool) -> Json<MaintenanceRequest> {
  Json(MaintenanceRequest {
    hostname: hostname.to_string(),
    enabled,
  })
}

/// A master-admin session whose selected org is `org` (so `effective_org`
/// resolves to `Some(org)` while the caller stays a master admin).
async fn master_with_org(state: &AppState, org: &str) -> HeaderMap {
  let token = seed_session(state, Role::Admin, None, Some(org.to_string())).await;
  cookie_headers(&token)
}

#[tokio::test]
async fn list_empty_by_default() {
  let state = Arc::new(test_state());
  let resp = maintenance_list_handler(State(state.clone()), admin_headers(&state).await).await;
  assert_eq!(resp.0.len(), 0);
}

#[tokio::test]
async fn list_is_org_scoped_and_sorted() {
  let state = Arc::new(test_state());
  {
    let mut set = state.maintenance.lock().await;
    // Two flags owned by the master org (None), one owned by "acme".
    set.insert("zeta.example".to_string(), None);
    set.insert("alpha.example".to_string(), None);
    set.insert("other.example".to_string(), Some("acme".to_string()));
  }

  // Master admin (org None) sees only its two flags, sorted.
  let resp = maintenance_list_handler(State(state.clone()), admin_headers(&state).await).await;
  assert_eq!(resp.0, vec!["alpha.example", "zeta.example"]);

  // A caller scoped to "acme" sees only the acme flag.
  let headers = master_with_org(&state, "acme").await;
  let resp = maintenance_list_handler(State(state.clone()), headers).await;
  assert_eq!(resp.0, vec!["other.example"]);
}

#[tokio::test]
async fn enable_wildcard_by_master_org() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req(" * ", true),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.maintenance.lock().await.contains_key("*"));
}

#[tokio::test]
async fn enable_wildcard_forbidden_for_non_master_org() {
  let state = Arc::new(test_state());
  let headers = master_with_org(&state, "acme").await;
  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("*", true),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(state.maintenance.lock().await.is_empty());
}

#[tokio::test]
async fn enable_invalid_hostname_is_bad_request() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("bad_host!", true),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn enable_specific_hostname_not_served_forbidden() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  // No client serves this hostname → refused.
  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("example.com", true),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(state.maintenance.lock().await.is_empty());
}

#[tokio::test]
async fn enable_specific_hostname_served_ok() {
  let state = Arc::new(test_state());
  // A master-org client serving example.com.
  state.clients.lock().await.insert(
    "c1".to_string(),
    mock_client(Some("example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;

  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("example.com", true),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let set = state.maintenance.lock().await;
  assert_eq!(set.get("example.com"), Some(&None));
}

#[tokio::test]
async fn enable_other_orgs_hostname_refused() {
  let state = Arc::new(test_state());
  // Client serves example.com but belongs to the master org (org_id None).
  state.clients.lock().await.insert(
    "c1".to_string(),
    mock_client(Some("example.com"), None, None, None),
  );
  // Caller scoped to "acme" — a different org from the client's None.
  let headers = master_with_org(&state, "acme").await;

  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("example.com", true),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(state.maintenance.lock().await.is_empty());
}

#[tokio::test]
async fn enable_twice_is_idempotent() {
  let state = Arc::new(test_state());
  state.clients.lock().await.insert(
    "c1".to_string(),
    mock_client(Some("example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;

  for _ in 0..2 {
    let resp = maintenance_set_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      headers.clone(),
      req("example.com", true),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
  }
  // Still a single flag; the second enable was a no-op (no audit/emit).
  assert_eq!(state.maintenance.lock().await.len(), 1);
}

#[tokio::test]
async fn disable_removes_own_flag() {
  let state = Arc::new(test_state());
  state
    .maintenance
    .lock()
    .await
    .insert("example.com".to_string(), None);
  let headers = admin_headers(&state).await;

  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("example.com", false),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.maintenance.lock().await.is_empty());
}

#[tokio::test]
async fn disable_other_orgs_flag_is_noop() {
  let state = Arc::new(test_state());
  // Flag owned by "acme"; master org (None) tries to clear it.
  state
    .maintenance
    .lock()
    .await
    .insert("example.com".to_string(), Some("acme".to_string()));
  let headers = admin_headers(&state).await;

  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("example.com", false),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  // Left untouched.
  assert_eq!(
    state.maintenance.lock().await.get("example.com"),
    Some(&Some("acme".to_string()))
  );
}

#[tokio::test]
async fn disable_absent_flag_is_noop() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("nope.example", false),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.maintenance.lock().await.is_empty());
}

#[tokio::test]
async fn disable_wildcard_removes_own_flag() {
  let state = Arc::new(test_state());
  state.maintenance.lock().await.insert("*".to_string(), None);
  let headers = admin_headers(&state).await;
  let resp = maintenance_set_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req("*", false),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.maintenance.lock().await.is_empty());
}
