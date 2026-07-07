use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::auth::valid_ip_entry;
use crate::routing::{extract_client_ip, normalize_hostname_bind, normalize_path_bind};
use crate::state::AppState;

/// Public view of a dynamic token record (never includes hash or secret).
#[derive(Serialize)]
pub(crate) struct TokenView {
  pub(crate) id: String,
  pub(crate) name: String,
  pub(crate) token_prefix: String,
  pub(crate) hostnames: Vec<String>,
  pub(crate) paths: Vec<String>,
  pub(crate) allowed_ips: Vec<String>,
  pub(crate) created_at: u64,
  pub(crate) expires_at: Option<u64>,
  pub(crate) expired: bool,
  pub(crate) max_rps: Option<f64>,
  pub(crate) daily_max_bytes: Option<u64>,
}

/// Lists dynamic API tokens (metadata only, secrets are never returned).
pub(crate) async fn tokens_list_handler(
  State(state): State<Arc<AppState>>,
) -> Json<Vec<TokenView>> {
  let store = state.token_store.lock().await;
  let views = store
    .list()
    .iter()
    .map(|t| TokenView {
      id: t.id.clone(),
      name: t.name.clone(),
      token_prefix: t.token_prefix.clone(),
      hostnames: t.hostnames.clone(),
      paths: t.paths.clone(),
      allowed_ips: t.allowed_ips.clone(),
      created_at: t.created_at,
      expires_at: t.expires_at,
      expired: t.is_expired(),
      max_rps: t.max_rps,
      daily_max_bytes: t.daily_max_bytes,
    })
    .collect();
  Json(views)
}

/// Payload for creating a dynamic token from the dashboard.
#[derive(Deserialize)]
pub(crate) struct TokenCreateRequest {
  pub(crate) name: String,
  /// Allowed hostnames; `["*"]` (or empty) = all hostnames.
  #[serde(default)]
  pub(crate) hostnames: Vec<String>,
  /// Allowed path binds; `["*"]` (or empty) = all paths.
  #[serde(default)]
  pub(crate) paths: Vec<String>,
  /// Source IPs/CIDRs allowed to connect. Defaults to `["0.0.0.0/0"]` (any).
  #[serde(default)]
  pub(crate) allowed_ips: Vec<String>,
  /// Optional lifetime in seconds; omitted = never expires.
  pub(crate) ttl_seconds: Option<u64>,
  /// Optional request rate limit (requests/second) for proxied traffic
  /// served through this token.
  pub(crate) max_rps: Option<f64>,
  /// Optional daily byte quota (request + response payload).
  pub(crate) daily_max_bytes: Option<u64>,
}

/// Payload for editing an existing token's scope without changing the secret.
/// Absent fields are left untouched; `ttl_seconds: 0` clears the expiry.
#[derive(Deserialize)]
pub(crate) struct TokenUpdateRequest {
  pub(crate) name: Option<String>,
  pub(crate) hostnames: Option<Vec<String>>,
  pub(crate) paths: Option<Vec<String>>,
  pub(crate) allowed_ips: Option<Vec<String>>,
  /// Some(0) = never expires; Some(n) = expires n seconds from now.
  pub(crate) ttl_seconds: Option<u64>,
  /// Some(0.0) clears the rate limit; Some(n) sets it to n req/s.
  pub(crate) max_rps: Option<f64>,
  /// Some(0) clears the quota; Some(n) sets it to n bytes/day.
  pub(crate) daily_max_bytes: Option<u64>,
}

/// Normalized (hostnames, paths, allowed_ips) permission lists.
type TokenPermLists = (Vec<String>, Vec<String>, Vec<String>);

/// Validates and normalizes token permission lists. Returns an error message
/// when an entry is invalid.
pub(crate) fn validate_token_perms(
  hostnames: &[String],
  paths: &[String],
  allowed_ips: &[String],
) -> Result<TokenPermLists, String> {
  let mut out_hosts = Vec::new();
  for h in hostnames {
    let trimmed = h.trim();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed == "*" {
      out_hosts.push("*".to_string());
      continue;
    }
    match normalize_hostname_bind(trimmed) {
      Some(valid) => out_hosts.push(valid),
      None => return Err(format!("Invalid hostname permission: {}", trimmed)),
    }
  }
  let mut out_paths = Vec::new();
  for p in paths {
    let trimmed = p.trim();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed == "*" {
      out_paths.push("*".to_string());
      continue;
    }
    match normalize_path_bind(trimmed) {
      Some(valid) => out_paths.push(valid),
      None => return Err(format!("Invalid path permission: {}", trimmed)),
    }
  }
  let mut out_ips = Vec::new();
  for entry in allowed_ips {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
      continue;
    }
    if !valid_ip_entry(trimmed) {
      return Err(format!("Invalid IP/CIDR entry: {}", trimmed));
    }
    out_ips.push(trimmed.to_string());
  }
  if out_ips.is_empty() {
    out_ips.push("0.0.0.0/0".to_string());
  }
  Ok((out_hosts, out_paths, out_ips))
}

