//! Tests for the dashboard user-management, TOTP, and session-management API.
//!
//! Handlers are called directly (the route-level auth/role guards live in the
//! dashboard middleware, exercised elsewhere); these cover the in-handler
//! branches: validation, self-vs-other, org scoping, unknown-id 404s, and the
//! full TOTP enrollment/disable lifecycle using real authenticator codes.

use super::*;
use crate::store::sessions::SessionInfo;
use crate::test_support::{
  admin_headers, cookie_headers, json_body, seed_session, test_peer, test_state,
};
use axum::extract::{ConnectInfo, Path, State};
use std::sync::Arc;

/// Produces a valid 6-digit TOTP code for a base32 secret at `now_secs`,
/// mirroring the RFC 6238 computation the server verifies against.
fn totp_code(secret_b32: &str, now_secs: u64) -> String {
  use hmac::{Hmac, Mac};
  let secret = crate::totp::base32_decode(secret_b32).unwrap();
  let counter = now_secs / 30;
  let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&secret).unwrap();
  mac.update(&counter.to_be_bytes());
  let digest = mac.finalize().into_bytes();
  let offset = (digest[19] & 0x0f) as usize;
  let bin = (u32::from(digest[offset]) & 0x7f) << 24
    | u32::from(digest[offset + 1]) << 16
    | u32::from(digest[offset + 2]) << 8
    | u32::from(digest[offset + 3]);
  format!("{:06}", bin % 1_000_000)
}

/// Seeds a named dashboard user directly in the store and returns its id.
async fn make_user(state: &Arc<AppState>, name: &str, role: Role, org: Option<&str>) -> String {
  state
    .users
    .lock()
    .await
    .create(name, "long-password", role, org.map(|s| s.to_string()))
    .unwrap()
    .id
    .clone()
}

// ---------------------------------------------------------------------------
// users_list_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_returns_only_effective_org_users() {
  let state = Arc::new(test_state());
  make_user(&state, "alice", Role::Admin, None).await; // master org
  make_user(&state, "carol", Role::Viewer, Some("org1")).await; // other org

  // Built-in admin (master org) sees only master-org users.
  let headers = admin_headers(&state).await;
  let resp = users_list_handler(State(state.clone()), headers).await;
  let list = resp.0;
  let arr = list.as_array().unwrap();
  assert_eq!(arr.len(), 1);
  assert_eq!(arr[0]["username"], "alice");
  assert_eq!(arr[0]["role"], "admin");
  assert_eq!(arr[0]["totp"], false);
  assert_eq!(arr[0]["org_id"], serde_json::Value::Null);

  // A named user scoped to org1 sees only org1's users.
  let token = seed_session(&state, Role::Admin, Some("carol"), None).await;
  let resp = users_list_handler(State(state.clone()), cookie_headers(&token)).await;
  let arr = resp.0;
  let arr = arr.as_array().unwrap();
  assert_eq!(arr.len(), 1);
  assert_eq!(arr[0]["username"], "carol");
  assert_eq!(arr[0]["org_id"], "org1");
}

// ---------------------------------------------------------------------------
// users_create_handler
// ---------------------------------------------------------------------------

fn create_req(username: &str, password: &str, role: &str) -> Json<UserCreateRequest> {
  Json(UserCreateRequest {
    username: username.to_string(),
    password: password.to_string(),
    role: role.to_string(),
  })
}

#[tokio::test]
async fn create_rejects_invalid_role() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = users_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    create_req("bob", "long-password", "superuser"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_succeeds_and_emits() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = users_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    create_req("bob", "long-password", "operator"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["username"], "bob");
  assert_eq!(body["role"], "operator");
  assert_eq!(body["enabled"], true);
  // Persisted in the store.
  assert!(state.users.lock().await.find_by_username("bob").is_some());
}

