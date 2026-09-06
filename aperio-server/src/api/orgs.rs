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
use tracing::info;

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
/// The response for a change to an organization that did not happen.
fn org_error(e: crate::store::orgs::OrgError) -> axum::response::Response {
  use crate::store::orgs::OrgError;
  match e {
    OrgError::NotSaved => crate::api::tokens::not_persisted(),
    OrgError::NoSuchOrg => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    OrgError::Invalid(m) => (StatusCode::BAD_REQUEST, m).into_response(),
  }
}

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
      "custom_name": org.custom_name,
      "master": false,
      "created_at": org.created_at,
      "users": c.0,
      "tokens": c.1,
      "hostnames": org.hostnames,
      "panel_hostname": org.panel_hostname,
    }));
  }
  Json(out).into_response()
}

/// Body of the create-org call.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgCreateRequest {
  /// The handle: a-z, 0-9 and `_`. Fixed once created, because it is what
  /// `payments@postgres` and an `expose:` rule name.
  pub(crate) name: String,
  /// What to call it on screen. Free text and editable later; absent means
  /// the handle is shown.
  #[serde(default)]
  pub(crate) custom_name: Option<String>,
  /// Optional hostname allowlist fencing every bind made inside the org:
  /// exact hostnames (`acme.com`) and/or subdomain wildcards (`*.acme.com`).
  /// Absent or empty = unrestricted.
  #[serde(default)]
  pub(crate) hostnames: Vec<String>,
  /// Optional panel hostname, a name inside the allowlist whose root is this
  /// organization's dashboard (`planned_features.md` #152).
  #[serde(default)]
  pub(crate) panel_hostname: Option<String>,
}

/// Body of the set-panel call: the hostname whose root is the organization's
/// dashboard, or empty to have none.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgPanelRequest {
  #[serde(default)]
  pub(crate) hostname: Option<String>,
}

/// Checks a panel hostname for organization `id` against everything that
/// can refuse it: the spelling, the fence (a panel is a name the tenant
/// claims, so it is fenced like a bind, and an unfenced organization has
/// nothing to check it against), the server's own panel, and every other
/// organization's. `Ok(None)` clears.
#[allow(clippy::result_large_err)] // see api/tokens.rs
async fn check_panel_hostname(
  state: &Arc<AppState>,
  id: &str,
  fence: &[String],
  raw: Option<&str>,
) -> Result<Option<String>, Response> {
  let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
    return Ok(None);
  };
  let Some(host) = crate::store::orgs::normalize_panel_hostname(raw) else {
    return Err(
      (
        StatusCode::BAD_REQUEST,
        format!("panel hostname {raw:?} is not a hostname: one exact name, no pattern"),
      )
        .into_response(),
    );
  };
  if fence.is_empty() {
    return Err(
      (
        StatusCode::BAD_REQUEST,
        "a panel hostname is a name the organization claims, so the organization needs a \
         hostname allowlist first; there is nothing to check it against",
      )
        .into_response(),
    );
  }
  if !crate::store::orgs::hostname_in_org_allowlist(&host, fence) {
    return Err(
      (
        StatusCode::FORBIDDEN,
        format!(
          "panel hostname {} is outside this organization's allowlist ({})",
          host,
          fence.join(", ")
        ),
      )
        .into_response(),
    );
  }
  if state.config().dashboard_hostname.as_deref() == Some(host.as_str()) {
    return Err(
      (
        StatusCode::CONFLICT,
        format!("{host} is the server's own dashboard hostname"),
      )
        .into_response(),
    );
  }
  let taken = state
    .org_store
    .lock()
    .await
    .panel_org_for(&host)
    .is_some_and(|o| o.id != id);
  if taken {
    return Err(
      (
        StatusCode::CONFLICT,
        format!("{host} is already another organization's panel"),
      )
        .into_response(),
    );
  }
  Ok(Some(host))
}

/// Sets or clears an organization's panel hostname. Open to an Admin of that
/// organization, which the master super-admin is through `*`: a tenant
/// picks a subdomain it owns and gets its own dashboard at the root of it.
/// A bind currently serving the name is dropped, as when a fence changes.
#[utoipa::path(put, path = "/aperio/api/orgs/{id}/panel", tag = "orgs",
  description = "Sets or clears an organization's panel hostname, a name inside its allowlist whose root is its dashboard (Admin of that organization).",
  request_body = OrgPanelRequest,
  responses((status = 200, description = "Updated org"), (status = 400, description = "Not a hostname, or the organization has no allowlist"), (status = 403, description = "Outside the allowlist, or not an Admin of the organization"), (status = 404, description = "Unknown org"), (status = 409, description = "Already a panel elsewhere")))]
