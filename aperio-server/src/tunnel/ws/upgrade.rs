//! The rest of what arrives on a tunnel connection: the answer to a relayed
//! WebSocket upgrade and its frames, the compression handshake, OTLP exports,
//! the drain announcement, and the cleanup that runs when the socket ends.

use axum::extract::ws::Message;
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

use super::*;
use crate::protocol::TunnelMessage;

impl ConnCtx {
  /// Handles the answer to a relayed WebSocket upgrade.
  pub(super) async fn on_upgrade_response(&self, msg: TunnelMessage) {
    let TunnelMessage::UpgradeResponse {
      id,
      status,
      headers,
    } = msg
    else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    let mut pending = state.pending_upgrades.lock().await;
    let is_owner = pending
      .get(&id)
      .is_some_and(|req| req.client_id == *client_id);
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
          body_raw: None,
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

  /// Handles one frame of a passed-through WebSocket.
  pub(super) async fn on_ws_data(&self, msg: TunnelMessage) {
    let TunnelMessage::WsData {
      stream_id,
      data,
      is_text,
    } = msg
    else {
      return;
    };
    let ws_msg = if is_text {
      Message::Text(data.into())
    } else {
      // Base64 fallback path; a v7 client sends FRAME_WS_DATA_BIN frames.
      use base64::prelude::*;
      match BASE64_STANDARD.decode(&data) {
        Ok(bytes) => Message::Binary(bytes.into()),
        Err(_) => {
          warn!("Failed to decode Base64 WsData for stream {}", stream_id);
          return;
        }
      }
    };
    self.deliver_ws_frame(&stream_id, ws_msg).await;
  }

  /// Relays one frame of a passed-through WebSocket this client owns,
  /// however it arrived. The sender is cloned out of the lock so `ws_streams`
  /// is never held across the hand-off: a slow public WS consumer applying
  /// backpressure would otherwise stall the whole tunnel read loop.
  pub(super) async fn deliver_ws_frame(&self, stream_id: &str, ws_msg: Message) {
    let state = &self.state;
    let client_id = &self.client_id;
    let chunk_tx = {
      let streams = state.ws_streams.lock().await;
      match streams.get(stream_id) {
        Some(handle) if handle.client_id == *client_id => Some(handle.tx.clone()),
        _ => None,
      }
    };
    if let Some(chunk_tx) = chunk_tx {
      // Non-blocking, mirroring deliver_response_chunk: the stream's pump
      // waits on a stalling consumer so this loop does not, and its flow
      // control pauses the producer if need be.
      if let Err(e) = chunk_tx.push(WsStreamMessage::Data(ws_msg)) {
        debug!("Dropping WS stream {} ({:?})", stream_id, e);
        state.ws_streams.lock().await.remove(stream_id);
      }
    }
  }

  pub(super) async fn on_ws_close(&self, msg: TunnelMessage) {
    let TunnelMessage::WsClose {
      stream_id,
      code: _,
      reason: _,
    } = msg
    else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    let chunk_tx = {
      let streams = state.ws_streams.lock().await;
      match streams.get(&stream_id) {
        Some(handle) if handle.client_id == *client_id => Some(handle.tx.clone()),
        _ => None,
      }
    };
    if let Some(chunk_tx) = chunk_tx {
      let _ = chunk_tx.push(WsStreamMessage::Close);
    }
  }

  /// Handles the tunnel compression acknowledgement.
  pub(super) fn on_compression_ack(&self) {
    let client_id = &self.client_id;
    let compress_out = &self.compress_out;

    info!("Client {} acknowledged tunnel compression", client_id);
    compress_out.store(true, Ordering::SeqCst);
  }

  /// Handles the client announcing a graceful drain.
  /// Forwards an OTLP export a client sent over the tunnel.
  ///
  /// Spawned rather than awaited: the read loop of a tunnel serves every
  /// request that client is handling, and a collector taking two seconds must
  /// not be two seconds of the tunnel standing still. The export is dropped
  /// with a warning if the bridge is off, which is the same answer the HTTP
  /// endpoint gives and for the same reason, a client that keeps exporting
  /// into a disabled bridge should be able to see that it is doing so.
  pub(super) async fn on_otlp_export(&self, signal: String, data: String) {
    use base64::Engine;
    if !self.state.config().otel_bridge {
      warn!(
        "Client {} sent an OTel export but the bridge is not enabled",
        self.client_id
      );
      return;
    }
    if !crate::api::otlp::may_bridge(&self.perms) {
      warn!(
        "Client {} sent an OTel export but its token does not carry allow_otel",
        self.client_id
      );
      return;
    }
    let path = match signal.as_str() {
      "traces" => "v1/traces",
      "metrics" => "v1/metrics",
      "logs" => "v1/logs",
      _ => {
        warn!(
          "Client {} sent an export for an unknown signal",
          self.client_id
        );
        return;
      }
    };
    let Ok(payload) = base64::engine::general_purpose::STANDARD.decode(&data) else {
      warn!(
        "Client {} sent an export that is not Base64",
        self.client_id
      );
      return;
    };
    let state = self.state.clone();
    let identity = crate::api::otlp::identity(&self.perms);
    let client_id = self.client_id.clone();
    tokio::spawn(async move {
      if let Err(e) =
        crate::api::otlp::forward(&state, path, &identity, axum::body::Bytes::from(payload)).await
      {
        warn!("OTel export from {client_id} could not be delivered: {e}");
      }
    });
  }

  pub(super) async fn on_draining(&self) {
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;

    info!(
      "Client {} is draining: no new requests will be routed to it",
      client_id
    );
    {
      let mut clients = state.clients.write().await;
      if let Some(handle) = clients.get_mut(client_id) {
        handle.draining = true;
      }
    }
    state
      .audit_in(
        "client_draining",
        "system",
        client_ip,
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

  /// Everything that must happen once the connection is gone, whether it
  /// closed, errored, or was force-disconnected: the audit trail, the client
  /// map, the round-robin prune, and every stream and pending request this
  /// connection owned.
  pub(super) async fn cleanup(&self) {
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    info!("Tunnel client disconnected: {}", client_id);
    // Settle the batched byte accounting of every stream this connection was
    // feeding, and drop the cached senders so removing the handles below is
    // what ends the pumps.
    let pending: u64 = {
      let mut cache = self.stream_cache.lock().unwrap();
      cache.drain().map(|(_, e)| e.unreported).sum()
    };
    if pending > 0 {
      self.flush_stream_bytes(pending).await;
    }
    state
      .audit_in(
        "client_disconnected",
        "system",
        client_ip,
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
      let mut clients = state.clients.write().await;
      let removed = clients.remove(client_id);
      let now_empty = clients.is_empty();

      // Prune round-robin indices for routing groups that no longer have any
      // matching client (prevents unbounded growth of the rr map). Clients can
      // belong to multiple hostname groups, so re-evaluate all keys.
      if removed.is_some() {
        let mut rr_map = state.path_rr.lock().await;
        rr_map.retain(|(host_key, path_key), _| {
          // Any service of any connection, because a routing group is a set of
          // services and that is what the index is indexing. Asked of the
          // connection it read the first service's binds, so a group kept
          // alive only by a *later* service of a multiplexed connection had
          // its round-robin position dropped and started over at zero.
          clients.values().any(|c| {
            c.services.iter().any(|s| {
              let host_ok = match host_key {
                Some(h) => s.matches_host(h),
                None => !s.has_hostname_bind(),
              };
              host_ok && s.effective_path_bind() == path_key.as_ref()
            })
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
        .filter(|(_, req)| req.client_id == *client_id)
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
        .filter(|(_, req)| req.client_id == *client_id)
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
      streams.retain(|_, handle| handle.client_id != *client_id);
    }

    // Close TCP and UDP tunnel streams owned by the disconnected client.
    {
      let mut streams = state.tcp_streams.lock().await;
      let closing: Vec<_> = streams
        .iter()
        .filter(|(_, h)| h.client_id == *client_id)
        .map(|(_, h)| h.tx.clone())
        .collect();
      streams.retain(|_, h| h.client_id != *client_id);
      drop(streams);
      for tx in closing {
        let _ = tx.push(TcpConsumerMsg::Close);
      }
    }
    {
      let mut streams = state.udp_streams.lock().await;
      let closing: Vec<_> = streams
        .iter()
        .filter(|(_, h)| h.client_id == *client_id)
        .map(|(_, h)| h.tx.clone())
        .collect();
      streams.retain(|_, h| h.client_id != *client_id);
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
        .filter(|(_, h)| h.client_id == *client_id)
        .map(|(_, h)| h.tx.clone())
        .collect();
      streams.retain(|_, h| h.client_id != *client_id);
      drop(streams);
      for tx in closing {
        let _ = tx.push(WsStreamMessage::Close);
      }
    }
  }
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;
