//! WebAuthn (passkey) sign-in for dashboard users, built on `webauthn-rs`.
//!
//! Enabled by setting `APERIO_WEBAUTHN_ORIGIN` to the public URL the
//! dashboard is reached at (e.g. `https://tunnel.example.com`) — the RP ID is
//! its domain, and browsers refuse credentials for a mismatched origin, so it
//! cannot be guessed. Registration is self-service for signed-in named users
//! (`/aperio/api/me/passkeys/*`); sign-in is passwordless from the login page
//! (`/aperio/auth/passkey/*`) and bypasses TOTP (a passkey is already a
//! possession factor with user verification).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use tracing::{info, warn};
use webauthn_rs::prelude::*;

use crate::state::{AppState, SessionInfo};

/// Builds the Webauthn verifier from `APERIO_WEBAUTHN_ORIGIN`; None (with a
/// log line) when the variable is unset or unusable.
pub(crate) fn build_webauthn() -> Option<Webauthn> {
  let origin_raw = std::env::var("APERIO_WEBAUTHN_ORIGIN").ok()?;
  let origin_raw = origin_raw.trim();
  if origin_raw.is_empty() {
    return None;
  }
  let origin = match Url::parse(origin_raw) {
    Ok(u) => u,
    Err(e) => {
      warn!(
        "Invalid APERIO_WEBAUTHN_ORIGIN '{}': {} — passkey sign-in disabled",
        origin_raw, e
      );
      return None;
    }
  };
  let Some(rp_id) = origin.domain().map(str::to_string) else {
    warn!(
      "APERIO_WEBAUTHN_ORIGIN '{}' has no domain (IP origins are not valid RP IDs) — passkey sign-in disabled",
      origin_raw
    );
    return None;
  };
  match WebauthnBuilder::new(&rp_id, &origin).map(|b| b.rp_name("Aperio").build()) {
    Ok(Ok(webauthn)) => {
      info!(
        "Passkey sign-in enabled (RP ID {}, origin {})",
        rp_id, origin
      );
      Some(webauthn)
    }
    Ok(Err(e)) | Err(e) => {
      warn!(
        "Could not initialize WebAuthn: {} — passkey sign-in disabled",
        e
      );
      None
    }
  }
}

/// How long an in-flight registration/authentication challenge stays valid.
const CHALLENGE_TTL: Duration = Duration::from_secs(300);

/// In-flight WebAuthn ceremonies, keyed by a one-time id.
#[derive(Default)]
pub(crate) struct WebauthnCeremonies {
  reg: HashMap<String, (Instant, String, PasskeyRegistration)>,
  auth: HashMap<String, (Instant, String, PasskeyAuthentication)>,
  /// Usernameless (discoverable) sign-ins: no user is known until the
  /// authenticator returns a credential with its user handle.
  disc: HashMap<String, (Instant, DiscoverableAuthentication)>,
}

impl WebauthnCeremonies {
  fn gc(&mut self) {
    let now = Instant::now();
    self
      .reg
      .retain(|_, (t, _, _)| now.duration_since(*t) < CHALLENGE_TTL);
    self
      .auth
      .retain(|_, (t, _, _)| now.duration_since(*t) < CHALLENGE_TTL);
    self
      .disc
      .retain(|_, (t, _)| now.duration_since(*t) < CHALLENGE_TTL);
  }
}

fn webauthn_or_disabled(state: &AppState) -> Result<&Webauthn, Box<Response>> {
  state.webauthn.as_ref().ok_or_else(|| {
    Box::new(
      (
        StatusCode::NOT_IMPLEMENTED,
        "Passkey sign-in is not configured; set APERIO_WEBAUTHN_ORIGIN to the dashboard's public URL",
      )
        .into_response(),
    )
  })
}

/// Resolves the calling session to its named user (id, username).
async fn session_user(
  state: &Arc<AppState>,
  headers: &HeaderMap,
) -> Result<(String, String), Response> {
  let Some(username) = crate::auth::dashboard_username(state, headers).await else {
    return Err(
      (
        StatusCode::BAD_REQUEST,
        "Passkeys apply to named dashboard users; the built-in admin signs in with the master token or dashboard password",
      )
        .into_response(),
    );
  };
  match state.users.lock().await.find_by_username(&username) {
    Some(user) => Ok((user.id.clone(), user.username.clone())),
    None => Err((StatusCode::BAD_REQUEST, "Unknown user").into_response()),
  }
}