pub(crate) async fn orgs_panel_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<OrgPanelRequest>,
) -> Response {
  let Some(caller) = crate::auth::resolve_caller(&state, &headers).await else {
    return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
  };
  if id == MASTER_ID {
    return (
      StatusCode::BAD_REQUEST,
      "the master organization's panel is the server's dashboard_hostname setting",
    )
      .into_response();
  }
  if caller.role_in(Some(&id)) != Some(crate::store::users::Role::Admin) {
    return (
      StatusCode::FORBIDDEN,
      "setting an organization's panel takes Admin in that organization",
    )
      .into_response();
  }
  let fence = match state.org_store.lock().await.find(&id) {
    Some(org) => org.hostnames.clone(),
    None => return (StatusCode::NOT_FOUND, "unknown organization id").into_response(),
  };
  let host = match check_panel_hostname(&state, &id, &fence, payload.hostname.as_deref()).await {
    Ok(h) => h,
    Err(resp) => return resp,
  };
  let updated = state
    .org_store
    .lock()
    .await
    .set_panel_hostname(&id, host.clone());
  match updated {
    Ok(org) => {
      let dropped = state.apply_panel_hostnames().await;
      if dropped > 0 {
        info!(
          "Dropped {} connection(s) serving {}, which is now a dashboard panel",
          dropped,
          host.as_deref().unwrap_or("-")
        );
      }
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit_in(
          "org_panel_set",
          &caller.actor(),
          &ip,
          Some(id.clone()),
          &format!(
            "id={} panel_hostname={} dropped_clients={}",
            id,
            host.as_deref().unwrap_or("(cleared)"),
            dropped
          ),
        )
        .await;
      Json(serde_json::json!({
        "id": org.id,
        "name": org.name,
        "panel_hostname": org.panel_hostname,
      }))
      .into_response()
    }
    Err(e) => org_error(e),
  }
}

/// Body of the set-hostnames call: the full replacement allowlist (an empty
/// list clears the fence).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgHostnamesRequest {
  #[serde(default)]
  pub(crate) hostnames: Vec<String>,
}

/// Normalizes an allowlist payload, rejecting entries that are not an exact
/// hostname, a `*.domain` wildcard, or a partial leftmost label like
/// `*-pi.domain`. Duplicates collapse; a bare `*` (or an empty list) means
/// unrestricted and normalizes to an empty list.
fn normalize_allowlist(raw: &[String]) -> Result<Vec<String>, String> {
  let mut out: Vec<String> = Vec::new();
  for entry in raw {
    if entry.trim().is_empty() {
      continue;
    }
    let Some(pattern) = crate::store::orgs::normalize_org_hostname_pattern(entry) else {
      return Err(format!(
        "invalid hostname pattern: {} (use acme.com, *.acme.com, or a single \
         placeholder in the leftmost label such as *-pi.acme.com)",
        entry.trim()
      ));
    };
    // An explicit `*` means no fence at all, so it subsumes every entry.
    if pattern == "*" {
      return Ok(Vec::new());
    }
    if !out.contains(&pattern) {
      out.push(pattern);
    }
  }
  Ok(out)
}

/// Body of the select-org call: the org to view (`master` or a child id;
/// `null`/absent = master).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgSelectRequest {
  #[serde(default)]
  pub(crate) id: Option<String>,
}

/// Switches the session's active organization. Open to any session with a
/// grant that reaches the target: the built-in super-admin reaches every
/// organization, and a named user reaches the ones on their record
/// (`planned_features.md` #153). A per-org OIDC login is fixed to its
/// organization, and an admin key has no session to switch. The selection is
/// stored on the session, so all subsequent list/stats calls scope to it.
#[utoipa::path(post, path = "/aperio/api/orgs/select", tag = "orgs",
  description = "Switches the session's active organization (stored on the session). The target has to be one the caller is granted.",
  request_body = OrgSelectRequest,
  responses((status = 200, description = "Selected", body = serde_json::Value), (status = 403, description = "No grant reaches that organization"), (status = 404, description = "Unknown org")))]
