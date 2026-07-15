use axum::{
  Json,
  extract::{ConnectInfo, Path, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::users::{Role, User};

/// A user as exposed through the API (never includes the password hash).
fn user_view(u: &User) -> serde_json::Value {
  serde_json::json!({
    "id": u.id,
    "username": u.username,
    "role": u.role.as_str(),
    "created_at": u.created_at,
    "enabled": u.enabled,
    "totp": u.totp_secret.is_some(),
  })
}

fn actor_ip(state: &Arc<AppState>, headers: &HeaderMap, addr: SocketAddr) -> String {
  extract_client_ip(
    headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string()
}

#[utoipa::path(get, path = "/aperio/api/users", tag = "users",
  description = "Lists dashboard users (admin only; password hashes are never exposed).",
  responses((status = 200, description = "User records", body = serde_json::Value)))]
pub(crate) async fn users_list_handler(
  State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
  let users = state.users.lock().await;
  Json(serde_json::json!(
    users.list().iter().map(user_view).collect::<Vec<_>>()
  ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct UserCreateRequest {
  pub(crate) username: String,
  /// At least 8 characters.
  pub(crate) password: String,
  /// One of `viewer`, `operator`, `admin`.
  pub(crate) role: String,
}

#[utoipa::path(post, path = "/aperio/api/users", tag = "users",
  description = "Creates a dashboard user with a role (admin only).",
  request_body = UserCreateRequest,
  responses((status = 200, description = "Created user", body = serde_json::Value), (status = 400, description = "Invalid username/password/role")))]
pub(crate) async fn users_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<UserCreateRequest>,
) -> Response {
  let Some(role) = Role::parse(&payload.role) else {
    return (
      StatusCode::BAD_REQUEST,
      "role must be viewer, operator, or admin",
    )
      .into_response();
  };
  let created = state
    .users
    .lock()
    .await
    .create(&payload.username, &payload.password, role, None);
  match created {
    Ok(user) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "user_created",
          &state.session_actor(&headers).await,
          &ip,
          &format!("username={} role={}", user.username, user.role.as_str()),
        )
        .await;
      state
        .emit_event(
          "user_created",
          serde_json::json!({"username": user.username, "role": user.role.as_str()}),
        )
        .await;
      Json(user_view(&user)).into_response()
    }
    Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
  }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct UserUpdateRequest {
  /// New role (`viewer` / `operator` / `admin`); omit to keep.
  pub(crate) role: Option<String>,
  /// Enable/disable the account; omit to keep.
  pub(crate) enabled: Option<bool>,
  /// New password (at least 8 characters); omit to keep.
  pub(crate) password: Option<String>,
}

#[utoipa::path(put, path = "/aperio/api/users/{id}", tag = "users",
  description = "Updates a user's role, enabled state, or password (admin only).",
  params(("id" = String, Path, description = "User record id")),
  request_body = UserUpdateRequest,
  responses((status = 200, description = "Updated user", body = serde_json::Value), (status = 400, description = "Invalid value"), (status = 404, description = "Unknown user id")))]
pub(crate) async fn users_update_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<UserUpdateRequest>,
) -> Response {
  let role = match payload.role.as_deref() {
    Some(raw) => match Role::parse(raw) {
      Some(r) => Some(r),
      None => {
        return (
          StatusCode::BAD_REQUEST,
          "role must be viewer, operator, or admin",
        )
          .into_response();
      }
    },
    None => None,
  };
  let updated =
    state
      .users
      .lock()
      .await
      .update(&id, role, payload.enabled, payload.password.as_deref());
  match updated {
    Ok(user) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "user_updated",
          &state.session_actor(&headers).await,
          &ip,
          &format!(
            "username={} role={} enabled={} password_changed={}",
            user.username,
            user.role.as_str(),
            user.enabled,
            payload.password.is_some()
          ),
        )
        .await;
      Json(user_view(&user)).into_response()
    }
    Err(e) if e == "unknown user id" => (StatusCode::NOT_FOUND, e).into_response(),
    Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
  }
}

#[utoipa::path(delete, path = "/aperio/api/users/{id}", tag = "users",
  description = "Deletes a dashboard user (admin only). Live sessions of that user are dropped.",
  params(("id" = String, Path, description = "User record id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown user id")))]
pub(crate) async fn users_delete_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let username = {
    let users = state.users.lock().await;
    users
      .list()
      .iter()
      .find(|u| u.id == id)
      .map(|u| u.username.clone())
  };
  let Some(username) = username else {
    return (StatusCode::NOT_FOUND, "unknown user id").into_response();
  };
  state.users.lock().await.delete(&id);
  // Deleting an account must end its live sessions too.
  state
    .sessions
    .lock()
    .await
    .retain(|info| info.username.as_deref() != Some(username.as_str()));
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "user_deleted",
      &state.session_actor(&headers).await,
      &ip,
      &format!("username={}", username),
    )
    .await;
  StatusCode::OK.into_response()
}

