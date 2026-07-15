//! Organization management API (master super-admin only). The master
//! organization is implicit (`org_id: None`) and is surfaced here as a
//! synthetic entry with id `master`; only child organizations are stored.

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
use crate::store::orgs::MASTER_ID;

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

/// Counts the users and tokens belonging to each org id (`None` = master).
async fn org_member_counts(
  state: &Arc<AppState>,
) -> std::collections::HashMap<Option<String>, (usize, usize)> {
  let mut counts: std::collections::HashMap<Option<String>, (usize, usize)> =
    std::collections::HashMap::new();
  for u in state.users.lock().await.list() {
    counts.entry(u.org_id.clone()).or_default().0 += 1;
  }
  for t in state.token_store.lock().await.list() {
    counts.entry(t.org_id.clone()).or_default().1 += 1;
  }
  counts
}

/// Lists organizations: the implicit master org first, then child orgs, each
/// with its user and token counts.
#[utoipa::path(get, path = "/aperio/api/orgs", tag = "orgs",
  description = "Lists organizations (master super-admin only): the implicit master org plus child orgs, with user/token counts.",
  responses((status = 200, description = "Organizations", body = serde_json::Value)))]
pub(crate) async fn orgs_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let counts = org_member_counts(&state).await;
  let master = counts.get(&None).copied().unwrap_or((0, 0));
  let mut out = vec![serde_json::json!({
    "id": MASTER_ID,
    "name": "master",
    "master": true,
    "users": master.0,
    "tokens": master.1,
  })];
  for org in state.org_store.lock().await.list() {
    let c = counts.get(&Some(org.id.clone())).copied().unwrap_or((0, 0));
    out.push(serde_json::json!({
      "id": org.id,
      "name": org.name,
      "master": false,
      "created_at": org.created_at,
      "users": c.0,
      "tokens": c.1,
    }));
  }
  Json(out).into_response()
}

/// Body of the create-org call.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgCreateRequest {
  pub(crate) name: String,
}

/// Creates a child organization.
#[utoipa::path(post, path = "/aperio/api/orgs", tag = "orgs",
  description = "Creates a child organization (master super-admin only).",
  request_body = OrgCreateRequest,
  responses((status = 200, description = "Created", body = serde_json::Value), (status = 400, description = "Invalid name")))]
pub(crate) async fn orgs_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<OrgCreateRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let created = state.org_store.lock().await.create(&payload.name);
  match created {
    Ok(org) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "org_created",
          &state.session_actor(&headers).await,
          &ip,
          &format!("name={} id={}", org.name, org.id),
        )
        .await;
      Json(serde_json::json!({ "id": org.id, "name": org.name })).into_response()
    }
    Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
  }
}

/// Deletes a child organization. Rejected while it still has users or tokens
/// (move or delete them first), so nothing is silently orphaned.
#[utoipa::path(delete, path = "/aperio/api/orgs/{id}", tag = "orgs",
  description = "Deletes an empty child organization (master super-admin only); rejected while it still has users or tokens.",
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown org"), (status = 409, description = "Organization not empty")))]
pub(crate) async fn orgs_delete_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if id == MASTER_ID {
    return (
      StatusCode::BAD_REQUEST,
      "the master organization cannot be deleted",
    )
      .into_response();
  }
  // Refuse to orphan members.
  let counts = org_member_counts(&state).await;
  if let Some((users, tokens)) = counts.get(&Some(id.clone()))
    && (*users > 0 || *tokens > 0)
  {
    return (
      StatusCode::CONFLICT,
      format!(
        "organization still has {users} user(s) and {tokens} token(s); move or delete them first"
      ),
    )
      .into_response();
  }
  if !state.org_store.lock().await.delete(&id) {
    return (StatusCode::NOT_FOUND, "unknown organization id").into_response();
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "org_deleted",
      &state.session_actor(&headers).await,
      &ip,
      &format!("id={}", id),
    )
    .await;
  StatusCode::OK.into_response()
}
