use axum::{
  body::Body,
  extract::{
    FromRequest,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, Method, StatusCode, Uri},
  response::{IntoResponse, Response},
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

use crate::access_log::{log_request_failure, sanitize_uri};
use crate::protocol::TunnelMessage;
use crate::proxy::gateway_timeout_response;
use crate::routing::{extract_request_host, pick_proxy_client};
use crate::settings::LbStrategy;
use crate::share::cookie_value;
use crate::state::{AppState, PendingRequest, TunnelResponse, WsStreamMessage};

/// Handles a WebSocket upgrade request from a public client.
/// Performs the same rate-limiting, auth, and client selection as normal HTTP proxy,
/// then establishes a bidirectional relay between the public WebSocket and the tunnel.
pub(crate) async fn handle_ws_proxy(
  state: Arc<AppState>,
  req: axum::extract::Request<Body>,
  method: Method,
  uri: Uri,
  headers: HeaderMap,
  _addr: SocketAddr,
  caller_ip: IpAddr,
) -> Response {
  let method_str = method.to_string();
  let uri_str = uri.to_string();
  let start_time = Instant::now();

  // 1. Per-IP Rate Limiting
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("Rate Limit Exceeded for IP {}", caller_ip)),
      None,
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - IP rate limit exceeded",
    )
      .into_response();
  }

  // Cap concurrently-live proxied WebSockets. They are long-lived, so they get
  // their own ceiling (max_ws_connections) separate from the short-lived HTTP
  // request slots; the RAII permit is held for the whole connection (moved into
  // the relay below) and released when it closes. Acquired before the expensive
  // setup so a flood can't pile up pending upgrades either.
  let ws_slot = match state.try_acquire_ws_slot() {
    Some(s) => s,
    None => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        503,
        start_time.elapsed(),
        Some("WebSocket connection limit exceeded"),
        None,
      )
      .await;
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        "503 Service Unavailable - WebSocket connection limit reached",
      )
        .into_response();
    }
  };

  // 2. Visitor-auth gate (shared with the HTTP path): a client-declared
  // per-service password supersedes the server's own gate; public routes skip
  // it. A share cookie set during the page load also covers its WebSockets.
  if let crate::proxy::VisitorGate::Deny(resp) = crate::proxy::check_visitor_gate(
    &state,
    &headers,
    &uri,
    extract_request_host(&headers).as_deref(),
  )
  .await
  {
    return resp;
  }

  // Client-declared visitor IP allowlists are enforced per candidate during
  // client selection below, exactly like the HTTP path.

  // 3. Wait for connection
  let (is_connected, _last_disc) = {
    let conn = state.connection_state.lock().await;
    (conn.connected, conn.last_disconnect)
  };
  if !is_connected {
    let mut rx = state.client_connected.subscribe();
    let timeout_fut = tokio::time::sleep(state.config().gateway_timeout);
    tokio::pin!(timeout_fut);

    let mut reconnected = false;
    loop {
      tokio::select! {
          _ = &mut timeout_fut => {
              break;
          }
          res = rx.changed() => {
              if res.is_ok() && *rx.borrow() {
                  reconnected = true;
                  break;
              }
          }
      }
    }

    if !reconnected {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("Gateway Timeout - Reconnect wait expired"),
        None,
      )
      .await;
      return gateway_timeout_response(
        &state,
        extract_request_host(&headers).as_deref(),
        "504 Gateway Timeout - No client connected in time",
      );
    }
  }

  // 4. Select a tunnel client (same hostname/path-aware routing as HTTP
  // proxy, including sticky affinity so a page's WebSockets land on the
  // same client as the page itself).
  let uri_path = uri_str.split('?').next().unwrap_or(&uri_str);
  let request_host = extract_request_host(&headers);
  let ws_affinity = if state.config().lb_strategy == LbStrategy::Sticky {
    cookie_value(&headers, "aperio_affinity")
  } else {
    None
  };
  let (chosen_client_id, client_tx, client_req_counter, ws_org) = match pick_proxy_client(
    &state,
    uri_path,
    request_host.as_deref(),
    None,
    ws_affinity.as_deref(),
    Some(caller_ip),
  )
  .await
  {
    crate::routing::PickOutcome::Selected(c) => (c.id, c.tx, c.request_count, c.org_id),
    crate::routing::PickOutcome::Denied(Some(redirect)) => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        302,
        start_time.elapsed(),
        Some(&format!(
          "Visitor IP {} rejected by every candidate; redirected to the denied page",
          caller_ip
        )),
        None,
      )
      .await;
      return axum::response::Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", redirect)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::FOUND.into_response());
    }
    outcome
    @ (crate::routing::PickOutcome::NoRoute | crate::routing::PickOutcome::Denied(None)) => {
      // Stealth: identical to the unclaimed-route answer (see the HTTP path).
      let reason = if matches!(outcome, crate::routing::PickOutcome::Denied(_)) {
        "Visitor IP rejected by every candidate (stealth answer)"
      } else {
        "No active client for WebSocket upgrade"
      };
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some(reason),
        None,
      )
      .await;
      return gateway_timeout_response(
        &state,
        request_host.as_deref(),
        "504 Gateway Timeout - No client available for WebSocket upgrade",
      );
    }
  };

  client_req_counter.fetch_add(1, Ordering::SeqCst);

  // Serialize headers (same filtering as normal proxy)
  let mut serialized_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      if k.as_str() == "cookie" {
        let filtered: String = val_str
          .split(';')
          .filter(|part| {
            let trimmed = part.trim();
            // Internal aperio cookies never reach backends.
            !trimmed.starts_with("aperio_session=")
              && !trimmed.starts_with("aperio_share=")
              && !trimmed.starts_with("aperio_affinity=")
          })
          .map(|part| part.trim())
          .collect::<Vec<&str>>()
          .join("; ");
        if !filtered.is_empty() {
          serialized_headers.push((k.to_string(), filtered));
        }
        continue;
      }
      serialized_headers.push((k.to_string(), val_str.to_string()));
    }
  }

  let stream_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();

  // Register pending upgrade response
  {
    let mut pending = state.pending_upgrades.lock().await;
    pending.insert(
      stream_id.clone(),
      PendingRequest {
        tx: tx_response,
        client_id: chosen_client_id.clone(),
      },
    );
  }

  // Send UpgradeRequest to client via tunnel
  let upgrade_req = TunnelMessage::UpgradeRequest {
    id: stream_id.clone(),
    method: method_str.clone(),
    uri: uri_str.clone(),
    headers: serialized_headers,
  };

  let req_json = match serde_json::to_string(&upgrade_req) {
    Ok(json) => json,
    Err(e) => {
      state.pending_upgrades.lock().await.remove(&stream_id);
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        500,
        start_time.elapsed(),
        Some(&format!("UpgradeRequest serialization failed: {}", e)),
        ws_org.clone(),
      )
      .await;
      return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
    }
  };

  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_upgrades.lock().await.remove(&stream_id);
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      502,
      start_time.elapsed(),
      Some("Failed to send UpgradeRequest to client"),
      ws_org.clone(),
    )
    .await;
    return (
      StatusCode::BAD_GATEWAY,
      "502 Bad Gateway - Client socket error",
    )
      .into_response();
  }

  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
  }

  // Await UpgradeResponse from client
  let timeout_fut = tokio::time::sleep(state.config().gateway_response_timeout);
  tokio::pin!(timeout_fut);

  let client_response = tokio::select! {
      _ = &mut timeout_fut => {
          state.pending_upgrades.lock().await.remove(&stream_id);
          log_request_failure(
              &state,
              &method_str,
              &uri_str,
              504,
              start_time.elapsed(),
              Some("WebSocket upgrade response timeout"),
            ws_org.clone(),
          )
          .await;
          return (StatusCode::GATEWAY_TIMEOUT, "504 Gateway Timeout - Upgrade response timeout").into_response();
      }
      res = rx_response => {
          match res {
              Ok(r) => r,
              Err(_) => {
                  log_request_failure(
                      &state,
                      &method_str,
                      &uri_str,
                      502,
                      start_time.elapsed(),
                      Some("Client disconnected during WebSocket upgrade"),
                    ws_org.clone(),
                  )
                  .await;
                  return (StatusCode::BAD_GATEWAY, "502 Bad Gateway - Client lost during upgrade").into_response();
              }
          }
      }
  };

  if client_response.status != 101 {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      client_response.status,
      start_time.elapsed(),
      Some("Client failed to establish backend WebSocket"),
      ws_org.clone(),
    )
    .await;
    return (
      StatusCode::from_u16(client_response.status).unwrap_or(StatusCode::BAD_GATEWAY),
      "Backend WebSocket connection failed",
    )
      .into_response();
  }

  // Client confirmed upgrade. Now perform the public-side WebSocket upgrade.
  let (parts, body) = req.into_parts();
  let req = axum::extract::Request::from_parts(parts, body);

  let upgrade_result: Result<WebSocketUpgrade, _> =
    WebSocketUpgrade::from_request(req, &state).await;

  match upgrade_result {
    Ok(ws) => {
      let state_clone = state.clone();
      let stream_id_clone = stream_id.clone();
      let client_tx_clone = client_tx.clone();
      let method_clone = method_str.clone();
      let uri_clone = uri_str.clone();
      let start_time_clone = start_time;

      let owner_client_id = chosen_client_id.clone();
      ws.on_upgrade(move |public_ws| async move {
        // Hold the WS slot for the whole life of the relay; it releases when
        // the connection closes and this future ends.
        let _ws_slot = ws_slot;
        relay_ws_stream(
          state_clone,
          stream_id_clone,
          owner_client_id,
          public_ws,
          client_tx_clone,
          method_clone,
          uri_clone,
          start_time_clone,
        )
        .await
      })
    }
    Err(rejection) => {
      // Send WsClose so the client tears down its backend connection
      let close_msg = TunnelMessage::WsClose {
        stream_id: stream_id.clone(),
        code: 1011,
        reason: "Server upgrade rejected".to_string(),
      };
      if let Ok(json) = serde_json::to_string(&close_msg) {
        let _ = client_tx.send(Message::Text(json)).await;
      }
      state.ws_streams.lock().await.remove(&stream_id);
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        400,
        start_time.elapsed(),
        Some(&format!("WebSocket upgrade rejected: {:?}", rejection)),
        ws_org.clone(),
      )
      .await;
      rejection.into_response()
    }
  }
}