/// Starts passkey registration for the signed-in user.
#[utoipa::path(post, path = "/aperio/api/me/passkeys/register/start", tag = "users",
  description = "Starts passkey registration for the signed-in dashboard user: returns the WebAuthn creation challenge and a ceremony id. Requires APERIO_WEBAUTHN_ORIGIN.",
  responses((status = 200, description = "Creation challenge", body = serde_json::Value), (status = 501, description = "Passkeys not configured")))]
pub(crate) async fn passkey_register_start_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let webauthn = match webauthn_or_disabled(&state) {
    Ok(w) => w,
    Err(resp) => return *resp,
  };
  let (user_id, username) = match session_user(&state, &headers).await {
    Ok(u) => u,
    Err(resp) => return resp,
  };
  // Exclude already-registered credentials so an authenticator is not
  // enrolled twice.
  let exclude: Vec<CredentialID> = {
    let users = state.users.lock().await;
    users
      .get(&user_id)
      .map(|u| {
        u.passkeys
          .iter()
          .filter_map(|p| serde_json::from_str::<Passkey>(&p.credential).ok())
          .map(|p| p.cred_id().clone())
          .collect()
      })
      .unwrap_or_default()
  };
  let unique_id = match uuid::Uuid::parse_str(&user_id) {
    Ok(u) => u,
    Err(_) => uuid::Uuid::new_v4(),
  };
  let (challenge, reg_state) = match webauthn.start_passkey_registration(
    unique_id,
    &username,
    &username,
    (!exclude.is_empty()).then_some(exclude),
  ) {
    Ok(x) => x,
    Err(e) => {
      warn!("Passkey registration start failed: {}", e);
      return (StatusCode::BAD_REQUEST, "Could not start registration").into_response();
    }
  };
  let ceremony_id = uuid::Uuid::new_v4().to_string();
  {
    let mut ceremonies = state.webauthn_ceremonies.lock().await;
    ceremonies.gc();
    ceremonies
      .reg
      .insert(ceremony_id.clone(), (Instant::now(), user_id, reg_state));
  }
  Json(serde_json::json!({ "ceremony_id": ceremony_id, "challenge": challenge })).into_response()
}

/// Body of the registration finish call.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PasskeyRegisterFinishRequest {
  pub(crate) ceremony_id: String,
  /// Display name for the new passkey (e.g. "YubiKey 5", "MacBook Touch ID").
  #[serde(default)]
  pub(crate) name: Option<String>,
  /// Allow signing in with this passkey without typing a username
  /// (requires a discoverable/resident credential).
  #[serde(default)]
  pub(crate) usernameless: bool,
  /// The browser's `navigator.credentials.create()` result.
  #[schema(value_type = Object)]
  pub(crate) credential: RegisterPublicKeyCredential,
}

/// Completes passkey registration.
#[utoipa::path(post, path = "/aperio/api/me/passkeys/register/finish", tag = "users",
  description = "Completes passkey registration with the browser's credential response and stores the passkey on the user.",
  request_body = PasskeyRegisterFinishRequest,
  responses((status = 200, description = "Registered", body = serde_json::Value), (status = 400, description = "Invalid ceremony or credential")))]
