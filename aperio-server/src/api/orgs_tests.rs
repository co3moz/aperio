//! Tests for the organization-management dashboard API.

use super::*;
use crate::store::tokens::TokenSpec;
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
  state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "master-tok".into(),
      ..Default::default()
    })
    .expect("the test store can be written to");
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
  state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "child-tok".into(),
      org_id: Some(org_id.clone()),
      ..Default::default()
    })
    .expect("the test store can be written to");

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
  let (_key, secret) = state
    .admin_key_store
    .lock()
    .await
    .create(
      "k".into(),
      Role::Admin,
      crate::store::grants::GrantOrg::All,
      None,
    )
    .expect("the test store can be written to");
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
      panel_hostname: None,
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
      panel_hostname: None,
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
      panel_hostname: None,
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
    default_role: None,
    group_grants: Vec::new(),
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
    .set_quota(&org_id, Some(Some(3)), Some(Some(7)), None, None)
    .expect("the test store can be written to");
  state
    .users
    .lock()
    .await
    .create("u", "password1", Role::Viewer, Some(org_id.clone()))
    .unwrap();
  state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "t".into(),
      org_id: Some(org_id.clone()),
      ..Default::default()
    })
    .expect("the test store can be written to");

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
      panel_hostname: None,
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
      panel_hostname: None,
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
      panel_hostname: None,
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
    c.sole_mut().declared_hostnames = vec![host.to_string()];
    c.sole_mut().assigned_hostnames = vec![host.to_string()];
    state.clients.write().await.insert(id.to_string(), c);
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
  let clients = state.clients.read().await;
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

#[tokio::test]
async fn deleting_an_org_clears_the_maintenance_flags_it_owns() {
  // Otherwise the hostname answers 503 until the next restart: a flag is
  // cleared by the organization that set it, and that organization is gone.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org_id = make_org(&state, "acme").await;
  {
    let mut set = state.maintenance.lock().await;
    let owned = |org: Option<&str>| crate::state::MaintenanceFlag {
      org: org.map(str::to_string),
      ..crate::state::MaintenanceFlag::default()
    };
    set.insert("acme.example".to_string(), owned(Some(&org_id)));
    set.insert("*.acme.example".to_string(), owned(Some(&org_id)));
    // Master's own flag is not the deleted org's business.
    set.insert("master.example".to_string(), owned(None));
  }

  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(org_id),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let set = state.maintenance.lock().await;
  assert_eq!(set.len(), 1);
  assert!(set.contains_key("master.example"));
}

