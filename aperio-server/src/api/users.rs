use axum::{
  Json,
  extract::{ConnectInfo, Path, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::auth::Caller;
use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::grants::{self, Grant, GrantOrg};
use crate::store::users::{Role, User, UserError};

/// A user as exposed through the API (never includes the password hash).
/// `role` is the role at home; `grants` is what decides anything.
fn user_view(u: &User) -> serde_json::Value {
  serde_json::json!({
    "id": u.id,
    "username": u.username,
    "role": u.role.as_str(),
    "created_at": u.created_at,
    "enabled": u.enabled,
    "totp": u.totp_secret.is_some(),
    "org_id": u.org_id,
    "grants": grants_view(&u.grants),
  })
}

fn grants_view(grants: &[Grant]) -> Vec<serde_json::Value> {
  grants
    .iter()
    .map(|g| serde_json::json!({ "org": g.org.as_str(), "role": g.role.as_str() }))
    .collect()
}

/// `acme:operator,beta:viewer`, for an audit line.
fn grants_label(grants: &[Grant]) -> String {
  grants
    .iter()
    .map(Grant::label)
    .collect::<Vec<_>>()
    .join(",")
}

/// One grant as the API spells it: an organization (`master`, `*`, or a
/// child id) and a role.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct GrantRequest {
  pub(crate) org: String,
  /// One of `viewer`, `operator`, `admin`.
  pub(crate) role: String,
}

/// Turns a request's grant list into grants: every role must parse, every
/// child id must exist, and inside a child organization every grant must
/// name that organization, since a user reaching more than one lives in
/// master where only master manages it.
#[allow(clippy::result_large_err)] // see api/tokens.rs
async fn parse_grants(
  state: &Arc<AppState>,
  effective: Option<&str>,
  raw: &[GrantRequest],
) -> Result<Vec<Grant>, Response> {
  let mut out = Vec::with_capacity(raw.len());
  for entry in raw {
    let Some(role) = Role::parse(&entry.role) else {
      return Err(
        (
          StatusCode::BAD_REQUEST,
          "role must be viewer, operator, or admin",
        )
          .into_response(),
      );
    };
    let org = GrantOrg::parse(&entry.org);
    if let GrantOrg::Child(id) = &org
      && state.org_store.lock().await.find(id).is_none()
    {
      return Err(
        (
          StatusCode::BAD_REQUEST,
          format!("unknown organization {id}"),
        )
          .into_response(),
      );
    }
    if let Some(here) = effective
      && !matches!(&org, GrantOrg::Child(id) if id == here)
    {
      return Err(
        (
          StatusCode::BAD_REQUEST,
          "a user created inside an organization is granted that organization only; a user \
           reaching several organizations is created and managed from master",
        )
          .into_response(),
      );
    }
    out.push(Grant::new(org, role));
  }
  grants::normalize(out).map_err(|m| (StatusCode::BAD_REQUEST, m).into_response())
}

/// A grant is bounded by the granter's: giving or taking away a role in an
/// organization takes Admin there, and `*` takes `*` Admin. Refused as a
/// whole, so a list is never half applied.
#[allow(clippy::result_large_err)] // see api/tokens.rs
fn check_granter(caller: &Caller, changed: &[Grant]) -> Result<(), Response> {
  for g in changed {
    if !caller.may_grant(g) {
      return Err(
        (
          StatusCode::FORBIDDEN,
          format!(
            "you cannot grant or revoke {}: that takes Admin in that organization, and `*` takes `*` Admin",
            g.label()
          ),
        )
          .into_response(),
      );
    }
  }
  Ok(())
}

/// Records one audit event per grant added or removed, naming the granter,
/// so "why does this person have Operator in Acme" has an answer.
async fn audit_grant_changes(
  state: &Arc<AppState>,
  headers: &HeaderMap,
  ip: &str,
  username: &str,
  added: &[Grant],
  removed: &[Grant],
) {
  for g in added {
    state
      .audit_session(
        "user_grant_added",
        headers,
        ip,
        &format!(
          "username={} org={} role={}",
          username,
          g.org.as_str(),
          g.role.as_str()
        ),
      )
      .await;
  }
  for g in removed {
    state
      .audit_session(
        "user_grant_removed",
        headers,
        ip,
        &format!(
          "username={} org={} role={}",
          username,
          g.org.as_str(),
          g.role.as_str()
        ),
      )
      .await;
  }
}

/// Whether a user id exists and belongs to the caller's effective org, so one
/// org cannot edit or delete another's users by id.
async fn user_in_effective_org(state: &Arc<AppState>, headers: &HeaderMap, id: &str) -> bool {
  let org = crate::auth::effective_org(state, headers).await;
  state
    .users
    .lock()
    .await
    .list()
    .iter()
    .any(|u| u.id == id && u.org_id == org)
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
  headers: HeaderMap,
) -> Json<serde_json::Value> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let users = state.users.lock().await;
  Json(serde_json::json!(
    users
      .list()
      .iter()
      .filter(|u| u.org_id == org)
      .map(user_view)
      .collect::<Vec<_>>()
  ))
}

