use axum::{
  Json,
  body::Body,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::oidc;

use crate::api::serve_embedded;
use crate::routing::extract_client_ip;
use crate::state::{AppState, ClientPerms, SessionInfo};

/// Serves the login page from the embedded dashboard build.
pub(crate) async fn auth_page_handler() -> Response {
  serve_embedded("auth.html", false)
}

/// Handles login form submission. Validates credentials and sets a session
/// cookie. Validation is host-aware: server/dashboard/master credentials create
/// a full (global) session, while a client-set per-service visitor password
/// creates a session scoped to that host only (it never unlocks the dashboard
/// or other hosts). A client override replaces the server's own visitor
/// password for that route — the server password no longer works there (master
/// and dashboard credentials always do).
pub(crate) async fn auth_login_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  headers: HeaderMap,
) -> Result<Response, StatusCode> {
  let cfg = state.config();
  // Rate limit login attempts per IP to mitigate brute-force attacks.
  let client_ip = extract_client_ip(
    &headers,
    addr.ip(),
    cfg.trust_proxy,
    cfg.real_ip_header.as_deref(),
  );
  if !state.check_rate_limit(client_ip).await {
    return Err(StatusCode::TOO_MANY_REQUESTS);
  }

  // Host the visitor is authenticating for (a proxied site or the dashboard).
  let host = headers
    .get("host")
    .and_then(|v| v.to_str().ok())
    .map(|h| h.split(':').next().unwrap_or(h).trim().to_ascii_lowercase())
    .filter(|h| !h.is_empty());

  // The route the visitor was heading to selects which service's client-set
  // credentials apply. A dashboard login (redirect under /aperio) never uses a
  // client override — the dashboard is always gated by server-level creds.
  let redirect_path = query
    .get("redirect")
    .map(|r| safe_redirect_path(r).to_string())
    .unwrap_or_else(|| "/".to_string());
  let custom_creds = if redirect_path.starts_with("/aperio") {
    None
  } else {
    crate::routing::route_visitor_auth(&state, &redirect_path, host.as_deref()).await
  };

  // The scope of the session to create, based on which credential matched:
  //   Some(None)       -> global (server / dashboard / master credentials)
  //   Some(Some(host)) -> scoped to this host (client-set visitor credentials)
  //   None             -> authentication failed
  let mut scope: Option<Option<String>> = None;
  if let Some(auth_header) = headers.get("authorization")
    && let Ok(auth_str) = auth_header.to_str()
    && let Some(stripped) = auth_str.strip_prefix("Basic ")
  {
    use base64::prelude::*;
    if let Ok(decoded) = BASE64_STANDARD.decode(stripped)
      && let Ok(decoded_str) = String::from_utf8(decoded)
    {
      // Master token (aperio:<token>) always grants full access.
      if constant_time_eq_str(&decoded_str, &format!("aperio:{}", cfg.token)) {
        scope = Some(None);
      }
      // Dashboard password (aperio:<pass>) grants full access.
      if scope.is_none()
        && let Ok(dash_pass) = std::env::var("APERIO_DASHBOARD_AUTH")
        && !dash_pass.trim().is_empty()
        && constant_time_eq_str(&decoded_str, &format!("aperio:{}", dash_pass))
      {
        scope = Some(None);
      }
      // Client-set visitor credentials for this route -> host-scoped session.
      if scope.is_none()
        && let Some(ref creds) = custom_creds
        && let Some(ref h) = host
        && constant_time_eq_str(&decoded_str, creds)
      {
        scope = Some(Some(h.clone()));
      }
      // Server visitor password -> full access, but only when the route is not
      // under a client override (an override supersedes the server's own creds
      // for that route).
      if scope.is_none()
        && custom_creds.is_none()
        && let Some(ref creds) = cfg.auth_credentials
        && constant_time_eq_str(&decoded_str, creds)
      {
        scope = Some(None);
      }
    }
  }

  let Some(session_scope) = scope else {
    state
      .audit(
        "login_failed",
        &client_ip.to_string(),
        "invalid credentials",
      )
      .await;
    return Err(StatusCode::UNAUTHORIZED);
  };
  let scope_desc = match &session_scope {
    Some(h) => format!("session created (scope={})", h),
    None => "session created (global)".to_string(),
  };
  state
    .audit("login_success", &client_ip.to_string(), &scope_desc)
    .await;

  // Create session
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    session_token.clone(),
    SessionInfo {
      expires_at: Instant::now() + Duration::from_secs(86400),
      scope_host: session_scope,
    },
  );

  let secure_flag = if cfg.secure_cookies { "; Secure" } else { "" };
  let cookie = format!(
    "aperio_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_token, secure_flag
  );

  Ok(
    Response::builder()
      .status(StatusCode::OK)
      .header("Set-Cookie", cookie)
      .body(Body::empty())
      .unwrap(),
  )
}

