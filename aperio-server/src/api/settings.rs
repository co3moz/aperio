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
