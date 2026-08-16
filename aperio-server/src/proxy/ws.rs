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
use crate::limits::{Limit, refuse};
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
  // Without the gate's own query parameter, exactly as the HTTP path does it.
  // `aperio_token=` is a credential for Aperio, and a backend has no more
  // business reading it than it has reading the session cookie. It matters
  // more here than there, in fact: a browser cannot put a header on a
  // `WebSocket`, so the query string is the form a gated socket has to use.
  let uri_str = crate::proxy::uri_without_token(&uri);
  let start_time = Instant::now();

  // 1. Per-IP Rate Limiting
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("{} (IP {})", Limit::Ip.log_detail(), caller_ip)),
      None,
    )
    .await;
    // The same refusal the HTTP path gives, through the same door: a visitor
    // whose WebSocket upgrade is rate-limited gets the header naming the
    // limit, and the refusal is counted. This path answered a bare 429 while
    // its sibling explained itself.
    return refuse(&state, Limit::Ip);
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
  // The identity is kept, not discarded: it says whether the visitor's
  // `Authorization` header was Aperio's credential rather than the backend's,
  // which decides whether it travels on.
  let visitor = match crate::proxy::check_visitor_gate(
    &state,
    // An upgrade is never a navigation: a browser opening a socket cannot be
    // sent to a login page, and a 401 is the only answer it can act on.
    &axum::http::Method::CONNECT,
    &headers,
    &uri,
    extract_request_host(&headers).as_deref(),
    caller_ip,
  )
  .await
  {
    crate::proxy::VisitorGate::Deny(resp) => return resp,
    // An upgrade has no cold start to wait for: there is no request to replay
    // once a client wakes, and a socket cannot be held open on the chance.
    // So "nothing declares this open" is final here.
    crate::proxy::VisitorGate::Undeclared(resp) => return resp,
    crate::proxy::VisitorGate::Allow(identity) => identity,
  };

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
    // A proxied WebSocket is one long-lived connection rather than a stream
    // of requests, so a split would only ever apply to the upgrade. Left out
    // deliberately: a canary that moved a live socket is not a canary, and
    // one that only chose where the socket landed would be a second, silent
    // rule beside the one written for HTTP.
    None,
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

  // Serialize headers, by the same rules as the HTTP path and through the same
  // code: an upgrade is a request like any other, and what Aperio keeps to
  // itself does not depend on which of the two paths a request arrived on.
  let carried_names = crate::proxy::carried_identity_names(&state);
  let consumed_authorization = visitor.as_ref().is_some_and(|v| v.consumed_authorization);
  let mut serialized_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      if crate::proxy::header_is_aperios(k.as_str(), &carried_names, consumed_authorization) {
        continue;
      }
      if k.as_str() == "cookie" {
        let filtered = crate::proxy::cookies_without_aperios(val_str);
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
  // Same backstop as a proxied request: a visitor that goes away while the
  // upgrade is in flight drops this handler, and the explicit removals below
  // are all on paths that are no longer running.
  let _pending_guard = crate::state::PendingGuard::new(
    state.clone(),
    crate::state::PendingMap::Upgrades,
    stream_id.clone(),
  );

  // Register the relay before the request even goes out, so it is in place no
  // matter how quickly the client answers.
  //
  // The backend is live the instant it returns 101, and protocols that greet
  // first (a Socket.IO open packet, MQTT over WebSocket, most chat protocols)
  // send immediately. The tunnel read loop delivers that answer to this task
  // and then carries straight on to the next frame, so registering here after
  // awaiting the answer would still lose whatever arrived in between, this
  // task may not have been scheduled yet. Registering up front removes the
  // window entirely; every early return below unregisters it again.
  let (relay_tx, relay_rx) = mpsc::channel::<WsStreamMessage>(64);
  // The read loop feeds a pump rather than this channel, so a visitor that
  // stops reading cannot stall the other streams on the same tunnel; past
  // the byte watermark the client is asked to pause producing this stream.
  let flow = crate::state::StreamFlow::new(
    stream_id.clone(),
    client_tx.clone(),
    state.client_supports_pause(&chosen_client_id).await,
    state.stream_limits(),
  );
  let relay_tx =
    crate::state::spawn_consumer_pump(relay_tx, state.config().gateway_response_timeout, flow);
  state.ws_streams.lock().await.insert(
    stream_id.clone(),
    crate::state::WsStreamHandle {
      tx: relay_tx,
      client_id: chosen_client_id.clone(),
    },
  );

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
      state.ws_streams.lock().await.remove(&stream_id);
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

  if client_tx
    .send(Message::Text(req_json.into()))
    .await
    .is_err()
  {
    state.pending_upgrades.lock().await.remove(&stream_id);
    state.ws_streams.lock().await.remove(&stream_id);
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
          state.ws_streams.lock().await.remove(&stream_id);
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
                  state.ws_streams.lock().await.remove(&stream_id);
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
    state.ws_streams.lock().await.remove(&stream_id);
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

  // Client confirmed the upgrade; the relay registered above has been
  // collecting anything the backend sent in the meantime.
  // Now perform the public-side WebSocket upgrade.
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

      ws.on_upgrade(move |public_ws| async move {
        // Hold the WS slot for the whole life of the relay; it releases when
        // the connection closes and this future ends.
        let _ws_slot = ws_slot;
        relay_ws_stream(
          state_clone,
          chosen_client_id,
          stream_id_clone,
          public_ws,
          relay_rx,
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
        let _ = client_tx.send(Message::Text(json.into())).await;
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
  client_id: String,
  stream_id: String,
  public_ws: WebSocket,
  mut relay_rx: mpsc::Receiver<WsStreamMessage>,
  tunnel_tx: mpsc::Sender<Message>,
  method: String,
  uri: String,
  start_time: Instant,
) {
  let (mut ws_sender, mut ws_receiver) = public_ws.split();

  let stream_id_clone = stream_id.clone();
  let tunnel_tx_clone = tunnel_tx.clone();
  // Read once per stream: which shape this client's binary frames travel in.
  let protocol = state.client_protocol(&client_id).await;

  // Task: read from public WS → send WsData through tunnel
  let ws_to_tunnel = tokio::spawn(async move {
    while let Some(result) = ws_receiver.next().await {
      match result {
        Ok(msg) => {
          // v7 takes a binary frame raw; text frames were never encoded, so
          // they keep the JSON shape whatever the peer speaks.
          if let Message::Binary(data) = &msg {
            let Some(frame) = crate::protocol::relay_frame(
              protocol,
              crate::protocol::FRAME_WS_DATA_BIN,
              &stream_id_clone,
              data,
              |encoded| TunnelMessage::WsData {
                stream_id: stream_id_clone.clone(),
                data: encoded,
                is_text: false,
              },
            ) else {
              break;
            };
            if tunnel_tx_clone.send(frame).await.is_err() {
              break;
            }
            continue;
          }
          let tunnel_msg = match msg {
            Message::Text(text) => TunnelMessage::WsData {
              stream_id: stream_id_clone.clone(),
              data: text.to_string(),
              is_text: true,
            },
            Message::Binary(_) => unreachable!("handled above"),
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
            && tunnel_tx_clone
              .send(Message::Text(json.into()))
              .await
              .is_err()
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
      let _ = tunnel_tx_clone.send(Message::Text(json.into())).await;
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
