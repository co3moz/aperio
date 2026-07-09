use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
  },
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

/// Computes the live statistics + active-connection snapshot shared by the
/// `/api/stats` endpoint and the SSE live stream.
pub(crate) async fn compute_stats(state: &AppState) -> EnhancedServerStats {
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
      visitor_auth: handle.visitor_auth.is_some(),
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

  EnhancedServerStats {
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
  }
}

/// Handler returning the live statistics + active connections detail in JSON.
#[utoipa::path(get, path = "/aperio/api/stats", tag = "dashboard",
  description = "Live statistics snapshot: counters, persistent stats, and the active client connections.",
  responses((status = 200, description = "Current statistics", body = EnhancedServerStats)))]
pub(crate) async fn stats_handler(State(state): State<Arc<AppState>>) -> Json<EnhancedServerStats> {
  Json(compute_stats(&state).await)
}

/// Handler returning the list of recent HTTP logs in JSON.
#[utoipa::path(get, path = "/aperio/api/logs", tag = "dashboard",
  description = "The most recent proxied requests (bounded ring buffer).",
  responses((status = 200, description = "Recent request log entries", body = Vec<RequestLog>)))]
pub(crate) async fn logs_handler(State(state): State<Arc<AppState>>) -> Json<Vec<RequestLog>> {
  let logs = state.recent_logs.lock().await;
  Json(logs.iter().cloned().collect())
}

/// Server-Sent Events stream powering the dashboard's live view, so it doesn't
/// poll: named `traffic` events (one per proxied request, as it completes) and
/// periodic `stats` events (the same snapshot as `/api/stats`, pushed every 2s
/// and once immediately on connect). A subscriber that falls behind the traffic
/// buffer skips the lagged span rather than closing the stream.
#[utoipa::path(get, path = "/aperio/api/stream", tag = "dashboard",
  description = "Server-Sent Events stream: named `traffic` events (one per proxied request) and periodic `stats` events.",
  responses((status = 200, description = "SSE stream (text/event-stream)")))]
pub(crate) async fn live_stream_handler(
  State(state): State<Arc<AppState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
  use std::time::Duration;
  use tokio::sync::broadcast::error::RecvError;
  use tokio::time::MissedTickBehavior;

  let rx = state.traffic_tx.subscribe();
  let mut interval = tokio::time::interval(Duration::from_secs(2));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  let stream = futures_util::stream::unfold(
    (state, rx, interval),
    |(state, mut rx, mut interval)| async move {
      loop {
        tokio::select! {
          // The first tick fires immediately, seeding the initial snapshot.
          _ = interval.tick() => {
            let snapshot = compute_stats(&state).await;
            let event = Event::default()
              .event("stats")
              .json_data(&snapshot)
              .unwrap_or_else(|_| Event::default());
            return Some((Ok(event), (state, rx, interval)));
          }
          recv = rx.recv() => match recv {
            Ok(log) => {
              let event = Event::default()
                .event("traffic")
                .json_data(&log)
                .unwrap_or_else(|_| Event::default());
              return Some((Ok(event), (state, rx, interval)));
            }
            // Slow subscriber: drop the missed span and keep streaming.
            Err(RecvError::Lagged(_)) => continue,
            // Server shutting down / sender gone: end the stream.
            Err(RecvError::Closed) => return None,
          }
        }
      }
    },
  );
  Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Request payload for the dashboard client override (overrule) endpoint.
/// Each field fully replaces the corresponding override: a non-empty string
/// sets it, an empty string or `null` clears it. Overrides are in-memory only
/// and disappear when the client reconnects or the server restarts.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ClientOverrideRequest {
  pub(crate) hostname_bind: Option<String>,
  pub(crate) path_bind: Option<String>,
}

/// Applies a temporary hostname/path bind override to a connected client.
/// Protected by the dashboard session middleware.
#[utoipa::path(post, path = "/aperio/api/clients/{id}/override", tag = "dashboard",
  description = "Temporarily overrule a client's hostname/path bind server-side (empty values clear the override).",
  params(("id" = String, Path, description = "Client connection id")),
  request_body = ClientOverrideRequest,
  responses((status = 200, description = "Override applied"), (status = 404, description = "No such client")))]
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
    &state.config().trusted_proxies,
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
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ClientEnabledRequest {
  pub(crate) enabled: bool,
}

/// Dashboard kill switch: temporarily removes a connected client from the
/// routing pool (or puts it back). In-flight requests always complete.
#[utoipa::path(post, path = "/aperio/api/clients/{id}/enabled", tag = "dashboard",
  description = "Kill switch: enable/disable routing to one client without dropping its tunnel.",
  params(("id" = String, Path, description = "Client connection id")),
  request_body = ClientEnabledRequest,
  responses((status = 200, description = "State changed"), (status = 404, description = "No such client")))]
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
    &state.config().trusted_proxies,
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
