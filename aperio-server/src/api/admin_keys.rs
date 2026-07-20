//! Dashboard API for programmatic admin keys (`/aperio/api/admin-keys`).
//!
//! Management is restricted to the master-organization admin: admin keys are
//! powerful, cross-org credentials, so only the top-level admin mints and
//! revokes them. Each key is scoped to a role + organization and its secret is
//! returned exactly once at creation.

use axum::{
  Json,
  extract::{ConnectInfo, Path, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::users::Role;

/// Public view of an admin key (never includes the hash or secret).
#[derive(Serialize)]
pub(crate) struct AdminKeyView {
  pub(crate) id: String,
  pub(crate) name: String,
  pub(crate) key_prefix: String,
  pub(crate) role: Role,
  pub(crate) org_id: Option<String>,
  pub(crate) created_at: u64,
  pub(crate) expires_at: Option<u64>,
  pub(crate) expired: bool,
}

/// Lists programmatic admin keys (metadata only).
#[utoipa::path(get, path = "/aperio/api/admin-keys", tag = "admin-keys",
  description = "Lists programmatic admin keys (hashes stripped).",
  responses((status = 200, description = "Admin key records", body = serde_json::Value)))]
pub(crate) async fn admin_keys_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let store = state.admin_key_store.lock().await;
  let views: Vec<AdminKeyView> = store
    .list()
    .iter()
    .map(|k| AdminKeyView {
      id: k.id.clone(),
      name: k.name.clone(),
      key_prefix: k.key_prefix.clone(),
      role: k.role,
      org_id: k.org_id.clone(),
      created_at: k.created_at,
      expires_at: k.expires_at,
      expired: k.is_expired(),
    })
    .collect();
  Json(views).into_response()
}

/// Payload for creating an admin key.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct AdminKeyCreateRequest {
  pub(crate) name: String,
  /// Role the key authenticates as: viewer / operator / admin.
  pub(crate) role: String,
  /// Organization the key acts within (None/absent = master).
  #[serde(default)]
  pub(crate) org_id: Option<String>,
  /// Optional lifetime in seconds; omitted = never expires.
  pub(crate) ttl_seconds: Option<u64>,
}

/// Creates an admin key. The plaintext secret is returned exactly once.
#[utoipa::path(post, path = "/aperio/api/admin-keys", tag = "admin-keys",
  description = "Creates a scoped admin key; the secret is returned once.",
  request_body = AdminKeyCreateRequest,
  responses((status = 200, description = "Created key + secret", body = serde_json::Value), (status = 400, description = "Invalid role")))]
pub(crate) async fn admin_keys_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<AdminKeyCreateRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  let name = payload.name.trim().to_string();
  if name.is_empty() || name.len() > 64 {
    return (StatusCode::BAD_REQUEST, "Key name must be 1-64 characters").into_response();
  }
  let Some(role) = Role::parse(&payload.role) else {
    return (
      StatusCode::BAD_REQUEST,
      "role must be viewer, operator or admin",
    )
      .into_response();
  };
  let org_id = payload
    .org_id
    .map(|o| o.trim().to_string())
    .filter(|o| !o.is_empty());

  // Validate the target org exists (None = master, always valid).
  if let Some(ref oid) = org_id
    && !state
      .org_store
      .lock()
      .await
      .list()
      .iter()
      .any(|o| &o.id == oid)
  {
    return (StatusCode::BAD_REQUEST, "unknown organization").into_response();
  }

  let (record, secret) =
    state
      .admin_key_store
      .lock()
      .await
      .create(name, role, org_id, payload.ttl_seconds);
  info!(
    "Admin key created: {} (id={}, role={}, org={:?})",
    record.name,
    record.id,
    record.role.as_str(),
    record.org_id
  );
  state
    .audit_session(
      "admin_key_created",
      &headers,
      &actor_ip,
      &format!(
        "name={} id={} role={} org={:?}",
        record.name,
        record.id,
        record.role.as_str(),
        record.org_id
      ),
    )
    .await;
  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": record.id,
      "name": record.name,
      "role": record.role.as_str(),
      "org_id": record.org_id,
      "expires_at": record.expires_at,
      "key": secret,
    })),
  )
    .into_response()
}

/// Revokes an admin key by id.
#[utoipa::path(delete, path = "/aperio/api/admin-keys/{id}", tag = "admin-keys",
  description = "Revokes an admin key.",
  params(("id" = String, Path, description = "Admin key id")),
  responses((status = 200, description = "Revoked"), (status = 404, description = "Unknown id")))]
pub(crate) async fn admin_keys_revoke_handler(
  State(state): State<Arc<AppState>>,
  Path(id): Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  let revoked = state.admin_key_store.lock().await.revoke(&id);
  if !revoked {
    return (StatusCode::NOT_FOUND, "Admin key not found").into_response();
  }
  info!("Admin key revoked: id={}", id);
  state
    .audit_session(
      "admin_key_revoked",
      &headers,
      &actor_ip,
      &format!("id={id}"),
    )
    .await;
  (StatusCode::OK, "revoked").into_response()
}

#[cfg(test)]
#[path = "admin_keys_tests.rs"]
mod tests;
