//! Tests for the organization-management dashboard API.

use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderValue;

/// Creates a child org directly in the store and returns its id.
async fn make_org(state: &Arc<AppState>, name: &str) -> String {
  state
    .org_store
    .lock()
    .await
    .create(name, Vec::new(), None)
    .unwrap()
    .id
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_requires_master_admin() {
  let state = Arc::new(test_state());
  // No session → 401.
  let resp = orgs_list_handler(State(state.clone()), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

  // Non-admin session → 403.
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let resp = orgs_list_handler(State(state.clone()), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_reports_master_and_child_counts() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  // A user + token in master, and a user + token in the child org.
  state
    .users
    .lock()
    .await
    .create("master-user", "password1", Role::Viewer, None)
    .unwrap();
  state.token_store.lock().await.create(
    "master-tok".into(),
    vec![],
    vec![],
    vec![],
    None,
    None,
    None,
    false,
    false,
    false,
    None,
    Vec::new(),
  );
  state
    .users
    .lock()
    .await
    .create(
      "child-user",
      "password1",
      Role::Viewer,
      Some(org_id.clone()),
    )
    .unwrap();
  state.token_store.lock().await.create(
    "child-tok".into(),
    vec![],
    vec![],
    vec![],
    None,
    None,
    None,
    false,
    false,
    false,
    Some(org_id.clone()),
    Vec::new(),
  );

  let resp = orgs_list_handler(State(state.clone()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let arr = body.as_array().unwrap();
  assert_eq!(arr.len(), 2);

  let master = &arr[0];
  assert_eq!(master["id"], MASTER_ID);
  assert_eq!(master["master"], true);
  assert_eq!(master["users"], 1);
  assert_eq!(master["tokens"], 1);

  let child = &arr[1];
  assert_eq!(child["id"], org_id);
  assert_eq!(child["master"], false);
  assert_eq!(child["users"], 1);
  assert_eq!(child["tokens"], 1);
  assert!(child["created_at"].is_number());
}

// ---------------------------------------------------------------------------
// select
// ---------------------------------------------------------------------------

#[tokio::test]
async fn select_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = orgs_select_handler(
    State(state.clone()),
    HeaderMap::new(),
    Json(OrgSelectRequest { id: None }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn select_master_via_synthetic_and_empty_and_none() {
  let state = Arc::new(test_state());
  for id in [None, Some(String::new()), Some(MASTER_ID.to_string())] {
    let headers = admin_headers(&state).await;
    let resp =
      orgs_select_handler(State(state.clone()), headers, Json(OrgSelectRequest { id })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["selected"], MASTER_ID);
  }
}

#[tokio::test]
async fn select_child_org_success_and_persists_on_session() {
  let state = Arc::new(test_state());
  let org_id = make_org(&state, "acme").await;
  let token = seed_session(&state, Role::Admin, None, None).await;
  let headers = cookie_headers(&token);

  let resp = orgs_select_handler(
    State(state.clone()),
    headers,
    Json(OrgSelectRequest {
      id: Some(org_id.clone()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["selected"], org_id);

  // The selection is stored on the session.
  let sel = state.sessions.lock().await.selected_org(&token);
  assert_eq!(sel, Some(Some(org_id)));
}

#[tokio::test]
async fn select_unknown_org_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_select_handler(
    State(state.clone()),
    headers,
    Json(OrgSelectRequest {
      id: Some("no-such-org".to_string()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn select_without_session_cookie_is_401() {
  // Authenticated as a master-admin via a programmatic admin key (Bearer) so
  // require_master_admin passes but there is no session cookie to mutate.
  let state = Arc::new(test_state());
  let (_key, secret) =
    state
      .admin_key_store
      .lock()
      .await
      .create("k".into(), Role::Admin, None, None);
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
  );

  let resp = orgs_select_handler(
    State(state.clone()),
    headers,
    Json(OrgSelectRequest { id: None }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_requires_master_admin() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
    Json(OrgCreateRequest {
      custom_name: None,
      name: "acme".into(),
      hostnames: Vec::new(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn create_success_and_duplicate() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(OrgCreateRequest {
      custom_name: None,
      name: "acme".into(),
      hostnames: Vec::new(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["name"], "acme");
  assert!(body["id"].as_str().unwrap().len() > 8);

  // Duplicate name → 400.
  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(OrgCreateRequest {
      custom_name: None,
      name: "acme".into(),
      hostnames: Vec::new(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Path("x".into()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_master_is_rejected() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(MASTER_ID.into()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_non_empty_org_is_409() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;
  state
    .users
    .lock()
    .await
    .create("u", "password1", Role::Viewer, Some(org_id.clone()))
    .unwrap();

  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn delete_unknown_org_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("no-such-org".into()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_empty_org_success() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id.clone()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.org_store.lock().await.find(&org_id).is_none());
}

// ---------------------------------------------------------------------------
// quota
// ---------------------------------------------------------------------------

fn quota_req(
  max_clients: Option<u64>,
  max_tokens: Option<u64>,
  max_users: Option<u64>,
  max_bytes_month: Option<u64>,
) -> Json<OrgQuotaRequest> {
  Json(OrgQuotaRequest {
    max_clients,
    max_tokens,
    max_users,
    max_bytes_month,
  })
}

#[tokio::test]
async fn quota_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = orgs_quota_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Path("x".into()),
    quota_req(None, None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn quota_on_master_is_rejected() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_quota_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(MASTER_ID.into()),
    quota_req(Some(1), None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn quota_unknown_org_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_quota_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("no-such-org".into()),
    quota_req(Some(1), None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn quota_set_and_clear() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  // Set: Some(n) sets, Some(0) clears, None leaves unchanged.
  let resp = orgs_quota_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(org_id.clone()),
    quota_req(Some(5), Some(0), Some(10), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["max_clients"], 5);
  assert_eq!(body["max_tokens"], serde_json::Value::Null);
  assert_eq!(body["max_users"], 10);
  assert_eq!(body["max_bytes_month"], serde_json::Value::Null);
}

// ---------------------------------------------------------------------------
// oidc
// ---------------------------------------------------------------------------

fn oidc_req(
  issuer: &str,
  client_id: &str,
  client_secret: &str,
  allowed_emails: Vec<&str>,
) -> Json<OrgOidcRequest> {
  Json(OrgOidcRequest {
    issuer: issuer.into(),
    client_id: client_id.into(),
    client_secret: client_secret.into(),
    allowed_emails: allowed_emails.into_iter().map(String::from).collect(),
  })
}

#[tokio::test]
async fn oidc_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Path("x".into()),
    oidc_req("", "", "", vec![]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_on_master_is_rejected() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(MASTER_ID.into()),
    oidc_req("https://issuer", "cid", "secret", vec!["a@x.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oidc_missing_credentials_is_400() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id),
    oidc_req("https://issuer", "  ", "", vec!["a@x.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oidc_empty_allowed_emails_is_400() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id),
    oidc_req("https://issuer", "cid", "secret", vec!["  ", ""]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oidc_set_then_clear_and_unknown_org() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  // Set a valid OIDC override (emails are trimmed + lowercased).
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(org_id.clone()),
    oidc_req("https://issuer ", " cid ", "secret", vec![" A@X.com "]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["configured"], true);
  assert_eq!(body["id"], org_id);
  let stored = state
    .org_store
    .lock()
    .await
    .find(&org_id)
    .unwrap()
    .oidc
    .clone()
    .unwrap();
  assert_eq!(stored.issuer, "https://issuer");
  assert_eq!(stored.client_id, "cid");
  assert_eq!(stored.allowed_emails, vec!["a@x.com".to_string()]);

  // Empty issuer clears it.
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(org_id.clone()),
    oidc_req("   ", "", "", vec![]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["configured"], false);
  assert!(
    state
      .org_store
      .lock()
      .await
      .find(&org_id)
      .unwrap()
      .oidc
      .is_none()
  );

  // Unknown org → 404.
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("no-such-org".into()),
    oidc_req("https://issuer", "cid", "secret", vec!["a@x.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = orgs_usage_handler(State(state.clone()), HeaderMap::new(), Path("x".into())).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn usage_for_master() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_usage_handler(State(state.clone()), headers, Path(MASTER_ID.into())).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["org_id"], MASTER_ID);
  // Master has no quota.
  assert_eq!(body["quota"], serde_json::Value::Null);
  assert!(body["month"].is_string());
}

#[tokio::test]
async fn usage_for_child_with_quota_and_members() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  // Give the org a quota and members so the counts/quota branches run.
  state
    .org_store
    .lock()
    .await
    .set_quota(&org_id, Some(Some(3)), Some(Some(7)), None, None);
  state
    .users
    .lock()
    .await
    .create("u", "password1", Role::Viewer, Some(org_id.clone()))
    .unwrap();
  state.token_store.lock().await.create(
    "t".into(),
    vec![],
    vec![],
    vec![],
    None,
    None,
    None,
    false,
    false,
    false,
    Some(org_id.clone()),
    Vec::new(),
  );

  let resp = orgs_usage_handler(State(state.clone()), headers, Path(org_id.clone())).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["org_id"], org_id);
  assert_eq!(body["users"], 1);
  assert_eq!(body["tokens"], 1);
  assert_eq!(body["clients"], 0);
  assert_eq!(body["quota"]["max_clients"], 3);
  assert_eq!(body["quota"]["max_tokens"], 7);
}

// ---------------------------------------------------------------------------
// hostname allowlist
// ---------------------------------------------------------------------------

fn hostnames_req(hostnames: &[&str]) -> Json<OrgHostnamesRequest> {
  Json(OrgHostnamesRequest {
    hostnames: hostnames.iter().map(|s| s.to_string()).collect(),
  })
}

#[tokio::test]
async fn create_accepts_an_optional_hostname_allowlist() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(OrgCreateRequest {
      custom_name: None,
      name: "acme".into(),
      // Mixed case, a trailing dot and a duplicate all normalize away.
      hostnames: vec![
        "Acme.COM.".into(),
        "*.acme.example.com".into(),
        "acme.com".into(),
      ],
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(
    body["hostnames"],
    serde_json::json!(["acme.com", "*.acme.example.com"])
  );

  // An invalid pattern is refused before the org is created.
  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(OrgCreateRequest {
      custom_name: None,
      name: "broken".into(),
      hostnames: vec!["app.*.com".into()],
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  assert!(
    !state
      .org_store
      .lock()
      .await
      .list()
      .iter()
      .any(|o| o.name == "broken")
  );
}

#[tokio::test]
async fn create_without_hostnames_leaves_the_org_unfenced() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(OrgCreateRequest {
      custom_name: None,
      name: "acme".into(),
      hostnames: Vec::new(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["hostnames"], serde_json::json!([]));
}

#[tokio::test]
async fn hostnames_set_replaces_and_clears() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(org_id.clone()),
    hostnames_req(&["*.acme.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    json_body(resp).await["hostnames"],
    serde_json::json!(["*.acme.com"])
  );

  // A bare `*` means "no fence" and collapses to an empty list.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(org_id.clone()),
    hostnames_req(&["*"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["hostnames"], serde_json::json!([]));

  // Empty list clears it too.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id.clone()),
    hostnames_req(&[]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    state
      .org_store
      .lock()
      .await
      .hostnames_of(Some(&org_id))
      .is_empty()
  );
}

#[tokio::test]
async fn hostnames_rejects_master_unknown_and_invalid() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;

  // The master org is never fenced.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(MASTER_ID.to_string()),
    hostnames_req(&["acme.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Unknown org id.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path("nope".to_string()),
    hostnames_req(&["acme.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // Invalid pattern.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id.clone()),
    hostnames_req(&["*.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hostnames_apply_to_live_connections() {
  let state = Arc::new(test_state());
  let org_id = make_org(&state, "acme").await;

  // One connection serving a hostname the new fence keeps, one it excludes.
  for (id, host) in [("keep", "ok.acme.com"), ("evict", "old.example.com")] {
    let mut c = crate::test_support::mock_client(Some(host), None, None, None);
    c.perms.org_id = Some(org_id.clone());
    c.declared_hostnames = vec![host.to_string()];
    c.assigned_hostnames = vec![host.to_string()];
    state.clients.lock().await.insert(id.to_string(), c);
  }

  let headers = admin_headers(&state).await;
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id.clone()),
    hostnames_req(&["*.acme.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // The allowlist is cached per connection at connect time, so tightening it
  // has to be pushed out; otherwise a just-revoked hostname kept being served
  // until the client happened to reconnect.
  let clients = state.clients.lock().await;
  assert_eq!(
    clients.get("keep").unwrap().perms.org_hostnames,
    vec!["*.acme.com".to_string()]
  );
  assert_eq!(
    clients.get("evict").unwrap().perms.org_hostnames,
    vec!["*.acme.com".to_string()]
  );
}

#[tokio::test]
async fn hostnames_requires_master_admin() {
  let state = Arc::new(test_state());
  let org_id = make_org(&state, "acme").await;
  // No session → 401.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Path(org_id.clone()),
    hostnames_req(&["acme.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

  // A viewer session → 403 (the role floor is checked before the org).
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
    Path(org_id),
    hostnames_req(&["acme.com"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
