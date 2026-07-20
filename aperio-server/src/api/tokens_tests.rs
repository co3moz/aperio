//! Tests for the dynamic API-token dashboard endpoints (create / list /
//! update / rotate / refresh / revoke) plus the `validate_token_perms` helper.
//! Handlers are called directly with forged authenticated requests.

use super::*;
use crate::store::users::Role;
use crate::test_support::{
  admin_headers, cookie_headers, json_body, mock_client, seed_session, test_config, test_peer,
  test_state, test_state_with,
};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderValue;
use std::sync::Arc;

// ----- helpers -------------------------------------------------------------

fn create_req(name: &str) -> TokenCreateRequest {
  TokenCreateRequest {
    name: name.to_string(),
    hostnames: Vec::new(),
    paths: Vec::new(),
    allowed_ips: Vec::new(),
    ttl_seconds: None,
    max_rps: None,
    daily_max_bytes: None,
    allow_public: false,
    canary: false,
  }
}

fn empty_update() -> TokenUpdateRequest {
  TokenUpdateRequest {
    name: None,
    hostnames: None,
    paths: None,
    allowed_ips: None,
    ttl_seconds: None,
    max_rps: None,
    daily_max_bytes: None,
    allow_public: None,
    canary: None,
  }
}

/// Seeds a token directly in the store within the given org, returning its id.
async fn seed_token(state: &AppState, name: &str, org: Option<String>) -> String {
  let (record, _secret) = state.token_store.lock().await.create(
    name.to_string(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    None,
    None,
    None,
    false,
    false,
    org,
  );
  record.id
}

// ----- validate_token_perms ------------------------------------------------

#[test]
fn perms_normalizes_and_defaults_ip() {
  // Empty / whitespace entries are skipped; "*" and valid entries kept; the
  // allowed-IP list defaults to 0.0.0.0/0 when nothing valid remains.
  let (hosts, paths, ips) = validate_token_perms(
    &["".into(), "  ".into(), "*".into(), "a.example.com".into()],
    &["".into(), "*".into(), "/api".into()],
    &[],
  )
  .unwrap();
  assert!(hosts.contains(&"*".to_string()));
  assert!(hosts.iter().any(|h| h.contains("example.com")));
  assert!(paths.contains(&"*".to_string()));
  assert!(paths.iter().any(|p| p.contains("api")));
  assert_eq!(ips, vec!["0.0.0.0/0".to_string()]);
}

#[test]
fn perms_accepts_valid_ip_and_cidr() {
  let (_, _, ips) =
    validate_token_perms(&[], &[], &["10.0.0.1".into(), "192.168.0.0/16".into()]).unwrap();
  assert_eq!(
    ips,
    vec!["10.0.0.1".to_string(), "192.168.0.0/16".to_string()]
  );
}

#[test]
fn perms_rejects_bad_entries() {
  assert!(validate_token_perms(&["not a host!".into()], &[], &[]).is_err());
  assert!(validate_token_perms(&[], &["/../etc".into()], &[]).is_err());
  assert!(validate_token_perms(&[], &[], &["999.999.999.999".into()]).is_err());
}

// ----- create --------------------------------------------------------------

#[tokio::test]
async fn create_full_roundtrip_and_list() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let mut req = create_req("ci-token");
  req.hostnames = vec!["a.example.com".into()];
  req.paths = vec!["*".into()];
  req.allowed_ips = vec!["10.0.0.0/8".into()];
  req.ttl_seconds = Some(3600);
  req.max_rps = Some(5.0);
  req.daily_max_bytes = Some(1024);
  req.allow_public = true;
  req.canary = true;

  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["name"], "ci-token");
  assert!(body["token"].as_str().unwrap().starts_with("apr_"));
  assert!(body["expires_at"].is_number());
  let id = body["id"].as_str().unwrap().to_string();

  // It appears in the list with its metadata (no secret).
  let resp = tokens_list_handler(State(state.clone()), headers.clone()).await;
  let list = resp.0;
  assert_eq!(list.len(), 1);
  assert_eq!(list[0].id, id);
  assert!(list[0].allow_public);
  assert!(list[0].canary);
  assert_eq!(list[0].max_rps, Some(5.0));
  assert_eq!(list[0].daily_max_bytes, Some(1024));
  assert!(!list[0].expired);
}

