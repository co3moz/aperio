//! The streamed-response half of a tunnel connection: decoding an incoming
//! frame into its envelope and body, and handing each chunk to the public
//! consumer waiting on it.

use axum::extract::ws::Message;
use tracing::{debug, warn};

use super::*;
use crate::protocol::TunnelMessage;

impl ConnCtx {
  /// Delivers one streamed response chunk to the waiting public consumer.
  /// Shared by the JSON (base64) and protocol v2 binary frame paths.
  ///
  /// The hot path here is deliberately cheap: the stream's sender is looked
  /// up in the global `response_streams` map (with the ownership check) only
  /// for the first chunk and cached per connection afterwards, and the byte
  /// accounting is batched per `STREAM_ACCOUNT_FLUSH_BYTES` instead of
  /// taking three shared locks per chunk. Ownership caching is sound because
  /// request ids are UUIDs that are never reused, and every stream is fed by
  /// exactly the connection that received its request.
  pub(super) async fn deliver_response_chunk(&self, id: &str, bytes: axum::body::Bytes) {
    let len = bytes.len() as u64;
    let cached = {
      let cache = self.stream_cache.lock().unwrap();
      cache.get(id).map(|e| e.tx.clone())
    };
    let chunk_tx = match cached {
      Some(tx) => tx,
      None => {
        let looked_up = {
          let streams = self.state.response_streams.lock().await;
          match streams.get(id) {
            Some(handle) if handle.client_id == self.client_id => Some(handle.tx.clone()),
            Some(_) => {
              warn!(
                "ResponseChunk for request ID {} rejected: not owned by client {}",
                id, self.client_id
              );
              None
            }
            None => None,
          }
        };
        let Some(tx) = looked_up else { return };
        self.stream_cache.lock().unwrap().insert(
          id.to_string(),
          StreamCacheEntry {
            tx: tx.clone(),
            unreported: 0,
          },
        );
        tx
      }
    };
    // Never block here: this runs on the read loop the whole tunnel shares.
    // The stream's pump task absorbs a stalling consumer, its flow control
    // pauses the producer past the byte watermark, and only a consumer gone
    // for good (or a producer that cannot be paused) ends the stream.
    match chunk_tx.push(Ok(crate::state::BodyFrame::Data(bytes))) {
      Ok(()) => {
        let flush = {
          let mut cache = self.stream_cache.lock().unwrap();
          match cache.get_mut(id) {
            Some(entry) => {
              entry.unreported += len;
              if entry.unreported >= STREAM_ACCOUNT_FLUSH_BYTES {
                std::mem::take(&mut entry.unreported)
              } else {
                0
              }
            }
            None => len,
          }
        };
        if flush > 0 {
          self.flush_stream_bytes(flush).await;
        }
      }
      Err(e) => {
        match e {
          crate::state::PumpPushError::ConsumerGone => debug!(
            "Dropping streamed response {} (consumer gone or stalled)",
            id
          ),
          crate::state::PumpPushError::BacklogFull => warn!(
            "Dropping streamed response {}: backlog cap hit and the producing client honors no pause",
            id
          ),
        }
        self.state.response_streams.lock().await.remove(id);
        self.finish_stream_accounting(id).await;
      }
    }
  }

  /// Charges `n` streamed bytes to the shared counters: server stats, the
  /// organization's byte totals and the serving token's daily quota, a
  /// streamed response body would otherwise escape the quota that a buffered
  /// response is charged for. Organization and token come from this
  /// connection's own permissions, which is what the per-chunk `clients`
  /// lookup used to resolve to.
  pub(super) async fn flush_stream_bytes(&self, n: u64) {
    let mut stats = self.state.stats.lock().await;
    stats.total_bytes_transferred += n;
    drop(stats);
    self
      .state
      .persistent_stats
      .lock()
      .await
      .record_bytes_sent(n, self.perms.org_id.as_deref());
    self
      .state
      .add_token_bytes(self.perms.token_id.as_deref(), n)
      .await;
  }

  /// Drops a stream from the delivery cache and flushes whatever bytes it
  /// had not reported yet. Called wherever a stream ends: End, Abort, a
  /// failed push, a poisoned base64 chunk, and connection cleanup.
  pub(super) async fn finish_stream_accounting(&self, id: &str) {
    let pending = {
      let mut cache = self.stream_cache.lock().unwrap();
      cache.remove(id).map(|e| e.unreported).unwrap_or(0)
    };
    if pending > 0 {
      self.flush_stream_bytes(pending).await;
    }
  }