/// Reads the `aperio_session` value out of the Cookie header, if present.
fn session_cookie(headers: &HeaderMap) -> Option<&str> {
  let cookie_str = headers.get("cookie")?.to_str().ok()?;
  cookie_str.split(';').find_map(|part| {
    let (k, v) = part.trim().split_once('=')?;
    (k == "aperio_session").then_some(v)
  })
}

/// Logs out the current dashboard session: drops it from the session store and
/// expires the cookie. Always answers 200 so a stale cookie still clears.
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
  let cookie = format!(
    "aperio_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
    secure_flag
  );
  Response::builder()
    .status(StatusCode::OK)
    .header("Set-Cookie", cookie)
    .body(Body::empty())
    .unwrap()
}

/// Session status for the dashboard header ("session expires in …"). Registered
/// behind the session middleware, so reaching it already implies a live session.
#[derive(serde::Serialize)]
pub(crate) struct SessionStatus {
  /// Seconds until the current session cookie expires.
  pub(crate) expires_in_seconds: u64,
}

pub(crate) async fn auth_session_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let remaining = match session_cookie(&headers) {
    Some(token) => {
      let sessions = state.sessions.lock().await;
      match sessions.get(token) {
        Some(info) => info
          .expires_at
          .saturating_duration_since(Instant::now())
          .as_secs(),
        None => 0,
      }
    }
    None => 0,
  };
  Json(SessionStatus {
    expires_in_seconds: remaining,
  })
  .into_response()
}

/// Extracts a Bearer token or `x-auth-token` value from request headers.
pub(crate) fn extract_token(headers: &HeaderMap) -> Option<String> {
  if let Some(auth_header) = headers.get("authorization")
    && let Ok(auth_str) = auth_header.to_str()
    && let Some(stripped) = auth_str.strip_prefix("Bearer ")
  {
    return Some(stripped.to_string());
  }
  if let Some(x_token) = headers.get("x-auth-token")
    && let Ok(x_token_str) = x_token.to_str()
  {
    return Some(x_token_str.to_string());
  }
  None
}

/// Helper function to extract Bearer token or `x-auth-token` from header values
/// and verify if it matches the configured server security token.
#[cfg(test)]
pub(crate) fn extract_and_verify_token(headers: &HeaderMap, server_token: &str) -> bool {
  match extract_token(headers) {
    Some(tok) => constant_time_eq_str(&tok, server_token),
    None => false,
  }
}

/// Resolves the permissions for a presented tunnel token: the master token
/// grants unrestricted access; otherwise the dynamic token store is consulted
/// (rejecting unknown and expired tokens).
pub(crate) async fn authorize_tunnel_token(
  state: &AppState,
  headers: &HeaderMap,
  client_ip: IpAddr,
) -> Option<ClientPerms> {
  let presented = extract_token(headers)?;
  if constant_time_eq_str(&presented, &state.config().token) {
    return Some(ClientPerms::master());
  }
  let store = state.token_store.lock().await;
  let token = store.verify(&presented)?;
  // Dynamic tokens can be restricted to source IPs/CIDRs.
  if !ip_allowed(client_ip, &token.allowed_ips) {
    warn!(
      "Token '{}' rejected: source IP {} not in allowed list {:?}",
      token.name, client_ip, token.allowed_ips
    );
    return None;
  }
  Some(ClientPerms {
    master: false,
    hostnames: token.hostnames.clone(),
    paths: token.paths.clone(),
    token_name: Some(token.name.clone()),
    token_id: Some(token.id.clone()),
    allow_public: token.allow_public,
  })
}