#[tokio::test]
async fn create_rejects_bad_name() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(create_req("   ")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(create_req(&"x".repeat(65))),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_rejects_bad_perms_and_rps() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let mut bad_host = create_req("k");
  bad_host.hostnames = vec!["bad host!".into()];
  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(bad_host),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  let mut bad_rps = create_req("k");
  bad_rps.max_rps = Some(-1.0);
  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(bad_rps),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  let mut nan_rps = create_req("k");
  nan_rps.max_rps = Some(f64::NAN);
  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(nan_rps),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_forbidden_when_org_quota_reached() {
  let state = Arc::new(test_state());
  // Create an org, cap it at one token, and give the master admin a session
  // that has selected that org (so the created token lands in it).
  let org_id = state
    .org_store
    .lock()
    .await
    .create("acme")
    .unwrap()
    .id
    .clone();
  state
    .org_store
    .lock()
    .await
    .set_quota(&org_id, None, Some(Some(1)), None, None);
  let token = seed_session(&state, Role::Admin, None, Some(org_id.clone())).await;
  let headers = cookie_headers(&token);

  // First create fills the quota.
  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(create_req("first")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // Second create is refused with 403.
  let resp = tokens_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(create_req("second")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ----- list org scoping ----------------------------------------------------

#[tokio::test]
async fn list_is_scoped_to_effective_org() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await; // effective org = master (None)
  seed_token(&state, "mine", None).await;
  seed_token(&state, "theirs", Some("other-org".to_string())).await;

  let resp = tokens_list_handler(State(state.clone()), headers).await;
  let list = resp.0;
  assert_eq!(list.len(), 1);
  assert_eq!(list[0].name, "mine");
}

// ----- update --------------------------------------------------------------

#[tokio::test]
async fn update_all_fields_success() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let id = seed_token(&state, "orig", None).await;

  let mut req = empty_update();
  req.name = Some("renamed".into());
  req.hostnames = Some(vec!["b.example.com".into()]);
  req.paths = Some(vec!["/api".into()]);
  req.allowed_ips = Some(vec!["10.0.0.1".into()]);
  req.ttl_seconds = Some(7200);
  req.max_rps = Some(2.0);
  req.daily_max_bytes = Some(500);
  req.allow_public = Some(true);
  req.canary = Some(true);

  let resp = tokens_update_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["status"], "ok");

  let store = state.token_store.lock().await;
  let t = store.list().iter().find(|t| t.id == id).unwrap();
  assert_eq!(t.name, "renamed");
  assert_eq!(t.max_rps, Some(2.0));
  assert_eq!(t.daily_max_bytes, Some(500));
  assert!(t.allow_public);
  assert!(t.canary);
  assert!(t.expires_at.is_some());
}

#[tokio::test]
async fn update_ttl_zero_clears_expiry() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  // Seed a token that expires, then clear its expiry with ttl_seconds = 0.
  let (record, _s) = state.token_store.lock().await.create(
    "ttl".to_string(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    Some(3600),
    None,
    None,
    false,
    false,
    None,
  );
  let mut req = empty_update();
  req.ttl_seconds = Some(0);
  let resp = tokens_update_handler(
    State(state.clone()),
    Path(record.id.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let store = state.token_store.lock().await;
  let t = store.list().iter().find(|t| t.id == record.id).unwrap();
  assert!(t.expires_at.is_none());
}

#[tokio::test]
async fn update_rejects_bad_input() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let id = seed_token(&state, "orig", None).await;

  // Bad name.
  let mut req = empty_update();
  req.name = Some("  ".into());
  let resp = tokens_update_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Bad perms.
  let mut req = empty_update();
  req.paths = Some(vec!["/../x".into()]);
  let resp = tokens_update_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Bad max_rps.
  let mut req = empty_update();
  req.max_rps = Some(-3.0);
  let resp = tokens_update_handler(
    State(state.clone()),
    Path(id),
    ConnectInfo(test_peer()),
    headers,
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_unknown_and_cross_org_are_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  // Unknown id.
  let resp = tokens_update_handler(
    State(state.clone()),
    Path("no-such-id".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(empty_update()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // Cross-org id (exists but not in the caller's effective org).
  let other = seed_token(&state, "theirs", Some("other-org".to_string())).await;
  let resp = tokens_update_handler(
    State(state.clone()),
    Path(other),
    ConnectInfo(test_peer()),
    headers,
    Json(empty_update()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ----- rotate --------------------------------------------------------------

#[tokio::test]
async fn rotate_success_returns_new_secret() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let id = seed_token(&state, "rot", None).await;

  let resp = tokens_rotate_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(TokenRotateRequest {
      grace_seconds: 3600,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["id"], id);
  assert!(body["token"].as_str().unwrap().starts_with("apr_"));
  assert!(body["prev_expires_at"].is_number());
}

#[tokio::test]
async fn rotate_rejects_excessive_grace() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let id = seed_token(&state, "rot", None).await;
  let resp = tokens_rotate_handler(
    State(state.clone()),
    Path(id),
    ConnectInfo(test_peer()),
    headers,
    Json(TokenRotateRequest {
      grace_seconds: 400 * 24 * 3600,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rotate_unknown_and_cross_org_are_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let resp = tokens_rotate_handler(
    State(state.clone()),
    Path("nope".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(TokenRotateRequest { grace_seconds: 0 }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  let other = seed_token(&state, "theirs", Some("other-org".to_string())).await;
  let resp = tokens_rotate_handler(
    State(state.clone()),
    Path(other),
    ConnectInfo(test_peer()),
    headers,
    Json(TokenRotateRequest { grace_seconds: 0 }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ----- refresh -------------------------------------------------------------

fn bearer(secret: &str) -> HeaderMap {
  let mut h = HeaderMap::new();
  h.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
  );
  h
}

#[tokio::test]
async fn refresh_success_slides_expiry() {
  let state = Arc::new(test_state());
  let (_record, secret) = state.token_store.lock().await.create(
    "ci".to_string(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    Some(3600),
    None,
    None,
    false,
    false,
    None,
  );
  let resp = tokens_refresh_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    bearer(&secret),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["status"], "ok");
  assert!(body["expires_at"].is_number());
}

#[tokio::test]
async fn refresh_without_secret_is_401() {
  let state = Arc::new(test_state());
  let resp = tokens_refresh_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_unknown_secret_is_401() {
  let state = Arc::new(test_state());
  let resp = tokens_refresh_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    bearer("apr_unknown"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_rate_limited_is_429() {
  // A config with a zero-token bucket rejects the very first refresh attempt.
  let mut cfg = test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let state = Arc::new(test_state_with(cfg));
  let resp = tokens_refresh_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    bearer("apr_whatever"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ----- revoke --------------------------------------------------------------

#[tokio::test]
async fn revoke_success_and_disconnects_clients() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let id = seed_token(&state, "victim", None).await;

  // Register a live client bound to this token so the disconnect branch runs.
  let mut handle = mock_client(None, None, None, None);
  handle.perms.token_id = Some(id.clone());
  state
    .clients
    .lock()
    .await
    .insert("client-1".to_string(), handle);

  let resp = tokens_revoke_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["status"], "ok");
  // The token is gone from the store.
  assert!(state.token_store.lock().await.list().is_empty());
}

#[tokio::test]
async fn revoke_unknown_and_cross_org_are_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let resp = tokens_revoke_handler(
    State(state.clone()),
    Path("nope".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  let other = seed_token(&state, "theirs", Some("other-org".to_string())).await;
  let resp = tokens_revoke_handler(
    State(state.clone()),
    Path(other),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
