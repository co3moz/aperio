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
    .create(&payload.username, &payload.password, role);
  match created {
    Ok(user) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "user_created",
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
    .retain(|_, info| info.username.as_deref() != Some(username.as_str()));
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit("user_deleted", &ip, &format!("username={}", username))
    .await;
  StatusCode::OK.into_response()
}
