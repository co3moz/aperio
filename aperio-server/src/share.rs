use axum::{
  Json,
  body::Body,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode, Uri},
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::{
  extract_client_ip, normalize_hostname_bind, normalize_path_bind, path_matches_bind,
  request_path_has_traversal,
};
use crate::state::AppState;

/// Payload for generating a share link (dashboard).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ShareCreateRequest {
  /// Hostname the link grants access to.
  pub(crate) hostname: String,
  /// Optional path prefix; omitted = the whole site.
  pub(crate) path: Option<String>,
  /// Lifetime in seconds; defaults to 3 days, capped at 30 days.
  pub(crate) ttl_seconds: Option<u64>,
}

/// Generates a signed share link: a URL that grants temporary, scoped access
/// to an auth-protected proxied site without a dashboard login. Stateless —
/// the token is HMAC-signed and simply expires; there is nothing to list or
/// revoke individually (rotating the master token invalidates all links).
#[utoipa::path(post, path = "/aperio/api/share", tag = "dashboard",
  description = "Mints a signed, expiring share link that grants visitors gate-free access to a host/path scope.",
  request_body = ShareCreateRequest,
  responses((status = 200, description = "Share URL + expiry", body = serde_json::Value), (status = 400, description = "Invalid scope/ttl")))]
pub(crate) async fn share_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ShareCreateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  let hostname = match normalize_hostname_bind(payload.hostname.trim()) {
    Some(h) => h,
    None => {
      return (
        StatusCode::BAD_REQUEST,
        format!("Invalid hostname: {}", payload.hostname),
      )
        .into_response();
    }
  };
  let path = match payload
    .path
    .as_deref()
    .map(str::trim)
    .filter(|p| !p.is_empty() && *p != "/")
  {
    None => None,
    Some(raw) => match normalize_path_bind(raw) {
      Some(p) => Some(p),
      None => {
        return (StatusCode::BAD_REQUEST, format!("Invalid path: {}", raw)).into_response();
      }
    },
  };
  // ttl_seconds: omitted = 3 days, 0 = the link never expires.
  let ttl = payload.ttl_seconds.unwrap_or(SHARE_DEFAULT_TTL_SECS);
  if ttl > SHARE_MAX_TTL_SECS {
    return (
      StatusCode::BAD_REQUEST,
      format!(
        "ttl_seconds must be at most {} (or 0 for never)",
        SHARE_MAX_TTL_SECS
      ),
    )
      .into_response();
  }

  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs();
  let claims = ShareClaims {
    host: hostname.clone(),
    path: path.clone(),
    exp: if ttl == 0 { None } else { Some(now + ttl) },
    id: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
  };
  let token = sign_share_claims(&claims, &share_signing_key(&state.config().token));
  let url = format!(
    "https://{}{}?aperio_share={}",
    hostname,
    path.as_deref().unwrap_or("/"),
    token
  );

  info!(
    "Share link created: id={} host={} path={:?} expires_at={:?}",
    claims.id, hostname, path, claims.exp
  );
  state
    .audit_session(
      "share_created",
      &headers,
      &actor_ip,
      &format!(
        "id={} hostname={} path={:?} expires_at={:?}",
        claims.id, hostname, path, claims.exp
      ),
    )
    .await;
  let share_org = crate::auth::effective_org(&state, &headers).await;
  state
    .emit_event_in(
      "share_created",
      serde_json::json!({"id": claims.id, "hostname": hostname, "path": path, "expires_at": claims.exp}),
      share_org,
    )
    .await;

  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": claims.id,
      "url": url,
      "token": token,
      "expires_at": claims.exp,
    })),
  )
    .into_response()
}

// --- Share links: signed, expiring, hostname/path-scoped access tokens ---

/// Cookie (and query parameter) name carrying a share token.
const SHARE_COOKIE: &str = "aperio_share";
/// Default share link lifetime: 3 days.
const SHARE_DEFAULT_TTL_SECS: u64 = 3 * 24 * 3600;
/// Maximum finite share link lifetime: 10 years (`ttl_seconds: 0` = never).
const SHARE_MAX_TTL_SECS: u64 = 10 * 365 * 24 * 3600;

/// Claims embedded in a share token. The token is
/// `base64url(json).base64url(hmac_sha256)`, signed with a key derived from
/// the master token — nothing is persisted server-side, so links survive
/// restarts and simply expire.
#[derive(Serialize, Deserialize)]
pub(crate) struct ShareClaims {
  /// Hostname the link grants access to.
  pub(crate) host: String,
  /// Path prefix the link grants access to (None = the whole site).
  #[serde(default)]
  pub(crate) path: Option<String>,
  /// Unix expiry timestamp in seconds (None = the link never expires).
  #[serde(default)]
  pub(crate) exp: Option<u64>,
  /// Random ID tying proxy-side usage back to the audit trail.
  pub(crate) id: String,
}