/// Checks whether `ip` matches an allowlist of plain IPs and CIDR ranges.
/// An empty list, `*`, `0.0.0.0/0` or `::/0` allow any address.
pub(crate) fn ip_allowed(ip: IpAddr, allowed: &[String]) -> bool {
  if allowed.is_empty() {
    return true;
  }
  allowed.iter().any(|entry| {
    let entry = entry.trim();
    if entry == "*" || entry == "0.0.0.0/0" || entry == "::/0" || entry == "0.0.0.0" {
      return true;
    }
    match entry.split_once('/') {
      Some((base, prefix)) => {
        let (Ok(base_ip), Ok(bits)) = (base.parse::<IpAddr>(), prefix.parse::<u32>()) else {
          return false;
        };
        cidr_contains(base_ip, bits, ip)
      }
      None => entry
        .parse::<IpAddr>()
        .is_ok_and(|allowed_ip| allowed_ip == ip),
    }
  })
}

/// True when `ip` falls inside the CIDR `base/bits` (families must match).
fn cidr_contains(base: IpAddr, bits: u32, ip: IpAddr) -> bool {
  match (base, ip) {
    (IpAddr::V4(b), IpAddr::V4(i)) => {
      if bits > 32 {
        return false;
      }
      if bits == 0 {
        return true;
      }
      let mask = u32::MAX << (32 - bits);
      (u32::from(b) & mask) == (u32::from(i) & mask)
    }
    (IpAddr::V6(b), IpAddr::V6(i)) => {
      if bits > 128 {
        return false;
      }
      if bits == 0 {
        return true;
      }
      let mask = u128::MAX << (128 - bits);
      (u128::from(b) & mask) == (u128::from(i) & mask)
    }
    _ => false,
  }
}

/// Validates an allowlist entry (plain IP or CIDR, or a wildcard form).
pub(crate) fn valid_ip_entry(entry: &str) -> bool {
  let entry = entry.trim();
  if entry == "*" {
    return true;
  }
  match entry.split_once('/') {
    Some((base, prefix)) => {
      let Ok(base_ip) = base.parse::<IpAddr>() else {
        return false;
      };
      match prefix.parse::<u32>() {
        Ok(bits) => match base_ip {
          IpAddr::V4(_) => bits <= 32,
          IpAddr::V6(_) => bits <= 128,
        },
        Err(_) => false,
      }
    }
    None => entry.parse::<IpAddr>().is_ok(),
  }
}

/// Constant-time string comparison to mitigate timing attacks on secrets.
/// Hashes both inputs with SHA-256 first so that length differences do not
/// leak through the comparison timing, then compares the digests using
/// `subtle::ConstantTimeEq`.
pub(crate) fn constant_time_eq_str(a: &str, b: &str) -> bool {
  use subtle::ConstantTimeEq;
  let mut ha = Sha256::default();
  ha.update(a.as_bytes());
  let mut hb = Sha256::default();
  hb.update(b.as_bytes());
  let da = ha.finalize();
  let db = hb.finalize();
  da.ct_eq(&db).into()
}

/// Resolves the scope of the active `aperio_session` cookie:
/// - `Some(None)` — a valid global session (dashboard + all proxied hosts).
/// - `Some(Some(host))` — a valid session scoped to `host` only.
/// - `None` — no valid session.
async fn session_scope(state: &AppState, headers: &HeaderMap) -> Option<Option<String>> {
  // Lazy garbage collection of expired sessions (runs at most once per 5 minutes).
  {
    let mut last_gc = state.last_session_gc.lock().await;
    if last_gc.elapsed() > Duration::from_secs(300) {
      let mut sessions = state.sessions.lock().await;
      let now = Instant::now();
      sessions.retain(|_, info| info.expires_at > now);
      *last_gc = now;
    }
  }

  let token = session_cookie(headers)?;
  // Reject cookie values that are not valid UUIDs (session tokens are always
  // generated with uuid::Uuid::new_v4). This avoids unnecessary HashMap lookups
  // and prevents injection of malformed keys.
  if uuid::Uuid::parse_str(token).is_err() {
    return None;
  }
  let mut sessions = state.sessions.lock().await;
  if let Some(info) = sessions.get(token) {
    if info.expires_at > Instant::now() {
      return Some(info.scope_host.clone());
    }
    sessions.remove(token);
  }
  None
}

