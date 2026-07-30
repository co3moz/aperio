//! Tests for the programmatic tunnel provisioning API (create + delete).

use super::*;
use crate::store::users::Role;
use crate::test_support::{
  admin_headers, cookie_headers, json_body, master_token_headers, seed_session, test_config,
  test_peer, test_state, test_state_with,
};
use axum::extract::{ConnectInfo, Path, State};

fn req(
  name: Option<&str>,
  hostname: Option<&str>,
  allowed_ips: Vec<String>,
  ttl: Option<u64>,
) -> Json<TunnelCreateRequest> {
  Json(TunnelCreateRequest {
    name: name.map(|s| s.to_string()),
    hostname: hostname.map(|s| s.to_string()),
    allowed_ips,
    ttl_seconds: ttl,
  })
}

#[tokio::test]
async fn create_requires_auth() {
  let state = Arc::new(test_state());
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    req(None, Some("svc.example.com"), vec![], None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_rejects_viewer_role() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let headers = cookie_headers(&token);
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req(None, Some("svc.example.com"), vec![], None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_with_master_token_succeeds() {
  let state = Arc::new(test_state());
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(
      Some("pr-preview"),
      Some("svc.example.com"),
      vec![],
      Some(60),
    ),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["name"], "pr-preview");
  assert_eq!(body["hostname"], "svc.example.com");
  assert_eq!(body["url"], "https://svc.example.com");
  assert!(body["token"].as_str().unwrap().starts_with("apr_"));
  // The token was persisted.
  assert_eq!(state.token_store.lock().await.list().len(), 1);
}

#[tokio::test]
async fn create_rejects_bad_ttl() {
  let state = Arc::new(test_state());
  // Zero.
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(None, Some("svc.example.com"), vec![], Some(0)),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  // Too large (> 7 days).
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(None, Some("svc.example.com"), vec![], Some(8 * 24 * 3600)),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_rejects_long_name() {
  let state = Arc::new(test_state());
  let long = "x".repeat(65);
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(Some(&long), Some("svc.example.com"), vec![], None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_rejects_invalid_hostname() {
  let state = Arc::new(test_state());
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(None, Some("bad host!"), vec![], None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_rejects_invalid_allowed_ip() {
  let state = Arc::new(test_state());
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(
      None,
      Some("svc.example.com"),
      vec!["not-an-ip".to_string()],
      None,
    ),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_without_hostname_and_no_random_is_400() {
  let state = Arc::new(test_state());
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(None, None, vec![], None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_with_random_subdomain_succeeds() {
  let mut config = test_config();
  config.random_subdomain_suffix = Some("*.preview.example.com".to_string());
  let state = Arc::new(test_state_with(config));
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    master_token_headers(),
    req(None, None, vec!["10.0.0.0/8".to_string()], None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let hostname = body["hostname"].as_str().unwrap();
  assert!(hostname.ends_with(".preview.example.com"));
  assert_eq!(body["name"], "tunnel"); // default name when omitted/blank.
}

#[tokio::test]
async fn delete_requires_auth() {
  let state = Arc::new(test_state());
  let resp = tunnels_delete_handler(
    State(state.clone()),
    Path("x".to_string()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_unknown_id_is_404() {
  let state = Arc::new(test_state());
  let resp = tunnels_delete_handler(
    State(state.clone()),
    Path("no-such".to_string()),
    ConnectInfo(test_peer()),
    master_token_headers(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_then_delete_roundtrip() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  // Create via dashboard admin session.
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    req(Some("t1"), Some("svc.example.com"), vec![], Some(120)),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let id = json_body(resp).await["id"].as_str().unwrap().to_string();

  // Delete it.
  let resp = tunnels_delete_handler(
    State(state.clone()),
    Path(id.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // Second delete: the token is now revoked, so the org scan misses it -> 404.
  let resp = tunnels_delete_handler(
    State(state.clone()),
    Path(id),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_is_org_scoped() {
  let state = Arc::new(test_state());
  // A tunnel token owned by "other" org.
  let (record, _secret) = state.token_store.lock().await.create(
    "foreign".to_string(),
    vec!["svc.example.com".to_string()],
    Vec::new(),
    vec!["0.0.0.0/0".to_string()],
    Some(60),
    None,
    None,
    false,
    false,
    false,
    Some("other".to_string()),
    Vec::new(),
    None,
  );
  // A master-admin session (master org) cannot see the foreign token.
  let headers = admin_headers(&state).await;
  let resp = tunnels_delete_handler(
    State(state.clone()),
    Path(record.id),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_refuses_a_hostname_outside_the_org_allowlist() {
  let mut config = test_config();
  config.random_subdomain_suffix = Some("*.preview.example.com".to_string());
  let state = Arc::new(test_state_with(config));
  let org_id = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["*.acme.com".to_string()], None)
    .unwrap()
    .id;
  let token = seed_session(&state, Role::Admin, None, Some(org_id)).await;
  let headers = cookie_headers(&token);

  // An explicitly requested hostname must be one the org may claim.
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    req(None, Some("evil.example.com"), Vec::new(), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(state.token_store.lock().await.list().is_empty());

  // Inside the fence it is provisioned.
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    req(None, Some("pr-7.acme.com"), Vec::new(), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // A server-generated random subdomain is exempt: the caller cannot choose
  // it, so it can never be another tenant's hostname.
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req(None, None, Vec::new(), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert!(
    body["hostname"]
      .as_str()
      .unwrap()
      .ends_with(".preview.example.com")
  );
}

#[tokio::test]
async fn create_respects_the_org_token_quota() {
  let mut config = test_config();
  config.random_subdomain_suffix = Some("*.preview.example.com".to_string());
  let state = Arc::new(test_state_with(config));
  let org_id = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  state
    .org_store
    .lock()
    .await
    .set_quota(&org_id, None, Some(Some(1)), None, None);
  let token = seed_session(&state, Role::Admin, None, Some(org_id)).await;
  let headers = cookie_headers(&token);

  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    req(None, None, Vec::new(), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // These are real credentials in the same store as dynamic tokens, so an org
  // capped at one may not mint a second one here either. Before the check,
  // this endpoint ignored the cap entirely.
  let resp = tunnels_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    req(None, None, Vec::new(), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert_eq!(state.token_store.lock().await.list().len(), 1);
}
