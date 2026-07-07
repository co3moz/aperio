use axum::{
  Json,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::oneshot;
use tracing::{error, info};

use crate::audit::{self};
use crate::stats::{self};
use crate::webhooks::{self};

use crate::auth::{constant_time_eq_str, valid_ip_entry};
use crate::protocol::{PROTOCOL_VERSION, TunnelMessage};
use crate::routing::{
  apply_lb_strategy, extract_client_ip, normalize_hostname_bind, normalize_path_bind,
  random_subdomain_hostname, select_client_pool,
};
use crate::settings::{
  SettingsOverrides, apply_settings_overrides, override_keys, parse_failover_mode,
  parse_lb_strategy, settings_view,
};
use crate::state::{
  AppState, ClientDetail, EnhancedServerStats, PendingRequest, RequestLog, TunnelResponse,
};

/// Dashboard frontend built from `aperio-dashboard/` (Vite + React) by
/// build.rs. In release builds the files are embedded into the binary; in
/// debug builds rust-embed reads them from disk so a rebuilt `dist/` is
/// picked up without recompiling.
#[derive(rust_embed::RustEmbed)]
#[folder = "../aperio-dashboard/dist"]
struct DashboardAssets;

/// Serves a file from the embedded dashboard build. Hashed assets are safe to
/// cache forever; HTML entry points must always be revalidated.
pub(crate) fn serve_embedded(path: &str, immutable: bool) -> Response {
  match DashboardAssets::get(path) {
    Some(file) => {
      let mime = mime_guess::from_path(path).first_or_octet_stream();
      let cache_control = if immutable {
        "public, max-age=31536000, immutable"
      } else {
        "no-cache"
      };
      (
        [
          (axum::http::header::CONTENT_TYPE, mime.as_ref()),
          (axum::http::header::CACHE_CONTROL, cache_control),
        ],
        file.data.into_owned(),
      )
        .into_response()
    }
    None => (StatusCode::NOT_FOUND, "Not found").into_response(),
  }
}

/// Handler serving the embedded dashboard SPA.
pub(crate) async fn dashboard_handler() -> Response {
  serve_embedded("index.html", false)
}

/// Serves the hashed static assets (JS/CSS) of the dashboard build. These are
/// public: the login page needs them before any session exists.
pub(crate) async fn dashboard_asset_handler(
  axum::extract::Path(path): axum::extract::Path<String>,
) -> Response {
  serve_embedded(&format!("assets/{path}"), true)
}

/// Handler returning live statistics and active connections detail in JSON.
pub(crate) async fn stats_handler(State(state): State<Arc<AppState>>) -> Json<EnhancedServerStats> {
  let raw_stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;

  let active_clients = clients
    .iter()
    .map(|(id, handle)| ClientDetail {
      id: id.clone(),
      ip: handle.client_ip.clone(),
      connected_for_seconds: handle.connected_at.elapsed().as_secs(),
      request_count: handle.request_count.load(Ordering::SeqCst),
      path_bind: handle
        .declared_path
        .clone()
        .or_else(|| handle.assigned_path.clone()),
      hostname_binds: {
        let mut set = handle.assigned_hostnames.clone();
        if let Some(d) = &handle.declared_hostname
          && !set.contains(d)
        {
          set.push(d.clone());
        }
        set
      },
      token_name: handle.perms.token_name.clone(),
      override_path_bind: handle.override_path_bind.clone(),
      override_hostname_bind: handle.override_hostname_bind.clone(),
      last_ping_seconds_ago: handle.last_ping_at.map(|t| t.elapsed().as_secs()),
      max_concurrent: handle.max_concurrent,
      version: handle.client_version.clone(),
      protocol: handle.client_protocol,
      protocol_mismatch: handle
        .client_protocol
        .is_some_and(|p| p != PROTOCOL_VERSION),
      backend_healthy: handle.backend_healthy,
      priority: handle.priority,
      bandwidth_bps: match handle.bandwidth_bps.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
      },
      healthy: handle.is_healthy(state.config().client_down_threshold),
      draining: handle.draining,
      enabled: handle.admin_enabled,
    })
    .collect();

  let pending_count = state.pending_requests.lock().await.len();
  let persistent = state.persistent_stats.lock().await.snapshot();
  let avg_response_ms = persistent.avg_response_ms();
  let today = persistent
    .periods
    .get(&stats::period_keys()[0])
    .cloned()
    .unwrap_or_default();

  Json(EnhancedServerStats {
    total_requests: raw_stats.total_requests,
    successful_requests: raw_stats.successful_requests,
    failed_requests: raw_stats.failed_requests,
    total_bytes_transferred: raw_stats.total_bytes_transferred,
    connected_clients_count: clients.len(),
    uptime_seconds: state.server_start_time.elapsed().as_secs(),
    pending_requests_count: pending_count,
    active_clients,
    persistent,
    avg_response_ms,
    today,
  })
}

