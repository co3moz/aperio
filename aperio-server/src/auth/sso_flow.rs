//! A visitor's `sso` sign-in: the authorization-code flow against the identity
//! provider a gate names, ending in a session for the hostname alone.
//!
//! It is the dashboard's OIDC flow with the one difference that is the reason
//! it exists: what comes back is a **visitor**. The dashboard login settles a
//! user record and hands out a role, so letting somebody see a site used to
//! mean giving them one on Aperio as well; this mints a session scoped to the
//! hostname the visitor signed in on, on the visitor plane, and records which
//! provider vouched for them. It opens the site's `sso` gate and nothing else.
//!
//! Both legs run on the visitor's own hostname, `/aperio/sso/login` and
//! `/aperio/sso/callback`, so the cookie lands where it is used. The provider
//! has to list that callback for every hostname the gate covers.

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::routing::extract_client_ip;
use crate::state::{AppState, SessionInfo, SsoPending};
use crate::visitor_auth::SsoConfig;

/// The `sso` method in force for a route: a site's `access` covering it, else
/// the server's own. Clients cannot declare one.
pub(crate) fn sso_for(state: &AppState, host: &str, path: &str) -> Option<SsoConfig> {
  let cfg = state.config();
  match cfg.sites.access_for(Some(host), path) {
    Some(crate::sites::SiteAccess::Gate(policy)) => policy.sso_methods().next().cloned(),
    Some(crate::sites::SiteAccess::Public) => None,
    None => cfg.visitor_auth.sso_methods().next().cloned(),
  }
}

/// The provider behind a method, discovered once and kept.
async fn runtime_for(
  state: &AppState,
  sso: &SsoConfig,
) -> Result<crate::oidc::OidcRuntime, String> {
  if let Some(rt) = state.sso_runtimes.lock().await.get(&sso.key()) {
    return Ok(rt.clone());
  }
  let rt = crate::oidc::build_runtime(
    &state.config().outbound_policy,
    &sso.issuer,
    &sso.client_id,
    &sso.client_secret,
    sso.allowed_emails.clone(),
    sso.scopes.clone(),
    None,
    crate::oidc::OidcGrantPolicy::default(),
  )
  .await?;
  state
    .sso_runtimes
    .lock()
    .await
    .insert(sso.key(), rt.clone());
  Ok(rt)
}

/// The hostname a visitor is signing in on: the request's own.
fn host_of(headers: &HeaderMap) -> Option<String> {
  crate::routing::extract_request_host(headers)
}

/// Where the provider sends the visitor back to: this hostname's callback.
fn callback_uri(state: &AppState, headers: &HeaderMap, host: &str, over_tls: bool) -> String {
  let proto = crate::tls::scheme(state, headers, over_tls);
  format!("{proto}://{host}/aperio/sso/callback")
}

