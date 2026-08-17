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

// Split by what each part answers: is this a valid credential, whose session is
// this, what may they act on, and the OIDC flow that mints one. `oidc_flow`
// rather than `oidc` because the crate already has an `oidc` module, and a glob
// re-export of the same name would shadow it here.
pub(crate) mod ip;
pub(crate) mod oidc_flow;
pub(crate) mod scope;
pub(crate) mod session;
pub(crate) mod token;

pub(crate) use ip::*;
pub(crate) use oidc_flow::*;
pub(crate) use scope::*;
pub(crate) use session::*;
pub(crate) use token::*;

use crate::oidc;

use crate::api::serve_embedded;
use crate::routing::extract_client_ip;
use crate::state::{AppState, ClientPerms, SessionInfo};
use crate::store::users::Role;

/// Serves the login page from the embedded dashboard build.
pub(crate) async fn auth_page_handler() -> Response {
  serve_embedded("auth.html", false)
}

/// Per-IP failed-login state for the brute-force lockout.
struct LoginFailures {
  /// Consecutive failures since the last success / completed lockout.
  consecutive: u32,
  /// Number of lockouts already served, for escalation.
  lockouts: u32,
  /// Active lockout end, if any.
  locked_until: Option<Instant>,
  /// Last activity, for garbage collection.
  last_seen: Instant,
}

/// Escalating per-IP login lockout: after `threshold` consecutive failures the
/// IP is locked out for `base` seconds, doubling with each subsequent lockout
/// up to [`LOCKOUT_MAX`]. A successful login clears the state. Pure (caller
/// supplies `now`), so the policy is unit-testable; [`AppState`] wraps one in
/// a mutex.
pub(crate) struct LockoutTracker {
  threshold: u32,
  base: Duration,
  map: HashMap<IpAddr, LoginFailures>,
}

/// Upper bound on an escalated lockout window.
const LOCKOUT_MAX: Duration = Duration::from_secs(3600);

/// Largest answer read back from an OIDC token or userinfo endpoint.
///
/// Both are a handful of fields, so this is generous rather than tight; what
/// it rules out is the endpoint deciding how much memory a login costs.
const OIDC_MAX_ANSWER_BYTES: usize = 256 * 1024;

impl LockoutTracker {
  pub(crate) fn new(threshold: u32, base: Duration) -> Self {
    Self {
      threshold: threshold.max(1),
      base: base.max(Duration::from_secs(1)),
      map: HashMap::new(),
    }
  }

  /// Replaces the lockout policy at runtime (dashboard settings). Existing
  /// per-IP failure counters keep running under the new values.
  pub(crate) fn set_policy(&mut self, threshold: u32, base: Duration) {
    self.threshold = threshold.max(1);
    self.base = base.max(Duration::from_secs(1));
  }

  /// Remaining lockout for `ip`, if it is currently locked out.
  pub(crate) fn locked(&mut self, ip: IpAddr, now: Instant) -> Option<Duration> {
    let entry = self.map.get_mut(&ip)?;
    match entry.locked_until {
      Some(until) if until > now => Some(until - now),
      Some(_) => {
        // Lockout served: failures start counting from zero again, but the
        // escalation counter is kept so a repeat offender locks out longer.
        entry.locked_until = None;
        entry.consecutive = 0;
        None
      }
      None => None,
    }
  }

  /// Records a failed login. Returns the lockout duration when this failure
  /// crosses the threshold and triggers (or re-triggers) a lockout.
  pub(crate) fn record_failure(&mut self, ip: IpAddr, now: Instant) -> Option<Duration> {
    self.gc(now);
    let entry = self.map.entry(ip).or_insert(LoginFailures {
      consecutive: 0,
      lockouts: 0,
      locked_until: None,
      last_seen: now,
    });
    entry.consecutive += 1;
    entry.last_seen = now;
    if entry.consecutive < self.threshold {
      return None;
    }
    entry.consecutive = 0;
    entry.lockouts = entry.lockouts.saturating_add(1);
    // base * 2^(lockouts-1), capped.
    let factor = 1u32 << (entry.lockouts - 1).min(16);
    let window = self.base.saturating_mul(factor).min(LOCKOUT_MAX);
    entry.locked_until = Some(now + window);
    Some(window)
  }

  /// Clears the failure state after a successful login.
  pub(crate) fn clear(&mut self, ip: IpAddr) {
    self.map.remove(&ip);
  }

