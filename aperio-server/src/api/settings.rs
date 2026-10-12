//! The server's settings over the API: read the document, replace it.
//!
//! The document is the whole configuration that is not needed before the
//! database opens (see `aperio_config::server_settings`). A change is built
//! into a config first, so a value the server cannot honour is refused with
//! the setting's path before anything moves; then it is stored, put in
//! force, and audited with the paths it changed.

use axum::{
  Json,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use aperio_config::server_settings::{
  RESTART_PATHS, SECRET_PATHS, SECURITY_PATHS, ServerSettings, path_in, unmask,
};

use crate::protocol::TunnelMessage;
use crate::routing::{extract_client_ip, random_subdomain_hostname};
use crate::settings::ServerConfig;
use crate::state::AppState;

/// How recent a dashboard sign-in must be to change a security-relevant
/// setting. An admin key is a credential somebody minted for the purpose,
/// and is not asked.
pub(crate) const STEP_UP_SECS: u64 = 10 * 60;

/// Returns the settings document in force, every field present and every
/// credential masked, plus the paths that are security-relevant, that hold a
/// credential, that need a restart, and that were changed and are waiting
/// for one.
#[utoipa::path(get, path = "/aperio/api/settings", tag = "dashboard",
  description = "The server's settings document, with which paths are security-relevant and which need a restart.",
  responses((status = 200, description = "Current settings", body = serde_json::Value)))]
pub(crate) async fn settings_get_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  // Server settings are global; only an Admin of master may read them.
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let current = crate::live::current();
  Json(serde_json::json!({
    "settings": current.masked(),
    "security_paths": SECURITY_PATHS,
    "secret_paths": SECRET_PATHS,
    "restart_paths": RESTART_PATHS,
    "restart_pending": restart_pending(&state, &current),
  }))
  .into_response()
}

/// The restart-only paths whose stored value differs from what the process
/// started with.
fn restart_pending(state: &AppState, current: &ServerSettings) -> Vec<String> {
  state
    .started_with
    .changed_paths(current)
    .into_iter()
    .filter(|p| path_in(p, RESTART_PATHS))
    .collect()
}

/// Replaces the settings document. The body is the whole document; whatever
/// it leaves out takes its default, and a credential still reading as the
/// mask keeps its stored value. A change to a security-relevant path
/// takes Admin in every organization and, for a dashboard session, a
/// sign-in from the last ten minutes.
#[utoipa::path(put, path = "/aperio/api/settings", tag = "dashboard",
  description = "Replaces the server's settings document; omitted fields take their defaults. Security-relevant paths take `*` Admin and a recent sign-in.",
  request_body = serde_json::Value,
  responses((status = 200, description = "Settings applied", body = serde_json::Value), (status = 400, description = "Invalid value"), (status = 403, description = "A security-relevant change without the authority for it")))]
pub(crate) async fn settings_put_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(mut payload): Json<serde_json::Value>,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if let Err(e) = unmask(&mut payload, &crate::live::current()) {
    return (StatusCode::BAD_REQUEST, e).into_response();
  }
  let new: ServerSettings = match serde_json::from_value(payload) {
    Ok(s) => s,
    Err(e) => return (StatusCode::BAD_REQUEST, format!("settings: {e}")).into_response(),
  };
  let changed = crate::live::current().changed_paths(&new);
  if let Err(resp) = authorize_change(&state, &headers, &changed).await {
    return resp;
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  let applied = match apply_settings(&state, new).await {
    Ok(applied) => applied,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };
  let actor = state.session_actor(&headers).await;
  info!("Settings changed ({})", applied.changed.join(", "));
  state
    .audit(
      "settings_updated",
      &actor,
      &actor_ip,
      &applied.changed.join(","),
    )
    .await;
  let security: Vec<&String> = applied
    .changed
    .iter()
    .filter(|p| path_in(p, SECURITY_PATHS))
    .collect();
  if !security.is_empty() {
    state
      .audit(
        "settings_security_changed",
        &actor,
        &actor_ip,
        &security
          .iter()
          .map(|p| p.as_str())
          .collect::<Vec<_>>()
          .join(","),
      )
      .await;
  }
  state
    .emit_event(
      "settings_updated",
      serde_json::json!({"changed": applied.changed}),
    )
    .await;
  let current = crate::live::current();
  (
    StatusCode::OK,
    Json(serde_json::json!({
      "settings": current.masked(),
      "changed": applied.changed,
      "restart_pending": restart_pending(&state, &current),
    })),
  )
    .into_response()
}