pub(crate) async fn passkey_register_finish_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<PasskeyRegisterFinishRequest>,
) -> Response {
  let webauthn = match webauthn_or_disabled(&state) {
    Ok(w) => w,
    Err(resp) => return *resp,
  };
  let (user_id, _) = match session_user(&state, &headers).await {
    Ok(u) => u,
    Err(resp) => return resp,
  };
  let reg_state = {
    let mut ceremonies = state.webauthn_ceremonies.lock().await;
    ceremonies.gc();
    match ceremonies.reg.remove(&payload.ceremony_id) {
      Some((_, owner, st)) if owner == user_id => st,
      _ => {
        return (StatusCode::BAD_REQUEST, "Unknown or expired ceremony").into_response();
      }
    }
  };
  let passkey = match webauthn.finish_passkey_registration(&payload.credential, &reg_state) {
    Ok(p) => p,
    Err(e) => {
      warn!("Passkey registration failed: {}", e);
      return (StatusCode::BAD_REQUEST, "Credential verification failed").into_response();
    }
  };
  let serialized = match serde_json::to_string(&passkey) {
    Ok(s) => s,
    Err(e) => {
      warn!("Passkey serialization failed: {}", e);
      return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
    }
  };
  let name = payload
    .name
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .unwrap_or("passkey")
    .chars()
    .take(64)
    .collect::<String>();
  let stored =
    match state
      .users
      .lock()
      .await
      .add_passkey(&user_id, &name, &serialized, payload.usernameless)
    {
      Ok(p) => p,
      Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
  let ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  state
    .audit_session(
      "passkey_registered",
      &headers,
      &ip,
      &format!("user_id={} passkey={}", user_id, stored.id),
    )
    .await;
  Json(serde_json::json!({"status": "ok", "id": stored.id, "name": stored.name})).into_response()
}

/// Lists the signed-in user's passkeys (never the credential material).
#[utoipa::path(get, path = "/aperio/api/me/passkeys", tag = "users",
  description = "Lists the signed-in user's registered passkeys (id, name, created_at only).",
  responses((status = 200, description = "Passkeys", body = serde_json::Value)))]
pub(crate) async fn passkeys_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let (user_id, _) = match session_user(&state, &headers).await {
    Ok(u) => u,
    Err(resp) => return resp,
  };
  let users = state.users.lock().await;
  let list: Vec<serde_json::Value> = users
    .get(&user_id)
    .map(|u| {
      u.passkeys
        .iter()
        .map(|p| serde_json::json!({"id": p.id, "name": p.name, "created_at": p.created_at, "usernameless": p.usernameless}))
        .collect()
    })
    .unwrap_or_default();
  Json(list).into_response()
}

/// Deletes one of the signed-in user's passkeys.
#[utoipa::path(delete, path = "/aperio/api/me/passkeys/{id}", tag = "users",
  description = "Deletes one of the signed-in user's passkeys.",
  params(("id" = String, Path, description = "Passkey id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown passkey")))]
pub(crate) async fn passkey_delete_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let (user_id, _) = match session_user(&state, &headers).await {
    Ok(u) => u,
    Err(resp) => return resp,
  };
  if !state.users.lock().await.remove_passkey(&user_id, &id) {
    return (StatusCode::NOT_FOUND, "Unknown passkey").into_response();
  }
  let ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  state
    .audit_session(
      "passkey_deleted",
      &headers,
      &ip,
      &format!("user_id={} passkey={}", user_id, id),
    )
    .await;
  Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Body of the login start call.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PasskeyLoginStartRequest {
  pub(crate) username: String,
}

/// Starts a passkey sign-in from the login page (no session required).
#[utoipa::path(post, path = "/aperio/auth/passkey/start", tag = "auth",
  description = "Starts a passkey sign-in for a username: returns the WebAuthn request challenge and a ceremony id. Rate-limited like password logins.",
  request_body = PasskeyLoginStartRequest,
  responses((status = 200, description = "Request challenge", body = serde_json::Value), (status = 401, description = "Unknown user or no passkeys"), (status = 501, description = "Passkeys not configured")))]
