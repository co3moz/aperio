//! Tests for the programmatic tunnel API: the listing's gate and scope, and
//! provisioning (create + delete).

use super::*;
use crate::protocol::TunnelDecl;
use crate::state::ClientPerms;
use crate::store::tokens::TokenSpec;
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

/// A connected client in `org` announcing one tunnel named `name`, keyed by
/// `cid` in the client map.
async fn declare(state: &Arc<AppState>, cid: &str, org: &str, name: &str) {
  let mut c = crate::test_support::mock_client(None, None, None, None);
  c.perms = ClientPerms {
    master: false,
    hostnames: Vec::new(),
    paths: Vec::new(),
    token_name: Some(format!("tok-{cid}")),
    token_id: Some(cid.to_string()),
    allow_public: false,
    allow_server_side: false,
    allow_bind: false,
    allow_otel: false,
    topics: Vec::new(),
    org_id: Some(org.to_string()),
    org_hostnames: Vec::new(),
    max_connections: None,
  };
  c.sole_mut().tunnels = vec![TunnelDecl {
    custom_name: None,
    name: Some(name.to_string()),
    target: "10.4.7.19:5432".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }];
  state.clients.write().await.insert(cid.to_string(), c);
}

async fn listed_names(state: &Arc<AppState>, headers: HeaderMap) -> (StatusCode, Vec<String>) {
  let resp =
    tunnels_declared_handler(State(state.clone()), ConnectInfo(test_peer()), headers).await;
  let status = resp.status();
  if status != StatusCode::OK {
    return (status, Vec::new());
  }
  let body = json_body(resp).await;
  let names = body
    .as_array()
    .expect("the listing is an array")
    .iter()
    .map(|t| t["name"].as_str().unwrap_or_default().to_string())
    .collect();
  (status, names)
}

/// The listing sits outside the dashboard's session middleware, so the
/// handler's own check is its only gate. Without one, an anonymous caller had
/// no organization, no organization is the master view, and the response was
/// every tenant's internal targets.
#[tokio::test]
async fn list_requires_auth() {
  let state = Arc::new(test_state());
  declare(&state, "a1", "org-a", "pg_main").await;
  declare(&state, "b1", "org-b", "redis").await;

  let (status, names) = listed_names(&state, HeaderMap::new()).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert!(names.is_empty());

  // A wrong master token is refused like no token at all.
  let mut wrong = HeaderMap::new();
  wrong.insert("authorization", "Bearer not-the-token".parse().unwrap());
  let (status, _) = listed_names(&state, wrong).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);

  // A visitor session (a proxied site's own password) is not a dashboard
  // session either, even though it carries the same cookie name.
  let now = crate::store::sessions::now_secs();
  let visitor = "visitor-session".to_string();
  state.sessions.lock().await.insert(
    &visitor,
    crate::store::sessions::SessionInfo {
      plane: crate::store::sessions::Plane::Visitor,
      expires_at: now + 86400,
      created_at: now,
      ip: None,
      user_agent: None,
      scope_host: None,
      username: None,
      role: Role::Viewer,
      selected_org: None,
      bound_org: None,
    },
  );
  let (status, _) = listed_names(&state, cookie_headers(&visitor)).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert!(
    state
      .audit
      .lock()
      .await
      .recent()
      .iter()
      .any(|e| e.event == "tunnel_denied"),
    "a refused listing is audited like a refused provisioning"
  );
}

/// Reading is a Viewer's right: the listing shows what exists, binding it
/// needs a tunnel token, which is a separate credential.
#[tokio::test]
async fn list_admits_a_viewer_session() {
  let state = Arc::new(test_state());
  declare(&state, "a1", "org-a", "pg_main").await;
  let token = seed_session(&state, Role::Viewer, None, None).await;
  let (status, names) = listed_names(&state, cookie_headers(&token)).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(names, vec!["pg_main".to_string()]);
}

/// The master token in a header lists everything, the same way it provisions:
/// CI reaches this endpoint with no browser login.
#[tokio::test]
async fn list_admits_the_master_token() {
  let state = Arc::new(test_state());
  declare(&state, "a1", "org-a", "pg_main").await;
  declare(&state, "b1", "org-b", "redis").await;
  let (status, mut names) = listed_names(&state, master_token_headers()).await;
  assert_eq!(status, StatusCode::OK);
  names.sort();
  assert_eq!(names, vec!["pg_main".to_string(), "redis".to_string()]);
}

/// A session acting in one organization sees that organization's tunnels and
/// not its neighbour's.
#[tokio::test]
async fn list_is_scoped_to_the_callers_organization() {
  let state = Arc::new(test_state());
  declare(&state, "a1", "org-a", "pg_main").await;
  declare(&state, "b1", "org-b", "redis").await;
  let token = seed_session(&state, Role::Admin, None, Some("org-a".to_string())).await;
  let (status, names) = listed_names(&state, cookie_headers(&token)).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(names, vec!["pg_main".to_string()]);
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
  let (record, _secret) = state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "foreign".to_string(),
      hostnames: vec!["svc.example.com".to_string()],
      allowed_ips: vec!["0.0.0.0/0".to_string()],
      ttl_seconds: Some(60),
      org_id: Some("other".to_string()),
      ..Default::default()
    })
    .expect("the test store can be written to");
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
    .set_quota(&org_id, None, Some(Some(1)), None, None)
    .expect("the test store can be written to");
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