/// `GET /aperio/sso/login?redirect=<path>`: starts a visitor's sign-in at the
/// identity provider the route's gate names.
pub(crate) async fn sso_login_handler(
  State(state): State<Arc<AppState>>,
  Query(query): Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  tls: Option<axum::Extension<crate::tls::ServedOverTls>>,
  headers: HeaderMap,
) -> Response {
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  // Priced as a credential attempt: this begins a login.
  if !state
    .check_rate_limit_cost(caller_ip, crate::state::RateCost::Guessable)
    .await
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let Some(host) = host_of(&headers) else {
    return (StatusCode::BAD_REQUEST, "Missing Host header").into_response();
  };
  let redirect_after = query
    .get("redirect")
    .map(|r| crate::auth::safe_redirect_path(r).to_string())
    .unwrap_or_else(|| "/".to_string());
  let path = redirect_after.split('?').next().unwrap_or("/");
  let Some(sso) = sso_for(&state, &host, path) else {
    return (
      StatusCode::NOT_FOUND,
      "No single sign-on is configured for this route",
    )
      .into_response();
  };
  let rt = match runtime_for(&state, &sso).await {
    Ok(rt) => rt,
    Err(e) => {
      tracing::error!("Single sign-on at {} is unavailable: {}", sso.issuer, e);
      return (
        StatusCode::BAD_GATEWAY,
        "The identity provider could not be reached",
      )
        .into_response();
    }
  };
  let redirect_uri = callback_uri(&state, &headers, &host, tls.is_some());
  let token = uuid::Uuid::new_v4().to_string();
  {
    let mut pending = state.sso_states.lock().await;
    let now = Instant::now();
    pending.retain(|_, p| p.expires > now);
    pending.insert(
      token.clone(),
      SsoPending {
        redirect_after,
        host,
        redirect_uri: redirect_uri.clone(),
        key: sso.key(),
        issuer: sso.issuer.clone(),
        expires: now + Duration::from_secs(600),
      },
    );
  }
  match url::Url::parse_with_params(
    &rt.authorization_endpoint,
    &[
      ("response_type", "code"),
      ("client_id", rt.client_id.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("scope", rt.scopes.as_str()),
      ("state", token.as_str()),
    ],
  ) {
    Ok(u) => Response::builder()
      .status(StatusCode::FOUND)
      .header("Location", u.to_string())
      .body(axum::body::Body::empty())
      .unwrap_or_default(),
    Err(e) => {
      tracing::error!("Failed to build the single sign-on URL: {}", e);
      (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Single sign-on configuration error",
      )
        .into_response()
    }
  }
}

/// `GET /aperio/sso/callback`: redeems the code, checks the email against the
/// gate's allowlist, and lets the visitor in to this hostname.
pub(crate) async fn sso_callback_handler(
  State(state): State<Arc<AppState>>,
  Query(query): Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  if !state
    .check_rate_limit_cost(caller_ip, crate::state::RateCost::Guessable)
    .await
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let (Some(code), Some(token)) = (query.get("code"), query.get("state")) else {
    return (StatusCode::BAD_REQUEST, "Missing code/state parameter").into_response();
  };
  let pending = match state.sso_states.lock().await.remove(token) {
    Some(p) if p.expires > Instant::now() => p,
    _ => return (StatusCode::BAD_REQUEST, "Invalid or expired sign-in").into_response(),
  };
  // The sign-in finishes on the hostname it started on, or not at all: a
  // session minted here is for this hostname, and one minted for another
  // would be a cookie a visitor never asked for.
  if host_of(&headers).as_deref() != Some(pending.host.as_str()) {
    return (StatusCode::BAD_REQUEST, "Invalid or expired sign-in").into_response();
  }
  // The method is read again rather than carried: a gate changed or removed
  // while the visitor was at the provider decides now, not as it was.
  let path = pending.redirect_after.split('?').next().unwrap_or("/");
  let Some(sso) = sso_for(&state, &pending.host, path).filter(|s| s.key() == pending.key) else {
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - This route no longer asks for this sign-in",
    )
      .into_response();
  };
  let rt = match runtime_for(&state, &sso).await {
    Ok(rt) => rt,
    Err(_) => {
      return (
        StatusCode::BAD_GATEWAY,
        "The identity provider could not be reached",
      )
        .into_response();
    }
  };
  let (info, _) = match crate::auth::redeem_code(&state, &rt, code, &pending.redirect_uri).await {
    Ok(found) => found,
    Err(resp) => return resp,
  };
  let email = info
    .get("email")
    .and_then(|v| v.as_str())
    .unwrap_or_default()
    .to_string();
  // The allowlist is keyed on email, so an address the provider itself says
  // is unverified is refused: at a provider where anyone may claim any
  // address, it is the visitor's word and not the provider's.
  if email.is_empty() || crate::oidc::email_verified_claim(&info) == Some(false) {
    state
      .audit(
        "sso_login_denied",
        &email,
        &caller_ip.to_string(),
        "email not verified",
      )
      .await;
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - Your email address is not verified",
    )
      .into_response();
  }
  if !sso.admits_email(&email) {
    state
      .audit(
        "sso_login_denied",
        &email,
        &caller_ip.to_string(),
        &format!("host={} not in allowlist", pending.host),
      )
      .await;
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - Your account is not allowed to see this site",
    )
      .into_response();
  }
  state
    .audit(
      "sso_login_success",
      &email,
      &caller_ip.to_string(),
      &format!("host={} issuer={}", pending.host, sso.issuer),
    )
    .await;
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &session_token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 86400,
      created_at: crate::store::sessions::now_secs(),
      ip: Some(caller_ip.to_string()),
      user_agent: crate::store::sessions::session_user_agent(&headers),
      plane: crate::store::sessions::Plane::Visitor,
      scope_host: Some(pending.host.clone()),
      username: Some(email),
      role: crate::store::users::Role::Viewer,
      selected_org: None,
      bound_org: None,
      login_host: Some(pending.host.clone()),
      sso_issuer: Some(pending.issuer.clone()),
    },
  );
  let cookie = crate::auth::session_set_cookie(state.config().secure_cookies, &session_token);
  Response::builder()
    .status(StatusCode::FOUND)
    .header("Set-Cookie", cookie)
    .header("Location", pending.redirect_after)
    .body(axum::body::Body::empty())
    .unwrap_or_default()
}

#[cfg(test)]
#[path = "sso_flow_tests.rs"]
mod tests;
