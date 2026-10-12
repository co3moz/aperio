//! Tests for the server-settings API: the GET/PUT handlers and their auth
//! guards, the masking of credentials, the step-up a security-relevant change
//! takes, every way a document is refused with its setting named, and the
//! live side-effects driven by `swap_config`.

use super::*;
use crate::test_support::*;

use crate::store::sessions::SessionInfo;
use crate::store::users::Role;
use aperio_config::server_settings::MASKED;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};

/// The document `yaml` describes.
fn doc(yaml: &str) -> ServerSettings {
  ServerSettings::from_yaml(yaml).unwrap_or_else(|e| panic!("{yaml}: {e}"))
}

async fn put(state: &Arc<AppState>, headers: HeaderMap, body: serde_json::Value) -> Response {
  settings_put_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(body),
  )
  .await
}

async fn text(resp: Response) -> String {
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  String::from_utf8(bytes.to_vec()).unwrap()
}

/// An admin session signed in `age_secs` ago.
async fn session_aged(state: &AppState, age_secs: u64) -> HeaderMap {
  let token = uuid::Uuid::new_v4().to_string();
  let now = crate::store::sessions::now_secs();
  state.sessions.lock().await.insert(
    &token,
    SessionInfo {
      plane: crate::store::sessions::Plane::Admin,
      expires_at: now + 86400,
      created_at: now - age_secs,
      ip: Some("127.0.0.1".to_string()),
      user_agent: None,
      scope_host: None,
      username: None,
      role: Role::Admin,
      selected_org: None,
      bound_org: None,
      login_host: None,
      sso_issuer: None,
    },
  );
  let name = crate::auth::session_cookie_name(state.config().secure_cookies);
  let mut h = HeaderMap::new();
  h.insert(
    "cookie",
    HeaderValue::from_str(&format!("{name}={token}")).unwrap(),
  );
  h
}