pub(crate) async fn orgs_select_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Json(payload): Json<OrgSelectRequest>,
) -> Response {
  let Some(caller) = crate::auth::resolve_caller(&state, &headers).await else {
    return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
  };
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
  let Some(token) = crate::auth::session_token(&state, &headers) else {
    return (StatusCode::UNAUTHORIZED, "no session").into_response();
  };
  if !caller.may_select(target.as_deref()) {
    return (
      StatusCode::FORBIDDEN,
      "no grant on this account reaches that organization",
    )
      .into_response();
  }
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
  responses((status = 200, description = "Created", body = serde_json::Value), (status = 400, description = "Invalid name"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn orgs_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<OrgCreateRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let hostnames = match normalize_allowlist(&payload.hostnames) {
    Ok(v) => v,
    Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
  };
  // Checked before the record exists, against the fence being written, so
  // a refusal leaves nothing behind. The conflict check uses a placeholder
  // id nothing carries, which every existing panel differs from.
  let panel =
    match check_panel_hostname(&state, "", &hostnames, payload.panel_hostname.as_deref()).await {
      Ok(h) => h,
      Err(resp) => return resp,
    };
  let created = state.org_store.lock().await.create(
    &payload.name,
    hostnames.clone(),
    payload.custom_name.clone(),
  );
  let created = match (created, panel) {
    (Ok(org), Some(host)) => {
      let set = state
        .org_store
        .lock()
        .await
        .set_panel_hostname(&org.id, Some(host));
      if set.is_ok() {
        state.apply_panel_hostnames().await;
      }
      set
    }
    (other, _) => other,
  };
  match created {
    Ok(org) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "org_created",
          &state.session_actor(&headers).await,
          &ip,
          &format!("name={} id={} hostnames={:?}", org.name, org.id, hostnames),
        )
        .await;
      Json(serde_json::json!({
        "id": org.id,
        "name": org.name,
        "custom_name": org.custom_name,
        "hostnames": org.hostnames,
      }))
      .into_response()
    }
    Err(e) => org_error(e),
  }
}

/// Replaces a child organization's hostname allowlist (master super-admin
/// only). Existing tokens keep their records, but a hostname that falls
/// outside the new fence stops being bindable immediately: the fence is
/// re-checked on every client connect, not only at token creation.
/// Body of the rename call.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgCustomNameRequest {
  /// What to call the organization on screen. Absent or blank goes back to
  /// showing the handle.
  #[serde(default)]
  pub(crate) custom_name: Option<String>,
}