  /// Turns one incoming WebSocket message into the envelope text the
  /// dispatcher matches on, delivering v2 chunk frames on the spot and
  /// carrying a v5 full-response body out as bytes.
  pub(super) async fn decode_incoming(
    &self,
    msg: Message,
  ) -> (Option<String>, Option<axum::body::Bytes>) {
    let state = &self.state;
    let client_id = &self.client_id;
    let max_inflated = self.max_inflated;
    let _ = (state, client_id, max_inflated);
    // Set by a v5 full-response frame and taken by the `Response` arm: the
    // body that came as bytes rather than base64 inside the envelope.
    let mut full_body: Option<axum::body::Bytes> = None;
    let text_opt = match msg {
      Message::Text(t) => Some(t.as_str().to_string()),
      Message::Binary(b) => {
        // v2 binary chunk frames carry a tag byte that never collides
        // with zlib-compressed JSON frames (0x78).
        match decode_binary_frame(&b) {
          // The payload of a chunk frame is the tail of the message, and the
          // message is refcounted: every delivery below slices `b` instead
          // of copying out of it.
          Some((FRAME_RESPONSE_CHUNK, fid, payload)) => {
            let fid = fid.to_string();
            let payload = b.slice(b.len() - payload.len()..);
            self.deliver_response_chunk(&fid, payload).await;
            None
          }
          // v7: relay payloads as raw bytes. Same ownership checks as their
          // JSON shapes, which stay for older clients.
          Some((crate::protocol::FRAME_TCP_DATA, sid, payload)) => {
            let sid = sid.to_string();
            let payload = b.slice(b.len() - payload.len()..);
            self.deliver_tcp_bytes(&sid, payload).await;
            None
          }
          Some((crate::protocol::FRAME_UDP_DATAGRAM, sid, payload)) => {
            let sid = sid.to_string();
            let payload = b.slice(b.len() - payload.len()..);
            self.deliver_udp_bytes(&sid, payload).await;
            None
          }
          Some((crate::protocol::FRAME_WS_DATA_BIN, sid, payload)) => {
            let sid = sid.to_string();
            let payload = b.slice(b.len() - payload.len()..);
            self.deliver_ws_frame(&sid, Message::Binary(payload)).await;
            None
          }
          // v5: envelope and body in one frame, deflated or not. The
          // body is kept aside as bytes and picked up by the `Response`
          // arm below, which is the only place that knows what to do with
          // it; everything else about the message is the same JSON it
          // always was.
          Some((tag, _fid, payload))
            if tag == FRAME_RESPONSE_FULL || tag == FRAME_RESPONSE_FULL_ZLIB =>
          {
            // Deflated payloads are inflated first, bounded like every
            // other decompression here so a small frame cannot ask for an
            // unbounded allocation. A payload that will not inflate is a
            // corrupt frame: dropped, and the request behind it times out
            // rather than being answered with nonsense.
            let inflated = (tag == FRAME_RESPONSE_FULL_ZLIB)
              .then(|| crate::protocol::inflate_payload(payload, max_inflated));
            let payload = match &inflated {
              Some(Some(bytes)) => bytes.as_slice(),
              Some(None) => {
                warn!(
                  "Client {} sent a full-response frame that would not inflate; dropping it",
                  client_id
                );
                &[][..]
              }
              None => payload,
            };
            match crate::protocol::split_full_response(payload) {
              Some((json, body)) => {
                // A slice of the message that arrived, not a copy of it:
                // `b` is refcounted, and the body is the tail of it. The
                // inflated case has no such backing, so it hands over the
                // buffer it just built.
                full_body = Some(match &inflated {
                  Some(_) => axum::body::Bytes::copy_from_slice(body),
                  None => {
                    let start = b.len() - body.len();
                    b.slice(start..)
                  }
                });
                Some(json.to_string())
              }
              None => {
                warn!(
                  "Client {} sent a malformed full-response frame ({} bytes); dropping it",
                  client_id,
                  b.len()
                );
                None
              }
            }
          }
          _ => decompress_frame(&b, max_inflated),
        }
      }
      _ => None,
    };
    (text_opt, full_body)
  }