/// Creates a dynamic token. The plaintext secret is returned exactly once.
pub(crate) async fn tokens_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenCreateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();
  let name = payload.name.trim().to_string();
  if name.is_empty() || name.len() > 64 {
    return (
      StatusCode::BAD_REQUEST,
      "Token name must be 1-64 characters",
    )
      .into_response();
  }

  let (hostnames, paths, allowed_ips) =
    match validate_token_perms(&payload.hostnames, &payload.paths, &payload.allowed_ips) {
      Ok(v) => v,
      Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

  if payload.max_rps.is_some_and(|v| !v.is_finite() || v < 0.0) {
    return (StatusCode::BAD_REQUEST, "max_rps must be a positive number").into_response();
  }

  let (record, secret) = {
    let mut store = state.token_store.lock().await;
    store.create(
      name,
      hostnames,
      paths,
      allowed_ips,
      payload.ttl_seconds,
      payload.max_rps.filter(|v| *v > 0.0),
      payload.daily_max_bytes.filter(|v| *v > 0),
    )
  };
  info!(
    "Dynamic token created: {} (id={}, hostnames={:?}, paths={:?}, ips={:?}, expires_at={:?})",
    record.name, record.id, record.hostnames, record.paths, record.allowed_ips, record.expires_at
  );
  state
    .audit(
      "token_created",
      &actor_ip,
      &format!(
        "name={} id={} hostnames={:?} paths={:?} ips={:?} expires_at={:?}",
        record.name,
        record.id,
        record.hostnames,
        record.paths,
        record.allowed_ips,
        record.expires_at
      ),
    )
    .await;
  state
    .emit_event(
      "token_created",
      serde_json::json!({"id": record.id, "name": record.name}),
    )
    .await;
  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": record.id,
      "name": record.name,
      "token": secret,
      "hostnames": record.hostnames,
      "paths": record.paths,
      "allowed_ips": record.allowed_ips,
      "expires_at": record.expires_at,
    })),
  )
    .into_response()
}

/// Edits an existing token's scope (name, hostnames, paths, allowed IPs,
/// expiry) without changing the secret. Live connections are unaffected.
pub(crate) async fn tokens_update_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenUpdateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();

  if let Some(ref n) = payload.name {
    let n = n.trim();
    if n.is_empty() || n.len() > 64 {
      return (
        StatusCode::BAD_REQUEST,
        "Token name must be 1-64 characters",
      )
        .into_response();
    }
  }
  let (hostnames, paths, allowed_ips) = match validate_token_perms(
    payload.hostnames.as_deref().unwrap_or(&[]),
    payload.paths.as_deref().unwrap_or(&[]),
    payload.allowed_ips.as_deref().unwrap_or(&[]),
  ) {
    Ok(v) => v,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };

  // ttl_seconds: absent = keep; 0 = never expires; n = now + n.
  let ttl = payload
    .ttl_seconds
    .map(|n| if n == 0 { None } else { Some(n) });

  if payload.max_rps.is_some_and(|v| !v.is_finite() || v < 0.0) {
    return (StatusCode::BAD_REQUEST, "max_rps must be a positive number").into_response();
  }

  let updated = state.token_store.lock().await.update(
    &id,
    payload.name.map(|n| n.trim().to_string()),
    payload.hostnames.map(|_| hostnames),
    payload.paths.map(|_| paths),
    payload.allowed_ips.map(|_| allowed_ips),
    ttl,
    // max_rps / daily_max_bytes: absent = keep; 0 = clear; n = set.
    payload.max_rps.map(Some),
    payload.daily_max_bytes.map(Some),
  );

  match updated {
    Some(record) => {
      info!(
        "Dynamic token updated: {} (id={}, hostnames={:?}, paths={:?}, ips={:?}, expires_at={:?})",
        record.name,
        record.id,
        record.hostnames,
        record.paths,
        record.allowed_ips,
        record.expires_at
      );
      state
        .audit(
          "token_updated",
          &actor_ip,
          &format!(
            "name={} id={} hostnames={:?} paths={:?} ips={:?} expires_at={:?}",
            record.name,
            record.id,
            record.hostnames,
            record.paths,
            record.allowed_ips,
            record.expires_at
          ),
        )
        .await;
      Json(serde_json::json!({"status": "ok"})).into_response()
    }
    None => (StatusCode::NOT_FOUND, "Token not found").into_response(),
  }
}

/// Revokes (deletes) a dynamic token. Existing tunnel connections that used
/// the token stay connected; only new connections are rejected.
pub(crate) async fn tokens_revoke_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();
  let revoked = state.token_store.lock().await.revoke(&id);
  if revoked {
    info!("Dynamic token revoked: {}", id);
    state
      .audit("token_revoked", &actor_ip, &format!("id={}", id))
      .await;
    state
      .emit_event("token_revoked", serde_json::json!({"id": id}))
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Token not found").into_response()
  }
}
