//! What an authenticated caller *is*, and what that lets them act on: the
//! scope their session carries, the organization it resolves to, the dashboard
//! role behind it, and the master-admin gate.

use axum::http::HeaderMap;

use super::*;
use crate::state::AppState;

/// Resolves the scope of the active `aperio_session` cookie:
/// - `Some(None)`, a valid global session (dashboard + all proxied hosts).
/// - `Some(Some(host))`, a valid session scoped to `host` only.
/// - `None`, no valid session.
async fn session_scope(state: &AppState, headers: &HeaderMap) -> Option<Option<String>> {
  // Expired sessions are swept by the background gc beat (`gc_tick_once`);
  // the lookup below still refuses an expired entry on its own, so a session
  // never outlives its expiry between beats, it only occupies memory.
  let token = session_cookie(headers, state.config().secure_cookies)?;
  // Reject cookie values that are not valid UUIDs (session tokens are always
  // generated with uuid::Uuid::new_v4). This avoids unnecessary HashMap lookups
  // and prevents injection of malformed keys.
  if uuid::Uuid::parse_str(token).is_err() {
    return None;
  }
  let (scope, username) = {
    let mut sessions = state.sessions.lock().await;
    match sessions.get(token) {
      Some(info) if info.expires_at > crate::store::sessions::now_secs() => {
        (info.scope_host.clone(), info.username.clone())
      }
      Some(_) => {
        sessions.remove(token);
        return None;
      }
      None => return None,
    }
  };
  if !named_user_active(state, username.as_deref()).await {
    return None;
  }
  Some(scope)
}

/// True unless the session belongs to a dashboard user that is now disabled.
///
/// Disabling an account has to strip its live sessions of all authority.
/// `caller_org` resolves a named session through `find_by_username`, which
/// skips disabled rows and so reports "no organization", which
/// `is_master_admin` reads as the master org. Without this check a disabled
/// sub-org admin would be *promoted* to master super-admin on their existing
/// session.
///
/// A username with no user row is an OIDC identity, not a disabled account, so
/// it stays valid. Never call this while holding the `sessions` lock: it takes
/// `users`.
async fn named_user_active(state: &AppState, username: Option<&str>) -> bool {
  match username {
    None => true,
    Some(name) => !state.users.lock().await.is_disabled_username(name),
  }
}

/// Validates the `aperio_session` cookie for full (global) access, the
/// dashboard, tunnel provisioning, and any proxied host. A host-scoped session
/// (a client-set visitor password login) does NOT satisfy this.
pub(crate) async fn validate_session(state: &AppState, headers: &HeaderMap) -> bool {
  matches!(session_scope(state, headers).await, Some(None))
}

/// Does this session get a visitor past the gate on `host`?
///
/// The dashboard and the visitor gate share one session store, and the gate
/// used to ask only [`validate_session`], "is this a global session". That is
/// the right question for the dashboard and the wrong one here: a session
/// fixed to an organization, a per-organization SSO login or a named user of
/// a child org, would walk past the gate on **every** hostname on the server,
/// including hostnames served for other tenants and for master. A read-only
/// Viewer of one organization could browse another's gated site.
///
/// So the organization is asked as well, with the same question the
/// maintenance flag and share links already ask: not merely "does the fence
/// cover this name" (the master token is never fenced, so a master client can
/// be serving a name inside an organization's fence) but "is this the
/// organization's to reach". Master sessions are unfenced, which is what they
/// are everywhere else, so an operator's own dashboard login behaves exactly
/// as it did.
///
/// This is the first half of separating the two planes
/// (`planned_features.md` #106); the second is that they stop sharing a
/// cookie at all.
pub(crate) async fn validate_session_for_visitor(
  state: &AppState,
  headers: &HeaderMap,
  host: Option<&str>,
) -> bool {
  if !validate_session(state, headers).await {
    return false;
  }
  // A global session that resolves to no admin identity is a visitor one,
  // made by the server's own visitor password: it belongs to no organization
  // and that gate is server-wide, so it is admitted everywhere, as it always
  // was. The plane check above `dashboard_role` keeps it off the dashboard.
  let Some(caller) = resolve_caller(state, headers).await else {
    return true;
  };
  // Any granted organization will do, not only the selected one, and Viewer
  // is enough: the question is whether the site is one of theirs.
  let Some(host) = host else {
    // No `Host` to fence against. A fenced organization has no claim on a
    // request that names nothing, and admitting it would be the whole hole
    // wearing a missing header as a disguise. Master stays unfenced.
    return caller.role_in(None).is_some();
  };
  for grant in caller.grants() {
    let admitted = match &grant.org {
      // Master, and `*`: unfenced here as everywhere else.
      crate::store::grants::GrantOrg::Master | crate::store::grants::GrantOrg::All => true,
      crate::store::grants::GrantOrg::Child(id) => {
        state.org_may_act_on_hostname(Some(id), host).await
      }
    };
    if admitted {
      return true;
    }
  }
  false
}