/// Relays WebSocket frames bidirectionally between the public WebSocket and the tunnel.
#[allow(clippy::too_many_arguments)]
async fn relay_ws_stream(
  state: Arc<AppState>,
  stream_id: String,
  owner_client_id: String,
  public_ws: WebSocket,
  tunnel_tx: mpsc::Sender<Message>,
  method: String,
  uri: String,
  start_time: Instant,
) {
  let (mut ws_sender, mut ws_receiver) = public_ws.split();

  // Channel for relaying frames from tunnel → public WS
  let (relay_tx, mut relay_rx) = mpsc::channel::<WsStreamMessage>(64);

  // Register the relay channel so handle_socket can push WsData frames to us,
  // tagged with the serving client's id for ownership verification.
  {
    let mut streams = state.ws_streams.lock().await;
    streams.insert(
      stream_id.clone(),
      crate::state::WsStreamHandle {
        tx: relay_tx,
        client_id: owner_client_id,
      },
    );
  }

  let stream_id_clone = stream_id.clone();
  let tunnel_tx_clone = tunnel_tx.clone();

  // Task: read from public WS → send WsData through tunnel
  let ws_to_tunnel = tokio::spawn(async move {
    while let Some(result) = ws_receiver.next().await {
      match result {
        Ok(msg) => {
          let tunnel_msg = match msg {
            Message::Text(text) => TunnelMessage::WsData {
              stream_id: stream_id_clone.clone(),
              data: text.to_string(),
              is_text: true,
            },
            Message::Binary(data) => {
              use base64::prelude::*;
              TunnelMessage::WsData {
                stream_id: stream_id_clone.clone(),
                data: BASE64_STANDARD.encode(&data),
                is_text: false,
              }
            }
            Message::Close(frame) => TunnelMessage::WsClose {
              stream_id: stream_id_clone.clone(),
              code: frame.as_ref().map(|f| f.code).unwrap_or(1000),
              reason: frame
                .as_ref()
                .map(|f| f.reason.to_string())
                .unwrap_or_default(),
            },
            Message::Ping(_) | Message::Pong(_) => {
              // Auto-handled by Axum, no need to forward
              continue;
            }
          };

          if let Ok(json) = serde_json::to_string(&tunnel_msg)
            && tunnel_tx_clone.send(Message::Text(json)).await.is_err()
          {
            break;
          }
        }
        Err(e) => {
          debug!(
            "Public WS read error for stream {}: {:?}",
            stream_id_clone, e
          );
          break;
        }
      }
    }

    // Send WsClose to tunnel when public WS disconnects
    let close_msg = TunnelMessage::WsClose {
      stream_id: stream_id_clone.clone(),
      code: 1000,
      reason: String::new(),
    };
    if let Ok(json) = serde_json::to_string(&close_msg) {
      let _ = tunnel_tx_clone.send(Message::Text(json)).await;
    }
  });

  // Task: read from relay channel (tunnel → public WS) → write to public WS
  let ws_writer = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        WsStreamMessage::Data(ws_msg) => {
          if ws_sender.send(ws_msg).await.is_err() {
            break;
          }
        }
        WsStreamMessage::Close => {
          let _ = ws_sender.send(Message::Close(None)).await;
          break;
        }
      }
    }
  });

  let ws_to_tunnel_abort = ws_to_tunnel.abort_handle();
  let ws_writer_abort = ws_writer.abort_handle();

  // Wait for either task to finish; abort the other
  tokio::select! {
      _ = ws_to_tunnel => {
          ws_writer_abort.abort();
      }
      _ = ws_writer => {
          ws_to_tunnel_abort.abort();
      }
  }

  state.ws_streams.lock().await.remove(&stream_id);

  let duration = start_time.elapsed();
  let safe_uri = sanitize_uri(&uri);
  info!(
    "WebSocket stream {} closed: {} {} after {}ms",
    stream_id,
    method,
    safe_uri,
    duration.as_millis()
  );
}

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;