/// The response for a change to a user that did not happen.
///
/// The three causes answer differently, and the middle one used to be found by
/// comparing the error's *text* against "unknown user id". A rename of that
/// string would have silently turned a 404 back into a 400.
pub(crate) fn user_error(e: UserError) -> axum::response::Response {
  match e {
    UserError::NotSaved => crate::api::tokens::not_persisted(),
    UserError::NoSuchUser => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    UserError::Invalid(m) => (StatusCode::BAD_REQUEST, m).into_response(),
  }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct UserCreateRequest {
  pub(crate) username: String,
  /// At least 8 characters.
  pub(crate) password: String,
  /// One of `viewer`, `operator`, `admin`: the role in the organization the
  /// caller is acting in. The one-grant spelling; `grants` is the general one
  /// and wins when both are sent.
  #[serde(default)]
  pub(crate) role: Option<String>,
  /// The organizations this user reaches and the role in each. A user
  /// reaching several organizations can only be created from master.
  #[serde(default)]
  pub(crate) grants: Option<Vec<GrantRequest>>,
}

#[utoipa::path(post, path = "/aperio/api/users", tag = "users",
  description = "Creates a dashboard user with a role, or a list of per-organization grants (admin only).",
  request_body = UserCreateRequest,
  responses((status = 200, description = "Created user", body = serde_json::Value), (status = 400, description = "Invalid username/password/role/grants"), (status = 403, description = "A grant the caller cannot give"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn users_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<UserCreateRequest>,
) -> Response {
  let Some(caller) = crate::auth::resolve_caller(&state, &headers).await else {
    return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
  };
  // New users belong to the caller's currently effective organization.
  let org = caller.effective_org();
  let grants = match &payload.grants {
    Some(raw) => match parse_grants(&state, org.as_deref(), raw).await {
      Ok(g) => g,
      Err(resp) => return resp,
    },
    None => {
      let Some(role) = payload.role.as_deref().and_then(Role::parse) else {
        return (
          StatusCode::BAD_REQUEST,
          "role must be viewer, operator, or admin",
        )
          .into_response();
      };
      vec![Grant::new(GrantOrg::from_org_id(org.as_deref()), role)]
    }
  };
  if let Err(resp) = check_granter(&caller, &grants) {
    return resp;
  }
  // Enforce the org user quota atomically with the create: hold the users lock
  // across the count and the insert so concurrent creates can't overshoot the
  // cap (the cap comes from the org store, fetched first).
  let quota_max = state
    .org_quota(org.as_deref())
    .await
    .and_then(|q| q.max_users);
  let created = {
    let mut users = state.users.lock().await;
    if let Some(max) = quota_max {
      let count = users
        .list()
        .iter()
        .filter(|u| u.org_id.as_deref() == org.as_deref())
        .count() as u64;
      if count >= max {
        return (
          StatusCode::FORBIDDEN,
          format!("organization user quota reached ({max})"),
        )
          .into_response();
      }
    }
    users.create_with_grants(&payload.username, &payload.password, org, grants)
  };
  match created {
    Ok(user) => {
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit_session(
          "user_created",
          &headers,
          &ip,
          &format!(
            "username={} role={} grants={}",
            user.username,
            user.role.as_str(),
            grants_label(&user.grants)
          ),
        )
        .await;
      audit_grant_changes(&state, &headers, &ip, &user.username, &user.grants, &[]).await;
      state
        .emit_event_in(
          "user_created",
          serde_json::json!({"username": user.username, "role": user.role.as_str()}),
          user.org_id.clone(),
        )
        .await;
      Json(user_view(&user)).into_response()
    }
    Err(e) => user_error(e),
  }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct UserUpdateRequest {
  /// New role (`viewer` / `operator` / `admin`) in the organization the
  /// caller is acting in; omit to keep. `grants` wins when both are sent.
  pub(crate) role: Option<String>,
  /// Enable/disable the account; omit to keep.
  pub(crate) enabled: Option<bool>,
  /// New password (at least 8 characters); omit to keep.
  pub(crate) password: Option<String>,
  /// The full replacement grant list; omit to keep. Every grant added or
  /// removed has to be one the caller could give.
  #[serde(default)]
  pub(crate) grants: Option<Vec<GrantRequest>>,
}

#[utoipa::path(put, path = "/aperio/api/users/{id}", tag = "users",
  description = "Updates a user's role or grants, enabled state, or password (admin only).",
  params(("id" = String, Path, description = "User record id")),
  request_body = UserUpdateRequest,
  responses((status = 200, description = "Updated user", body = serde_json::Value), (status = 400, description = "Invalid value"), (status = 403, description = "A grant the caller cannot give or take away"), (status = 404, description = "Unknown user id")))]
pub(crate) async fn users_update_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Json(payload): Json<UserUpdateRequest>,
) -> Response {
  let Some(caller) = crate::auth::resolve_caller(&state, &headers).await else {
    return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
  };
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
  // Isolation: only users in the caller's effective org may be edited.
  if !user_in_effective_org(&state, &headers, &id).await {
    return (StatusCode::NOT_FOUND, "unknown user id").into_response();
  }
  let effective = caller.effective_org();
  let current = match state.users.lock().await.get(&id) {
    Some(u) => u.grants.clone(),
    None => return (StatusCode::NOT_FOUND, "unknown user id").into_response(),
  };
  // The new grant list: the full replacement when one is sent, else the
  // role rewritten for the organization the caller is acting in, else
  // nothing changes.
  let wanted = match (&payload.grants, role) {
    (Some(raw), _) => match parse_grants(&state, effective.as_deref(), raw).await {
      Ok(g) => Some(g),
      Err(resp) => return resp,
    },
    (None, Some(r)) => {
      let here = GrantOrg::from_org_id(effective.as_deref());
      let mut next: Vec<Grant> = current.iter().filter(|g| g.org != here).cloned().collect();
      next.push(Grant::new(here, r));
      Some(next)
    }
    (None, None) => None,
  };
  let (added, removed) = match &wanted {
    Some(next) => grants::diff(&current, next),
    None => (Vec::new(), Vec::new()),
  };
  if let Err(resp) = check_granter(&caller, &added) {
    return resp;
  }
  if let Err(resp) = check_granter(&caller, &removed) {
    return resp;
  }
  let updated = {
    let mut users = state.users.lock().await;
    let mut result = users.update(&id, None, payload.enabled, payload.password.as_deref());
    if result.is_ok()
      && let Some(next) = wanted
      && !(added.is_empty() && removed.is_empty())
    {
      result = users.set_grants(&id, next);
    }
    result
  };
  match updated {
    Ok(user) => {
      // Disabling an account must end its live sessions, exactly as deleting
      // one does: an account is disabled precisely when it is no longer
      // trusted, so leaving a session valid for up to 24 hours defeats it.
      if payload.enabled == Some(false) {
        state
          .sessions
          .lock()
          .await
          .retain(|info| info.username.as_deref() != Some(user.username.as_str()));
      }
      let ip = actor_ip(&state, &headers, addr);
      state
        .audit_session(
          "user_updated",
          &headers,
          &ip,
          &format!(
            "username={} role={} enabled={} password_changed={} grants={}",
            user.username,
            user
              .role_in(effective.as_deref())
              .map(|r| r.as_str())
              .unwrap_or("-"),
            user.enabled,
            payload.password.is_some(),
            grants_label(&user.grants)
          ),
        )
        .await;
      audit_grant_changes(&state, &headers, &ip, &user.username, &added, &removed).await;
      Json(user_view(&user)).into_response()
    }
    Err(e) => user_error(e),
  }
}