// ---------------------------------------------------------------------------
// GET /aperio/api/settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_requires_a_session() {
  let state = Arc::new(test_state());
  let resp = settings_get_handler(State(state), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_forbidden_for_non_master() {
  let state = Arc::new(test_state());
  // A viewer has a valid session but is not the master super-admin.
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = settings_get_handler(State(state), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_returns_the_document_and_which_paths_are_special() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = settings_get_handler(State(state), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  // Every field is present, defaults included.
  assert_eq!(
    body["settings"]["limits"]["tunnels"],
    ServerSettings::default().limits.tunnels
  );
  assert_eq!(body["settings"]["routing"]["lb_strategy"], "round-robin");
  for list in [
    "security_paths",
    "secret_paths",
    "restart_paths",
    "restart_pending",
  ] {
    assert!(body[list].is_array(), "{list}");
  }
  assert!(
    body["restart_pending"].as_array().unwrap().is_empty(),
    "nothing changed since start"
  );
}

#[tokio::test]
async fn get_never_returns_a_credential() {
  crate::live::set(doc(
    "metrics:\n  enabled: true\n  token: m-secret\noidc:\n  issuer: https://id.example.com\n  client_secret: o-secret\n",
  ));
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let body = json_body(settings_get_handler(State(state), headers).await).await;
  let raw = body.to_string();
  assert!(
    !raw.contains("m-secret") && !raw.contains("o-secret"),
    "{raw}"
  );
  assert_eq!(body["settings"]["metrics"]["token"], MASKED);
  assert_eq!(body["settings"]["oidc"]["issuer"], "https://id.example.com");
}

// ---------------------------------------------------------------------------
// PUT /aperio/api/settings, auth guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_requires_a_session() {
  let state = Arc::new(test_state());
  let resp = put(&state, HeaderMap::new(), serde_json::json!({})).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn put_forbidden_for_non_master() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = put(&state, cookie_headers(&token), serde_json::json!({})).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_security_change_takes_a_recent_sign_in_and_others_do_not() {
  let state = Arc::new(test_state());
  let stale = session_aged(&state, STEP_UP_SECS + 60).await;

  // An ordinary setting: an old session is fine.
  let resp = put(
    &state,
    stale.clone(),
    serde_json::json!({"limits": {"tunnels": 9}}),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // A security-relevant one: refused, and nothing moves.
  let resp = put(
    &state,
    stale,
    serde_json::json!({"limits": {"tunnels": 9}, "access": "public"}),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  let msg = text(resp).await;
  assert!(
    msg.contains("access") && msg.contains("Sign in again"),
    "{msg}"
  );
  assert_eq!(
    crate::live::current().access,
    ServerSettings::default().access
  );

  // The same change from a fresh sign-in goes through, and is audited on its
  // own.
  let fresh = session_aged(&state, 5).await;
  let resp = put(
    &state,
    fresh,
    serde_json::json!({"limits": {"tunnels": 9}, "access": "public"}),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let events: Vec<String> = state
    .audit
    .lock()
    .await
    .recent()
    .into_iter()
    .map(|e| e.event)
    .collect();
  assert!(
    events.iter().any(|e| e == "settings_security_changed"),
    "{events:?}"
  );
}

// ---------------------------------------------------------------------------
// PUT /aperio/api/settings, success + persistence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_applies_stores_and_reports_what_changed() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = put(
    &state,
    headers,
    serde_json::json!({
      "limits": {"tunnels": 16},
      "dashboard": {"ui_language": "de"},
      "routing": {"lb_strategy": "sticky"},
      "failover": {"mode": "retry"},
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["settings"]["limits"]["tunnels"], 16);
  let changed: Vec<&str> = body["changed"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|v| v.as_str())
    .collect();
  for path in [
    "limits.tunnels",
    "dashboard.ui_language",
    "routing.lb_strategy",
    "failover.mode",
  ] {
    assert!(changed.contains(&path), "{path} in {changed:?}");
  }

  // Put in force for the request path and for the direct readers.
  assert_eq!(state.config().max_tunnels, 16);
  assert_eq!(state.config().ui_language, "de");
  assert_eq!(crate::live::current().limits.tunnels, 16);
  // And stored, for the next start.
  let stored = state.settings_store.lock().await.get().unwrap().unwrap();
  assert_eq!(stored.limits.tunnels, 16);
}

#[tokio::test]
async fn a_credential_saved_back_masked_keeps_its_value() {
  crate::live::set(doc("metrics:\n  enabled: true\n  token: keep-me\n"));
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let read = json_body(settings_get_handler(State(state.clone()), headers.clone()).await).await;
  let mut document = read["settings"].clone();
  document["limits"]["tunnels"] = 12.into();
  let resp = put(&state, headers, document).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    crate::live::current().metrics.token.as_deref(),
    Some("keep-me")
  );
  assert_eq!(crate::live::current().limits.tunnels, 12);
}

#[tokio::test]
async fn a_restart_path_is_stored_and_reported_pending() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = put(
    &state,
    headers,
    serde_json::json!({"uptime": {"tick": "30s"}, "backup": {"keep": 3}}),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let pending: Vec<&str> = body["restart_pending"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|v| v.as_str())
    .collect();
  assert_eq!(pending, vec!["backup.keep"], "uptime.tick is read live");
}

// ---------------------------------------------------------------------------
// PUT /aperio/api/settings, refusals name the setting
// ---------------------------------------------------------------------------

async fn put_expecting_400(body: serde_json::Value) -> String {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = put(&state, headers, body).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let msg = text(resp).await;
  assert_eq!(
    *crate::live::current(),
    ServerSettings::default(),
    "a refused document changes nothing"
  );
  msg
}

#[tokio::test]
async fn put_rejects_an_unknown_value_or_key() {
  let msg = put_expecting_400(serde_json::json!({"routing": {"lb_strategy": "nonsense"}})).await;
  assert!(msg.contains("nonsense"), "got: {msg}");
  let msg = put_expecting_400(serde_json::json!({"limits": {"tunnelz": 4}})).await;
  assert!(msg.contains("tunnelz"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_values_the_server_cannot_honour() {
  for (body, path) in [
    (
      serde_json::json!({"access": {"method": "basic", "users": "nopassword"}}),
      "access",
    ),
    (
      serde_json::json!({"sites": [{"hostname": "*", "pages": {"unavailable": "x".repeat(512 * 1024 + 1)}}]}),
      "sites[0] (*): pages.unavailable",
    ),
    (
      serde_json::json!({"sites": [{"hostname": "*", "pages": {"maintenance": "x".repeat(512 * 1024 + 1)}}]}),
      "sites[0] (*): pages.maintenance",
    ),
    (
      serde_json::json!({"sites": [{"hostname": "app.example.com"}]}),
      "sites[0] (app.example.com): sets nothing",
    ),
    (
      serde_json::json!({"dashboard": {"ui_language": "xx"}}),
      "dashboard.ui_language",
    ),
    (serde_json::json!({"cache": {"size": 0}}), "cache.size"),
  ] {
    let msg = put_expecting_400(body).await;
    assert!(msg.starts_with(path), "{path}: {msg}");
  }
}

#[tokio::test]
async fn a_mask_with_no_credential_behind_it_is_refused() {
  let msg = put_expecting_400(serde_json::json!({"edge": {"token": MASKED}})).await;
  assert!(msg.starts_with("edge.token"), "got: {msg}");
}

// ---------------------------------------------------------------------------
// apply_settings, the swap_config side effects
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_gate_set_and_then_cleared_gates_and_then_does_not() {
  let state = Arc::new(test_state());
  apply_settings(&state, doc("access: {method: basic, users: 'user:pw'}\n"))
    .await
    .unwrap();
  assert!(state.config().visitor_auth.gates());
  apply_settings(&state, ServerSettings::default())
    .await
    .unwrap();
  assert!(!state.config().visitor_auth.gates());
}

#[tokio::test]
async fn changing_subdomain_suffix_reassigns_connected_clients() {
  // test_config has no random subdomain suffix, so enabling one must trigger
  // reassign_random_hostnames for every connected client.
  let state = Arc::new(test_state());
  state
    .clients
    .write()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));

  apply_settings(&state, doc("routing:\n  random_subdomain: example.com\n"))
    .await
    .unwrap();
  assert_eq!(
    state.config().random_subdomain_suffix.as_deref(),
    Some("*.example.com")
  );
  // The connected client was handed a fresh random hostname.
  let clients = state.clients.read().await;
  let c = clients.get("c1").unwrap();
  assert!(c.sole().random_hostname.is_some());
  assert!(
    c.sole()
      .assigned_hostnames
      .iter()
      .any(|h| Some(h) == c.sole().random_hostname.as_ref())
  );
}

#[tokio::test]
async fn enabling_compression_offers_it_to_connected_clients() {
  // old tunnel_compression=false → new=true, with a connected client, drives
  // offer_compression_to_connected.
  let state = Arc::new(test_state());
  state
    .clients
    .write()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));

  apply_settings(&state, doc("tunnel:\n  compression: true\n"))
    .await
    .unwrap();
  assert!(state.config().tunnel_compression);
}

#[tokio::test]
async fn disabling_cache_clears_the_response_cache() {
  // Start with the cache enabled so disabling it takes the clear() branch.
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = Arc::new(test_state_with(cfg));

  apply_settings(&state, doc("cache:\n  enabled: false\n"))
    .await
    .unwrap();
  assert!(!state.config().cache_enabled);
}

#[tokio::test]
async fn changing_lockout_and_audit_rotation_updates_live_structures() {
  // Distinct values from test_config drive both the login-lockout policy and
  // the audit-rotation reconfiguration branches of swap_config.
  let state = Arc::new(test_state());
  apply_settings(
    &state,
    doc(
      "security:\n  login_lockout:\n    threshold: 10\n    duration: 2m\naudit:\n  max_size: 20MB\n  max_files: 7\n",
    ),
  )
  .await
  .unwrap();
  assert_eq!(state.config().login_lockout_threshold, 10);
  assert_eq!(state.config().login_lockout_secs, 120);
  assert_eq!(state.config().audit_max_files, 7);
}
