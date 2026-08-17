//! The dashboard session cookie: which name it takes, how it is set and
//! cleared, and what a caller is told about the session they hold.
//!
//! The `__Host-` prefix is the point of the two names: a browser refuses to
//! let a neighbouring host set a cookie carrying it, so on https the session
//! cannot be displaced by a subdomain nobody trusts.

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Name of the session cookie when it can carry the `__Host-` prefix.
///
/// The prefix is a promise the *browser* enforces: such a cookie may only be
/// set by a page on this exact host, over https, with `Path=/` and no
/// `Domain`. That is what this deployment needs, because the server also
/// serves other people's sites: a tenant on `evil.tunnel.example.com` can set
/// a cookie for `.tunnel.example.com`, and without the prefix that cookie is
/// indistinguishable from the dashboard's own, an operator could be walked
/// into a session someone else chose.
pub(crate) const SESSION_COOKIE_SECURE: &str = "__Host-aperio_session";
/// The name used when the prefix cannot be: `__Host-` requires `Secure`, so a
/// plain-http deployment would have the browser reject the cookie outright.
pub(crate) const SESSION_COOKIE_PLAIN: &str = "aperio_session";

/// The `Set-Cookie` header value that issues a session, for every sign-in
/// path there is.
///
/// A function rather than a format string repeated per path, because the
/// repetition already went wrong once: password sign-in asked
/// `session_cookie_name` for the name while OIDC and both passkey paths wrote
/// `aperio_session=` verbatim, so signing in with a passkey handed out an
/// unprefixed cookie on a deployment whose whole neighbour defence is the
/// prefix. Whatever else drifts between these paths, the cookie they set
/// cannot, because there is one place that spells it.
pub(crate) fn session_set_cookie(secure: bool, token: &str) -> String {
  format!(
    "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_cookie_name(secure),
    token,
    if secure { "; Secure" } else { "" }
  )
}

/// The cookie name this configuration issues.
pub(crate) fn session_cookie_name(secure: bool) -> &'static str {
  if secure {
    SESSION_COOKIE_SECURE
  } else {
    SESSION_COOKIE_PLAIN
  }
}

/// Reads the session token out of the Cookie header, if present.
///
/// The prefixed name wins when both are there. That is the whole defence: a
/// cookie set by a neighbouring host can only ever be the unprefixed one, so
/// it cannot displace a session issued with the prefix, it is simply not
/// looked at. The unprefixed name is still accepted on its own, so sessions
/// issued before this (and every plain-http deployment) keep working.
pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<&str> {
  let cookie_str = headers.get("cookie")?.to_str().ok()?;
  let read = |want: &str| {
    cookie_str.split(';').find_map(|part| {
      let (k, v) = part.trim().split_once('=')?;
      (k == want).then_some(v)
    })
  };
  read(SESSION_COOKIE_SECURE).or_else(|| read(SESSION_COOKIE_PLAIN))
}

/// Logs out the current dashboard session: drops it from the session store and
/// expires the cookie. Always answers 200 so a stale cookie still clears.
#[utoipa::path(post, path = "/aperio/auth/logout", tag = "auth",
  description = "Drops the server-side session and expires the session cookie.",
  responses((status = 200, description = "Logged out")))]
pub(crate) async fn auth_logout_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  if let Some(token) = session_cookie(&headers) {
    state.sessions.lock().await.remove(token);
  }
  let secure_flag = if state.config().secure_cookies {
    "; Secure"
  } else {
    ""
  };
  // Both names, because logging out must clear whatever this browser is
  // carrying, including a cookie issued before the prefix existed.
  let expire =
    |name: &str| format!("{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure_flag}");
  Response::builder()
    .status(StatusCode::OK)
    .header("Set-Cookie", expire(SESSION_COOKIE_SECURE))
    .header("Set-Cookie", expire(SESSION_COOKIE_PLAIN))
    .body(Body::empty())
    .unwrap()
}

/// Session status for the dashboard header ("session expires in …"). Registered
/// behind the session middleware, so reaching it already implies a live session.
#[derive(serde::Serialize)]
pub(crate) struct SessionStatus {
  /// Seconds until the current session cookie expires.
  pub(crate) expires_in_seconds: u64,
  /// Username of the session's dashboard user ("aperio" for the built-in
  /// master/dashboard credentials and OIDC logins).
  pub(crate) username: String,
  /// Role the middleware enforces for this session.
  pub(crate) role: &'static str,
  /// True when the session's user has TOTP two-factor auth enabled (always
  /// false for the built-in admin, which has no user row).
  pub(crate) totp: bool,
  /// True for the built-in `aperio` super-admin, who may switch between
  /// organizations. Named users are pinned to their own org.
  pub(crate) master_admin: bool,
  /// The organization the session is currently viewing: `master` for the
  /// implicit master org, or a child org id.
  pub(crate) selected_org: String,
}

#[utoipa::path(get, path = "/aperio/api/session", tag = "auth",
  description = "Remaining lifetime of the presented dashboard session.",
  responses((status = 200, description = "Session info", body = serde_json::Value), (status = 401, description = "No valid session")))]
pub(crate) async fn auth_session_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let (remaining, username, role) = match session_cookie(&headers) {
    Some(token) => {
      let sessions = state.sessions.lock().await;
      match sessions.get(token) {
        Some(info) => (
          info
            .expires_at
            .saturating_sub(crate::store::sessions::now_secs()),
          info
            .username
            .clone()
            .unwrap_or_else(|| "aperio".to_string()),
          info.role.as_str(),
        ),
        None => (0, "aperio".to_string(), Role::Admin.as_str()),
      }
    }
    None => (0, "aperio".to_string(), Role::Admin.as_str()),
  };
  let totp = {
    let users = state.users.lock().await;
    users
      .find_by_username(&username)
      .is_some_and(|u| u.totp_secret.is_some())
  };
  let master_admin = is_master_admin(&state, &headers).await;
  let selected_org = effective_org(&state, &headers)
    .await
    .unwrap_or_else(|| crate::store::orgs::MASTER_ID.to_string());
  Json(SessionStatus {
    expires_in_seconds: remaining,
    username,
    role,
    totp,
    master_admin,
    selected_org,
  })
  .into_response()
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