  /// Drops stale entries so the map stays bounded (no failures nor lockout
  /// activity for 24h). Cheap enough to run inline on each recorded failure.
  fn gc(&mut self, now: Instant) {
    if self.map.len() > 1024 {
      self
        .map
        .retain(|_, e| now.duration_since(e.last_seen) < Duration::from_secs(24 * 3600));
    }
  }
}

/// Handles login form submission. Validates credentials and sets a session
/// cookie. Validation is host-aware: server/dashboard/master credentials create
/// a full (global) session, while a client-set per-service visitor password
/// creates a session scoped to that host only (it never unlocks the dashboard
/// or other hosts). A client override replaces the server's own visitor
/// password for that route, the server password no longer works there (master
/// and dashboard credentials always do).
#[utoipa::path(post, path = "/aperio/auth", tag = "auth",
  description = "Login form submission (form-encoded username/password). On success sets the aperio_session cookie and redirects. Rate-limited with an escalating per-IP lockout.",
  responses((status = 302, description = "Redirect (success or back to the form)"), (status = 429, description = "Locked out / rate limited")))]
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
    &cfg.trusted_proxies,
  );
  // The bucket is the only thing between a stolen password list and this server.
  if !state
    .check_rate_limit_cost(client_ip, crate::state::RateCost::Guessable)
    .await
  {
    return Err(StatusCode::TOO_MANY_REQUESTS);
  }
  // Brute-force lockout: an IP over the failed-login threshold is refused
  // outright (no credential check) until its escalating window passes.
  if let Some(remaining) = state
    .login_lockout
    .lock()
    .await
    .locked(client_ip, Instant::now())
  {
    warn!(
      "Login attempt from {} refused: locked out for {}s more",
      client_ip,
      remaining.as_secs()
    );
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
  // client override, the dashboard is always gated by server-level creds.
  let redirect_path = query
    .get("redirect")
    .map(|r| safe_redirect_path(r).to_string())
    .unwrap_or_else(|| "/".to_string());
  let custom_creds = if redirect_path.starts_with("/aperio") {
    None
  } else {
    crate::routing::route_visitor_auth(&state, &redirect_path, host.as_deref()).await
  };
  // The richer form of the same override, and it has to be read here too: a
  // `basic` method naming more than one user has no scalar spelling, so
  // `route_visitor_auth` returns nothing for it. The gate still sends those
  // visitors to this form, and before this was read, every credential the
  // policy listed was refused here, which locked the route to everyone it was
  // written to admit.
  let custom_policy = if redirect_path.starts_with("/aperio") {
    None
  } else {
    crate::routing::route_visitor_policy(&state, &redirect_path, host.as_deref()).await
  };

  // The scope of the session to create, based on which credential matched:
  //   Some(None)       -> global (server / dashboard / master credentials)
  //   Some(Some(host)) -> scoped to this host (client-set visitor credentials)
  //   None             -> authentication failed
  let mut scope: Option<Option<String>> = None;
  // Which plane the matched credential belongs to. The master token and a
  // named user administer Aperio; the visitor passwords, the server's own and
  // a client's, are for seeing a site behind it. Until this was tracked, both
  // produced the same session and "no host scope" was read as full access, so
  // the value handed to whoever should see the site opened the dashboard.
  let mut plane = crate::store::sessions::Plane::Admin;
  // Dashboard identity of the matched credential (username + role); master
  // and dashboard-password logins act as the built-in admin "aperio".
  let mut identity: (Option<String>, Role) = (None, Role::Admin);
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
      // Dashboard users (username:password) -> full session with their role.
      // A user with TOTP enabled must additionally present a valid code (or
      // an unused recovery code) in the X-Aperio-Totp header.
      if scope.is_none()
        && let Some((username, password)) = decoded_str.split_once(':')
      {
        let verified = {
          let users = state.users.lock().await;
          users.verify(username, password).map(|u| {
            (
              u.id.clone(),
              u.username.clone(),
              u.role,
              u.totp_secret.clone(),
            )
          })
        };
        if let Some((user_id, user_name, role, totp_secret)) = verified {
          match totp_secret {
            None => {
              scope = Some(None);
              identity = (Some(user_name), role);
            }
            Some(secret) => {
              let code = headers
                .get("x-aperio-totp")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .unwrap_or("");
              if code.is_empty() {
                // The password was right; the second factor is simply
                // missing. Signal the login page to ask for it, this is
                // not a lockout-worthy failure.
                return Ok(
                  (
                    StatusCode::UNAUTHORIZED,
                    [("x-aperio-totp", "required")],
                    "TOTP code required",
                  )
                    .into_response(),
                );
              }
              let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
              // Replay-hardened: a valid code is accepted only if its step is
              // newer than the last one this user logged in with, so a code
              // captured in transit can't be reused within its validity window.
              let ok = match crate::totp::verify_step(&secret, code, now_secs) {
                Some(step) => state
                  .users
                  .lock()
                  .await
                  .totp_try_advance_step(&user_id, step),
                None => state.users.lock().await.consume_recovery(&user_id, code),
              };
              if ok {
                scope = Some(None);
                identity = (Some(user_name), role);
              }
              // A wrong code falls through to the shared failure path
              // (audited and counted towards the brute-force lockout).
            }
          }
        }
      }
      // Client-set visitor credentials for this route -> host-scoped session.
      // The policy is asked the same way the server's own gate is below, so a
      // client's `basic` naming several users admits any of them; the scalar
      // is the spelling of a policy that names one, and is compared directly
      // for the clients that only ever sent that.
      if scope.is_none()
        && let Some(ref h) = host
        && match (&custom_policy, &custom_creds) {
          (Some(policy), _) => policy.admits_credential(&decoded_str),
          (None, Some(creds)) => constant_time_eq_str(&decoded_str, creds),
          (None, None) => false,
        }
      {
        scope = Some(Some(h.clone()));
        plane = crate::store::sessions::Plane::Visitor;
      }
      // Server visitor gate -> full access, but only when the route is not
      // under a client override (an override supersedes the server's own gate
      // for that route). The policy is asked rather than one credential
      // compared, so a `basic` method naming several users admits any of them
      // and the scalar spelling behaves exactly as it always did.
      if scope.is_none()
        && custom_creds.is_none()
        && custom_policy.is_none()
        && cfg.visitor_auth.admits_credential(&decoded_str)
      {
        // Every proxied host, since this gate is server-wide, and **not** the
        // dashboard: the scope of a session and the plane it belongs to are
        // two different questions, and answering only the first is what made
        // this credential an admin one.
        scope = Some(None);
        plane = crate::store::sessions::Plane::Visitor;
      }
    }
  }

  let Some(session_scope) = scope else {
    state
      .audit(
        "login_failed",
        "-",
        &client_ip.to_string(),
        "invalid credentials",
      )
      .await;
    // Count towards the lockout; audit when this failure triggers one.
    let locked = state
      .login_lockout
      .lock()
      .await
      .record_failure(client_ip, Instant::now());
    if let Some(window) = locked {
      warn!(
        "Locking out {} after repeated failed logins ({}s)",
        client_ip,
        window.as_secs()
      );
      state
        .audit(
          "login_lockout",
          "-",
          &client_ip.to_string(),
          &format!("window_secs={}", window.as_secs()),
        )
        .await;
    }
    return Err(StatusCode::UNAUTHORIZED);
  };
  state.login_lockout.lock().await.clear(client_ip);
  let scope_desc = match (&session_scope, &identity.0) {
    (Some(h), _) => format!("session created (scope={})", h),
    (None, Some(user)) => format!(
      "session created (user={}, role={})",
      user,
      identity.1.as_str()
    ),
    (None, None) => "session created (global)".to_string(),
  };
  // A named user's login is filed under their own organization; the built-in
  // admin (no username) and visitor-scoped sessions belong to master.
  let login_org = match identity.0.as_deref() {
    Some(user) => state
      .users
      .lock()
      .await
      .find_by_username(user)
      .and_then(|u| u.org_id.clone()),
    None => None,
  };
  state
    .audit_in(
      "login_success",
      identity.0.as_deref().unwrap_or("aperio"),
      &client_ip.to_string(),
      login_org,
      &scope_desc,
    )
    .await;

  // Create session
  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &session_token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 86400,
      created_at: crate::store::sessions::now_secs(),
      ip: Some(client_ip.to_string()),
      user_agent: crate::store::sessions::session_user_agent(&headers),
      plane,
      scope_host: session_scope,
      username: identity.0,
      role: identity.1,
      selected_org: None,
      bound_org: None,
    },
  );

  let cookie = session_set_cookie(cfg.secure_cookies, &session_token);

  Ok(
    Response::builder()
      .status(StatusCode::OK)
      .header("Set-Cookie", cookie)
      .body(Body::empty())
      .unwrap(),
  )
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
