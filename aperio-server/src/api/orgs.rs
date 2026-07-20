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

/// Body of the select-org call: the org to view (`master` or a child id;
/// `null`/absent = master).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgSelectRequest {
  #[serde(default)]
  pub(crate) id: Option<String>,
}

/// Switches the master super-admin's active organization. Only the built-in
/// `aperio` super-admin may switch orgs; a named user is pinned to their own
/// org and this is a no-op error for them. The selection is stored on the
/// session, so all subsequent list/stats calls scope to it.
#[utoipa::path(post, path = "/aperio/api/orgs/select", tag = "orgs",
  description = "Switches the master super-admin's active organization (stored on the session). Master super-admin only.",
  request_body = OrgSelectRequest,
  responses((status = 200, description = "Selected", body = serde_json::Value), (status = 404, description = "Unknown org")))]
pub(crate) async fn orgs_select_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Json(payload): Json<OrgSelectRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  // Normalize: the synthetic `master` id and empty mean "master org" (None).
  let target = match payload.id.as_deref() {
    None | Some("") | Some(MASTER_ID) => None,
    Some(id) => {
      // A child id must actually exist.
      if !state
        .org_store
        .lock()
        .await
        .list()
        .iter()
        .any(|o| o.id == id)
      {
        return (StatusCode::NOT_FOUND, "unknown organization id").into_response();
      }
      Some(id.to_string())
    }
  };
  let Some(token) = crate::auth::session_token(&headers) else {
    return (StatusCode::UNAUTHORIZED, "no session").into_response();
  };
  state
    .sessions
    .lock()
    .await
    .set_selected_org(&token, target.clone());
  Json(serde_json::json!({
    "selected": target.as_deref().unwrap_or(MASTER_ID),
  }))
  .into_response()
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

/// Payload for setting an org's quotas. `Some(0)` clears a quota, `Some(n)`
/// sets it, an absent field is left unchanged.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgQuotaRequest {
  pub(crate) max_clients: Option<u64>,
  pub(crate) max_tokens: Option<u64>,
  pub(crate) max_users: Option<u64>,
  pub(crate) max_bytes_month: Option<u64>,
}

/// Sets a child organization's quotas (master super-admin only).
#[utoipa::path(put, path = "/aperio/api/orgs/{id}/quota", tag = "orgs",
  description = "Sets a child org's quotas (max clients/tokens/users, monthly bytes).",
  request_body = OrgQuotaRequest,
  responses((status = 200, description = "Updated org"), (status = 404, description = "Unknown org")))]
pub(crate) async fn orgs_quota_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<OrgQuotaRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if id == MASTER_ID {
    return (
      StatusCode::BAD_REQUEST,
      "the master organization has no quota",
    )
      .into_response();
  }
  // Map Some(0) → clear, Some(n) → set, None → keep.
  let to_opt = |v: Option<u64>| v.map(|n| if n == 0 { None } else { Some(n) });
  let updated = state.org_store.lock().await.set_quota(
    &id,
    to_opt(payload.max_clients),
    to_opt(payload.max_tokens),
    to_opt(payload.max_users),
    to_opt(payload.max_bytes_month),
  );
  match updated {
    Some(org) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "org_quota_updated",
          &state.session_actor(&headers).await,
          &ip,
          &format!(
            "id={} max_clients={:?} max_tokens={:?} max_users={:?} max_bytes_month={:?}",
            org.id, org.max_clients, org.max_tokens, org.max_users, org.max_bytes_month
          ),
        )
        .await;
      Json(serde_json::json!({
        "id": org.id,
        "name": org.name,
        "max_clients": org.max_clients,
        "max_tokens": org.max_tokens,
        "max_users": org.max_users,
        "max_bytes_month": org.max_bytes_month,
      }))
      .into_response()
    }
    None => (StatusCode::NOT_FOUND, "unknown organization id").into_response(),
  }
}

/// Reports an organization's current-month usage against its quotas, and emits
/// an `org_usage` webhook event (a billing integration can subscribe or poll
/// this endpoint on a schedule).
#[utoipa::path(get, path = "/aperio/api/orgs/{id}/usage", tag = "orgs",
  description = "Current-month usage vs quota for an organization; also emits an org_usage webhook.",
  responses((status = 200, description = "Usage report", body = serde_json::Value)))]
pub(crate) async fn orgs_usage_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let org_key: Option<&str> = if id == MASTER_ID {
    None
  } else {
    Some(id.as_str())
  };
  let org_id_opt: Option<String> = org_key.map(|s| s.to_string());

  let month = crate::store::stats::period_keys()[2].clone();
  let period = {
    let stats = state.persistent_stats.lock().await;
    stats
      .snapshot_for_org(org_key)
      .periods
      .get(&month)
      .cloned()
      .unwrap_or_default()
  };
  let month_bytes = period.bytes_sent + period.bytes_received;

  let counts = org_member_counts(&state).await;
  let (users, tokens) = counts.get(&org_id_opt).copied().unwrap_or((0, 0));
  let clients = state
    .clients
    .lock()
    .await
    .values()
    .filter(|c| c.perms.org_id.as_deref() == org_key)
    .count();
  let quota = state.org_quota(org_key).await;

  let usage = serde_json::json!({
    "org_id": id,
    "month": month,
    "requests": period.requests,
    "bytes": month_bytes,
    "clients": clients,
    "tokens": tokens,
    "users": users,
    "quota": quota.as_ref().map(|q| serde_json::json!({
      "max_clients": q.max_clients,
      "max_tokens": q.max_tokens,
      "max_users": q.max_users,
      "max_bytes_month": q.max_bytes_month,
    })),
  });
  // Billing signal: subscribers to `org_usage` receive the same figures.
  state
    .emit_event_in("org_usage", usage.clone(), org_id_opt)
    .await;
  Json(usage).into_response()
}
