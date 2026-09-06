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
use crate::store::grants::GrantOrg;
use crate::store::users::Role;

/// The `org_id` an admin key reports: `null` for master, `*` for every
/// organization, else the child id. One field, so the CLI and the dashboard
/// read a `*` key where they read every other.
fn scope_view(scope: &GrantOrg) -> Option<String> {
  match scope {
    GrantOrg::Master => None,
    GrantOrg::All | GrantOrg::Child(_) => Some(scope.as_str().to_string()),
  }
}

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
      org_id: scope_view(&k.scope()),
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
  /// Organization the key acts within: a child id, `master` (or absent),
  /// or `*` for every organization, which only a holder of `*` Admin may
  /// mint.
  #[serde(default)]
  pub(crate) org_id: Option<String>,
  /// Optional lifetime in seconds; omitted = never expires.
  pub(crate) ttl_seconds: Option<u64>,
}

/// Creates an admin key. The plaintext secret is returned exactly once.
#[utoipa::path(post, path = "/aperio/api/admin-keys", tag = "admin-keys",
  description = "Creates a scoped admin key; the secret is returned once.",
  request_body = AdminKeyCreateRequest,
  responses((status = 200, description = "Created key + secret", body = serde_json::Value), (status = 400, description = "Invalid role"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
  let scope = GrantOrg::parse(payload.org_id.as_deref().unwrap_or(""));
  match &scope {
    // A child id must exist; master always does.
    GrantOrg::Child(oid) => {
      if !state
        .org_store
        .lock()
        .await
        .list()
        .iter()
        .any(|o| &o.id == oid)
      {
        return (StatusCode::BAD_REQUEST, "unknown organization").into_response();
      }
    }
    // `*` is a grant like any other and is bounded by the granter's: a master
    // Admin runs the server and still cannot mint a credential wider than
    // their own reach.
    GrantOrg::All => {
      let holds_all = crate::auth::resolve_caller(&state, &headers)
        .await
        .is_some_and(|c| c.holds_all_admin());
      if !holds_all {
        return (
          StatusCode::FORBIDDEN,
          "a key for every organization (*) takes a caller granted every organization",
        )
          .into_response();
      }
    }
    GrantOrg::Master => {}
  }

  let created = state
    .admin_key_store
    .lock()
    .await
    .create(name, role, scope, payload.ttl_seconds);
  let Some((record, secret)) = created else {
    return crate::api::tokens::not_persisted();
  };
  info!(
    "Admin key created: {} (id={}, role={}, org={})",
    record.name,
    record.id,
    record.role.as_str(),
    record.scope().as_str()
  );
  state
    .audit_session(
      "admin_key_created",
      &headers,
      &actor_ip,
      &format!(
        "name={} id={} role={} org={}",
        record.name,
        record.id,
        record.role.as_str(),
        record.scope().as_str()
      ),
    )
    .await;
  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": record.id,
      "name": record.name,
      "role": record.role.as_str(),
      "org_id": scope_view(&record.scope()),
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
  responses((status = 200, description = "Revoked"), (status = 404, description = "Unknown id"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
  match state.admin_key_store.lock().await.revoke(&id) {
    Ok(()) => {}
    Err(crate::store::NotWritten::NoSuchRecord) => {
      return (StatusCode::NOT_FOUND, "Admin key not found").into_response();
    }
    // Never a 404 here, whatever it costs to say so: this key still
    // authenticates, and "not found" is the one answer that would be read as
    // "already revoked" by the operator pulling a compromised credential.
    Err(crate::store::NotWritten::NotPersisted) => {
      return crate::api::tokens::not_persisted();
    }
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