/// Handler returning the list of recent HTTP logs in JSON.
pub(crate) async fn logs_handler(State(state): State<Arc<AppState>>) -> Json<Vec<RequestLog>> {
  let logs = state.recent_logs.lock().await;
  Json(logs.iter().cloned().collect())
}

/// Request payload for the dashboard client override (overrule) endpoint.
/// Each field fully replaces the corresponding override: a non-empty string
/// sets it, an empty string or `null` clears it. Overrides are in-memory only
/// and disappear when the client reconnects or the server restarts.
#[derive(Deserialize)]
pub(crate) struct ClientOverrideRequest {
  pub(crate) hostname_bind: Option<String>,
  pub(crate) path_bind: Option<String>,
}

/// Applies a temporary hostname/path bind override to a connected client.
/// Protected by the dashboard session middleware.
pub(crate) async fn client_override_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientOverrideRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();
  // Validate before mutating: reject invalid values with 400.
  let new_hostname = match payload.hostname_bind.as_deref() {
    None | Some("") => None,
    Some(raw) => match normalize_hostname_bind(raw) {
      Some(h) => Some(h),
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid hostname_bind value").into_response();
      }
    },
  };
  let new_path = match payload.path_bind.as_deref() {
    None | Some("") => None,
    Some(raw) => match normalize_path_bind(raw) {
      Some(p) => Some(p),
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid path_bind value").into_response();
      }
    },
  };

  let found = {
    let mut clients = state.clients.lock().await;
    match clients.get_mut(&client_id) {
      Some(handle) => {
        handle.override_hostname_bind = new_hostname.clone();
        handle.override_path_bind = new_path.clone();
        true
      }
      None => false,
    }
  };
  if found {
    info!(
      "Dashboard overrule applied to client {}: hostname_bind={:?} path_bind={:?}",
      client_id, new_hostname, new_path
    );
    state
      .audit(
        "client_overrule",
        &actor_ip,
        &format!(
          "client={} hostname={:?} path={:?}",
          client_id, new_hostname, new_path
        ),
      )
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

/// Returns recent audit events (dashboard).
pub(crate) async fn audit_handler(
  State(state): State<Arc<AppState>>,
) -> Json<Vec<audit::AuditEvent>> {
  Json(state.audit.lock().await.recent())
}

/// Payload for creating a webhook definition.
#[derive(Deserialize)]
pub(crate) struct WebhookCreateRequest {
  pub(crate) name: String,
  pub(crate) url: String,
  /// Subscribed events; `["*"]` (or empty) = all events.
  #[serde(default)]
  pub(crate) events: Vec<String>,
}

/// Lists webhook definitions.
pub(crate) async fn webhooks_list_handler(
  State(state): State<Arc<AppState>>,
) -> Json<Vec<webhooks::Webhook>> {
  Json(state.webhook_store.lock().await.list().to_vec())
}

/// Creates a webhook definition. Only http/https URLs are accepted.
pub(crate) async fn webhooks_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<WebhookCreateRequest>,
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
      "Webhook name must be 1-64 characters",
    )
      .into_response();
  }
  let url = payload.url.trim().to_string();
  if !(url.starts_with("http://") || url.starts_with("https://")) {
    return (StatusCode::BAD_REQUEST, "Webhook URL must be http(s)").into_response();
  }
  let events: Vec<String> = payload
    .events
    .iter()
    .map(|e| e.trim().to_string())
    .filter(|e| !e.is_empty())
    .collect();

  let hook = state.webhook_store.lock().await.create(name, url, events);
  info!("Webhook created: {} -> {}", hook.name, hook.url);
  state
    .audit(
      "webhook_created",
      &actor_ip,
      &format!(
        "name={} url={} events={:?}",
        hook.name, hook.url, hook.events
      ),
    )
    .await;
  Json(serde_json::json!({"status": "ok", "id": hook.id})).into_response()
}