pub(crate) async fn passkey_login_start_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<PasskeyLoginStartRequest>,
) -> Response {
  let webauthn = match webauthn_or_disabled(&state) {
    Ok(w) => w,
    Err(resp) => return *resp,
  };
  let cfg = state.config();
  let client_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    cfg.trust_proxy,
    cfg.real_ip_header.as_deref(),
    &cfg.trusted_proxies,
  );
  if !state.check_rate_limit(client_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Rate limited").into_response();
  }
  if state
    .login_lockout
    .lock()
    .await
    .locked(client_ip, Instant::now())
    .is_some()
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Locked out").into_response();
  }

  let (user_id, passkeys) = {
    let users = state.users.lock().await;
    match users.find_by_username(&payload.username) {
      Some(user) if !user.passkeys.is_empty() => (
        user.id.clone(),
        user
          .passkeys
          .iter()
          .filter_map(|p| serde_json::from_str::<Passkey>(&p.credential).ok())
          .collect::<Vec<_>>(),
      ),
      // A uniform 401 for unknown users and users without passkeys, so the
      // endpoint does not become a username oracle.
      _ => return (StatusCode::UNAUTHORIZED, "No passkeys for this user").into_response(),
    }
  };
  let (challenge, auth_state) = match webauthn.start_passkey_authentication(&passkeys) {
    Ok(x) => x,
    Err(e) => {
      warn!("Passkey authentication start failed: {}", e);
      return (StatusCode::BAD_REQUEST, "Could not start authentication").into_response();
    }
  };
  let ceremony_id = uuid::Uuid::new_v4().to_string();
  {
    let mut ceremonies = state.webauthn_ceremonies.lock().await;
    ceremonies.gc();
    ceremonies
      .auth
      .insert(ceremony_id.clone(), (Instant::now(), user_id, auth_state));
  }
  Json(serde_json::json!({ "ceremony_id": ceremony_id, "challenge": challenge })).into_response()
}

/// Body of the login finish call.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PasskeyLoginFinishRequest {
  pub(crate) ceremony_id: String,
  /// The browser's `navigator.credentials.get()` result.
  #[schema(value_type = Object)]
  pub(crate) credential: PublicKeyCredential,
}

/// Completes a passkey sign-in and issues the session cookie.
#[utoipa::path(post, path = "/aperio/auth/passkey/finish", tag = "auth",
  description = "Completes a passkey sign-in; on success the aperio_session cookie is set (TOTP is not required for passkey sign-ins).",
  request_body = PasskeyLoginFinishRequest,
  responses((status = 200, description = "Signed in"), (status = 401, description = "Verification failed")))]
pub(crate) async fn passkey_login_finish_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<PasskeyLoginFinishRequest>,
) -> Response {
  let webauthn = match webauthn_or_disabled(&state) {
    Ok(w) => w,
    Err(resp) => return *resp,
  };
  let cfg = state.config();
  let client_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    cfg.trust_proxy,
    cfg.real_ip_header.as_deref(),
    &cfg.trusted_proxies,
  );
  let auth_entry = {
    let mut ceremonies = state.webauthn_ceremonies.lock().await;
    ceremonies.gc();
    ceremonies.auth.remove(&payload.ceremony_id)
  };
  let Some((_, user_id, auth_state)) = auth_entry else {
    return (StatusCode::BAD_REQUEST, "Unknown or expired ceremony").into_response();
  };
  let result = match webauthn.finish_passkey_authentication(&payload.credential, &auth_state) {
    Ok(r) => r,
    Err(e) => {
      warn!("Passkey authentication failed for user {}: {}", user_id, e);
      state
        .audit(
          "login_failed",
          "-",
          &client_ip.to_string(),
          "passkey verification failed",
        )
        .await;
      let locked = state
        .login_lockout
        .lock()
        .await
        .record_failure(client_ip, Instant::now());
      if locked.is_some() {
        state
          .audit(
            "login_lockout",
            "-",
            &client_ip.to_string(),
            "passkey failures",
          )
          .await;
      }
      return (StatusCode::UNAUTHORIZED, "Verification failed").into_response();
    }
  };

  // Persist authenticator counter updates (clone detection state).
  let (username, role, org) = {
    let mut users = state.users.lock().await;
    users.update_passkey_after_auth(&user_id, &result);
    match users.get(&user_id) {
      Some(u) if u.enabled => (u.username.clone(), u.role, u.org_id.clone()),
      _ => return (StatusCode::UNAUTHORIZED, "User disabled").into_response(),
    }
  };
  state.login_lockout.lock().await.clear(client_ip);
  state
    .audit_in(
      "login_success",
      &username,
      &client_ip.to_string(),
      org,
      &format!(
        "session created (user={}, role={}, passkey)",
        username,
        role.as_str()
      ),
    )
    .await;

  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &session_token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 86400,
      created_at: crate::store::sessions::now_secs(),
      ip: Some(client_ip.to_string()),
      user_agent: crate::store::sessions::session_user_agent(&headers),
      scope_host: None,
      username: Some(username),
      role,
      selected_org: None,
      bound_org: None,
    },
  );
  let secure_flag = if cfg.secure_cookies { "; Secure" } else { "" };
  let cookie = format!(
    "aperio_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_token, secure_flag
  );
  Response::builder()
    .status(StatusCode::OK)
    .header("Set-Cookie", cookie)
    .body(axum::body::Body::empty())
    .unwrap()
}