/// True when the caller holds Admin in the master organization, directly or
/// through `*`: the built-in admin, or a named user granted it. Only they may
/// manage organizations and reach the server-global surfaces.
pub(crate) async fn is_master_admin(state: &AppState, headers: &HeaderMap) -> bool {
  resolve_caller(state, headers)
    .await
    .is_some_and(|c| c.is_master_admin())
}

/// The organization whose resources the caller may see and act on: the one
/// selected on their session when a grant reaches it (`POST /api/orgs/select`),
/// else master when granted, else the first organization granted. A named
/// user with one grant is simply in that organization.
pub(crate) async fn effective_org(state: &AppState, headers: &HeaderMap) -> Option<String> {
  resolve_caller(state, headers)
    .await
    .and_then(|c| c.effective_org())
}

/// The raw session cookie value, for endpoints that mutate or exempt the
/// caller's own session (switching organizations, signing out everywhere
/// else). Read under the name this deployment issues, like every other
/// lookup, so on https it is the `__Host-` cookie and nothing a neighbour set.
pub(crate) fn session_token(state: &AppState, headers: &HeaderMap) -> Option<String> {
  session_cookie(headers, state.config().secure_cookies).map(str::to_string)
}

/// Gate for organization-management endpoints: 401 without a session, 403 for
/// non-master-admins.
#[allow(clippy::result_large_err)] // see api/tokens.rs
pub(crate) async fn require_master_admin(
  state: &AppState,
  headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
  use axum::response::IntoResponse;
  if dashboard_role(state, headers).await.is_none() {
    return Err(
      (
        axum::http::StatusCode::UNAUTHORIZED,
        "Authentication required",
      )
        .into_response(),
    );
  }
  if !is_master_admin(state, headers).await {
    return Err(
      (
        axum::http::StatusCode::FORBIDDEN,
        "Only a master-organization admin may manage organizations",
      )
        .into_response(),
    );
  }
  Ok(())
}

/// Role of the presented caller **in the organization they are acting in**:
/// a global dashboard session cookie, or a programmatic admin API key
/// (Bearer). None when neither is valid, and none for a named user whose
/// grants reach nothing.
///
/// Read from the user's grants on every request, not from the session: the
/// session records who signed in, the store records what they may do now, and
/// a grant taken away has to be gone at the next request, the way a disabled
/// account's session grants nothing.
pub(crate) async fn dashboard_role(state: &AppState, headers: &HeaderMap) -> Option<Role> {
  let caller = resolve_caller(state, headers).await?;
  let org = caller.effective_org();
  caller.role_in(org.as_deref())
}

/// Username of the presented global dashboard session; None for a missing/
/// host-scoped session or the built-in admin (which has no user row).
pub(crate) async fn dashboard_username(state: &AppState, headers: &HeaderMap) -> Option<String> {
  let token = session_cookie(headers, state.config().secure_cookies)?;
  let username = {
    let sessions = state.sessions.lock().await;
    let info = sessions.get(token)?;
    if info.expires_at <= crate::store::sessions::now_secs()
      || info.scope_host.is_some()
      || info.plane != crate::store::sessions::Plane::Admin
    {
      return None;
    }
    info.username.clone()?
  };
  named_user_active(state, Some(&username))
    .await
    .then_some(username)
}

/// The username behind a session cookie, whichever plane it was created on
/// and whether or not it is host-scoped.
///
/// Broader than [`dashboard_username`] on purpose: this answers "who is this
/// visitor" for the identity a backend may be told (`planned_features.md`
/// #109), and a host-scoped session is exactly the kind that has a visitor
/// behind it. It is never an authorization answer, only a name.
pub(crate) async fn session_username_any_scope(
  state: &AppState,
  headers: &HeaderMap,
) -> Option<String> {
  let token = session_cookie(headers, state.config().secure_cookies)?;
  let sessions = state.sessions.lock().await;
  let info = sessions.get(token)?;
  if info.expires_at <= crate::store::sessions::now_secs() {
    return None;
  }
  info.username.clone()
}

/// Validates the `aperio_session` cookie for a proxied request to `host`,
/// where the gate in front of it is a **client's own**.
///
/// A session scoped to exactly this hostname is what the per-service login
/// creates, and it is already as narrow as any fence could make it: it was
/// issued by the gate on this very name and reaches nothing else.
///
/// A *global* session is the other kind, and it reaches every hostname on the
/// server, which is the same reach [`validate_session_for_visitor`] exists to
/// fence. So it is asked the same question here. This function used to answer
/// `true` for one unconditionally, which left the cross-tenant hole open on
/// exactly the routes a client gates for itself: the fix that closed it for
/// the server's own gate did not reach the two branches that come first.
pub(crate) async fn validate_session_for_host(
  state: &AppState,
  headers: &HeaderMap,
  host: Option<&str>,
) -> bool {
  match session_scope(state, headers).await {
    Some(None) => validate_session_for_visitor(state, headers, host).await,
    Some(Some(scope)) => host.is_some_and(|h| h == scope),
    None => false,
  }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