/// Deletes a webhook definition.
pub(crate) async fn webhooks_delete_handler(
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
  if state.webhook_store.lock().await.delete(&id) {
    state
      .audit("webhook_deleted", &actor_ip, &format!("id={}", id))
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Webhook not found").into_response()
  }
}

/// Payload for the client enable/disable toggle.
#[derive(Deserialize)]
pub(crate) struct ClientEnabledRequest {
  pub(crate) enabled: bool,
}

/// Dashboard kill switch: temporarily removes a connected client from the
/// routing pool (or puts it back). In-flight requests always complete.
pub(crate) async fn client_enabled_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientEnabledRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();
  let found = {
    let mut clients = state.clients.lock().await;
    match clients.get_mut(&client_id) {
      Some(handle) => {
        handle.admin_enabled = payload.enabled;
        true
      }
      None => false,
    }
  };
  if found {
    info!(
      "Client {} {} via dashboard",
      client_id,
      if payload.enabled {
        "enabled"
      } else {
        "disabled"
      }
    );
    state
      .audit(
        if payload.enabled {
          "client_enabled"
        } else {
          "client_disabled"
        },
        &actor_ip,
        &format!("client={}", client_id),
      )
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

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

  let (record, secret) = {
    let mut store = state.token_store.lock().await;
    store.create(name, hostnames, paths, allowed_ips, payload.ttl_seconds)
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

  let updated = state.token_store.lock().await.update(
    &id,
    payload.name.map(|n| n.trim().to_string()),
    payload.hostnames.map(|_| hostnames),
    payload.paths.map(|_| paths),
    payload.allowed_ips.map(|_| allowed_ips),
    ttl,
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

/// Returns the dashboard-editable settings: effective values, environment
/// defaults, and the persisted overrides.
pub(crate) async fn settings_get_handler(
  State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
  let overrides = state.settings_overrides.lock().await.clone();
  Json(serde_json::json!({
    "effective": settings_view(&state.config()),
    "defaults": settings_view(&state.config_env_defaults),
    "overrides": overrides,
  }))
}

/// Replaces the settings overrides. The body is the full overrides object:
/// missing/null fields fall back to the environment default. Changes apply
/// live (config swap), persist to `<data_dir>/settings.json`, and are
/// audited with the list of overridden keys.
pub(crate) async fn settings_put_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<SettingsOverrides>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();

  // Reject values that apply_settings_overrides would silently skip.
  if let Some(ref s) = payload.lb_strategy
    && parse_lb_strategy(s).is_none()
  {
    return (
      StatusCode::BAD_REQUEST,
      format!("Invalid lb_strategy: {}", s),
    )
      .into_response();
  }
  if let Some(ref s) = payload.failover_mode
    && parse_failover_mode(s).is_none()
  {
    return (
      StatusCode::BAD_REQUEST,
      format!("Invalid failover_mode: {}", s),
    )
      .into_response();
  }
  if let Some(ref creds) = payload.auth_credentials
    && !creds.is_empty()
    && !creds.contains(':')
  {
    return (
      StatusCode::BAD_REQUEST,
      "auth_credentials must be in user:password form",
    )
      .into_response();
  }
  for (label, page) in [
    ("custom_504_page", &payload.custom_504_page),
    ("custom_503_page", &payload.custom_503_page),
  ] {
    if let Some(html) = page
      && html.len() > 512 * 1024
    {
      return (StatusCode::BAD_REQUEST, format!("{} exceeds 512 KB", label)).into_response();
    }
  }

  let old_config = state.config();
  let effective = apply_settings_overrides(&state.config_env_defaults, &payload);
  *state.config_store.write().expect("config lock poisoned") = Arc::new(effective);
  *state.settings_overrides.lock().await = payload.clone();
  let new_config = state.config();

  // Settings that involve connected clients are pushed out immediately
  // instead of waiting for reconnects.
  if old_config.random_subdomain_suffix != new_config.random_subdomain_suffix {
    reassign_random_hostnames(&state).await;
  }
  if !old_config.tunnel_compression && new_config.tunnel_compression {
    offer_compression_to_connected(&state).await;
  }

  match serde_json::to_string_pretty(&payload) {
    Ok(json) => {
      if let Err(e) = std::fs::write(&state.settings_path, json) {
        error!(
          "Failed to persist settings to {:?}: {}",
          state.settings_path, e
        );
      }
    }
    Err(e) => error!("Failed to serialize settings: {}", e),
  }

  let keys = override_keys(&payload);
  info!(
    "Settings updated from the dashboard (overridden: {:?})",
    keys
  );
  state
    .audit("settings_updated", &actor_ip, &keys.join(","))
    .await;
  state
    .emit_event("settings_updated", serde_json::json!({"overridden": keys}))
    .await;

  (
    StatusCode::OK,
    Json(serde_json::json!({"effective": settings_view(&state.config())})),
  )
    .into_response()
}

/// Re-issues random hostnames after the subdomain pattern changed at
/// runtime: every connected client loses its old random hostname (it points
/// at the retired pattern) and, when a pattern is still configured, receives
/// a fresh assignment pushed immediately via `HostnameAssigned`.
async fn reassign_random_hostnames(state: &Arc<AppState>) {
  let pattern = state.config().random_subdomain_suffix.clone();
  let mut notifications = Vec::new();
  {
    let mut clients = state.clients.lock().await;
    for (id, c) in clients.iter_mut() {
      if let Some(ref old) = c.random_hostname {
        c.assigned_hostnames.retain(|h| h != old);
      }
      c.random_hostname = pattern.as_deref().map(random_subdomain_hostname);
      if let Some(ref h) = c.random_hostname {
        c.assigned_hostnames.push(h.clone());
        info!("Reassigned random hostname {} to client {}", h, id);
        notifications.push((c.tx.clone(), h.clone()));
      }
    }
  }
  // Send outside the clients lock so a slow client cannot stall the map.
  for (tx, hostname) in notifications {
    if let Ok(json) = serde_json::to_string(&TunnelMessage::HostnameAssigned { hostname }) {
      let _ = tx.send(Message::Text(json)).await;
    }
  }
}

/// Offers zlib tunnel compression to every already-connected client after
/// the setting was enabled at runtime (each client switches on ack). There
/// is no protocol message to stop compression, so disabling only affects
/// new connections.
async fn offer_compression_to_connected(state: &Arc<AppState>) {
  let txs: Vec<_> = {
    let clients = state.clients.lock().await;
    clients.values().map(|c| c.tx.clone()).collect()
  };
  if txs.is_empty() {
    return;
  }
  info!(
    "Tunnel compression enabled at runtime; offering it to {} connected client(s)",
    txs.len()
  );
  for tx in txs {
    if let Ok(json) = serde_json::to_string(&TunnelMessage::CompressionStart {}) {
      let _ = tx.send(Message::Text(json)).await;
    }
  }
}

/// Payload for toggling maintenance mode on a hostname (dashboard).
#[derive(Deserialize)]
pub(crate) struct MaintenanceRequest {
  /// Hostname to toggle, or `*` for every hostname.
  pub(crate) hostname: String,
  pub(crate) enabled: bool,
}

/// Lists hostnames currently in maintenance mode.
pub(crate) async fn maintenance_list_handler(
  State(state): State<Arc<AppState>>,
) -> Json<Vec<String>> {
  let set = state.maintenance.lock().await;
  let mut list: Vec<String> = set.iter().cloned().collect();
  list.sort();
  Json(list)
}

/// Enables/disables maintenance mode for a hostname. In-memory only, like
/// bind overrides: a server restart clears all maintenance flags.
pub(crate) async fn maintenance_set_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<MaintenanceRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  )
  .to_string();
  let raw = payload.hostname.trim();
  let hostname = if raw == "*" {
    "*".to_string()
  } else {
    match normalize_hostname_bind(raw) {
      Some(h) => h,
      None => {
        return (
          StatusCode::BAD_REQUEST,
          format!("Invalid hostname: {}", raw),
        )
          .into_response();
      }
    }
  };

  let changed = {
    let mut set = state.maintenance.lock().await;
    if payload.enabled {
      set.insert(hostname.clone())
    } else {
      set.remove(&hostname)
    }
  };
  if changed {
    let event = if payload.enabled {
      "maintenance_on"
    } else {
      "maintenance_off"
    };
    info!(
      "Maintenance mode {} for {}",
      if payload.enabled {
        "enabled"
      } else {
        "disabled"
      },
      hostname
    );
    state
      .audit(event, &actor_ip, &format!("hostname={}", hostname))
      .await;
    state
      .emit_event(event, serde_json::json!({"hostname": hostname}))
      .await;
  }
  (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// Returns the full captured detail of a recent request (dashboard inspector).
pub(crate) async fn request_detail_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
  let captured = state.captured_requests.lock().await;
  match captured.iter().find(|c| c.id == id) {
    Some(entry) => Json(entry.clone()).into_response(),
    None => (
      StatusCode::NOT_FOUND,
      "Request not captured (only recent proxied requests are kept)",
    )
      .into_response(),
  }
}

/// Replays a captured request through the tunnel and returns the new outcome.
pub(crate) async fn request_replay_handler(
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
  let captured = {
    let store = state.captured_requests.lock().await;
    store.iter().find(|c| c.id == id).cloned()
  };
  let captured = match captured {
    Some(c) => c,
    None => return (StatusCode::NOT_FOUND, "Request not captured").into_response(),
  };
  if captured.req_body_truncated {
    return (
      StatusCode::BAD_REQUEST,
      "Request body was truncated at capture time; replay would be incomplete",
    )
      .into_response();
  }

  // Select a tunnel client with the same routing rules as live traffic.
  let uri_path = captured.uri.split('?').next().unwrap_or(&captured.uri);
  let request_host = captured
    .req_headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case("host"))
    .and_then(|(_, v)| {
      let lower = v.trim().to_ascii_lowercase();
      lower.split(':').next().map(|s| s.to_string())
    });
  let client_info = {
    let clients = state.clients.lock().await;
    match select_client_pool(
      &clients,
      uri_path,
      request_host.as_deref(),
      state.config().require_hostname_bind,
      state.config().client_down_threshold,
    ) {
      None => None,
      Some((pool, group_key)) => {
        let pool = apply_lb_strategy(pool, &clients, state.config().lb_strategy);
        let mut rr_map = state.path_rr.lock().await;
        let idx = rr_map.entry(group_key).or_insert(0);
        let chosen_id = &pool[*idx % pool.len()];
        *idx = (*idx + 1) % pool.len();
        clients
          .get(chosen_id)
          .map(|c| (chosen_id.clone(), c.tx.clone(), c.request_count.clone()))
      }
    }
  };
  let (chosen_client_id, client_tx, client_req_counter) = match client_info {
    Some(info) => info,
    None => {
      return (
        StatusCode::GATEWAY_TIMEOUT,
        "No tunnel client available for replay",
      )
        .into_response();
    }
  };

  let replay_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    replay_id.clone(),
    PendingRequest {
      tx: tx_response,
      client_id: chosen_client_id,
    },
  );

  let tunnel_req = TunnelMessage::Request {
    id: replay_id.clone(),
    method: captured.method.clone(),
    uri: captured.uri.clone(),
    headers: captured.req_headers.clone(),
    body: captured.req_body.clone(),
  };
  let req_json = match serde_json::to_string(&tunnel_req) {
    Ok(json) => json,
    Err(_) => {
      state.pending_requests.lock().await.remove(&replay_id);
      return (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response();
    }
  };
  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_requests.lock().await.remove(&replay_id);
    return (StatusCode::BAD_GATEWAY, "Tunnel client socket error").into_response();
  }
  client_req_counter.fetch_add(1, Ordering::SeqCst);
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
  }

  let start = Instant::now();
  let result = tokio::time::timeout(state.config().gateway_response_timeout, rx_response).await;
  state.pending_requests.lock().await.remove(&replay_id);

  match result {
    Ok(Ok(tunnel_res)) => {
      // Streamed replay bodies are discarded: dropping stream_rx makes the
      // tunnel read loop clean the stream up on the next chunk.
      {
        let mut stats = state.stats.lock().await;
        if tunnel_res.status >= 500 {
          stats.failed_requests += 1;
        } else {
          stats.successful_requests += 1;
        }
      }
      info!(
        "Replayed request {} → {} ({} ms)",
        id,
        tunnel_res.status,
        start.elapsed().as_millis()
      );
      state
        .audit(
          "request_replayed",
          &actor_ip,
          &format!(
            "id={} {} {} -> {}",
            id, captured.method, captured.uri, tunnel_res.status
          ),
        )
        .await;
      Json(serde_json::json!({
        "replayed_id": id,
        "status": tunnel_res.status,
        "duration_ms": start.elapsed().as_millis() as u64,
      }))
      .into_response()
    }
    Ok(Err(_)) => (
      StatusCode::BAD_GATEWAY,
      "Client connection lost during replay",
    )
      .into_response(),
    Err(_) => (StatusCode::GATEWAY_TIMEOUT, "Replay response timeout").into_response(),
  }
}