#[tokio::test]
async fn create_rejects_duplicate_username() {
  let state = Arc::new(test_state());
  make_user(&state, "bob", Role::Viewer, None).await;
  let headers = admin_headers(&state).await;
  let resp = users_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    create_req("bob", "long-password", "viewer"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_enforces_org_user_quota() {
  let state = Arc::new(test_state());
  // An org whose user quota is already met by its single existing member.
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
    .set_quota(&org_id, None, None, Some(Some(1)), None);
  make_user(&state, "boss", Role::Admin, Some(&org_id)).await;

  // "boss" (org acme, at quota) tries to create another user → 403.
  let token = seed_session(&state, Role::Admin, Some("boss"), None).await;
  let resp = users_create_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
    create_req("newbie", "long-password", "viewer"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// users_update_handler
// ---------------------------------------------------------------------------

fn update_req(
  role: Option<&str>,
  enabled: Option<bool>,
  password: Option<&str>,
) -> Json<UserUpdateRequest> {
  Json(UserUpdateRequest {
    role: role.map(|s| s.to_string()),
    enabled,
    password: password.map(|s| s.to_string()),
  })
}

#[tokio::test]
async fn update_rejects_invalid_role() {
  let state = Arc::new(test_state());
  let id = make_user(&state, "bob", Role::Viewer, None).await;
  let headers = admin_headers(&state).await;
  let resp = users_update_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id),
    update_req(Some("nope"), None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_unknown_or_other_org_is_404() {
  let state = Arc::new(test_state());
  // Unknown id.
  let headers = admin_headers(&state).await;
  let resp = users_update_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path("no-such-id".to_string()),
    update_req(Some("admin"), None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // A user in another org is invisible to the master admin → 404.
  let other = make_user(&state, "carol", Role::Viewer, Some("org1")).await;
  let resp = users_update_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(other),
    update_req(Some("admin"), None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_succeeds() {
  let state = Arc::new(test_state());
  let id = make_user(&state, "bob", Role::Viewer, None).await;
  let headers = admin_headers(&state).await;
  let resp = users_update_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id.clone()),
    update_req(Some("admin"), Some(false), Some("brand-new-password")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["role"], "admin");
  assert_eq!(body["enabled"], false);
}

#[tokio::test]
async fn update_rejects_short_password() {
  let state = Arc::new(test_state());
  let id = make_user(&state, "bob", Role::Viewer, None).await;
  let headers = admin_headers(&state).await;
  // Short password passes the org guard, then the store rejects it (400, not 404).
  let resp = users_update_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id),
    update_req(None, None, Some("short")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// users_delete_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_unknown_or_other_org_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = users_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path("no-such-id".to_string()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  let other = make_user(&state, "carol", Role::Viewer, Some("org1")).await;
  let resp = users_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(other),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_succeeds_and_drops_sessions() {
  let state = Arc::new(test_state());
  let id = make_user(&state, "bob", Role::Operator, None).await;
  // A live session for bob that must be dropped on delete.
  seed_session(&state, Role::Operator, Some("bob"), None).await;
  assert_eq!(state.sessions.lock().await.entries().len(), 1);

  let headers = admin_headers(&state).await;
  let resp = users_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.users.lock().await.find_by_username("bob").is_none());
  // bob's session is gone; the admin_headers session is not for a named user.
  assert!(
    !state
      .sessions
      .lock()
      .await
      .entries()
      .iter()
      .any(|(_, i)| i.username.as_deref() == Some("bob"))
  );
}

// ---------------------------------------------------------------------------
// session_user_id error paths (via totp_setup_handler)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn totp_setup_rejects_builtin_admin() {
  let state = Arc::new(test_state());
  // Built-in admin session has no named user row.
  let headers = admin_headers(&state).await;
  let resp = totp_setup_handler(State(state.clone()), headers).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn totp_setup_rejects_unknown_user() {
  let state = Arc::new(test_state());
  // Session references a username with no matching user row.
  let token = seed_session(&state, Role::Admin, Some("ghost"), None).await;
  let resp = totp_setup_handler(State(state.clone()), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// TOTP enrollment lifecycle: setup -> enable -> disable
// ---------------------------------------------------------------------------

/// Enrolls TOTP for `username` (must already exist) and returns
/// (session cookie headers, secret, recovery codes).
async fn enroll_totp(
  state: &Arc<AppState>,
  username: &str,
) -> (axum::http::HeaderMap, String, Vec<String>) {
  let token = seed_session(state, Role::Operator, Some(username), None).await;
  let headers = cookie_headers(&token);

  let resp = totp_setup_handler(State(state.clone()), headers.clone()).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let secret = body["secret"].as_str().unwrap().to_string();
  assert!(body["otpauth_url"].as_str().unwrap().contains("otpauth://"));

  let now = now_secs();
  let resp = totp_enable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(TotpCodeRequest {
      code: totp_code(&secret, now),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let codes: Vec<String> = body["recovery_codes"]
    .as_array()
    .unwrap()
    .iter()
    .map(|v| v.as_str().unwrap().to_string())
    .collect();
  assert_eq!(codes.len(), 8);
  (headers, secret, codes)
}

#[tokio::test]
async fn totp_enable_rejects_invalid_code() {
  let state = Arc::new(test_state());
  make_user(&state, "mfa", Role::Operator, None).await;
  let token = seed_session(&state, Role::Operator, Some("mfa"), None).await;
  let headers = cookie_headers(&token);
  // Begin enrollment so a pending secret exists.
  let _ = totp_setup_handler(State(state.clone()), headers.clone()).await;
  let resp = totp_enable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(TotpCodeRequest {
      code: "000000".to_string(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn totp_full_lifecycle_disable_with_code() {
  let state = Arc::new(test_state());
  make_user(&state, "mfa", Role::Operator, None).await;
  let (headers, secret, _codes) = enroll_totp(&state, "mfa").await;

  // Disable with a valid current authenticator code.
  let now = now_secs();
  let resp = totp_disable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(TotpCodeRequest {
      code: totp_code(&secret, now),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  // TOTP is off again.
  let uid = state
    .users
    .lock()
    .await
    .find_by_username("mfa")
    .unwrap()
    .id
    .clone();
  assert!(
    state
      .users
      .lock()
      .await
      .get(&uid)
      .unwrap()
      .totp_secret
      .is_none()
  );
}

#[tokio::test]
async fn totp_disable_with_recovery_code() {
  let state = Arc::new(test_state());
  make_user(&state, "mfa", Role::Operator, None).await;
  let (headers, _secret, codes) = enroll_totp(&state, "mfa").await;

  // A single-use recovery code also authorizes disabling.
  let resp = totp_disable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(TotpCodeRequest {
      code: codes[0].clone(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn totp_disable_when_not_enabled() {
  let state = Arc::new(test_state());
  make_user(&state, "mfa", Role::Operator, None).await;
  let token = seed_session(&state, Role::Operator, Some("mfa"), None).await;
  let resp = totp_disable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
    Json(TotpCodeRequest {
      code: "000000".to_string(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn totp_disable_rejects_invalid_code() {
  let state = Arc::new(test_state());
  make_user(&state, "mfa", Role::Operator, None).await;
  let (headers, _secret, _codes) = enroll_totp(&state, "mfa").await;
  // Neither a valid TOTP nor a valid recovery code → 401.
  let resp = totp_disable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(TotpCodeRequest {
      code: "000000".to_string(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn totp_disable_rejects_builtin_admin() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = totp_disable_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(TotpCodeRequest {
      code: "000000".to_string(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// totp_admin_reset_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn totp_admin_reset_unknown_or_other_org_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = totp_admin_reset_handler(
    State(state.clone()),
    Path("no-such-id".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // Another org's user is out of reach.
  let other = make_user(&state, "carol", Role::Viewer, Some("org1")).await;
  let resp = totp_admin_reset_handler(
    State(state.clone()),
    Path(other),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn totp_admin_reset_succeeds() {
  let state = Arc::new(test_state());
  make_user(&state, "mfa", Role::Operator, None).await;
  let (_headers, _secret, _codes) = enroll_totp(&state, "mfa").await;
  let uid = state
    .users
    .lock()
    .await
    .find_by_username("mfa")
    .unwrap()
    .id
    .clone();

  let headers = admin_headers(&state).await;
  let resp = totp_admin_reset_handler(
    State(state.clone()),
    Path(uid.clone()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    state
      .users
      .lock()
      .await
      .get(&uid)
      .unwrap()
      .totp_secret
      .is_none()
  );
}

// ---------------------------------------------------------------------------
// sessions_list_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sessions_list_filters_by_org_and_marks_current() {
  let state = Arc::new(test_state());
  make_user(&state, "alice", Role::Admin, None).await; // master org
  make_user(&state, "carol", Role::Viewer, Some("org1")).await; // other org

  // Own admin session (master org) — marked current.
  let own = seed_session(&state, Role::Admin, None, None).await;
  // A master-org named-user session.
  seed_session(&state, Role::Admin, Some("alice"), None).await;
  // An org1 session that must be filtered out for a master-org caller.
  seed_session(&state, Role::Viewer, Some("carol"), None).await;

  let resp = sessions_list_handler(State(state.clone()), cookie_headers(&own)).await;
  let list = resp.0;
  assert_eq!(list.len(), 2, "only master-org sessions are listed");
  let usernames: Vec<&str> = list
    .iter()
    .map(|s| s["username"].as_str().unwrap())
    .collect();
  assert!(usernames.contains(&"aperio")); // built-in admin session default label
  assert!(usernames.contains(&"alice"));
  assert!(!usernames.contains(&"carol"));
  // Exactly one entry is the caller's own session.
  assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
}

#[tokio::test]
async fn sessions_list_without_cookie() {
  let state = Arc::new(test_state());
  // No cookie: own token is None, effective org is master.
  seed_session(&state, Role::Admin, None, None).await;
  let resp = sessions_list_handler(State(state.clone()), HeaderMap::new()).await;
  let list = resp.0;
  assert_eq!(list.len(), 1);
  assert_eq!(list[0]["current"], false);
}

// ---------------------------------------------------------------------------
// session_revoke_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_revoke_unknown_or_other_org_is_404() {
  let state = Arc::new(test_state());
  make_user(&state, "carol", Role::Viewer, Some("org1")).await;
  let headers = admin_headers(&state).await;

  // Unknown id.
  let resp = session_revoke_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path("no-such-session".to_string()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // A session in another org cannot be revoked by a master-org caller.
  seed_session(&state, Role::Viewer, Some("carol"), None).await;
  let key = state
    .sessions
    .lock()
    .await
    .entries()
    .iter()
    .find(|(_, i)| i.username.as_deref() == Some("carol"))
    .map(|(k, _)| k.clone())
    .unwrap();
  let resp = session_revoke_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(key),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn session_revoke_succeeds() {
  let state = Arc::new(test_state());
  make_user(&state, "alice", Role::Admin, None).await;
  let headers = admin_headers(&state).await;
  seed_session(&state, Role::Admin, Some("alice"), None).await;
  let key = state
    .sessions
    .lock()
    .await
    .entries()
    .iter()
    .find(|(_, i)| i.username.as_deref() == Some("alice"))
    .map(|(k, _)| k.clone())
    .unwrap();

  let resp = session_revoke_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(key.clone()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    !state
      .sessions
      .lock()
      .await
      .entries()
      .iter()
      .any(|(k, _)| *k == key)
  );
}

// ---------------------------------------------------------------------------
// sessions_clear_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sessions_clear_ends_others_in_org_only() {
  let state = Arc::new(test_state());
  make_user(&state, "alice", Role::Admin, None).await; // master org
  make_user(&state, "carol", Role::Viewer, Some("org1")).await; // other org

  let own = seed_session(&state, Role::Admin, None, None).await; // caller (master)
  seed_session(&state, Role::Admin, Some("alice"), None).await; // master, ended
  seed_session(&state, Role::Viewer, Some("carol"), None).await; // org1, kept

  let resp = sessions_clear_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    cookie_headers(&own),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["ended"], 1, "only the other master-org session ends");

  // Own session and the org1 session survive.
  let entries = state.sessions.lock().await.entries();
  assert!(
    entries
      .iter()
      .any(|(_, i)| i.username.as_deref() == Some("carol"))
  );
  assert!(
    !entries
      .iter()
      .any(|(_, i)| i.username.as_deref() == Some("alice"))
  );
}

// ---------------------------------------------------------------------------
// session_org: the None-username branch (built-in/visitor sessions -> master)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_org_maps_unnamed_session_to_master() {
  let state = Arc::new(test_state());
  let map = username_org_map(&state).await;
  let info = SessionInfo {
    expires_at: 0,
    created_at: 0,
    ip: None,
    user_agent: None,
    scope_host: None,
    username: None,
    role: Role::Admin,
    selected_org: None,
    bound_org: None,
  };
  assert_eq!(session_org(&info, &map), None);
}
