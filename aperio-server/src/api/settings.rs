use axum::{
  Json,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};

use crate::protocol::TunnelMessage;
use crate::routing::{extract_client_ip, random_subdomain_hostname};
use crate::settings::{
  SettingsOverrides, apply_settings_overrides, override_keys, parse_failover_mode,
  parse_lb_strategy, settings_view,
};
use crate::state::AppState;

/// Returns the dashboard-editable settings: effective values, environment
/// defaults, and the persisted overrides.
#[utoipa::path(get, path = "/aperio/api/settings", tag = "dashboard",
  description = "Effective server settings plus which keys are overridden from the dashboard.",
  responses((status = 200, description = "Current settings", body = serde_json::Value)))]
pub(crate) async fn settings_get_handler(
  State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
  let overrides = state.settings_overrides.lock().await.clone();
  Json(serde_json::json!({
    "effective": settings_view(&state.config()),
    "defaults": settings_view(&state.config_env_defaults),
    "overrides": overrides,
    "environment": environment_report(&state),
  }))
}

/// True when the server appears to run inside a container, so the dashboard
/// can show the right way to change env-only flags.
fn running_in_container() -> bool {
  if std::path::Path::new("/.dockerenv").exists() {
    return true;
  }
  std::fs::read_to_string("/proc/1/cgroup")
    .map(|c| c.contains("docker") || c.contains("containerd") || c.contains("kubepods"))
    .unwrap_or(false)
}

/// Read-only report of env-only flags for the dashboard reference table.
/// Secrets are never included — only whether they are set.
fn environment_report(state: &Arc<AppState>) -> serde_json::Value {
  let c = state.config();
  let env_or = |key: &str, fallback: &str| std::env::var(key).unwrap_or_else(|_| fallback.into());
  let flags = serde_json::json!([
    { "key": "APERIO_TRUST_PROXY", "value": if c.trust_proxy { "on" } else { "off" } },
    { "key": "APERIO_TRUSTED_PROXIES",
      "value": if c.trusted_proxies.is_empty() { "(unset — first XFF entry is trusted)".to_string() }
               else { env_or("APERIO_TRUSTED_PROXIES", "") } },
    { "key": "APERIO_TRUST_CF_HEADER", "value": env_or("APERIO_TRUST_CF_HEADER", "off") },
    { "key": "APERIO_REAL_IP_HEADER",
      "value": c.real_ip_header.clone().unwrap_or_else(|| "(unset)".into()) },
    { "key": "APERIO_SECURE_COOKIES", "value": if c.secure_cookies { "on" } else { "off" } },
    { "key": "APERIO_IGNORE_CLIENT_AUTH", "value": if c.ignore_client_auth { "on" } else { "off" } },
    { "key": "APERIO_OIDC_*",
      "value": match state.oidc {
        Some(_) => format!("configured ({})", env_or("APERIO_OIDC_ISSUER", "?")),
        None => "not configured".to_string(),
      } },
    { "key": "APERIO_METRICS", "value": env_or("APERIO_METRICS", "off") },
    { "key": "APERIO_METRICS_TOKEN",
      "value": if c.metrics_token.is_some() { "set (value hidden)" } else { "not set" } },
    { "key": "APERIO_ACCESS_LOG", "value": env_or("APERIO_ACCESS_LOG", "(disabled)") },
  ]);
  serde_json::json!({
    "runtime": if running_in_container() { "docker" } else { "native" },
    "flags": flags,
  })
}

/// Replaces the settings overrides. The body is the full overrides object:
/// missing/null fields fall back to the environment default. Changes apply
/// live (config swap), persist to `<data_dir>/settings.json`, and are
/// audited with the list of overridden keys.
#[utoipa::path(put, path = "/aperio/api/settings", tag = "dashboard",
  description = "Applies dashboard settings overrides live and persists them (missing keys keep env defaults).",
  request_body = SettingsOverrides,
  responses((status = 200, description = "Settings applied", body = serde_json::Value), (status = 400, description = "Invalid value")))]
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
    &state.config().trusted_proxies,
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
  if let Some(ref lang) = payload.ui_language
    && !crate::settings::UI_LANGUAGES.contains(&lang.as_str())
  {
    return (
      StatusCode::BAD_REQUEST,
      format!(
        "Unsupported ui_language: {} (supported: {})",
        lang,
        crate::settings::UI_LANGUAGES.join(", ")
      ),
    )
      .into_response();
  }
  if payload.cache_max_bytes == Some(0) {
    return (
      StatusCode::BAD_REQUEST,
      "cache_max_bytes must be positive (disable the cache with cache_enabled instead)",
    )
      .into_response();
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
  // Settings backed by live structures (not just config reads) are pushed
  // into them here.
  if old_config.cache_enabled && !new_config.cache_enabled {
    state.response_cache.lock().await.clear();
  }
  if old_config.login_lockout_threshold != new_config.login_lockout_threshold
    || old_config.login_lockout_secs != new_config.login_lockout_secs
  {
    state.login_lockout.lock().await.set_policy(
      new_config.login_lockout_threshold,
      std::time::Duration::from_secs(new_config.login_lockout_secs),
    );
  }
  if old_config.audit_max_size != new_config.audit_max_size
    || old_config.audit_max_files != new_config.audit_max_files
  {
    state
      .audit
      .lock()
      .await
      .set_rotation(new_config.audit_max_size, new_config.audit_max_files);
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