/// Prometheus text-format metrics endpoint (`/aperio/metrics`).
/// Enabled with `APERIO_METRICS=1`. Requires a token, presented either as
/// `?token=<value>` (convenient for Prometheus scrape configs) or as an
/// `Authorization: Bearer <value>` header.
pub(crate) async fn metrics_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  headers: HeaderMap,
) -> Response {
  if let Some(ref token) = state.config().metrics_token {
    let bearer_ok = headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.strip_prefix("Bearer "))
      .is_some_and(|t| constant_time_eq_str(t, token));
    let query_ok = query
      .get("token")
      .is_some_and(|t| constant_time_eq_str(t, token));
    if !bearer_ok && !query_ok {
      return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
  }

  let stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;
  let connected = clients.len();
  let per_client: Vec<(String, u64)> = clients
    .iter()
    .map(|(id, c)| (id.clone(), c.request_count.load(Ordering::SeqCst)))
    .collect();
  drop(clients);
  let pending = state.pending_requests.lock().await.len();
  let ws_streams = state.ws_streams.lock().await.len();
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut out = String::with_capacity(1024);
  out.push_str("# HELP aperio_requests_total Total proxied requests received.\n");
  out.push_str("# TYPE aperio_requests_total counter\n");
  out.push_str(&format!("aperio_requests_total {}\n", stats.total_requests));
  out.push_str("# HELP aperio_requests_success_total Successfully proxied requests.\n");
  out.push_str("# TYPE aperio_requests_success_total counter\n");
  out.push_str(&format!(
    "aperio_requests_success_total {}\n",
    stats.successful_requests
  ));
  out.push_str(
    "# HELP aperio_requests_failed_total Failed proxied requests (5xx / gateway errors).\n",
  );
  out.push_str("# TYPE aperio_requests_failed_total counter\n");
  out.push_str(&format!(
    "aperio_requests_failed_total {}\n",
    stats.failed_requests
  ));
  out.push_str("# HELP aperio_bytes_transferred_total Total payload bytes transferred.\n");
  out.push_str("# TYPE aperio_bytes_transferred_total counter\n");
  out.push_str(&format!(
    "aperio_bytes_transferred_total {}\n",
    stats.total_bytes_transferred
  ));
  out.push_str("# HELP aperio_connected_clients Currently connected tunnel clients.\n");
  out.push_str("# TYPE aperio_connected_clients gauge\n");
  out.push_str(&format!("aperio_connected_clients {}\n", connected));
  out.push_str("# HELP aperio_pending_requests Requests currently awaiting a client response.\n");
  out.push_str("# TYPE aperio_pending_requests gauge\n");
  out.push_str(&format!("aperio_pending_requests {}\n", pending));
  out.push_str("# HELP aperio_ws_streams_active Active proxied WebSocket streams.\n");
  out.push_str("# TYPE aperio_ws_streams_active gauge\n");
  out.push_str(&format!("aperio_ws_streams_active {}\n", ws_streams));
  out.push_str("# HELP aperio_uptime_seconds Server uptime in seconds.\n");
  out.push_str("# TYPE aperio_uptime_seconds gauge\n");
  out.push_str(&format!("aperio_uptime_seconds {}\n", uptime));
  state.duration_histogram.render(&mut out);
  out.push_str(
    "# HELP aperio_client_requests_total Requests handled per connected tunnel client.\n",
  );
  out.push_str("# TYPE aperio_client_requests_total counter\n");
  for (id, count) in per_client {
    out.push_str(&format!(
      "aperio_client_requests_total{{client_id=\"{}\"}} {}\n",
      id, count
    ));
  }

  (
    StatusCode::OK,
    [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
    out,
  )
    .into_response()
}

/// Health check endpoint returning status, active connection counts, and uptime.
pub(crate) async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
  let clients_count = state.clients.lock().await.len();
  let stats = state.stats.lock().await;
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut health_info = HashMap::new();
  health_info.insert("status", serde_json::json!("healthy"));
  health_info.insert("version", serde_json::json!(env!("CARGO_PKG_VERSION")));
  health_info.insert("protocol", serde_json::json!(PROTOCOL_VERSION));
  health_info.insert("connected_clients", serde_json::json!(clients_count));
  health_info.insert("uptime_seconds", serde_json::json!(uptime));
  health_info.insert("total_requests", serde_json::json!(stats.total_requests));

  (StatusCode::OK, Json(health_info))
}