/// Renames what an organization is *called*.
///
/// The handle is not touched and is not touchable: an `expose:` rule, a
/// binder's config and every `<org>@<tunnel>` written down elsewhere point at
/// it, and none of those can be updated from this screen. Which is exactly
/// why a display name exists, so the thing people read can change without
/// the thing machines read moving underneath them.
#[utoipa::path(put, path = "/aperio/api/orgs/{id}/custom-name", tag = "dashboard",
  description = "Sets an organization's display name; the handle it is addressed by never changes (master admin).",
  request_body = OrgCustomNameRequest,
  responses((status = 200, description = "Updated"), (status = 404, description = "No such organization"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn orgs_custom_name_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<OrgCustomNameRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if id == MASTER_ID {
    return (
      StatusCode::BAD_REQUEST,
      "the master organization is built in and cannot be renamed",
    )
      .into_response();
  }
  let renamed = state
    .org_store
    .lock()
    .await
    .set_custom_name(&id, payload.custom_name.clone());
  if let Err(e) = renamed {
    return org_error(e);
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit(
      "org_renamed",
      &state.session_actor(&headers).await,
      &ip,
      &format!(
        "id={id} custom_name={}",
        payload.custom_name.as_deref().unwrap_or("(cleared)")
      ),
    )
    .await;
  StatusCode::OK.into_response()
}

#[utoipa::path(put, path = "/aperio/api/orgs/{id}/hostnames", tag = "orgs",
  description = "Replaces a child org's hostname allowlist (empty list = unrestricted).",
  request_body = OrgHostnamesRequest,
  responses((status = 200, description = "Updated org"), (status = 400, description = "Invalid pattern"), (status = 404, description = "Unknown org"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn orgs_hostnames_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<OrgHostnamesRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if id == MASTER_ID {
    return (
      StatusCode::BAD_REQUEST,
      "the master organization is never fenced to a hostname list",
    )
      .into_response();
  }
  let hostnames = match normalize_allowlist(&payload.hostnames) {
    Ok(v) => v,
    Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
  };
  let updated = state
    .org_store
    .lock()
    .await
    .set_hostnames(&id, hostnames.clone());
  // A panel is a name inside the fence; a fence that no longer covers it
  // takes it away, rather than leaving a dashboard on a name the tenant no
  // longer claims.
  let panel_cleared = {
    let mut orgs = state.org_store.lock().await;
    let stale = orgs
      .find(&id)
      .and_then(|o| o.panel_hostname.clone())
      .filter(|panel| {
        hostnames.is_empty() || !crate::store::orgs::hostname_in_org_allowlist(panel, &hostnames)
      });
    match stale {
      Some(panel) => {
        let _ = orgs.set_panel_hostname(&id, None);
        Some(panel)
      }
      None => None,
    }
  };
  if let Some(panel) = &panel_cleared {
    info!(
      "Organization {} no longer claims {}, so it is no longer its panel",
      id, panel
    );
    state.refresh_panel_hostnames().await;
  }
  match updated {
    Ok(org) => {
      // Push the new fence onto the org's live connections so it really does
      // apply at once, rather than at each client's next reconnect.
      let dropped = state.apply_org_hostnames(&id, &hostnames).await;
      if dropped > 0 {
        info!(
          "Dropped {} connection(s) of organization {} now serving a hostname outside its allowlist",
          dropped, id
        );
      }
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "org_hostnames_set",
          &state.session_actor(&headers).await,
          &ip,
          &format!(
            "id={} hostnames={:?} dropped_clients={}",
            id, hostnames, dropped
          ),
        )
        .await;
      Json(serde_json::json!({
        "id": org.id,
        "name": org.name,
        "hostnames": org.hostnames,
      }))
      .into_response()
    }
    Err(e) => org_error(e),
  }
}

/// Deletes a child organization. Rejected while it still has users or tokens
/// (move or delete them first), so nothing is silently orphaned.
#[utoipa::path(delete, path = "/aperio/api/orgs/{id}", tag = "orgs",
  description = "Deletes an empty child organization (master super-admin only); rejected while it still has users or tokens.",
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown org"), (status = 409, description = "Organization not empty"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
  if let Err(e) = state.org_store.lock().await.delete(&id) {
    return org_error(e);
  }
  // Clear the maintenance flags it owns. Nothing else could: a flag is
  // cleared by the organization that set it, and that organization no longer
  // exists, so the hostname would answer 503 until the next restart with no
  // screen anywhere showing why.
  let cleared = {
    let mut set = state.maintenance.lock().await;
    let before = set.len();
    set.retain(|_, flag| flag.org.as_deref() != Some(id.as_str()));
    before - set.len()
  };
  if cleared > 0 {
    info!(
      "Cleared {} maintenance flag(s) owned by the deleted organization {}",
      cleared, id
    );
  }
  // Drop any cached per-org OIDC runtime so a `?org=<deleted-id>` login can no
  // longer complete against the phantom org after it is gone.
  state.org_oidc.lock().await.remove(&id);
  // And take it off any session that was viewing it: a selection pointing at
  // an organization that no longer exists shows every org-scoped screen as
  // empty, which reads as "everything is gone" rather than as "you are
  // looking at nothing".
  state.sessions.lock().await.clear_selected_org(&id);
  // Its panel hostname, if it had one, is nobody's now.
  state.refresh_panel_hostnames().await;
  // And off every user it was granted to. The organization had no users of
  // its own (refused above), but a user living in master may reach it by a
  // grant, and a grant naming nothing is a row that says something false.
  let stripped = state.users.lock().await.remove_org_grants(&id);
  if !stripped.is_empty() {
    info!(
      "Removed grants on the deleted organization {} from {} user(s): {}",
      id,
      stripped.len(),
      stripped.join(", ")
    );
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
  responses((status = 200, description = "Updated org"), (status = 404, description = "Unknown org"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
    Ok(org) => {
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
    Err(e) => org_error(e),
  }
}

/// Payload for setting an org's OIDC override. Empty `issuer` clears it.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct OrgOidcRequest {
  #[serde(default)]
  pub(crate) issuer: String,
  #[serde(default)]
  pub(crate) client_id: String,
  #[serde(default)]
  pub(crate) client_secret: String,
  #[serde(default)]
  pub(crate) allowed_emails: Vec<String>,
  /// The role an email with no record gets in this organization at its
  /// first login: `admin` (the default, what such a login always was),
  /// `operator`, `viewer`, or `none` for nothing until an admin grants it.
  #[serde(default)]
  pub(crate) default_role: Option<String>,
  /// What each value of the groups claim means here, `<group>=<role>`.
  #[serde(default)]
  pub(crate) group_grants: Vec<String>,
}

/// Sets or clears a child org's OIDC SSO override (master super-admin only).
/// The client secret is write-only; the response never echoes it.
#[utoipa::path(put, path = "/aperio/api/orgs/{id}/oidc", tag = "orgs",
  description = "Sets or clears a child org's OIDC override (empty issuer clears).",
  request_body = OrgOidcRequest,
  responses((status = 200, description = "Updated"), (status = 404, description = "Unknown org")))]
pub(crate) async fn orgs_oidc_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<OrgOidcRequest>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if id == MASTER_ID {
    return (
      StatusCode::BAD_REQUEST,
      "the master organization uses the global OIDC settings",
    )
      .into_response();
  }
  let oidc = if payload.issuer.trim().is_empty() {
    None
  } else {
    let allowed_emails: Vec<String> = payload
      .allowed_emails
      .iter()
      .map(|e| e.trim().to_ascii_lowercase())
      .filter(|e| !e.is_empty())
      .collect();
    if payload.client_id.trim().is_empty() || payload.client_secret.trim().is_empty() {
      return (
        StatusCode::BAD_REQUEST,
        "client_id and client_secret are required",
      )
        .into_response();
    }
    if allowed_emails.is_empty() {
      return (
        StatusCode::BAD_REQUEST,
        "allowed_emails must list at least one pattern",
      )
        .into_response();
    }
    let default_role = match payload.default_role.as_deref().map(str::trim) {
      None | Some("") | Some("admin") => Some(crate::store::users::Role::Admin),
      Some("none") => None,
      Some(raw) => match crate::store::users::Role::parse(raw) {
        Some(role) => Some(role),
        None => {
          return (
            StatusCode::BAD_REQUEST,
            "default_role must be admin, operator, viewer, or none",
          )
            .into_response();
        }
      },
    };
    let mut group_grants = Vec::new();
    for entry in &payload.group_grants {
      let entry = entry.trim();
      if entry.is_empty() {
        continue;
      }
      let ok = entry.split_once('=').is_some_and(|(g, r)| {
        !g.trim().is_empty() && crate::store::users::Role::parse(r).is_some()
      });
      if !ok {
        return (
          StatusCode::BAD_REQUEST,
          format!("group_grants entries are written <group>=<role>, got {entry:?}"),
        )
          .into_response();
      }
      group_grants.push(entry.to_string());
    }
    Some(crate::store::orgs::OrgOidc {
      issuer: payload.issuer.trim().to_string(),
      client_id: payload.client_id.trim().to_string(),
      client_secret: payload.client_secret,
      allowed_emails,
      default_role,
      group_grants,
    })
  };
  let configured = oidc.is_some();
  let updated = state.org_store.lock().await.set_oidc(&id, oidc);
  // Drop any cached runtime so the next login rebuilds from the new config.
  state.org_oidc.lock().await.remove(&id);
  match updated {
    Ok(_) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit(
          "org_oidc_updated",
          &state.session_actor(&headers).await,
          &ip,
          &format!("id={id} configured={configured}"),
        )
        .await;
      Json(serde_json::json!({ "id": id, "configured": configured })).into_response()
    }
    Err(e) => org_error(e),
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
    .write()
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
    "hostnames": quota.as_ref().map(|q| q.hostnames.clone()).unwrap_or_default(),
    "panel_hostname": match org_key {
      Some(oid) => state.org_store.lock().await.find(oid).and_then(|o| o.panel_hostname.clone()),
      None => state.config().dashboard_hostname.clone(),
    },
  });
  // Billing signal: subscribers to `org_usage` receive the same figures.
  state
    .emit_event_in("org_usage", usage.clone(), org_id_opt)
    .await;
  Json(usage).into_response()
}

#[cfg(test)]
#[path = "orgs_tests.rs"]
mod tests;