/// Validates the `aperio_session` cookie for full (global) access — the
/// dashboard, tunnel provisioning, and any proxied host. A host-scoped session
/// (a client-set visitor password login) does NOT satisfy this.
pub(crate) async fn validate_session(state: &AppState, headers: &HeaderMap) -> bool {
  matches!(session_scope(state, headers).await, Some(None))
}

/// Validates the `aperio_session` cookie for a proxied request to `host`.
/// Accepts a global session, or a session scoped to exactly this host.
pub(crate) async fn validate_session_for_host(
  state: &AppState,
  headers: &HeaderMap,
  host: Option<&str>,
) -> bool {
  match session_scope(state, headers).await {
    Some(None) => true,
    Some(Some(scope)) => host.is_some_and(|h| h == scope),
    None => false,
  }
}

/// Derives the OIDC redirect URI for this deployment: the explicit override
/// wins, otherwise it is built from the request Host header (and
/// X-Forwarded-Proto when running behind a trusted proxy).
fn oidc_redirect_uri(state: &AppState, headers: &HeaderMap) -> Option<String> {
  let rt = state.oidc.as_ref()?;
  if let Some(ref fixed) = rt.redirect_url_override {
    return Some(fixed.clone());
  }
  let host = headers.get("host").and_then(|v| v.to_str().ok())?;
  let proto = if state.config().trust_proxy {
    headers
      .get("x-forwarded-proto")
      .and_then(|v| v.to_str().ok())
      .unwrap_or("http")
  } else {
    "http"
  };
  Some(format!("{}://{}/aperio/oidc/callback", proto, host))
}

/// Starts the OIDC authorization code flow: stores a CSRF state token and
/// redirects the browser to the identity provider.
pub(crate) async fn oidc_login_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let Some(rt) = state.oidc.clone() else {
    return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
  };
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  );
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let redirect_after = query
    .get("redirect")
    .map(|r| safe_redirect_path(r).to_string())
    .unwrap_or_else(|| "/".to_string());
  let Some(redirect_uri) = oidc_redirect_uri(&state, &headers) else {
    return (StatusCode::BAD_REQUEST, "Missing Host header").into_response();
  };

  // Register the CSRF state (10 min TTL, opportunistic GC).
  let state_token = uuid::Uuid::new_v4().to_string();
  {
    let mut states = state.oidc_states.lock().await;
    let now = Instant::now();
    states.retain(|_, (_, exp)| *exp > now);
    states.insert(
      state_token.clone(),
      (redirect_after, now + Duration::from_secs(600)),
    );
  }

  let authorize = url::Url::parse_with_params(
    &rt.authorization_endpoint,
    &[
      ("response_type", "code"),
      ("client_id", rt.client_id.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("scope", rt.scopes.as_str()),
      ("state", state_token.as_str()),
    ],
  );
  match authorize {
    Ok(u) => Response::builder()
      .status(StatusCode::FOUND)
      .header("Location", u.to_string())
      .body(Body::empty())
      .unwrap(),
    Err(e) => {
      error!("Failed to build OIDC authorize URL: {}", e);
      (
        StatusCode::INTERNAL_SERVER_ERROR,
        "OIDC configuration error",
      )
        .into_response()
    }
  }
}