/// Resolves the calling session to its user row. The built-in admin
/// ("aperio", from the master token / dashboard password / OIDC) has no user
/// row and cannot enroll TOTP.
async fn session_user_id(state: &Arc<AppState>, headers: &HeaderMap) -> Result<String, Response> {
  let Some(username) = crate::auth::dashboard_username(state, headers).await else {
    return Err(
      (
        StatusCode::BAD_REQUEST,
        "Two-factor authentication applies to named dashboard users; the built-in admin signs in with the master token or dashboard password",
      )
        .into_response(),
    );
  };
  match state.users.lock().await.find_by_username(&username) {
    Some(user) => Ok(user.id.clone()),
    None => Err((StatusCode::BAD_REQUEST, "Unknown user").into_response()),
  }
}

fn now_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

/// Starts TOTP enrollment for the signed-in user.
#[utoipa::path(post, path = "/aperio/api/me/totp/setup", tag = "users",
  description = "Begins TOTP enrollment for the signed-in dashboard user: returns a fresh secret and otpauth:// URL. Enrollment takes effect only after /aperio/api/me/totp/enable verifies a code.",
  responses((status = 200, description = "Pending secret and provisioning URL", body = serde_json::Value), (status = 400, description = "No user row (built-in admin)")))]
pub(crate) async fn totp_setup_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let user_id = match session_user_id(&state, &headers).await {
    Ok(id) => id,
    Err(resp) => return resp,
  };
  let (secret, username) = {
    let mut users = state.users.lock().await;
    let secret = match users.totp_begin(&user_id) {
      Ok(s) => s,
      Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let username = users
      .get(&user_id)
      .map(|u| u.username.clone())
      .unwrap_or_default();
    (secret, username)
  };
  Json(serde_json::json!({
    "secret": secret,
    "otpauth_url": crate::totp::otpauth_url(&username, &secret),
  }))
  .into_response()
}

/// Body for TOTP enable/disable: the current authenticator code.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct TotpCodeRequest {
  pub(crate) code: String,
}

/// Completes TOTP enrollment for the signed-in user.
#[utoipa::path(post, path = "/aperio/api/me/totp/enable", tag = "users",
  description = "Completes TOTP enrollment by verifying a code against the pending secret. Returns the single-use recovery codes — shown exactly once.",
  request_body = TotpCodeRequest,
  responses((status = 200, description = "Recovery codes", body = serde_json::Value), (status = 400, description = "Invalid code or no enrollment in progress")))]
pub(crate) async fn totp_enable_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TotpCodeRequest>,
) -> Response {
  let user_id = match session_user_id(&state, &headers).await {
    Ok(id) => id,
    Err(resp) => return resp,
  };
  let result = state
    .users
    .lock()
    .await
    .totp_enable(&user_id, &payload.code, now_secs());
  match result {
    Ok(recovery_codes) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "totp_enabled",
          &state.session_actor(&headers).await,
          &ip,
          &format!("user_id={}", user_id),
        )
        .await;
      Json(serde_json::json!({ "recovery_codes": recovery_codes })).into_response()
    }
    Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
  }
}

/// Disables TOTP for the signed-in user (requires a valid current code).
#[utoipa::path(delete, path = "/aperio/api/me/totp", tag = "users",
  description = "Disables TOTP for the signed-in user. Requires a currently valid authenticator code (or an unused recovery code).",
  request_body = TotpCodeRequest,
  responses((status = 200, description = "Disabled"), (status = 400, description = "TOTP not enabled"), (status = 401, description = "Invalid code")))]
pub(crate) async fn totp_disable_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TotpCodeRequest>,
) -> Response {
  let user_id = match session_user_id(&state, &headers).await {
    Ok(id) => id,
    Err(resp) => return resp,
  };
  let secret = {
    let users = state.users.lock().await;
    match users.get(&user_id).and_then(|u| u.totp_secret.clone()) {
      Some(s) => s,
      None => {
        return (StatusCode::BAD_REQUEST, "TOTP is not enabled for this user").into_response();
      }
    }
  };
  let ok = crate::totp::verify(&secret, &payload.code, now_secs())
    || state
      .users
      .lock()
      .await
      .consume_recovery(&user_id, &payload.code);
  if !ok {
    return (StatusCode::UNAUTHORIZED, "Invalid code").into_response();
  }
  if let Err(e) = state.users.lock().await.totp_disable(&user_id) {
    return (StatusCode::BAD_REQUEST, e).into_response();
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "totp_disabled",
      &state.session_actor(&headers).await,
      &ip,
      &format!("user_id={}", user_id),
    )
    .await;
  Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Admin reset: clears TOTP for a locked-out user.
#[utoipa::path(delete, path = "/aperio/api/users/{id}/totp", tag = "users",
  description = "Clears TOTP for a user (admin only) — the escape hatch when someone loses their authenticator and recovery codes.",
  params(("id" = String, Path, description = "User id")),
  responses((status = 200, description = "Cleared"), (status = 404, description = "Unknown user")))]