/// Starts a usernameless (discoverable-credential) sign-in: no username is
/// needed — the authenticator's account picker supplies the identity.
#[utoipa::path(post, path = "/aperio/auth/passkey/discoverable/start", tag = "auth",
  description = "Starts a usernameless passkey sign-in; the returned challenge lets the authenticator pick from its resident credentials.",
  responses((status = 200, description = "Request challenge", body = serde_json::Value), (status = 501, description = "Passkeys not configured")))]
pub(crate) async fn passkey_discoverable_start_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let webauthn = match webauthn_or_disabled(&state) {
    Ok(w) => w,
    Err(resp) => return *resp,
  };
  let cfg = state.config();
  let client_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    cfg.trust_proxy,
    cfg.real_ip_header.as_deref(),
    &cfg.trusted_proxies,
  );
  if !state.check_rate_limit(client_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Rate limited").into_response();
  }
  if state
    .login_lockout
    .lock()
    .await
    .locked(client_ip, Instant::now())
    .is_some()
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Locked out").into_response();
  }
  let (challenge, disc_state) = match webauthn.start_discoverable_authentication() {
    Ok(x) => x,
    Err(e) => {
      warn!("Discoverable authentication start failed: {}", e);
      return (StatusCode::BAD_REQUEST, "Could not start authentication").into_response();
    }
  };
  let ceremony_id = uuid::Uuid::new_v4().to_string();
  {
    let mut ceremonies = state.webauthn_ceremonies.lock().await;
    ceremonies.gc();
    ceremonies
      .disc
      .insert(ceremony_id.clone(), (Instant::now(), disc_state));
  }
  Json(serde_json::json!({ "ceremony_id": ceremony_id, "challenge": challenge })).into_response()
}

/// Completes a usernameless passkey sign-in. Only passkeys registered with
/// the usernameless opt-in may sign in this way.
#[utoipa::path(post, path = "/aperio/auth/passkey/discoverable/finish", tag = "auth",
  description = "Completes a usernameless passkey sign-in; the credential's user handle identifies the account. Only passkeys registered with the usernameless opt-in are accepted.",
  request_body = PasskeyLoginFinishRequest,
  responses((status = 200, description = "Signed in"), (status = 401, description = "Verification failed or the passkey is not enabled for usernameless sign-in")))]