/// The authority a change to these paths takes, beyond Admin of master.
#[allow(clippy::result_large_err)] // see api/tokens.rs
pub(crate) async fn authorize_change(
  state: &AppState,
  headers: &HeaderMap,
  changed: &[String],
) -> Result<(), Response> {
  let security: Vec<&String> = changed
    .iter()
    .filter(|p| path_in(p, SECURITY_PATHS))
    .collect();
  let Some(first) = security.first() else {
    return Ok(());
  };
  let caller = crate::auth::resolve_caller(state, headers).await;
  if !caller.as_ref().is_some_and(|c| c.holds_all_admin()) {
    return Err(
      (
        StatusCode::FORBIDDEN,
        format!(
          "{first} is security-relevant, and changing it takes Admin in every organization (*)"
        ),
      )
        .into_response(),
    );
  }
  if let Some(age) = crate::auth::session_age(state, headers).await
    && age > STEP_UP_SECS
  {
    return Err(
      (
        StatusCode::FORBIDDEN,
        format!(
          "{first} is security-relevant, and changing it takes a sign-in from the last {} minutes. Sign in again and retry.",
          STEP_UP_SECS / 60
        ),
      )
        .into_response(),
    );
  }
  Ok(())
}

/// What applying a document changed.
pub(crate) struct Applied {
  pub(crate) changed: Vec<String>,
}

/// Builds, stores and puts a settings document in force. `Err` is the
/// message naming the setting that was refused; nothing has changed then.
/// No authorization: the callers do their own.
pub(crate) async fn apply_settings(
  state: &Arc<AppState>,
  new: ServerSettings,
) -> Result<Applied, String> {
  let config =
    crate::server::state_build::config::build_config(&new, &state.config().token, &state.data_dir)?;
  let changed = crate::live::current().changed_paths(&new);
  if !state.settings_store.lock().await.put(&new) {
    return Err("the settings could not be saved to the database".to_string());
  }
  // The egress (`outbound.proxy`) is fixed for the life of the process,
  // since the clients built from it are kept; it is a restart path.
  crate::live::set(new);
  swap_config(state, config).await;
  Ok(Applied { changed })
}

/// Swaps in a freshly built config and pushes settings that back live
/// structures (not just config reads) into those structures.
pub(crate) async fn swap_config(state: &Arc<AppState>, new_config: ServerConfig) {
  let old_config = state.config();
  *state
    .config_store
    .write()
    .unwrap_or_else(|e| e.into_inner()) = Arc::new(new_config);
  let new_config = state.config();

  if old_config.random_subdomain_suffix != new_config.random_subdomain_suffix {
    reassign_random_hostnames(state).await;
  }
  if !old_config.tunnel_compression && new_config.tunnel_compression {
    offer_compression_to_connected(state).await;
  }
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
}

/// Re-issues random hostnames after the subdomain pattern changed at
/// runtime: every connected client loses its old random hostname (it points
/// at the retired pattern) and, when a pattern is still configured, receives
/// a fresh assignment pushed immediately via `HostnameAssigned`.
async fn reassign_random_hostnames(state: &Arc<AppState>) {
  let pattern = state.config().random_subdomain_suffix.clone();
  let mut notifications = Vec::new();
  {
    let mut clients = state.clients.write().await;
    for (id, c) in clients.iter_mut() {
      // Every service on the connection, because the random hostname is the
      // connection's: it is derived from the instance group at connect time
      // and handed to whatever the connection serves. Reassigning it on the
      // first service alone left the others answering for a name that points
      // at the retired pattern.
      let fresh = pattern.as_deref().map(random_subdomain_hostname);
      for service in c.services.iter_mut() {
        if let Some(old) = service.random_hostname.take() {
          service.assigned_hostnames.retain(|h| *h != old);
        }
        service.random_hostname = fresh.clone();
        if let Some(h) = fresh.clone() {
          service.assigned_hostnames.push(h);
        }
      }
      if let Some(h) = fresh {
        info!("Reassigned random hostname {} to client {}", h, id);
        notifications.push((c.tx.clone(), h));
      }
    }
  }
  // Send outside the clients lock so a slow client cannot stall the map.
  for (tx, hostname) in notifications {
    if let Ok(json) = serde_json::to_string(&TunnelMessage::HostnameAssigned { hostname }) {
      let _ = tx.send(Message::Text(json.into())).await;
    }
  }
}

/// Offers zlib tunnel compression to every already-connected client after
/// the setting was enabled at runtime (each client switches on ack). There
/// is no protocol message to stop compression, so disabling only affects
/// new connections.
async fn offer_compression_to_connected(state: &Arc<AppState>) {
  let txs: Vec<_> = {
    let clients = state.clients.read().await;
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
      let _ = tx.send(Message::Text(json.into())).await;
    }
  }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
