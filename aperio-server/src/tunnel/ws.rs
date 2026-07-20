use axum::{
  extract::{
    ConnectInfo, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, mpsc};
use tracing::{debug, error, info, warn};

use crate::auth::authorize_tunnel_token;
use crate::protocol::{
  FRAME_RESPONSE_CHUNK, PROTOCOL_VERSION, TunnelMessage, compress_frame, decode_binary_frame,
  decompress_frame,
};
use crate::routing::{
  extract_client_ip, normalize_hostname_bind, normalize_path_bind, random_subdomain_hostname,
};
use crate::state::{
  AppState, ClientHandle, ClientPerms, ResponseStreamHandle, TcpConsumerMsg, TunnelResponse,
  WsStreamMessage,
};

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;

/// Delivers one streamed response chunk to the waiting public consumer,
/// verifying stream ownership and accounting the bytes. Shared by the JSON
/// (base64) and protocol v2 binary frame paths.
async fn deliver_response_chunk(state: &Arc<AppState>, client_id: &str, id: &str, bytes: Vec<u8>) {
  // Look up the stream and verify the sender owns it.
  let chunk_tx = {
    let streams = state.response_streams.lock().await;
    match streams.get(id) {
      Some(handle) if handle.client_id == client_id => Some(handle.tx.clone()),
      Some(_) => {
        warn!(
          "ResponseChunk for request ID {} rejected: not owned by client {}",
          id, client_id
        );
        None
      }
      None => None,
    }
  };
  if let Some(chunk_tx) = chunk_tx {
    let len = bytes.len() as u64;
    // Bounded send with timeout: if the public consumer stalls for too
    // long, drop the stream instead of blocking the tunnel read loop.
    let send_res = tokio::time::timeout(
      state.config().gateway_response_timeout,
      chunk_tx.send(Ok(crate::state::BodyFrame::Data(bytes))),
    )
    .await;
    match send_res {
      Ok(Ok(())) => {
        let mut stats = state.stats.lock().await;
        stats.total_bytes_transferred += len;
        drop(stats);
        // Attribute streamed bytes to the sending client's organization and to
        // the serving token's daily byte quota — a streamed response body would
        // otherwise escape the quota that a buffered response is charged for.
        let (org, token_id) = {
          let clients = state.clients.lock().await;
          match clients.get(client_id) {
            Some(c) => (c.perms.org_id.clone(), c.perms.token_id.clone()),
            None => (None, None),
          }
        };
        state
          .persistent_stats
          .lock()
          .await
          .record_bytes_sent(len, org.as_deref());
        state.add_token_bytes(token_id.as_deref(), len).await;
      }
      _ => {
        debug!(
          "Dropping streamed response {} (consumer gone or stalled)",
          id
        );
        state.response_streams.lock().await.remove(id);
      }
    }
  }
}

/// Upgrade WebSocket endpoint. Extracts and verifies security tokens.
pub(crate) async fn ws_handler(
  ws: WebSocketUpgrade,
  headers: HeaderMap,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  State(state): State<Arc<AppState>>,
) -> Response {
  let tunnel_client_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  let perms = match authorize_tunnel_token(&state, &headers, tunnel_client_ip).await {
    Some(p) => p,
    None => {
      info!("Unauthorized connection attempt blocked.");
      return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
  };

  // Per-organization client quota (max_clients): reject when the org already
  // has its allowed number of clients connected.
  if let Err(msg) = state.check_org_client_quota(perms.org_id.as_deref()).await {
    warn!("Tunnel connection rejected: {}", msg);
    return (StatusCode::SERVICE_UNAVAILABLE, msg).into_response();
  }

  // Validate maximum active tunnels limit (protects against file descriptor exhaustion).
  // Uses an atomic counter so that concurrent upgrade attempts cannot race past the limit.
  loop {
    let current = state.active_tunnel_count.load(Ordering::SeqCst);
    if current >= state.config().max_tunnels {
      warn!(
        "WebSocket upgrade connection rejected from {}: Maximum tunnels count reached ({}/{})",
        addr,
        current,
        state.config().max_tunnels
      );
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable - Maximum active tunnels limit reached",
      )
        .into_response();
    }
    // Atomically reserve our slot; retry if another connection raced ahead.
    if state
      .active_tunnel_count
      .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
    {
      break;
    }
  }

  // Use saturating arithmetic to prevent usize overflow with very large max_body_size.
  ws.max_message_size(state.config().max_body_size.saturating_mul(2))
    .max_frame_size(state.config().max_body_size)
    .on_upgrade(move |socket| handle_socket(socket, tunnel_client_ip.to_string(), state, perms))
}

/// WebSocket processing logic. Listens for client frame inputs (Responses/Pings).
pub(crate) async fn handle_socket(
  socket: WebSocket,
  client_ip: String,
  state: Arc<AppState>,
  perms: ClientPerms,
) {
  let (mut ws_sender, mut ws_receiver) = socket.split();
  let client_id = uuid::Uuid::new_v4().to_string();

  // Create channel to handle writes asynchronously
  let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);

  // Per-connection compression state: outgoing frames are compressed once
  // the client acknowledges the CompressionStart offer.
  let compress_out = Arc::new(AtomicBool::new(false));

  // Announced downstream link capacity of the client in bytes/second
  // (0 = unlimited). Updated from Ping, read by the writer task.
  let bandwidth_bps = Arc::new(AtomicU64::new(0));

  // Spawn a writer task for this connection
  let writer_client_id = client_id.clone();
  let compress_out_writer = compress_out.clone();
  let bandwidth_writer = bandwidth_bps.clone();
  let writer_task = tokio::spawn(async move {
    // Bandwidth shaping: when the client announced a limited link, pace all
    // outgoing tunnel frames with a token bucket (1 s burst, average rate =
    // announced capacity) so the server never pushes faster than the
    // client's network can drain. Frames larger than the burst drive the
    // bucket negative and pay the remainder as sleep time.
    let mut bucket_tokens: f64 = 0.0;
    let mut bucket_refilled_at = Instant::now();
    while let Some(msg) = rx_write.recv().await {
      let msg = match msg {
        Message::Text(t) if compress_out_writer.load(Ordering::SeqCst) => {
          Message::Binary(compress_frame(&t))
        }
        other => other,
      };
      let rate = bandwidth_writer.load(Ordering::Relaxed);
      if rate > 0 {
        let size = match &msg {
          Message::Text(t) => t.len(),
          Message::Binary(b) => b.len(),
          _ => 0,
        } as f64;
        let rate_f = rate as f64;
        let now = Instant::now();
        bucket_tokens = (bucket_tokens
          + now.duration_since(bucket_refilled_at).as_secs_f64() * rate_f)
          .min(rate_f);
        bucket_refilled_at = now;
        bucket_tokens -= size;
        if bucket_tokens < 0.0 {
          tokio::time::sleep(Duration::from_secs_f64(-bucket_tokens / rate_f)).await;
        }
      }
      if let Err(e) = ws_sender.send(msg).await {
        error!(
          "Error writing to websocket client {}: {:?}",
          writer_client_id, e
        );
        break;
      }
    }
  });

  info!("Tunnel client connected: {} (IP: {})", client_id, client_ip);
  state
    .audit_in(
      "client_connected",
      "system",
      &client_ip,
      perms.org_id.clone(),
      &format!(
        "client={} token={}",
        client_id,
        perms.token_name.as_deref().unwrap_or("master")
      ),
    )
    .await;
  state
    .emit_event_in(
      "client_connected",
      serde_json::json!({
        "client_id": client_id,
        "ip": client_ip,
        "token": perms.token_name.as_deref().unwrap_or("master"),
      }),
      perms.org_id.clone(),
    )
    .await;

  let client_req_count = Arc::new(AtomicU64::new(0));

  // Token-granted binds apply immediately, before the first Ping. When the
  // random subdomain feature is on, the random hostname is added on top of
  // any token-granted hostnames — the client serves both.
  let mut assigned_hostnames = perms.granted_hostnames();
  let random_hostname = state
    .config()
    .random_subdomain_suffix
    .as_ref()
    .map(|pattern| random_subdomain_hostname(pattern));
  if let Some(ref h) = random_hostname {
    assigned_hostnames.push(h.clone());
  }

  // Signalled to force this connection's read loop to end (e.g. token revoke).
  let disconnect = Arc::new(Notify::new());

  // Register active client
  {
    let mut clients = state.clients.lock().await;
    clients.insert(
      client_id.clone(),
      ClientHandle {
        tx: tx_write.clone(),
        disconnect: disconnect.clone(),
        connected_at: Instant::now(),
        client_ip: client_ip.clone(),
        request_count: client_req_count.clone(),
        declared_path: None,
        assigned_path: perms.granted_path(),
        declared_hostname: None,
        declared_hostnames: Vec::new(),
        assigned_hostnames,
        random_hostname: random_hostname.clone(),
        override_path_bind: None,
        override_hostname_bind: None,
        last_ping_at: None,
        perms: perms.clone(),
        max_concurrent: None,
        inflight_limiter: None,
        draining: false,
        admin_enabled: true,
        tcp_enabled: false,
        client_version: None,
        client_protocol: None,
        backend_healthy: true,
        backend_probed: true,
        priority: 0,
        reported_instance_id: None,
        bandwidth_bps: bandwidth_bps.clone(),
        service_name: None,
        public: false,
        public_denied_warned: false,
        visitor_auth: None,
        visitor_auth_denied_warned: false,
        allowed_ips: Vec::new(),
        allowed_ips_invalid_warned: false,
        tunnels: Vec::new(),
        cache: false,
        resilience: false,
        max_request_body: None,
        response_timeout: None,
        webhook_inbox: false,
        denied: None,
        recent_failures: VecDeque::new(),
        ejected_until: None,
      },
    );
    drop(clients);
    let mut conn = state.connection_state.lock().await;
    conn.connected = true;
    conn.last_disconnect = None;
    state.client_connected.send_replace(true);
  }

  // Inform the client of its randomly assigned hostname (if any).
  if let Some(hostname) = random_hostname {
    info!(
      "Assigned random hostname {} to client {}",
      hostname, client_id
    );
    let msg = TunnelMessage::HostnameAssigned { hostname };
    if let Ok(json) = serde_json::to_string(&msg) {
      let _ = tx_write.send(Message::Text(json)).await;
    }
  }

  // Offer tunnel compression; frames stay uncompressed until the client Acks.
  if state.config().tunnel_compression
    && let Ok(json) = serde_json::to_string(&TunnelMessage::CompressionStart {})
  {
    let _ = tx_write.send(Message::Text(json)).await;
  }

  // Cap for decompressed tunnel frames (defends against zlib bombs).
  let max_inflated = state
    .config()
    .max_body_size
    .saturating_mul(4)
    .max(8 * 1024 * 1024);

  // Read loop. Ends on the client closing the socket, or when `disconnect` is
  // signalled (e.g. the token this client connected with was revoked), which
  // yields `None` so the loop falls through to the normal cleanup below.
  while let Some(result) = tokio::select! {
    msg = ws_receiver.next() => msg,
    _ = disconnect.notified() => {
      info!("Force-disconnecting tunnel client {} (server request, e.g. token revoked)", client_id);
      None
    }
  } {
    match result {
      Ok(msg) => {
        let text_opt = match msg {
          Message::Text(t) => Some(t),
          Message::Binary(b) => {
            // v2 binary chunk frames carry a tag byte that never collides
            // with zlib-compressed JSON frames (0x78).
            if let Some((FRAME_RESPONSE_CHUNK, fid, payload)) = decode_binary_frame(&b) {
              let fid = fid.to_string();
              deliver_response_chunk(&state, &client_id, &fid, payload.to_vec()).await;
              None
            } else {
              decompress_frame(&b, max_inflated)
            }
          }
          _ => None,
        };
        if let Some(text) = text_opt
          && let Ok(tunnel_msg) = serde_json::from_str::<TunnelMessage>(&text)
        {
          match tunnel_msg {
            TunnelMessage::Response {
              id,
              status,
              headers,
              body,
              trailers,
              timings,
            } => {
              let mut pending = state.pending_requests.lock().await;
              // Verify that this response originates from the client that was
              // assigned the request. Prevents a malicious tunnel client from
              // injecting spoofed responses for another client's requests.
              let is_owner = pending
                .get(&id)
                .is_some_and(|req| req.client_id == client_id);
              if !is_owner {
                if pending.contains_key(&id) {
                  warn!(
                    "Response for request ID {} rejected: sent by client {} but owned by a different client",
                    id, client_id
                  );
                }
              } else if let Some(req) = pending.remove(&id)
                && req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body,
                    trailers,
                    stream_rx: None,
                    timings,
                  })
                  .is_err()
              {
                warn!(
                  "Pending request oneshot receiver was dropped for request ID: {}",
                  id
                );
              }
            }
            TunnelMessage::ResponseStart {
              id,
              status,
              headers,
            } => {
              let mut pending = state.pending_requests.lock().await;
              let is_owner = pending
                .get(&id)
                .is_some_and(|req| req.client_id == client_id);
              if !is_owner {
                if pending.contains_key(&id) {
                  warn!(
                    "ResponseStart for request ID {} rejected: sent by client {} but owned by a different client",
                    id, client_id
                  );
                }
              } else if let Some(req) = pending.remove(&id) {
                // Register the chunk channel before resolving the head so no
                // ResponseChunk can race past an unregistered stream.
                let (chunk_tx, chunk_rx) =
                  mpsc::channel::<Result<crate::state::BodyFrame, std::io::Error>>(32);
                state.response_streams.lock().await.insert(
                  id.clone(),
                  ResponseStreamHandle {
                    tx: chunk_tx,
                    client_id: client_id.clone(),
                  },
                );
                if req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body: None,
                    trailers: None,
                    stream_rx: Some(chunk_rx),
                    timings: None,
                  })
                  .is_err()
                {
                  warn!(
                    "Pending request oneshot receiver was dropped for streamed request ID: {}",
                    id
                  );
                  state.response_streams.lock().await.remove(&id);
                }
              }
            }
            TunnelMessage::ResponseChunk { id, data } => {
              // Base64 fallback path; v2 clients send raw binary frames.
              use base64::prelude::*;
              match BASE64_STANDARD.decode(&data) {
                Ok(bytes) => deliver_response_chunk(&state, &client_id, &id, bytes).await,
                Err(_) => {
                  warn!("Failed to decode Base64 ResponseChunk for request {}", id);
                  state.response_streams.lock().await.remove(&id);
                }
              }
            }
            TunnelMessage::ResponseEnd { id, trailers } => {
              // Dropping the sender ends the public body stream; trailers
              // (e.g. gRPC's grpc-status) are delivered as the final frame.
              let removed = state.response_streams.lock().await.remove(&id);
              if let Some(handle) = removed {
                if handle.client_id != client_id {
                  // Ownership violation: re-insert and ignore.
                  warn!(
                    "ResponseEnd for request ID {} rejected: not owned by client {}",
                    id, client_id
                  );
                  state.response_streams.lock().await.insert(id, handle);
                } else if let Some(trailers) = trailers {
                  let _ = handle
                    .tx
                    .send(Ok(crate::state::BodyFrame::Trailers(trailers)))
                    .await;
                }
              }
            }
            TunnelMessage::TcpData { stream_id, data } => {
              let consumer_tx = {
                let streams = state.tcp_streams.lock().await;
                match streams.get(&stream_id) {
                  Some(h) if h.client_id == client_id => Some(h.tx.clone()),
                  Some(_) => {
                    warn!(
                      "TcpData for stream {} rejected: not owned by client {}",
                      stream_id, client_id
                    );
                    None
                  }
                  None => None,
                }
              };
              if let Some(consumer_tx) = consumer_tx {
                use base64::prelude::*;
                match BASE64_STANDARD.decode(&data) {
                  Ok(bytes) => {
                    if consumer_tx.send(TcpConsumerMsg::Data(bytes)).await.is_err() {
                      state.tcp_streams.lock().await.remove(&stream_id);
                    }
                  }
                  Err(_) => {
                    warn!("Failed to decode Base64 TcpData for stream {}", stream_id);
                  }
                }
              }
            }
            TunnelMessage::TcpClose { stream_id } => {
              let removed = state.tcp_streams.lock().await.remove(&stream_id);
              if let Some(h) = removed {
                if h.client_id == client_id {
                  let _ = h.tx.send(TcpConsumerMsg::Close).await;
                } else {
                  state.tcp_streams.lock().await.insert(stream_id, h);
                }
              }
            }
            TunnelMessage::UdpDatagram { stream_id, data } => {
              let consumer_tx = {
                let streams = state.udp_streams.lock().await;
                match streams.get(&stream_id) {
                  Some(h) if h.client_id == client_id => Some(h.tx.clone()),
                  Some(_) => {
                    warn!(
                      "UdpDatagram for stream {} rejected: not owned by client {}",
                      stream_id, client_id
                    );
                    None
                  }
                  None => None,
                }
              };
              if let Some(consumer_tx) = consumer_tx {
                use base64::prelude::*;
                match BASE64_STANDARD.decode(&data) {
                  Ok(bytes) => {
                    // Best-effort: a congested consumer drops datagrams.
                    if let Err(mpsc::error::TrySendError::Closed(_)) =
                      consumer_tx.try_send(TcpConsumerMsg::Data(bytes))
                    {
                      state.udp_streams.lock().await.remove(&stream_id);
                    }
                  }
                  Err(_) => {
                    warn!(
                      "Failed to decode Base64 UdpDatagram for stream {}",
                      stream_id
                    );
                  }
                }
              }
            }
            TunnelMessage::UdpClose { stream_id } => {
              let removed = state.udp_streams.lock().await.remove(&stream_id);
              if let Some(h) = removed {
                if h.client_id == client_id {
                  let _ = h.tx.send(TcpConsumerMsg::Close).await;
                } else {
                  state.udp_streams.lock().await.insert(stream_id, h);
                }
              }
            }
            TunnelMessage::CompressionAck {} => {
              info!("Client {} acknowledged tunnel compression", client_id);
              compress_out.store(true, Ordering::SeqCst);
            }
            TunnelMessage::Draining {} => {
              info!(
                "Client {} is draining: no new requests will be routed to it",
                client_id
              );
              {
                let mut clients = state.clients.lock().await;
                if let Some(handle) = clients.get_mut(&client_id) {
                  handle.draining = true;
                }
              }
              state
                .audit_in(
                  "client_draining",
                  "system",
                  &client_ip,
                  perms.org_id.clone(),
                  &format!("client={}", client_id),
                )
                .await;
              state
                .emit_event_in(
                  "client_draining",
                  serde_json::json!({"client_id": client_id, "ip": client_ip}),
                  perms.org_id.clone(),
                )
                .await;
            }
            TunnelMessage::Ping {
              client_id: cid,
              timestamp,
              path_bind,
              hostname_bind,
              hostname_binds,
              max_concurrent,
              tcp,
              version,
              protocol,
              backend_healthy,
              backend_probed,
              priority,
              bandwidth_bps,
              service,
              public,
              visitor_auth,
              allowed_ips,
              tunnels,
              cache,
              resilience,
              max_request_body,
              response_timeout,
              client_key,
              webhook_inbox,
              denied,
            } => {
              debug!("Heartbeat from client {}: {}", cid, timestamp);
              // Update client's reported binds and heartbeat time. Only the
              // server-assigned connection ID is trusted for state updates;
              // the client-declared `cid` is ignored to prevent a client from
              // mutating another connection's state.
              let normalized_path = path_bind.and_then(|b| normalize_path_bind(&b));
              let normalized_host = hostname_bind.and_then(|h| normalize_hostname_bind(&h));
              // Token pinning context captured under the clients lock and used
              // after it is released: (token id, token name, org).
              let mut pin_ctx: Option<(String, String, Option<String>)> = None;
              {
                let mut clients = state.clients.lock().await;
                if let Some(handle) = clients.get_mut(&client_id) {
                  // Declared binds must be permitted by the token used to connect.
                  if let Some(p) = normalized_path {
                    if handle.perms.path_allowed(&p) {
                      handle.declared_path = Some(p);
                    } else {
                      warn!(
                        "Client {} declared path bind {} not permitted by its token; ignored",
                        client_id, p
                      );
                    }
                  }
                  if let Some(h) = normalized_host {
                    if handle.perms.hostname_allowed(&h) {
                      handle.declared_hostname = Some(h);
                    } else {
                      warn!(
                        "Client {} declared hostname bind {} not permitted by its token; ignored",
                        client_id, h
                      );
                    }
                  }
                  // Additional multi-hostname binds: normalize and admit each
                  // that the token permits (others are dropped with a warning).
                  if !hostname_binds.is_empty() {
                    let mut admitted = Vec::new();
                    for raw in &hostname_binds {
                      let Some(h) = normalize_hostname_bind(raw) else {
                        continue;
                      };
                      if handle.perms.hostname_allowed(&h) {
                        if !admitted.contains(&h) {
                          admitted.push(h);
                        }
                      } else {
                        warn!(
                          "Client {} declared hostname bind {} not permitted by its token; ignored",
                          client_id, h
                        );
                      }
                    }
                    handle.declared_hostnames = admitted;
                  }
                  // Create the concurrency limiter on the first Ping that
                  // announces a limit; the limit is fixed for the connection.
                  if handle.inflight_limiter.is_none()
                    && let Some(n) = max_concurrent
                    && n > 0
                  {
                    handle.max_concurrent = Some(n);
                    handle.inflight_limiter = Some(Arc::new(Semaphore::new(n as usize)));
                    info!(
                      "Client {} announced concurrency limit: {} — excess requests will be queued",
                      client_id, n
                    );
                  }
                  handle.tcp_enabled = tcp;
                  if handle.cache != cache {
                    handle.cache = cache;
                    if cache {
                      info!(
                        "Client {} opted into the server-side response cache",
                        client_id
                      );
                    }
                  }
                  if handle.max_request_body != max_request_body {
                    handle.max_request_body = max_request_body;
                    if let Some(limit) = max_request_body {
                      info!(
                        "Client {} declared a request body cap of {} bytes; bigger uploads are rejected with 413 before dispatch",
                        client_id, limit
                      );
                    }
                  }
                  if handle.response_timeout != response_timeout {
                    handle.response_timeout = response_timeout;
                    if let Some(secs) = response_timeout {
                      info!(
                        "Client {} declared a per-service response timeout of {}s (overrides the global gateway response timeout)",
                        client_id, secs
                      );
                    }
                  }
                  // Denied-redirect declaration: only well-formed absolute
                  // http(s) URLs are honored; anything else stays stealth.
                  let denied = denied
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                    .filter(|u| url::Url::parse(u).is_ok());
                  if handle.denied != denied {
                    if let Some(url) = &denied {
                      info!(
                        "Client {} declares a denied-visitor redirect: {}",
                        client_id, url
                      );
                    }
                    handle.denied = denied;
                  }
                  if handle.webhook_inbox != webhook_inbox {
                    handle.webhook_inbox = webhook_inbox;
                    if webhook_inbox {
                      info!(
                        "Client {} opted into the webhook inbox: inbound POSTs are persisted for re-firing",
                        client_id
                      );
                    }
                  }
                  if handle.resilience != resilience {
                    handle.resilience = resilience;
                    if resilience {
                      info!(
                        "Client {} asked for serve-stale resilience: cached responses outlive its disconnects",
                        client_id
                      );
                    }
                  }
                  if handle.tunnels != tunnels {
                    info!(
                      "Client {} declares {} bindable tunnel(s)",
                      client_id,
                      tunnels.len()
                    );
                    handle.tunnels = tunnels;
                  }
                  // Log backend health transitions reported by the client's
                  // own probe; the eligibility filter honours the flag.
                  handle.backend_probed = backend_probed;
                  if handle.backend_healthy != backend_healthy {
                    handle.backend_healthy = backend_healthy;
                    if backend_healthy {
                      info!(
                        "Client {} reports its backend is healthy again; back in routing",
                        client_id
                      );
                    } else {
                      warn!(
                        "Client {} reports its backend as unhealthy; excluded from routing (tunnel stays connected)",
                        client_id
                      );
                    }
                  }
                  if handle.priority != priority {
                    info!(
                      "Client {} announced load-balancing priority {}",
                      client_id, priority
                    );
                    handle.priority = priority;
                  }
                  // The self-reported instance ID is remembered (first value
                  // wins) so failover `wait` mode can recognize this client
                  // process when it reconnects under a new connection ID.
                  if handle.reported_instance_id.is_none() && !cid.is_empty() {
                    handle.reported_instance_id = Some(cid.clone());
                  }
                  // Announced link capacity feeds the writer task's shaper.
                  let announced_bw = bandwidth_bps.unwrap_or(0);
                  if handle.bandwidth_bps.swap(announced_bw, Ordering::Relaxed) != announced_bw
                    && announced_bw > 0
                  {
                    info!(
                      "Client {} announced a bandwidth limit of {} bytes/s; pacing outgoing frames",
                      client_id, announced_bw
                    );
                  }
                  if let Some(v) = version {
                    handle.client_version = Some(v);
                  }
                  if service.is_some() {
                    handle.service_name = service;
                  }
                  // Public declaration: honored only when the token permits
                  // publishing public services.
                  let effective_public = public && handle.perms.allow_public;
                  if public && !handle.perms.allow_public && !handle.public_denied_warned {
                    handle.public_denied_warned = true;
                    warn!(
                      "Client {} declared itself public but its token does not permit publishing public services; keeping the visitor auth gate",
                      client_id
                    );
                  }
                  if handle.public != effective_public {
                    handle.public = effective_public;
                    if effective_public {
                      info!(
                        "Client {} serves public traffic: the visitor auth gate is skipped for its routes",
                        client_id
                      );
                    }
                  }
                  // Client-declared visitor password override: honored only
                  // when the token may control the visitor gate (same
                  // permission as `public`) and the value is a well-formed
                  // "user:password". None/empty clears any previous override.
                  let requested_auth = visitor_auth
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                  let effective_auth = match requested_auth {
                    Some(_) if !handle.perms.allow_public => {
                      if !handle.visitor_auth_denied_warned {
                        handle.visitor_auth_denied_warned = true;
                        warn!(
                          "Client {} declared a visitor password but its token does not permit controlling the visitor gate; ignoring it",
                          client_id
                        );
                      }
                      None
                    }
                    Some(ref creds) if !crate::routing::valid_visitor_creds(creds) => {
                      if !handle.visitor_auth_denied_warned {
                        handle.visitor_auth_denied_warned = true;
                        warn!(
                          "Client {} declared an invalid visitor password (expected user:password); ignoring it",
                          client_id
                        );
                      }
                      None
                    }
                    other => other,
                  };
                  if handle.visitor_auth != effective_auth {
                    let now_set = effective_auth.is_some();
                    handle.visitor_auth = effective_auth;
                    if now_set {
                      info!(
                        "Client {} gates its service behind a client-set visitor login",
                        client_id
                      );
                    }
                  }
                  // Client-declared visitor IP allowlist. Purely restrictive
                  // (it can only narrow who reaches the client), so no token
                  // permission is required; invalid entries are dropped so a
                  // typo can never widen access.
                  let mut effective_ips: Vec<String> = allowed_ips
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                  let before = effective_ips.len();
                  effective_ips.retain(|e| crate::auth::valid_ip_entry(e));
                  if effective_ips.len() != before && !handle.allowed_ips_invalid_warned {
                    handle.allowed_ips_invalid_warned = true;
                    warn!(
                      "Client {} declared allowed_ips with invalid entries; dropping them",
                      client_id
                    );
                  }
                  if handle.allowed_ips != effective_ips {
                    if !effective_ips.is_empty() {
                      info!(
                        "Client {} restricts visitors to {:?}",
                        client_id, effective_ips
                      );
                    }
                    handle.allowed_ips = effective_ips;
                  }
                  // Warn once per change, not on every heartbeat.
                  if protocol.is_some() && handle.client_protocol != protocol {
                    handle.client_protocol = protocol;
                    if let Some(p) = protocol
                      && p != PROTOCOL_VERSION
                    {
                      warn!(
                        "Client {} speaks tunnel protocol v{} but this server speaks v{}; \
                         update the older side to avoid subtle incompatibilities",
                        client_id, p, PROTOCOL_VERSION
                      );
                    }
                  }
                  handle.last_ping_at = Some(Instant::now());
                  // Dynamic-token clients are subject to token pinning.
                  if let Some(id) = handle.perms.token_id.clone() {
                    pin_ctx = Some((
                      id,
                      handle.perms.token_name.clone().unwrap_or_default(),
                      handle.perms.org_id.clone(),
                    ));
                  }
                }
              }

              // Trust-on-first-use token pinning (APERIO_TOKEN_PINNING): pin the
              // first device key seen for a dynamic token and reject a later
              // connection that presents a different (or missing) key. Done
              // outside the clients lock so we never hold two store locks.
              if state.config().token_pinning
                && let Some((token_id, token_name, org)) = pin_ctx
              {
                let verdict = {
                  let mut store = state.token_store.lock().await;
                  match client_key.as_deref() {
                    Some(key) => store.pin_key(&token_id, key),
                    // No key announced while pinning is required: reject (fail
                    // closed). A key-less client can never satisfy pinning, so
                    // enabling APERIO_TOKEN_PINNING requires every client to
                    // carry a device key (APERIO_DEVICE_KEY[_FILE]).
                    None => Some(crate::store::tokens::PinOutcome::Mismatch),
                  }
                };
                match verdict {
                  Some(crate::store::tokens::PinOutcome::Mismatch) => {
                    warn!(
                      "Token pinning: client {} presented token '{}' without a matching device key — rejecting the connection",
                      client_id, token_name
                    );
                    state
                      .audit_in(
                        "token_pin_mismatch",
                        &token_name,
                        &client_ip,
                        org.clone(),
                        &format!("token={token_name} client={client_id}"),
                      )
                      .await;
                    state
                      .emit_event_in(
                        "token_pin_mismatch",
                        serde_json::json!({"token": token_name, "client_id": client_id}),
                        org,
                      )
                      .await;
                    break;
                  }
                  Some(crate::store::tokens::PinOutcome::Pinned) => {
                    info!(
                      "Token pinning: pinned token '{}' to the connecting device",
                      token_name
                    );
                  }
                  _ => {}
                }
              }

              let pong = TunnelMessage::Pong {
                timestamp,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                protocol: Some(PROTOCOL_VERSION),
              };
              if let Ok(pong_str) = serde_json::to_string(&pong) {
                let _ = tx_write.send(Message::Text(pong_str)).await;
              }
            }
            TunnelMessage::UpgradeResponse {
              id,
              status,
              headers,
            } => {
              let mut pending = state.pending_upgrades.lock().await;
              let is_owner = pending
                .get(&id)
                .is_some_and(|req| req.client_id == client_id);
              if !is_owner {
                if pending.contains_key(&id) {
                  warn!(
                    "UpgradeResponse for stream ID {} rejected: sent by client {} but owned by a different client",
                    id, client_id
                  );
                }
              } else if let Some(req) = pending.remove(&id)
                && req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body: None,
                    trailers: None,
                    stream_rx: None,
                    timings: None,
                  })
                  .is_err()
              {
                warn!(
                  "Pending upgrade oneshot receiver was dropped for stream ID: {}",
                  id
                );
              }
            }
            TunnelMessage::WsData {
              stream_id,
              data,
              is_text,
            } => {
              // Relay WebSocket frame to the public WS via the registered
              // channel — but only if this client owns the stream, matching the
              // ownership check every other stream type performs. Clone the
              // sender out of the lock so `ws_streams` is never held across the
              // bounded, awaited send: a slow public WS consumer applying
              // backpressure would otherwise stall the whole tunnel read loop
              // and block every other client's ws_streams access.
              let chunk_tx = {
                let streams = state.ws_streams.lock().await;
                match streams.get(&stream_id) {
                  Some(handle) if handle.client_id == client_id => Some(handle.tx.clone()),
                  _ => None,
                }
              };
              if let Some(chunk_tx) = chunk_tx {
                let ws_msg = if is_text {
                  Message::Text(data)
                } else {
                  use base64::prelude::*;
                  match BASE64_STANDARD.decode(&data) {
                    Ok(bytes) => Message::Binary(bytes),
                    Err(_) => {
                      warn!("Failed to decode Base64 WsData for stream {}", stream_id);
                      continue;
                    }
                  }
                };
                // Bounded send: drop the stream if the public consumer stalls,
                // mirroring deliver_response_chunk.
                let send_res = tokio::time::timeout(
                  state.config().gateway_response_timeout,
                  chunk_tx.send(WsStreamMessage::Data(ws_msg)),
                )
                .await;
                if !matches!(send_res, Ok(Ok(()))) {
                  debug!(
                    "Dropping WS stream {} (consumer gone or stalled)",
                    stream_id
                  );
                  state.ws_streams.lock().await.remove(&stream_id);
                }
              }
            }
            TunnelMessage::WsClose {
              stream_id,
              code: _,
              reason: _,
            } => {
              let chunk_tx = {
                let streams = state.ws_streams.lock().await;
                match streams.get(&stream_id) {
                  Some(handle) if handle.client_id == client_id => Some(handle.tx.clone()),
                  _ => None,
                }
              };
              if let Some(chunk_tx) = chunk_tx {
                let _ = tokio::time::timeout(
                  state.config().gateway_response_timeout,
                  chunk_tx.send(WsStreamMessage::Close),
                )
                .await;
              }
            }
            _ => {}
          }
        }
      }
      Err(e) => {
        error!("WebSocket reading error for client {}: {:?}", client_id, e);
        break;
      }
    }
  }

  // Client cleanup
  writer_task.abort();
  info!("Tunnel client disconnected: {}", client_id);
  state
    .audit_in(
      "client_disconnected",
      "system",
      &client_ip,
      perms.org_id.clone(),
      &format!("client={}", client_id),
    )
    .await;
  state
    .emit_event_in(
      "client_disconnected",
      serde_json::json!({"client_id": client_id, "ip": client_ip}),
      perms.org_id.clone(),
    )
    .await;
  {
    let mut clients = state.clients.lock().await;
    let removed = clients.remove(&client_id);
    let now_empty = clients.is_empty();

    // Prune round-robin indices for routing groups that no longer have any
    // matching client (prevents unbounded growth of the rr map). Clients can
    // belong to multiple hostname groups, so re-evaluate all keys.
    if removed.is_some() {
      let mut rr_map = state.path_rr.lock().await;
      rr_map.retain(|(host_key, path_key), _| {
        clients.values().any(|c| {
          let host_ok = match host_key {
            Some(h) => c.matches_host(h),
            None => !c.has_hostname_bind(),
          };
          host_ok && c.effective_path_bind() == path_key.as_ref()
        })
      });
    }

    drop(clients);

    if now_empty {
      let mut conn = state.connection_state.lock().await;
      conn.connected = false;
      conn.last_disconnect = Some(Instant::now());
      drop(conn);
      state.client_connected.send_replace(false);
    }
  }
  // Release the reserved tunnel slot.
  state.active_tunnel_count.fetch_sub(1, Ordering::SeqCst);

  // Instantly abort pending requests that were routed to the disconnected client
  {
    let mut pending = state.pending_requests.lock().await;
    let keys_to_remove: Vec<String> = pending
      .iter()
      .filter(|(_, req)| req.client_id == client_id)
      .map(|(k, _)| k.clone())
      .collect();

    for k in keys_to_remove {
      if let Some(_req) = pending.remove(&k) {
        // Drop the sender channel, triggering an immediate channel cancellation / 502 Bad Gateway
        debug!(
          "Aborted pending request ID {} due to active client connection loss",
          k
        );
        // The oneshot channel dropping will wake the handler thread to reply immediately.
      }
    }
  }

  // Abort pending upgrade responses routed to the disconnected client
  {
    let mut pending = state.pending_upgrades.lock().await;
    let keys_to_remove: Vec<String> = pending
      .iter()
      .filter(|(_, req)| req.client_id == client_id)
      .map(|(k, _)| k.clone())
      .collect();
    for k in keys_to_remove {
      pending.remove(&k);
    }
  }

  // Terminate in-flight streamed response bodies from the disconnected client
  // (dropping the senders ends the corresponding public HTTP bodies).
  {
    let mut streams = state.response_streams.lock().await;
    streams.retain(|_, handle| handle.client_id != client_id);
  }

  // Close TCP and UDP tunnel streams owned by the disconnected client.
  for map in [&state.tcp_streams, &state.udp_streams] {
    let mut streams = map.lock().await;
    let closing: Vec<_> = streams
      .iter()
      .filter(|(_, h)| h.client_id == client_id)
      .map(|(_, h)| h.tx.clone())
      .collect();
    streams.retain(|_, h| h.client_id != client_id);
    drop(streams);
    for tx in closing {
      let _ = tx.send(TcpConsumerMsg::Close).await;
    }
  }

  // Close proxied public WebSockets served by the disconnected client, so a
  // passive listener does not hang forever and the ws_streams entry + its
  // relay tasks are not leaked (the sibling of the TCP/UDP cleanup above).
  {
    let mut streams = state.ws_streams.lock().await;
    let closing: Vec<_> = streams
      .iter()
      .filter(|(_, h)| h.client_id == client_id)
      .map(|(_, h)| h.tx.clone())
      .collect();
    streams.retain(|_, h| h.client_id != client_id);
    drop(streams);
    for tx in closing {
      let _ = tx.send(WsStreamMessage::Close).await;
    }
  }
}