pub(crate) async fn passkey_discoverable_finish_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<PasskeyLoginFinishRequest>,
) -> Response {
  let webauthn = match webauthn_or_disabled(&state) {
    Ok(w) => w,
    Err(resp) => return *resp,
  };
  let cfg = state.config();
  let client_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    cfg.trust_proxy,
    cfg.real_ip_header.as_deref(),
    &cfg.trusted_proxies,
  );
  let disc_entry = {
    let mut ceremonies = state.webauthn_ceremonies.lock().await;
    ceremonies.gc();
    ceremonies.disc.remove(&payload.ceremony_id)
  };
  let Some((_, disc_state)) = disc_entry else {
    return (StatusCode::BAD_REQUEST, "Unknown or expired ceremony").into_response();
  };

  let fail = |reason: String| -> Response {
    warn!("Usernameless passkey sign-in failed: {}", reason);
    (StatusCode::UNAUTHORIZED, "Verification failed").into_response()
  };

  // The credential carries its user handle: the UUID we registered with.
  let (user_uuid, cred_id) =
    match webauthn.identify_discoverable_authentication(&payload.credential) {
      Ok(x) => x,
      Err(e) => {
        state
          .audit(
            "login_failed",
            "-",
            &client_ip.to_string(),
            "usernameless passkey: unidentifiable credential",
          )
          .await;
        let locked = state
          .login_lockout
          .lock()
          .await
          .record_failure(client_ip, Instant::now());
        if locked.is_some() {
          state
            .audit(
              "login_lockout",
              "-",
              &client_ip.to_string(),
              "passkey failures",
            )
            .await;
        }
        return fail(format!("unidentifiable credential: {e}"));
      }
    };
  let user_id = user_uuid.to_string();

  // Resolve the user and the exact passkey; the usernameless opt-in gates
  // this path — a passkey registered without it stays username-first only.
  let (keys, allowed) = {
    let users = state.users.lock().await;
    match users.get(&user_id) {
      Some(user) if user.enabled => {
        let parsed: Vec<(Passkey, bool)> = user
          .passkeys
          .iter()
          .filter_map(|p| {
            serde_json::from_str::<Passkey>(&p.credential)
              .ok()
              .map(|key| (key, p.usernameless))
          })
          .collect();
        let allowed = parsed
          .iter()
          .any(|(key, usernameless)| *key.cred_id() == cred_id && *usernameless);
        (
          parsed
            .iter()
            .map(|(key, _)| DiscoverableKey::from(key))
            .collect::<Vec<_>>(),
          allowed,
        )
      }
      _ => (Vec::new(), false),
    }
  };
  if keys.is_empty() {
    return fail("no passkeys for the identified user".to_string());
  }
  if !allowed {
    state
      .audit(
        "login_failed",
        "-",
        &client_ip.to_string(),
        "usernameless passkey: credential not opted in",
      )
      .await;
    return fail("the passkey is not enabled for usernameless sign-in".to_string());
  }

  let result =
    match webauthn.finish_discoverable_authentication(&payload.credential, disc_state, &keys) {
      Ok(r) => r,
      Err(e) => {
        state
          .audit(
            "login_failed",
            "-",
            &client_ip.to_string(),
            "usernameless passkey verification failed",
          )
          .await;
        let locked = state
          .login_lockout
          .lock()
          .await
          .record_failure(client_ip, Instant::now());
        if locked.is_some() {
          state
            .audit(
              "login_lockout",
              "-",
              &client_ip.to_string(),
              "passkey failures",
            )
            .await;
        }
        return fail(format!("verification failed: {e}"));
      }
    };

  let (username, role, org) = {
    let mut users = state.users.lock().await;
    users.update_passkey_after_auth(&user_id, &result);
    match users.get(&user_id) {
      Some(u) if u.enabled => (u.username.clone(), u.role, u.org_id.clone()),
      _ => return (StatusCode::UNAUTHORIZED, "User disabled").into_response(),
    }
  };
  state.login_lockout.lock().await.clear(client_ip);
  state
    .audit_in(
      "login_success",
      &username,
      &client_ip.to_string(),
      org,
      &format!(
        "session created (user={}, role={}, passkey, usernameless)",
        username,
        role.as_str()
      ),
    )
    .await;

  let session_token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &session_token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 86400,
      created_at: crate::store::sessions::now_secs(),
      ip: Some(client_ip.to_string()),
      user_agent: crate::store::sessions::session_user_agent(&headers),
      scope_host: None,
      username: Some(username),
      role,
      selected_org: None,
      bound_org: None,
    },
  );
  let secure_flag = if cfg.secure_cookies { "; Secure" } else { "" };
  let cookie = format!(
    "aperio_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
    session_token, secure_flag
  );
  Response::builder()
    .status(StatusCode::OK)
    .header("Set-Cookie", cookie)
    .body(axum::body::Body::empty())
    .unwrap()
}

/// Public probe for the login page: whether passkey sign-in is configured.
#[utoipa::path(get, path = "/aperio/auth/passkey", tag = "auth",
  description = "True when passkey sign-in is configured (APERIO_WEBAUTHN_ORIGIN set).",
  responses((status = 200, description = "Availability", body = serde_json::Value)))]
pub(crate) async fn passkey_available_handler(State(state): State<Arc<AppState>>) -> Response {
  Json(serde_json::json!({ "available": state.webauthn.is_some() })).into_response()
}