#[utoipa::path(delete, path = "/aperio/api/users/{id}", tag = "users",
  description = "Deletes a dashboard user (admin only). Live sessions of that user are dropped.",
  params(("id" = String, Path, description = "User record id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown user id"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn users_delete_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  let username = {
    let users = state.users.lock().await;
    users
      .list()
      .iter()
      .find(|u| u.id == id && u.org_id == org)
      .map(|u| u.username.clone())
  };
  let Some(username) = username else {
    return (StatusCode::NOT_FOUND, "unknown user id").into_response();
  };
  // A deletion that could not be written down did not happen, so the sessions
  // must not be dropped either: the account still exists, and taking its
  // sessions away would leave a user locked out of an account the store still
  // has, with the operator told it was deleted.
  if let Err(e) = state.users.lock().await.delete(&id) {
    return user_error(e);
  }
  // Deleting an account must end its live sessions too.
  state
    .sessions
    .lock()
    .await
    .retain(|info| info.username.as_deref() != Some(username.as_str()));
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit_session(
      "user_deleted",
      &headers,
      &ip,
      &format!("username={}", username),
    )
    .await;
  StatusCode::OK.into_response()
}

/// Resolves the calling session to its user row. The built-in admin
/// ("aperio", from the master token / dashboard password / OIDC) has no user
/// row and cannot enroll TOTP.
#[allow(clippy::result_large_err)] // see api/tokens.rs
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
  responses((status = 200, description = "Pending secret and provisioning URL", body = serde_json::Value), (status = 400, description = "No user row (built-in admin)"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
      Err(e) => return user_error(e),
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
  description = "Completes TOTP enrollment by verifying a code against the pending secret. Returns the single-use recovery codes, shown exactly once.",
  request_body = TotpCodeRequest,
  responses((status = 200, description = "Recovery codes", body = serde_json::Value), (status = 400, description = "Invalid code or no enrollment in progress"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
        .audit_session(
          "totp_enabled",
          &headers,
          &ip,
          &format!("user_id={}", user_id),
        )
        .await;
      Json(serde_json::json!({ "recovery_codes": recovery_codes })).into_response()
    }
    Err(e) => user_error(e),
  }
}

/// Disables TOTP for the signed-in user (requires a valid current code).
#[utoipa::path(delete, path = "/aperio/api/me/totp", tag = "users",
  description = "Disables TOTP for the signed-in user. Requires a currently valid authenticator code (or an unused recovery code).",
  request_body = TotpCodeRequest,
  responses((status = 200, description = "Disabled"), (status = 400, description = "TOTP not enabled"), (status = 401, description = "Invalid code"), (status = 500, description = "The change could not be saved and was rolled back")))]
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
  // Replay-hardened like the login path: a TOTP code already used to sign in
  // can't be replayed here to disable the second factor within its window.
  let ok = match crate::totp::verify_step(&secret, &payload.code, now_secs()) {
    Some(step) => state
      .users
      .lock()
      .await
      .totp_try_advance_step(&user_id, step),
    None => state
      .users
      .lock()
      .await
      .consume_recovery(&user_id, &payload.code),
  };
  if !ok {
    return (StatusCode::UNAUTHORIZED, "Invalid code").into_response();
  }
  if let Err(e) = state.users.lock().await.totp_disable(&user_id) {
    return user_error(e);
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit_session(
      "totp_disabled",
      &headers,
      &ip,
      &format!("user_id={}", user_id),
    )
    .await;
  Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Admin reset: clears TOTP for a locked-out user.
#[utoipa::path(delete, path = "/aperio/api/users/{id}/totp", tag = "users",
  description = "Clears TOTP for a user (admin only), the escape hatch when someone loses their authenticator and recovery codes.",
  params(("id" = String, Path, description = "User id")),
  responses((status = 200, description = "Cleared"), (status = 404, description = "Unknown user"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn totp_admin_reset_handler(
  State(state): State<Arc<AppState>>,
  Path(id): Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  // Isolation: only users in the caller's effective org may be reset (this
  // guard was missing, unlike users update/delete).
  if !user_in_effective_org(&state, &headers, &id).await {
    return (StatusCode::NOT_FOUND, "Unknown user").into_response();
  }
  if let Err(e) = state.users.lock().await.totp_disable(&id) {
    return user_error(e);
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit_session(
      "totp_admin_reset",
      &headers,
      &ip,
      &format!("user_id={}", id),
    )
    .await;
  Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Maps every dashboard username to its organization (`None` = master), for
/// resolving which org a live session belongs to.
async fn username_org_map(
  state: &Arc<AppState>,
) -> std::collections::HashMap<String, Option<String>> {
  state
    .users
    .lock()
    .await
    .list()
    .iter()
    .map(|u| (u.username.clone(), u.org_id.clone()))
    .collect()
}

/// The organization a live session belongs to: a named user's session takes
/// that user's org; the built-in admin and visitor/unknown sessions belong to
/// master (`None`).
fn session_org(
  info: &crate::store::sessions::SessionInfo,
  user_orgs: &std::collections::HashMap<String, Option<String>>,
) -> Option<String> {
  match info.username.as_deref() {
    Some(name) => user_orgs.get(name).cloned().flatten(),
    None => None,
  }
}

/// Lists live sessions (admin): who is signed in from where. Ids are the
/// SHA-256 of the session token, usable for revocation, useless for
/// hijacking.
#[utoipa::path(get, path = "/aperio/api/sessions", tag = "users",
  description = "Live sessions with identity, IP, User-Agent and age; the caller's own session is marked.",
  responses((status = 200, description = "Live sessions", body = serde_json::Value)))]
pub(crate) async fn sessions_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  let own = crate::auth::session_token(&state, &headers);
  let org = crate::auth::effective_org(&state, &headers).await;
  let user_orgs = username_org_map(&state).await;
  let mut entries = state.sessions.lock().await.entries();
  entries.sort_by_key(|(_, info)| std::cmp::Reverse(info.created_at));
  Json(
    entries
      .into_iter()
      // Only sessions belonging to the caller's effective organization.
      .filter(|(_, info)| session_org(info, &user_orgs) == org)
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
  // Isolation: a caller may only end sessions in their effective org.
  let org = crate::auth::effective_org(&state, &headers).await;
  let user_orgs = username_org_map(&state).await;
  let in_org = state
    .sessions
    .lock()
    .await
    .entries()
    .iter()
    .any(|(k, info)| k == &id && session_org(info, &user_orgs) == org);
  if !in_org {
    return (StatusCode::NOT_FOUND, "unknown session id").into_response();
  }
  let removed = state.sessions.lock().await.remove_by_key(&id);
  if !removed {
    return (StatusCode::NOT_FOUND, "unknown session id").into_response();
  }
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit_session(
      "session_revoked",
      &headers,
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
  let own = crate::auth::session_token(&state, &headers);
  let org = crate::auth::effective_org(&state, &headers).await;
  let user_orgs = username_org_map(&state).await;
  let mut sessions = state.sessions.lock().await;
  let before = sessions.entries().len();
  // Keep the caller's own session and every session outside the effective org;
  // "sign out everywhere else" only reaches the caller's own organization.
  let keep_keys: Vec<String> = sessions
    .entries()
    .into_iter()
    .filter(|(k, info)| {
      let is_own = own
        .as_deref()
        .is_some_and(|token| crate::store::sessions::SessionStore::token_matches_key(token, k));
      is_own || session_org(info, &user_orgs) != org
    })
    .map(|(k, _)| k)
    .collect();
  sessions.retain_keys(&keep_keys);
  let ended = before - keep_keys.len();
  drop(sessions);
  let ip = actor_ip(&state, &headers, addr);
  state
    .audit_session("sessions_cleared", &headers, &ip, &format!("ended={ended}"))
    .await;
  Json(serde_json::json!({ "ended": ended })).into_response()
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