/// Derives the HMAC signing key for share tokens from the master token, so
/// links stay valid across restarts without persisting key material.
pub(crate) fn share_signing_key(server_token: &str) -> [u8; 32] {
  let mut hasher = Sha256::default();
  hasher.update(b"aperio-share-signing-key:");
  hasher.update(server_token.as_bytes());
  hasher.finalize().into()
}

pub(crate) fn sign_share_claims(claims: &ShareClaims, key: &[u8]) -> String {
  use base64::prelude::*;
  use hmac::Mac;
  let payload = BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap_or_default());
  let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
  mac.update(payload.as_bytes());
  let sig = BASE64_URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
  format!("{payload}.{sig}")
}

/// Verifies signature and expiry; returns the claims when the token is valid.
pub(crate) fn verify_share_token(token: &str, key: &[u8]) -> Option<ShareClaims> {
  use base64::prelude::*;
  use hmac::Mac;
  let (payload, sig) = token.split_once('.')?;
  let sig_bytes = BASE64_URL_SAFE_NO_PAD.decode(sig).ok()?;
  let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).ok()?;
  mac.update(payload.as_bytes());
  mac.verify_slice(&sig_bytes).ok()?;
  let claims: ShareClaims =
    serde_json::from_slice(&BASE64_URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .ok()?
    .as_secs();
  if let Some(exp) = claims.exp
    && exp <= now
  {
    return None;
  }
  Some(claims)
}

/// True when the claims cover the given request host and path.
pub(crate) fn share_claims_cover(claims: &ShareClaims, host: Option<&str>, uri_path: &str) -> bool {
  // A traversal segment in the request path can widen the granted scope
  // (`/public/../admin` starts with `/public/`), so never treat such a path as
  // covered — the request falls back to the normal login gate.
  if request_path_has_traversal(uri_path) {
    return false;
  }
  if host != Some(claims.host.as_str()) {
    return false;
  }
  match &claims.path {
    None => true,
    Some(p) => path_matches_bind(uri_path, p),
  }
}

/// Extracts a named cookie value from the Cookie header.
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
  let cookie_str = headers.get("cookie")?.to_str().ok()?;
  for part in cookie_str.split(';') {
    let kv: Vec<&str> = part.trim().splitn(2, '=').collect();
    if kv.len() == 2 && kv[0] == name {
      return Some(kv[1].to_string());
    }
  }
  None
}

/// Extracts a share token from the Cookie header.
fn share_token_from_cookies(headers: &HeaderMap) -> Option<String> {
  cookie_value(headers, SHARE_COOKIE)
}

/// Checks share-link access for a request without a dashboard session:
/// - `Some(None)` — a valid share cookie covers this request; proceed.
/// - `Some(Some(response))` — a valid token arrived via the `aperio_share`
///   query parameter (first click on a link): redirect to the clean URL,
///   setting the cookie for subsequent requests.
/// - `None` — no valid share credential.
pub(crate) fn check_share_access(
  state: &AppState,
  headers: &HeaderMap,
  uri: &Uri,
  host: Option<&str>,
) -> Option<Option<Response>> {
  let key = share_signing_key(&state.config().token);
  let uri_path = uri.path();

  // 1. Token in the query string: the first click on a generated link.
  if let Some(query) = uri.query() {
    let mut rest: Vec<&str> = Vec::new();
    let mut presented: Option<&str> = None;
    for pair in query.split('&') {
      match pair.strip_prefix("aperio_share=") {
        // The token alphabet is base64url plus '.', so no percent-decoding
        // is needed here.
        Some(v) => presented = Some(v),
        None => {
          if !pair.is_empty() {
            rest.push(pair);
          }
        }
      }
    }
    if let Some(raw) = presented
      && let Some(claims) = verify_share_token(raw, &key)
      && share_claims_cover(&claims, host, uri_path)
    {
      let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
      let clean_url = if rest.is_empty() {
        uri_path.to_string()
      } else {
        format!("{}?{}", uri_path, rest.join("&"))
      };
      let secure_flag = if state.config().secure_cookies {
        "; Secure"
      } else {
        ""
      };
      // Never-expiring links get a 10-year cookie.
      let max_age = claims
        .exp
        .map(|exp| exp.saturating_sub(now))
        .unwrap_or(SHARE_MAX_TTL_SECS);
      let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        SHARE_COOKIE, raw, max_age, secure_flag
      );
      info!(
        "Share link {} used for {} (path {})",
        claims.id, claims.host, uri_path
      );
      let resp = Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", clean_url)
        .header("Set-Cookie", cookie)
        .body(Body::empty())
        .unwrap();
      return Some(Some(resp));
    }
  }

  // 2. Share cookie from a previous visit.
  if let Some(token) = share_token_from_cookies(headers)
    && let Some(claims) = verify_share_token(&token, &key)
    && share_claims_cover(&claims, host, uri_path)
  {
    return Some(None);
  }
  None
}
