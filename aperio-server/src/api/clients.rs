use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::info;

use crate::protocol::PROTOCOL_VERSION;
use crate::routing::{extract_client_ip, normalize_hostname_bind, normalize_path_bind};
use crate::state::{AppState, ClientDetail, EnhancedServerStats, RequestLog};
use crate::store::stats::{self};

/// Handler returning live statistics and active connections detail in JSON.
pub(crate) async fn stats_handler(State(state): State<Arc<AppState>>) -> Json<EnhancedServerStats> {
  let raw_stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;

  // Instance ids reported by more than one live connection: a
  // misconfiguration worth flagging (`--bind-tunnels` / failover `wait`
  // lookups become ambiguous).
  let mut instance_counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
  for handle in clients.values() {
    if let Some(iid) = handle.reported_instance_id.as_deref() {
      *instance_counts.entry(iid).or_insert(0) += 1;
    }
  }

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
      service: handle.service_name.clone(),
      public: handle.public,
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
      instance_id: handle.reported_instance_id.clone(),
      instance_id_shared: handle
        .reported_instance_id
        .as_deref()
        .is_some_and(|iid| instance_counts.get(iid).copied().unwrap_or(0) > 1),
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
