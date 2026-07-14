use axum::{
  Json,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::oneshot;
use tracing::info;

use crate::protocol::TunnelMessage;
use crate::routing::{apply_lb_strategy, extract_client_ip, select_client_pool};
use crate::state::{AppState, PendingRequest, TunnelResponse};

/// Returns the full captured detail of a recent request (dashboard inspector).
#[utoipa::path(get, path = "/aperio/api/requests/{id}", tag = "dashboard",
  description = "Full captured transaction (headers and possibly-truncated bodies) for the request inspector.",
  params(("id" = String, Path, description = "Request id from the traffic log")),
  responses((status = 200, description = "Captured transaction", body = serde_json::Value), (status = 404, description = "Not captured (or evicted)")))]
pub(crate) async fn request_detail_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
  let captured = state.captured_requests.lock().await;
  match captured.iter().find(|c| c.id == id) {
    // Serve the redacted view: credentials and secret-looking body fields
    // are masked before anything leaves the server (the raw capture stays
    // in memory so replay re-sends the original request).
    Some(entry) => Json(crate::redact::redacted_view(entry)).into_response(),
    None => (
      StatusCode::NOT_FOUND,
      "Request not captured (only recent proxied requests are kept)",
    )
      .into_response(),
  }
}

/// Replays a captured request through the tunnel and returns the new outcome.
#[utoipa::path(post, path = "/aperio/api/requests/{id}/replay", tag = "dashboard",
  description = "Re-dispatches a captured request through the tunnel and returns the fresh response.",
  params(("id" = String, Path, description = "Request id from the traffic log")),
  responses((status = 200, description = "Replay result", body = serde_json::Value), (status = 404, description = "Not captured"), (status = 409, description = "Body was truncated; replay disabled")))]
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
    &state.config().trusted_proxies,
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