pub(crate) async fn totp_admin_reset_handler(
  State(state): State<Arc<AppState>>,
  Path(id): Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  if state.users.lock().await.get(&id).is_none() {
    return (StatusCode::NOT_FOUND, "Unknown user").into_response();
  }
  if let Err(e) = state.users.lock().await.totp_disable(&id) {
    return (StatusCode::BAD_REQUEST, e).into_response();
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "totp_admin_reset",
      &state.session_actor(&headers).await,
      &ip,
      &format!("user_id={}", id),
    )
    .await;
  Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Reads the caller's `aperio_session` cookie value (to mark or exempt the
/// current session in the management endpoints).
fn own_session_token(headers: &HeaderMap) -> Option<String> {
  let cookie_str = headers.get("cookie")?.to_str().ok()?;
  cookie_str.split(';').find_map(|part| {
    let (k, v) = part.trim().split_once('=')?;
    (k == "aperio_session").then(|| v.to_string())
  })
}

/// Lists live sessions (admin): who is signed in from where. Ids are the
/// SHA-256 of the session token — usable for revocation, useless for
/// hijacking.
#[utoipa::path(get, path = "/aperio/api/sessions", tag = "users",
  description = "Live sessions with identity, IP, User-Agent and age; the caller's own session is marked.",
  responses((status = 200, description = "Live sessions", body = serde_json::Value)))]
pub(crate) async fn sessions_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  let own = own_session_token(&headers);
  let mut entries = state.sessions.lock().await.entries();
  entries.sort_by_key(|(_, info)| std::cmp::Reverse(info.created_at));
  Json(
    entries
      .into_iter()
      .map(|(key, info)| {
        let current = own.as_deref().is_some_and(|token| {
          crate::store::sessions::SessionStore::token_matches_key(token, &key)
        });
        serde_json::json!({
          "id": key,
          "username": info.username.as_deref().unwrap_or("aperio"),
          "role": info.role.as_str(),
          "scope_host": info.scope_host,
          "ip": info.ip,
          "user_agent": info.user_agent,
          "created_at": info.created_at,
          "expires_at": info.expires_at,
          "current": current,
        })
      })
      .collect(),
  )
}

/// Revokes one session by its id (admin).
#[utoipa::path(delete, path = "/aperio/api/sessions/{id}", tag = "users",
  description = "Ends one session immediately; its cookie stops working on the next request.",
  responses((status = 200, description = "Session ended"), (status = 404, description = "Unknown session id")))]
pub(crate) async fn session_revoke_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let removed = state.sessions.lock().await.remove_by_key(&id);
  if !removed {
    return (StatusCode::NOT_FOUND, "unknown session id").into_response();
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "session_revoked",
      &state.session_actor(&headers).await,
      &ip,
      &format!("session={}", &id[..12.min(id.len())]),
    )
    .await;
  StatusCode::OK.into_response()
}

/// Ends every session except the caller's ("sign out everywhere", admin).
#[utoipa::path(delete, path = "/aperio/api/sessions", tag = "users",
  description = "Ends every live session except the caller's own; everyone else must sign in again.",
  responses((status = 200, description = "Sessions ended", body = serde_json::Value)))]
pub(crate) async fn sessions_clear_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let own = own_session_token(&headers);
  let mut sessions = state.sessions.lock().await;
  let before = sessions.entries().len();
  let own_keys: Vec<String> = sessions
    .entries()
    .into_iter()
    .map(|(k, _)| k)
    .filter(|k| {
      own
        .as_deref()
        .is_some_and(|token| crate::store::sessions::SessionStore::token_matches_key(token, k))
    })
    .collect();
  sessions.retain_keys(&own_keys);
  let ended = before - own_keys.len();
  drop(sessions);
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "sessions_cleared",
      &state.session_actor(&headers).await,
      &ip,
      &format!("ended={ended}"),
    )
    .await;
  Json(serde_json::json!({ "ended": ended })).into_response()
}