/// OIDC callback: validates the CSRF state, exchanges the code for tokens,
/// fetches the userinfo email, checks it against the allowlist, and creates
/// a session identical to the password login.
pub(crate) async fn oidc_callback_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let Some(rt) = state.oidc.clone() else {
    return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
  };
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  );
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let (Some(code), Some(state_param)) = (query.get("code"), query.get("state")) else {
    return (StatusCode::BAD_REQUEST, "Missing code/state parameter").into_response();
  };

  // Validate and consume the CSRF state.
  let redirect_after = {
    let mut states = state.oidc_states.lock().await;
    match states.remove(state_param) {
      Some((redirect, exp)) if exp > Instant::now() => redirect,
      _ => {
        return (StatusCode::BAD_REQUEST, "Invalid or expired OIDC state").into_response();
      }
    }
  };
  let Some(redirect_uri) = oidc_redirect_uri(&state, &headers) else {
    return (StatusCode::BAD_REQUEST, "Missing Host header").into_response();
  };

  // Exchange the authorization code for an access token.
  let http = reqwest::Client::builder()
    .timeout(Duration::from_secs(15))
    .build()
    .unwrap_or_default();
  let token_res = http
    .post(&rt.token_endpoint)
    .form(&[
      ("grant_type", "authorization_code"),
      ("code", code.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
      ("client_id", rt.client_id.as_str()),
      ("client_secret", rt.client_secret.as_str()),
    ])
    .send()
    .await;
  #[derive(Deserialize)]
  struct TokenResponse {
    access_token: String,
  }
  let access_token = match token_res {
    Ok(res) if res.status().is_success() => match res.json::<TokenResponse>().await {
      Ok(t) => t.access_token,
      Err(e) => {
        error!("OIDC token response parse error: {}", e);
        return (StatusCode::BAD_GATEWAY, "OIDC token exchange failed").into_response();
      }
    },
    Ok(res) => {
      warn!("OIDC token endpoint returned {}", res.status());
      return (StatusCode::UNAUTHORIZED, "OIDC token exchange rejected").into_response();
    }
    Err(e) => {
      error!("OIDC token exchange failed: {}", e);
      return (StatusCode::BAD_GATEWAY, "OIDC token exchange failed").into_response();
    }
  };

  // Fetch the verified identity from the issuer (trusted via TLS).
  #[derive(Deserialize)]
  struct UserInfo {
    email: Option<String>,
  }
  let userinfo = http
    .get(&rt.userinfo_endpoint)
    .bearer_auth(&access_token)
    .send()
    .await;
  let email = match userinfo {
    Ok(res) if res.status().is_success() => match res.json::<UserInfo>().await {
      Ok(u) => u.email.unwrap_or_default(),
      Err(e) => {
        error!("OIDC userinfo parse error: {}", e);
        return (StatusCode::BAD_GATEWAY, "OIDC userinfo failed").into_response();
      }
    },
    _ => {
      return (StatusCode::BAD_GATEWAY, "OIDC userinfo failed").into_response();
    }
  };

  if !oidc::email_allowed(&email, &rt.allowed_emails) {
    warn!("OIDC login denied for {} (not in allowlist)", email);
    state
      .audit(
        "oidc_login_denied",
        &caller_ip.to_string(),
        &format!("email={}", email),
      )
      .await;
    return (
      StatusCode::FORBIDDEN,
      "403 Forbidden - Your account is not allowed to access this service",
    )
      .into_response();
  }

  info!("OIDC login success for {}", email);
  state
    .audit(
      "oidc_login_success",
      &caller_ip.to_string(),
      &format!("email={}", email),
    )
    .await;

  // Create a global session identical to the password login flow.
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    session_token.clone(),
    SessionInfo {
      expires_at: Instant::now() + Duration::from_secs(86400),
      scope_host: None,
    },
  );
  let secure_flag = if state.config().secure_cookies {
    "; Secure"
  } else {
    ""
  };
  let cookie = format!(
    "aperio_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_token, secure_flag
  );
  Response::builder()
    .status(StatusCode::FOUND)
    .header("Set-Cookie", cookie)
    .header("Location", redirect_after)
    .body(Body::empty())
    .unwrap()
}

/// Validates a redirect path to prevent open redirect attacks.
/// Only allows same-origin relative paths (starting with `/`) and rejects
/// protocol-relative URLs (`//evil.com`) and backslash-based bypasses (`/\`).
pub(crate) fn safe_redirect_path(uri: &str) -> &str {
  if uri.starts_with('/') && !uri.starts_with("//") && !uri.starts_with("/\\") {
    uri
  } else {
    "/"
  }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