#[tokio::test]
async fn a_partial_label_pattern_is_accepted_by_the_allowlist_endpoint() {
  // The shape a fleet naming convention needs: every `<name>-pi` box under a
  // domain, without handing the tenant the domain.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let id = make_org(&state, "robogon").await;

  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(id.clone()),
    Json(OrgHostnamesRequest {
      hostnames: vec![" *-PI.Robogon.com ".into(), "robogon.com".into()],
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    state.org_store.lock().await.hostnames_of(Some(&id)),
    vec!["*-pi.robogon.com".to_string(), "robogon.com".to_string()]
  );

  // And the refusal names the third shape now, so the message is a
  // description of what is accepted rather than of half of it.
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id),
    Json(OrgHostnamesRequest {
      hostnames: vec!["*-pi-*.robogon.com".into()],
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let text = String::from_utf8(body.to_vec()).unwrap();
  assert!(text.contains("*-pi.acme.com"), "{text}");
}

// ---------------------------------------------------------------------------
// select and delete, with grants (planned_features.md #153)
// ---------------------------------------------------------------------------

async fn granted_user(state: &Arc<AppState>, name: &str, grants: Vec<(&str, Role)>) -> String {
  use crate::store::grants::{Grant, GrantOrg};
  state
    .users
    .lock()
    .await
    .create_with_grants(
      name,
      "long-password",
      None,
      grants
        .into_iter()
        .map(|(org, role)| Grant::new(GrantOrg::parse(org), role))
        .collect(),
    )
    .unwrap();
  seed_session(state, Role::Viewer, Some(name), None).await
}

async fn select_status(state: &Arc<AppState>, token: &str, id: &str) -> StatusCode {
  orgs_select_handler(
    State(state.clone()),
    cookie_headers(token),
    Json(OrgSelectRequest {
      id: Some(id.to_string()),
    }),
  )
  .await
  .status()
}

#[tokio::test]
async fn a_granted_user_switches_between_its_organizations_and_nowhere_else() {
  let state = Arc::new(test_state());
  let acme = make_org(&state, "acme").await;
  let beta = make_org(&state, "beta").await;
  let gamma = make_org(&state, "gamma").await;
  let token = granted_user(
    &state,
    "carol",
    vec![(&acme, Role::Admin), (&beta, Role::Viewer)],
  )
  .await;

  assert_eq!(select_status(&state, &token, &acme).await, StatusCode::OK);
  assert_eq!(select_status(&state, &token, &beta).await, StatusCode::OK);
  assert_eq!(
    state.sessions.lock().await.selected_org(&token),
    Some(Some(beta.clone()))
  );
  // The role follows the selection: Viewer in Beta, Admin in Acme.
  assert_eq!(
    crate::auth::dashboard_role(&state, &cookie_headers(&token)).await,
    Some(Role::Viewer)
  );
  assert_eq!(select_status(&state, &token, &acme).await, StatusCode::OK);
  assert_eq!(
    crate::auth::dashboard_role(&state, &cookie_headers(&token)).await,
    Some(Role::Admin)
  );
  // Nothing reaches Gamma or master.
  assert_eq!(
    select_status(&state, &token, &gamma).await,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    select_status(&state, &token, MASTER_ID).await,
    StatusCode::FORBIDDEN
  );
  // And the selection is still Acme after the refusals.
  assert_eq!(
    crate::auth::effective_org(&state, &cookie_headers(&token)).await,
    Some(acme)
  );
}

#[tokio::test]
async fn a_master_admin_without_star_reaches_no_child() {
  let state = Arc::new(test_state());
  let acme = make_org(&state, "acme").await;
  let token = granted_user(&state, "root", vec![(MASTER_ID, Role::Admin)]).await;
  assert!(crate::auth::is_master_admin(&state, &cookie_headers(&token)).await);
  assert_eq!(
    select_status(&state, &token, MASTER_ID).await,
    StatusCode::OK
  );
  assert_eq!(
    select_status(&state, &token, &acme).await,
    StatusCode::FORBIDDEN
  );
  // The listing is master's, so it answers; the child is simply not theirs
  // to act in.
  let resp = orgs_list_handler(State(state.clone()), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn deleting_an_organization_strips_the_grants_that_named_it() {
  let state = Arc::new(test_state());
  let acme = make_org(&state, "acme").await;
  let beta = make_org(&state, "beta").await;
  granted_user(
    &state,
    "carol",
    vec![(&acme, Role::Admin), (&beta, Role::Viewer)],
  )
  .await;
  let resp = orgs_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Path(beta.clone()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let labels: Vec<String> = state
    .users
    .lock()
    .await
    .find_by_username("carol")
    .unwrap()
    .grants
    .iter()
    .map(|g| g.label())
    .collect();
  assert_eq!(labels, vec![format!("{acme}:admin")]);
}

// ---------------------------------------------------------------------------
// per-organization OIDC grant policy (planned_features.md #154)
// ---------------------------------------------------------------------------

fn oidc_policy_req(default_role: Option<&str>, group_grants: &[&str]) -> Json<OrgOidcRequest> {
  Json(OrgOidcRequest {
    issuer: "https://issuer.example".into(),
    client_id: "cid".into(),
    client_secret: "secret".into(),
    allowed_emails: vec!["*@acme.com".into()],
    default_role: default_role.map(str::to_string),
    group_grants: group_grants.iter().map(|s| s.to_string()).collect(),
  })
}

#[tokio::test]
async fn the_org_oidc_policy_is_stored_and_validated() {
  let state = Arc::new(test_state());
  let org_id = make_org(&state, "acme").await;
  let stored = |state: &Arc<AppState>| {
    let state = state.clone();
    let org_id = org_id.clone();
    async move {
      state
        .org_store
        .lock()
        .await
        .find(&org_id)
        .unwrap()
        .oidc
        .clone()
        .unwrap()
    }
  };

  // Absent means Admin, what such a login always was.
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Path(org_id.clone()),
    oidc_policy_req(None, &[]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(stored(&state).await.default_role, Some(Role::Admin));

  // `none` is nothing until an admin grants it; a map entry is kept as
  // written.
  let resp = orgs_oidc_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Path(org_id.clone()),
    oidc_policy_req(
      Some("none"),
      &["acme-ops=operator", " acme-admins = admin "],
    ),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let cfg = stored(&state).await;
  assert_eq!(cfg.default_role, None);
  assert_eq!(
    cfg.group_grants,
    vec!["acme-ops=operator", "acme-admins = admin"]
  );

  // A role that is not one, and a map entry that names no role, are refused.
  for (role, map) in [
    (Some("root"), vec![]),
    (None, vec!["acme-ops"]),
    (None, vec!["=admin"]),
    (None, vec!["acme-ops=owner"]),
  ] {
    let resp = orgs_oidc_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      admin_headers(&state).await,
      Path(org_id.clone()),
      oidc_policy_req(role, &map),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{role:?} {map:?}");
  }
}

// ---------------------------------------------------------------------------
// panel hostname (planned_features.md #152)
// ---------------------------------------------------------------------------

async fn set_panel(
  state: &Arc<AppState>,
  headers: HeaderMap,
  id: &str,
  hostname: Option<&str>,
) -> axum::response::Response {
  orgs_panel_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id.to_string()),
    Json(OrgPanelRequest {
      hostname: hostname.map(str::to_string),
    }),
  )
  .await
}

async fn fenced_org(state: &Arc<AppState>, name: &str, fence: &str) -> String {
  state
    .org_store
    .lock()
    .await
    .create(name, vec![fence.to_string()], None)
    .unwrap()
    .id
}

async fn panel_of(state: &Arc<AppState>, id: &str) -> Option<String> {
  state
    .org_store
    .lock()
    .await
    .find(id)
    .and_then(|o| o.panel_hostname.clone())
}

#[tokio::test]
async fn a_panel_is_set_by_an_admin_of_the_organization_inside_its_fence() {
  let state = Arc::new(test_state());
  let acme = fenced_org(&state, "acme", "*.acme.test").await;
  let beta = fenced_org(&state, "beta", "*.beta.test").await;
  let acme_admin = granted_user(&state, "acme-admin", vec![(&acme, Role::Admin)]).await;
  let acme_viewer = granted_user(&state, "acme-viewer", vec![(&acme, Role::Viewer)]).await;
  let beta_admin = granted_user(&state, "beta-admin", vec![(&beta, Role::Admin)]).await;

  // Nobody: 401. Admin elsewhere, or Viewer here: 403.
  assert_eq!(
    set_panel(&state, HeaderMap::new(), &acme, Some("panel.acme.test"))
      .await
      .status(),
    StatusCode::UNAUTHORIZED
  );
  for token in [&beta_admin, &acme_viewer] {
    assert_eq!(
      set_panel(
        &state,
        cookie_headers(token),
        &acme,
        Some("panel.acme.test")
      )
      .await
      .status(),
      StatusCode::FORBIDDEN
    );
  }
  // Admin here: the name lands, normalized, and the cache knows it.
  let resp = set_panel(
    &state,
    cookie_headers(&acme_admin),
    &acme,
    Some(" Panel.ACME.test. "),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    panel_of(&state, &acme).await.as_deref(),
    Some("panel.acme.test")
  );
  assert!(state.is_panel_hostname("panel.acme.test"));
  // Outside the fence, a pattern, and the master organization are refused.
  assert_eq!(
    set_panel(
      &state,
      cookie_headers(&acme_admin),
      &acme,
      Some("panel.beta.test")
    )
    .await
    .status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    set_panel(
      &state,
      cookie_headers(&acme_admin),
      &acme,
      Some("*.acme.test")
    )
    .await
    .status(),
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    set_panel(
      &state,
      admin_headers(&state).await,
      MASTER_ID,
      Some("panel.test")
    )
    .await
    .status(),
    StatusCode::BAD_REQUEST
  );
  // Another organization cannot take the same name, and the super-admin
  // reaches every organization's panel through `*`.
  state
    .org_store
    .lock()
    .await
    .set_hostnames(&beta, vec!["*.acme.test".to_string()])
    .unwrap();
  assert_eq!(
    set_panel(
      &state,
      admin_headers(&state).await,
      &beta,
      Some("panel.acme.test")
    )
    .await
    .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(
    set_panel(
      &state,
      admin_headers(&state).await,
      &beta,
      Some("other.acme.test")
    )
    .await
    .status(),
    StatusCode::OK
  );
  // Cleared with an empty name, and the cache follows.
  assert_eq!(
    set_panel(&state, cookie_headers(&acme_admin), &acme, Some(""))
      .await
      .status(),
    StatusCode::OK
  );
  assert_eq!(panel_of(&state, &acme).await, None);
  assert!(!state.is_panel_hostname("panel.acme.test"));
}

#[tokio::test]
async fn a_panel_needs_a_fence_and_cannot_be_the_servers_own() {
  let mut cfg = test_config();
  cfg.dashboard_hostname = Some("panel.test".to_string());
  let state = Arc::new(test_state_with(cfg));
  let open = make_org(&state, "open").await;
  assert_eq!(
    set_panel(
      &state,
      admin_headers(&state).await,
      &open,
      Some("panel.open.test")
    )
    .await
    .status(),
    StatusCode::BAD_REQUEST,
    "an unfenced organization has nothing to check a panel against"
  );
  let fenced = fenced_org(&state, "fenced", "*.test").await;
  assert_eq!(
    set_panel(
      &state,
      admin_headers(&state).await,
      &fenced,
      Some("panel.test")
    )
    .await
    .status(),
    StatusCode::CONFLICT,
    "the server's own dashboard hostname"
  );
}

#[tokio::test]
async fn a_fence_that_no_longer_covers_the_panel_takes_it_away() {
  let state = Arc::new(test_state());
  let acme = fenced_org(&state, "acme", "*.acme.test").await;
  assert_eq!(
    set_panel(
      &state,
      admin_headers(&state).await,
      &acme,
      Some("panel.acme.test")
    )
    .await
    .status(),
    StatusCode::OK
  );
  let resp = orgs_hostnames_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Path(acme.clone()),
    hostnames_req(&["*.other.test"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(panel_of(&state, &acme).await, None);
  assert!(!state.is_panel_hostname("panel.acme.test"));
}

#[tokio::test]
async fn an_organization_can_be_created_with_its_panel() {
  let state = Arc::new(test_state());
  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Json(OrgCreateRequest {
      name: "acme".into(),
      custom_name: None,
      hostnames: vec!["*.acme.test".into()],
      panel_hostname: Some("aperio.acme.test".into()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let id = body["id"].as_str().unwrap().to_string();
  assert_eq!(
    panel_of(&state, &id).await.as_deref(),
    Some("aperio.acme.test")
  );
  assert!(state.is_panel_hostname("aperio.acme.test"));
  // Refused as a whole when the panel is outside the fence: no record.
  let resp = orgs_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Json(OrgCreateRequest {
      name: "beta".into(),
      custom_name: None,
      hostnames: vec!["*.beta.test".into()],
      panel_hostname: Some("aperio.acme.test".into()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(
    state
      .org_store
      .lock()
      .await
      .list()
      .iter()
      .all(|o| o.name != "beta")
  );
}