  /// Handles the buffered answer to one proxied request.
  pub(super) async fn on_response(&self, msg: TunnelMessage, body_raw: Option<axum::body::Bytes>) {
    let TunnelMessage::Response {
      id,
      status,
      headers,
      body,
      trailers,
      timings,
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

    let mut pending = state.pending_requests.lock().await;
    // Verify that this response originates from the client that was
    // assigned the request. Prevents a malicious tunnel client from
    // injecting spoofed responses for another client's requests.
    let is_owner = pending
      .get(&id)
      .is_some_and(|req| req.client_id == *client_id);
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
          body_raw,
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

  /// Handles the head of a streamed response.
  pub(super) async fn on_response_start(&self, msg: TunnelMessage) {
    let TunnelMessage::ResponseStart {
      id,
      status,
      headers,
      timings,
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

    let mut pending = state.pending_requests.lock().await;
    let is_owner = pending
      .get(&id)
      .is_some_and(|req| req.client_id == *client_id);
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
      // The read loop feeds the pump, never the visitor's channel
      // directly, so a visitor that stops reading cannot stall the
      // other streams sharing this tunnel; past the byte watermark
      // the client is asked to pause producing this stream.
      let flow = crate::state::StreamFlow::new(
        id.clone(),
        tx_write.clone(),
        state.client_supports_pause(client_id).await,
        state.stream_limits(),
      );
      let chunk_tx = spawn_consumer_pump(chunk_tx, state.config().gateway_response_timeout, flow);
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
          body_raw: None,
          trailers: None,
          stream_rx: Some(chunk_rx),
          timings,
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

  /// Handles a base64 body chunk (pre-v2 clients).
  pub(super) async fn on_response_chunk(&self, msg: TunnelMessage) {
    let TunnelMessage::ResponseChunk { id, data } = msg else {
      return;
    };
    let state = &self.state;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    // Base64 fallback path; v2 clients send raw binary frames.
    use base64::prelude::*;
    match BASE64_STANDARD.decode(&data) {
      Ok(bytes) => self.deliver_response_chunk(&id, bytes.into()).await,
      Err(_) => {
        warn!("Failed to decode Base64 ResponseChunk for request {}", id);
        state.response_streams.lock().await.remove(&id);
        self.finish_stream_accounting(&id).await;
      }
    }
  }

  /// Handles the end of a streamed response, with optional trailers.
  pub(super) async fn on_response_end(&self, msg: TunnelMessage) {
    let TunnelMessage::ResponseEnd { id, trailers } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    // Dropping the sender ends the public body stream; trailers
    // (e.g. gRPC's grpc-status) are delivered as the final frame.
    let owned = take_owned_stream(&state.response_streams, &id, client_id, |h| &h.client_id).await;
    match owned {
      Some(handle) => {
        if let Some(trailers) = trailers {
          let _ = handle
            .tx
            .push(Ok(crate::state::BodyFrame::Trailers(trailers)));
        }
      }
      None => debug!(
        "ResponseEnd for request ID {} ignored: unknown or not owned by client {}",
        id, client_id
      ),
    }
    // Settle the byte accounting either way: the cache only ever holds
    // streams this connection owns, so a foreign id is a no-op here.
    self.finish_stream_accounting(&id).await;
  }

  /// Handles a stream the backend failed mid-way.
  pub(super) async fn on_response_abort(&self, msg: TunnelMessage) {
    let TunnelMessage::ResponseAbort { id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    // Drop the visitor's body stream with an error so an incomplete
    // response (size limit hit, or backend errored mid-stream) is not
    // delivered as a clean success. The error propagates through the
    // body, terminating the visitor connection abnormally.
    let owned = take_owned_stream(&state.response_streams, &id, client_id, |h| &h.client_id).await;
    match owned {
      Some(handle) => {
        let _ = handle.tx.push(Err(std::io::Error::other(
          "response aborted by client (size limit or backend error)",
        )));
      }
      None => debug!(
        "ResponseAbort for request ID {} ignored: unknown or not owned by client {}",
        id, client_id
      ),
    }
    // Same settling as ResponseEnd: charge what the stream had delivered.
    self.finish_stream_accounting(&id).await;
  }
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;
