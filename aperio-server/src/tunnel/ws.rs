use axum::{
  extract::{
    ConnectInfo, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use futures_util::{sink::SinkExt, stream::StreamExt};

/// Handshake header carrying the parallel-connection ceiling for the token
/// that just connected. The client sizes its fan of connections from it, so
/// the number lives on the server where the resource is.
pub(crate) const MAX_CONNECTIONS_HEADER: &str = "x-aperio-max-connections";

/// The visitor-auth methods a client may declare, announced on the handshake
/// response (`planned_features.md` #111).
///
/// It is announced *here*, on the upgrade response, rather than in the `Pong`,
/// and that placement is the whole safety of the feature. A client reads it
/// before it has sent anything, so a client whose `auth:` needs a method this
/// server does not know can leave without ever declaring a service. On a
/// server too old to send the header at all there is nothing to read, which
/// is the same answer: an absent header means "assume only the two that
/// always travelled". Negotiated rather than assumed, which is what rule 23
/// asks for, and the alternative, sending a rich policy to a server that
/// silently ignores it, would bring the route up with no gate at all.
pub(crate) const VISITOR_AUTH_METHODS_HEADER: &str = "x-aperio-visitor-auth-methods";

/// The methods a *client* may declare, which is not the whole set.
///
/// `forward` is deliberately absent. The URL would be called by the server,
/// from the server's network, so a client writing `localhost:7070` would mean
/// the server's localhost and not its own: a footgun whose safe version
/// carries the check over the tunnel, and that is a feature rather than a
/// field (#111).
pub(crate) const CLIENT_DECLARABLE_METHODS: &[&str] = &["none", "basic", "bearer", "jwt"];
/// Other servers a client may fall back to (planned_features #52),
/// comma-separated.
pub(crate) const ALTERNATE_SERVERS_HEADER: &str = "x-aperio-alternate-servers";

/// Most alternates a server will announce.
///
/// A fence rather than a policy: clients try this list in rotation, so a long
/// one turns every reconnect into a walk through addresses nobody chose.
pub(crate) const MAX_ALTERNATES: usize = 8;

/// Reads `alternate_servers` from its comma-separated spelling.
///
/// Only the two schemes a tunnel is dialed with survive. The list is announced
/// to every client and tried by every client, so a typo here reaches further
/// than most, and dropping what cannot possibly be a tunnel URL is cheaper
/// than every client discovering it one reconnect at a time.
pub(crate) fn parse_alternates(raw: &str) -> Vec<String> {
  raw
    .split(',')
    .map(str::trim)
    .filter(|v| v.starts_with("ws://") || v.starts_with("wss://"))
    .map(str::to_string)
    .take(MAX_ALTERNATES)
    .collect()
}
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, mpsc};
use tracing::{debug, error, info, warn};

use crate::auth::authorize_tunnel_token;
use crate::protocol::{
  FRAME_RESPONSE_CHUNK, FRAME_RESPONSE_FULL, FRAME_RESPONSE_FULL_ZLIB, PROTOCOL_VERSION,
  TunnelMessage, compress_frame, decode_binary_frame, decompress_frame,
};
use crate::routing::{
  extract_client_ip, normalize_hostname_bind, normalize_path_bind, random_subdomain_hostname,
  random_subdomain_hostname_seeded,
};
use crate::state::{
  AppState, ClientHandle, ClientPerms, ResponseStreamHandle, TcpConsumerMsg, TunnelResponse,
  WsStreamMessage, spawn_consumer_pump,
};

/// Bind context captured for the autoscaling upsert: the hostnames this
/// connection serves, its path bind, its organization, and the token that
/// armed it.
/// The scaling declaration and the binds it was captured for. The
/// declaration travels with the context so a later service's `scaling:`
/// cannot be paired with the first service's block, or with none at all.
type ScalingBindCtx = (
  crate::protocol::ScalingDecl,
  Vec<String>,
  Option<String>,
  Option<String>,
  Option<String>,
);

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;

/// Removes a stream from `map`, but only if `client_id` owns it, the check
/// and the removal happen under a single lock.
///
/// The frame handlers used to remove first and put the handle back when the
/// sender turned out not to own it. That left a window in which the genuine
/// owner's frames looked up a stream that was momentarily absent and were
/// dropped without a trace, so one client sending a bogus stream id could make
/// another client's response lose data.
async fn take_owned_stream<T>(
  map: &tokio::sync::Mutex<std::collections::HashMap<String, T>>,
  id: &str,
  client_id: &str,
  owner_of: impl Fn(&T) -> &str,
) -> Option<T> {
  let mut streams = map.lock().await;
  match streams.get(id) {
    Some(handle) if owner_of(handle) == client_id => streams.remove(id),
    _ => None,
  }
}

/// One stream this connection is feeding: the pump sender it already proved
/// ownership of, plus bytes delivered but not yet flushed to the shared
/// counters. Lives in the connection's `stream_cache` so the per-chunk hot
/// path touches no global lock (planned_features #23).
struct StreamCacheEntry {
  tx: crate::state::PumpedSender<Result<crate::state::BodyFrame, std::io::Error>>,
  unreported: u64,
}

/// Byte-accounting batch size for streamed responses: the shared counters
/// (server stats, org bytes, token daily quota) are updated once per this
/// many delivered bytes rather than once per chunk, and the remainder is
/// flushed when the stream ends. Quotas are megabytes-to-gigabytes sized, so
/// lagging a stream's charge by under this much changes no decision.
const STREAM_ACCOUNT_FLUSH_BYTES: u64 = 1024 * 1024;

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
  // Rate-limit before token verification, matching the sibling /aperio/tcp and
  // /aperio/udp endpoints. Without this, repeated upgrade attempts from one IP
  // allow unbounded token brute-force and canary/webhook event spam.
  if !state.check_rate_limit(tunnel_client_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
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

  // The pairing itself, before a socket exists (#113). A client too old for
  // this server is refused here, where the cause is the answer, rather than
  // being allowed to establish and then fail somewhere three layers deeper
  // for a reason nothing connects back to its version. A client that
  // announces nothing is admitted: silence predates the header and is inside
  // the documented window, and reading it as age would take a fleet down on
  // the upgrade that introduced this.
  if let Some(refused) = aperio_config::pairing::check(
    headers
      .get(aperio_config::pairing::CLIENT_RELEASE_HEADER)
      .and_then(|v| v.to_str().ok()),
    aperio_config::pairing::MIN_SUPPORTED_CLIENT,
    aperio_config::pairing::Side::Client,
  ) {
    let msg = refused.message();
    warn!("Tunnel connection refused from {addr}: {msg}");
    // 426 says what happened in the status as well as the body: this is not
    // the token, not the quota, not the server being busy.
    return (StatusCode::UPGRADE_REQUIRED, msg).into_response();
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
  let slot = TunnelSlot {
    state: state.clone(),
    armed: true,
  };

  // Process-wide instance group (the client's raw `client_id` base): groups a
  // process's parallel connections in the dashboard and shares one random
  // hostname across them. Optional, older clients omit it.
  let instance_group = headers
    .get("x-aperio-instance")
    .and_then(|v| v.to_str().ok())
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

  // The parallel-connection ceiling in force for this token, announced on the
  // handshake so the client can size itself instead of opening sockets the
  // server will only close. Read by aperio-client; older clients ignore it and
  // keep their own default, which is this same 16.
  let ceiling = perms.connection_ceiling(state.config().max_connections_per_service);
  // Read before the state is moved into the upgrade callback below.
  let alternates = state.config().alternate_servers.join(",");
  // And before `perms` is: what this connection may declare is a property of
  // its token, not of the build.
  let may_declare_gate = perms.allow_public;

  // Use saturating arithmetic to prevent usize overflow with very large max_body_size.
  let mut response = ws
    .max_message_size(state.config().max_body_size.saturating_mul(2))
    .max_frame_size(state.config().max_body_size)
    .on_upgrade(move |socket| {
      slot.handed_off();
      handle_socket(
        socket,
        tunnel_client_ip.to_string(),
        state,
        perms,
        instance_group,
      )
    });
  if let Ok(value) = axum::http::HeaderValue::from_str(&ceiling.to_string()) {
    response.headers_mut().insert(MAX_CONNECTIONS_HEADER, value);
  }
  // What this server is and what it accepts, so the other direction of the
  // window can be judged by the only side able to judge it: a server cannot
  // know it is too old for something a future client wants, but the client
  // can, and it now has the number to compare against (#113).
  if let Ok(value) = axum::http::HeaderValue::from_str(env!("CARGO_PKG_VERSION")) {
    response
      .headers_mut()
      .insert(aperio_config::pairing::SERVER_RELEASE_HEADER, value);
  }
  if let Ok(value) = axum::http::HeaderValue::from_str(aperio_config::pairing::MIN_SUPPORTED_CLIENT)
  {
    response
      .headers_mut()
      .insert(aperio_config::pairing::MIN_CLIENT_HEADER, value);
  }
  // What this connection may declare, which is not the same as what this
  // build supports: controlling the visitor gate is a token permission, and a
  // token without it has its declaration dropped on the Ping. Announcing the
  // full list to such a token would be the server contradicting itself one
  // message later, and the client, having been told its gate was accepted,
  // would serve the route believing it closed. So the answer is the empty
  // list, which the negotiation already understands as "nothing may be
  // declared here" and refuses on, rather than an announcement that is true of
  // the build and false of this connection.
  let declarable = if may_declare_gate {
    CLIENT_DECLARABLE_METHODS.join(",")
  } else {
    String::new()
  };
  if let Ok(value) = axum::http::HeaderValue::from_str(&declarable) {
    response
      .headers_mut()
      .insert(VISITOR_AUTH_METHODS_HEADER, value);
  }
  // Announced on every handshake rather than once: a migration is set up by
  // editing this server, and a client that reconnects should pick up the new
  // list without anybody restarting it.
  if !alternates.is_empty()
    && let Ok(value) = axum::http::HeaderValue::from_str(&alternates)
  {
    response
      .headers_mut()
      .insert(ALTERNATE_SERVERS_HEADER, value);
  }
  response
}

/// The service a declared connection id belongs to.
///
/// The client names its connections `<base>-<service>` for the first of a
/// service and `<base>-<service>-c<N>` for the rest, so trimming the `-c<N>`
/// suffix leaves the service. Anything that does not follow the shape is its
/// own service, which is the safe reading: an unrecognized id is not silently
/// merged into somebody else's fan.
fn service_of(declared_id: &str) -> &str {
  match declared_id.rsplit_once("-c") {
    Some((base, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => base,
    _ => declared_id,
  }
}

/// True when this connection is one too many for its service.
///
/// Counts the live connections of the same process (`instance_group`) that
/// declared the same service, excluding this one. Grouping on the process
/// matters: two clients that happen to choose the same `client_id` base are
/// still two processes, and neither should be able to spend the other's
/// allowance.
fn service_connection_over_ceiling(
  clients: &std::collections::HashMap<String, ClientHandle>,
  own_connection_id: &str,
  instance_group: Option<&str>,
  declared_id: &str,
  ceiling: u32,
) -> bool {
  let service = service_of(declared_id);
  let siblings = clients
    .iter()
    .filter(|(id, handle)| {
      id.as_str() != own_connection_id
        && handle.instance_group.as_deref() == instance_group
        && handle
          .declared_client_id
          .as_deref()
          .is_some_and(|other| service_of(other) == service)
    })
    .count();
  siblings as u32 >= ceiling
}

/// Holds the `active_tunnel_count` slot reserved before the upgrade, and gives
/// it back if the upgrade never happens.
///
/// Only `handle_socket` releases the slot, and axum drops the `on_upgrade`
/// callback without ever calling it when the connection dies during the
/// handshake. Each such failed handshake would otherwise raise the counter
/// permanently, until it reaches `max_tunnels` and every new tunnel is refused
/// with 503 for the rest of the server's life.
struct TunnelSlot {
  state: Arc<AppState>,
  armed: bool,
}

impl TunnelSlot {
  /// The upgrade callback ran: `handle_socket` owns the slot from here on.
  fn handed_off(mut self) {
    self.armed = false;
  }
}

impl Drop for TunnelSlot {
  fn drop(&mut self) {
    if self.armed {
      self
        .state
        .active_tunnel_count
        .fetch_sub(1, Ordering::SeqCst);
    }
  }
}

/// The writer task's one transformation: an outgoing frame, compressed the
/// way this connection negotiated. Text frames deflate whole; a v6
/// full-request frame is binary, so the text path above never sees it, and
/// it is re-encoded under its zlib tag here, only when deflating wins, so
/// the negotiated flag lives in one place. Everything else passes through.
pub(crate) fn writer_transform(msg: Message, compress: bool) -> Message {
  if !compress {
    return msg;
  }
  match msg {
    Message::Text(t) => Message::Binary(compress_frame(&t).into()),
    Message::Binary(b) if b.first() == Some(&crate::protocol::FRAME_REQUEST_FULL) => {
      match crate::protocol::decode_binary_frame(&b) {
        Some((_, id, payload)) => match crate::protocol::deflate_payload(payload) {
          Some(deflated) => match crate::protocol::encode_binary_frame(
            crate::protocol::FRAME_REQUEST_FULL_ZLIB,
            id,
            &deflated,
          ) {
            Some(frame) => Message::Binary(frame.into()),
            None => Message::Binary(b),
          },
          None => Message::Binary(b),
        },
        None => Message::Binary(b),
      }
    }
    other => other,
  }
}

/// The writer task's bandwidth shaper: a token bucket with a one-second
/// burst, average rate = the client's announced capacity. Frames larger than
/// the burst drive the bucket negative and pay the remainder as sleep time,
/// which this returns rather than sleeps, so the arithmetic is testable on a
/// clock the test controls.
pub(crate) struct SendPacer {
  tokens: f64,
  refilled_at: Instant,
}

impl SendPacer {
  pub(crate) fn new(now: Instant) -> Self {
    SendPacer {
      tokens: 0.0,
      refilled_at: now,
    }
  }

  /// Spends `size` bytes at `rate` bytes/second; `Some(delay)` when the
  /// bucket went negative and the writer owes that much time.
  pub(crate) fn debt(&mut self, size: usize, rate: u64, now: Instant) -> Option<Duration> {
    let rate = rate as f64;
    self.tokens =
      (self.tokens + now.duration_since(self.refilled_at).as_secs_f64() * rate).min(rate);
    self.refilled_at = now;
    self.tokens -= size as f64;
    (self.tokens < 0.0).then(|| Duration::from_secs_f64(-self.tokens / rate))
  }
}

/// Everything one connection's message handlers share, so each arm of the
/// read loop can be a named function instead of a page of an eleven-hundred
/// line match. The loop that remains only decodes and dispatches
/// (planned_features #21).
struct ConnCtx {
  state: Arc<AppState>,
  client_id: String,
  client_ip: String,
  tx_write: mpsc::Sender<Message>,
  compress_out: Arc<AtomicBool>,
  perms: ClientPerms,
  server_max_connections: u32,
  max_inflated: usize,
  /// Streams this connection has delivered chunks to, keyed by request id.
  /// A `std` mutex: only this connection's read loop touches it, and it is
  /// never held across an await.
  stream_cache: std::sync::Mutex<HashMap<String, StreamCacheEntry>>,
}

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
  async fn deliver_response_chunk(&self, id: &str, bytes: axum::body::Bytes) {
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
  async fn flush_stream_bytes(&self, n: u64) {
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
  async fn finish_stream_accounting(&self, id: &str) {
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
  async fn decode_incoming(&self, msg: Message) -> (Option<String>, Option<axum::body::Bytes>) {
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
  async fn on_response(&self, msg: TunnelMessage, body_raw: Option<axum::body::Bytes>) {
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
  async fn on_response_start(&self, msg: TunnelMessage) {
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
  async fn on_response_chunk(&self, msg: TunnelMessage) {
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
  async fn on_response_end(&self, msg: TunnelMessage) {
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
  async fn on_response_abort(&self, msg: TunnelMessage) {
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

  /// Handles bytes of a TCP relay stream.
  async fn on_tcp_data(&self, msg: TunnelMessage) {
    let TunnelMessage::TcpData { stream_id, data } = msg else {
      return;
    };
    // Base64 fallback path; a v7 client sends FRAME_TCP_DATA binary frames.
    use base64::prelude::*;
    match BASE64_STANDARD.decode(&data) {
      Ok(bytes) => self.deliver_tcp_bytes(&stream_id, bytes.into()).await,
      Err(_) => {
        warn!("Failed to decode Base64 TcpData for stream {}", stream_id);
      }
    }
  }

  /// Delivers one chunk of a TCP relay this client owns, however it arrived
  /// (base64 in JSON, or a v7 binary frame).
  async fn deliver_tcp_bytes(&self, stream_id: &str, bytes: axum::body::Bytes) {
    let state = &self.state;
    let client_id = &self.client_id;
    let consumer_tx = {
      let streams = state.tcp_streams.lock().await;
      match streams.get(stream_id) {
        Some(h) if h.client_id == *client_id => Some(h.tx.clone()),
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
      // Non-blocking, like the HTTP chunk path: the stream's pump waits on a
      // slow consumer so this loop does not, and its flow control pauses the
      // producer if need be.
      if let Err(e) = consumer_tx.push(TcpConsumerMsg::Data(bytes)) {
        debug!("Dropping TCP stream {} ({:?})", stream_id, e);
        state.tcp_streams.lock().await.remove(stream_id);
      }
    }
  }

  async fn on_tcp_close(&self, msg: TunnelMessage) {
    let TunnelMessage::TcpClose { stream_id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    if let Some(h) =
      take_owned_stream(&state.tcp_streams, &stream_id, client_id, |h| &h.client_id).await
    {
      let _ = h.tx.push(TcpConsumerMsg::Close);
    }
  }

  /// Handles one datagram of a UDP relay.
  async fn on_udp_datagram(&self, msg: TunnelMessage) {
    let TunnelMessage::UdpDatagram { stream_id, data } = msg else {
      return;
    };
    // Base64 fallback path; a v7 client sends FRAME_UDP_DATAGRAM frames.
    use base64::prelude::*;
    match BASE64_STANDARD.decode(&data) {
      Ok(bytes) => self.deliver_udp_bytes(&stream_id, bytes.into()).await,
      Err(_) => {
        warn!(
          "Failed to decode Base64 UdpDatagram for stream {}",
          stream_id
        );
      }
    }
  }

  /// Delivers one relayed datagram this client owns, however it arrived.
  async fn deliver_udp_bytes(&self, stream_id: &str, bytes: axum::body::Bytes) {
    let state = &self.state;
    let client_id = &self.client_id;
    let consumer_tx = {
      let streams = state.udp_streams.lock().await;
      match streams.get(stream_id) {
        Some(h) if h.client_id == *client_id => Some(h.tx.clone()),
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
      // Best-effort: a congested consumer drops datagrams.
      if let Err(mpsc::error::TrySendError::Closed(_)) =
        consumer_tx.try_send(TcpConsumerMsg::Data(bytes))
      {
        state.udp_streams.lock().await.remove(stream_id);
      }
    }
  }

  async fn on_udp_close(&self, msg: TunnelMessage) {
    let TunnelMessage::UdpClose { stream_id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    if let Some(h) =
      take_owned_stream(&state.udp_streams, &stream_id, client_id, |h| &h.client_id).await
    {
      let _ = h.tx.try_send(TcpConsumerMsg::Close);
    }
  }

  /// Handles topic filters this process wants; refusals are reported back.
  async fn on_subscribe(&self, msg: TunnelMessage) {
    let TunnelMessage::Subscribe { topics } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    let refused = crate::tunnel::pubsub::set_subscriptions(state, client_id, topics, true).await;
    for (topic, why) in refused {
      warn!("Client {client_id} cannot subscribe to '{topic}': {why}");
      // Tell the client too. A refusal only the server can see
      // leaves the other operator watching a subscription that
      // never delivers, which looks exactly like a topic nobody
      // publishes on.
      let notice = TunnelMessage::SubscribeRefused { topic, reason: why };
      if let Ok(json) = serde_json::to_string(&notice) {
        let clients = state.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
          let _ = handle.tx.try_send(Message::Text(json.into()));
        }
      }
    }
  }

  /// Handles topic filters this process no longer wants.
  async fn on_unsubscribe(&self, msg: TunnelMessage) {
    let TunnelMessage::Unsubscribe { topics } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    crate::tunnel::pubsub::set_subscriptions(state, client_id, topics, false).await;
  }

  /// Handles a QoS 1 delivery acknowledged.
  async fn on_publish_ack(&self, msg: TunnelMessage) {
    let TunnelMessage::PublishAck { id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    crate::tunnel::pubsub::acknowledge(state, client_id, &id).await;
  }

  /// Handles a message published by this client into its organization.
  async fn on_publish(&self, msg: TunnelMessage) {
    let TunnelMessage::Publish {
      topic,
      payload,
      qos,
      ..
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

    // Every refusal is reported back, not only logged: the local
    // application that sent this was told the message was accepted
    // the moment it reached the client, so the server's log is the
    // only other place the truth exists.
    let refusal = if !crate::tunnel::pubsub::may_use_topic(perms, &topic) {
      Some("the token does not carry this topic; add it to the token's topics".to_string())
    } else {
      use base64::prelude::*;
      match BASE64_STANDARD.decode(&payload) {
        Err(e) => Some(format!("the payload is not valid Base64: {e}")),
        Ok(bytes) => crate::tunnel::pubsub::publish(
          state,
          perms.org_id.as_deref(),
          &topic,
          &bytes,
          crate::tunnel::pubsub::Publisher::Client(client_id),
          qos,
        )
        .await
        .err(),
      }
    };
    if let Some(why) = refusal {
      warn!("Client {client_id} cannot publish to '{topic}': {why}");
      let notice = TunnelMessage::PublishRefused { topic, reason: why };
      if let Ok(json) = serde_json::to_string(&notice) {
        let clients = state.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
          let _ = handle.tx.try_send(Message::Text(json.into()));
        }
      }
    }
  }

  /// Handles the heartbeat and full service announcement.
  async fn on_ping(&self, msg: TunnelMessage) -> bool {
    let TunnelMessage::Ping {
      services,
      client_id: cid,
      timestamp,
      visitor_auth_methods,
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
      service_custom_name,
      public,
      visitor_auth,
      allowed_ips,
      tunnels,
      cache,
      resilience,
      no_capture,
      max_request_body,
      response_timeout,
      client_key,
      webhook_inbox,
      denied,
      scaling,
      connections,
      connections_min,
      connections_max,
      config_notes,
      metrics_labels,
      drain_secs,
      cpu_percent,
      rss_bytes,
      rtt_ms,
      jitter_ms,
      reconnects,
    } = msg
    else {
      return true;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    debug!("Heartbeat from client {}: {}", cid, timestamp);

    // The one place that says what a Ping describes (#46). Today it is always
    // exactly one service, whichever spelling arrived, and the rest of this
    // function reads that one. When the server learns to serve several, this
    // is the line that stops being a `[0]`.
    //
    // A list of more than one is refused rather than half-served. The server
    // cannot route to a second service yet, so accepting the declaration
    // would be a connection that establishes and then serves less than it was
    // told to, which is the failure mode the whole protocol-gate work exists
    // to prevent.
    // A list of several is served now. What used to be refused here, and the
    // reason the refusal stood so long, was that routing, ejection and the
    // per-service state all keyed on the connection; each of those was moved
    // onto the service before this line was allowed to change.
    //
    // An empty list is still nothing to serve. A client saying so means it
    // has withdrawn everything, which is a disconnect written the long way,
    // and treating it as "no list" would silently keep serving what it just
    // retired.
    let declared_services = match services {
      Some(list) if list.is_empty() => {
        warn!(
          "Client {} declared an empty service list; refusing rather than going on serving \
           what it just said it no longer offers",
          cid
        );
        return false;
      }
      Some(list) => list,
      None => Vec::new(),
    };
    // The names, taken before the list is consumed below: they are what says
    // which existing service each declaration is about.
    let declared_names: Vec<Option<String>> = declared_services
      .iter()
      .map(|d| d.service.clone())
      .collect();

    // The list is authoritative when it is there, which is the promise the
    // protocol makes and the reason the two spellings can never half-agree:
    // a client that describes its work as a list has said everything about
    // that service inside the entry, so reading a singular field alongside
    // it would let a value the client did not write win over one it did.
    // Absent, every singular field stands exactly as before.
    //
    // Written as one binding rather than a field-by-field merge on purpose.
    // A merge is where a field gets forgotten, and forgetting one here is
    // silent: the entry's value is dropped and the singular default takes
    // its place, so the service comes up with a setting the client did not
    // ask for and no error anywhere.
    // The declarations this Ping is about, as `ServiceDecl` values rather
    // than as a tuple of loose locals.
    //
    // A client that sent no list still described exactly one service, in the
    // singular fields, so it becomes one entry here. That is what lets the
    // rest of this function talk about *a declaration* without caring which
    // spelling it arrived in, and what lets it eventually talk about several.
    let declarations: Vec<crate::protocol::ServiceDecl> = if declared_services.is_empty() {
      vec![crate::protocol::ServiceDecl {
        service,
        service_custom_name,
        path_bind,
        hostname_bind,
        hostname_binds,
        max_concurrent,
        tcp,
        backend_healthy,
        backend_probed,
        priority,
        bandwidth_bps,
        public,
        visitor_auth,
        visitor_auth_methods,
        allowed_ips,
        tunnels,
        cache,
        resilience,
        no_capture,
        max_request_body,
        response_timeout,
        webhook_inbox,
        denied,
        scaling,
        connections,
        connections_min,
        connections_max,
        config_notes,
        metrics_labels,
      }]
    } else {
      declared_services
    };

    // Update client's reported binds and heartbeat time. Only the
    // server-assigned connection ID is trusted for state updates;
    // the client-declared `cid` is ignored to prevent a client from
    // mutating another connection's state.
    // Token pinning context captured under the clients lock and used
    // after it is released: (token id, token name, org).
    let mut pin_ctx: Option<(String, String, Option<String>)> = None;
    // Set under the clients lock, acted on after it: a connection
    // beyond what the token is allowed for this service.
    let mut over_ceiling = false;
    let mut ceiling_ctx: Option<(Option<String>, u32)> = None;
    // Bind context for the autoscaling upsert, captured under the
    // clients lock and used after it is released (the scaling store
    // must never be locked while the clients map is).
    let mut scaling_ctx: Option<ScalingBindCtx> = None;
    {
      let mut clients = state.clients.write().await;
      if let Some(handle) = clients.get_mut(client_id) {
        // Which of this connection's services the declaration is about.
        //
        // Without a list there is nothing to match: the singular fields have
        // always described the connection's one service, and still do. With
        // one, the answer comes from `match_declarations` rather than from
        // position, so a client that names its services keeps each one's
        // ejection state, warn-once flags and counters across a heartbeat
        // even if it reorders them.
        // Which of this connection's services each declaration is about,
        // decided once for the whole Ping before any of them is applied.
        let indexes: Vec<usize> = if declared_names.is_empty() {
          vec![0]
        } else {
          match crate::state::match_declarations(&handle.services, &declared_names) {
            // Unreachable while the length refusal above stands: two
            // declarations are needed to repeat a name, and a list of two is
            // turned away before the names are read. Kept, and said out loud
            // rather than left to be found: it is what stops a duplicate
            // becoming a silent merge on the day the length refusal goes, and
            // that day is the point of this entry.
            Err(name) => {
              warn!(
                "Client {} declared two services both named '{}'; refusing the connection \
                 rather than guessing which one it meant",
                cid, name
              );
              return false;
            }
            // A declaration this connection does not carry means the client
            // changed what the connection serves while it was open. Refused
            // for the same reason a second service is: the server would have
            // to decide what becomes of the old service's ejection state and
            // statistics, and every answer to that is a guess. Reconnecting
            // is unambiguous and is what the client already does on a config
            // change.
            // Reconcile: the connection ends up carrying exactly what the
            // Ping declared, in the order it declared it. A service that
            // matched keeps everything the wire does not carry, which is the
            // point of matching by name; one this connection has just gained
            // starts fresh; one no longer declared is dropped by not being
            // moved into the new list.
            //
            // Rebuilt rather than patched in place because removing an entry
            // shifts every index after it, and the indexes are what the rest
            // of this function writes through.
            Ok(matched) => {
              let pacer_cell = handle
                .services
                .first()
                .map(|s| s.bandwidth_bps.clone())
                .unwrap_or_default();
              let mut old: Vec<Option<crate::state::ServiceState>> =
                handle.services.drain(..).map(Some).collect();
              let mut fresh = Vec::with_capacity(matched.len());
              for m in &matched {
                let carried = m
                  .and_then(|i| old.get_mut(i).and_then(Option::take))
                  .unwrap_or_else(|| {
                    // The connection's own cell, so a service added mid-flight
                    // announces into the one the writer actually reads.
                    crate::state::ServiceState::newly_declared(pacer_cell.clone())
                  });
                fresh.push(carried);
              }
              let retired = old.iter().filter(|s| s.is_some()).count();
              if retired > 0 {
                info!(
                  "Client {} stopped declaring {} service(s); they leave routing",
                  cid, retired
                );
              }
              handle.services = fresh;
              (0..matched.len()).collect()
            }
          }
        };

        // One pass per declaration. The body below describes a single
        // service, which is what it always did; what changed is that it now
        // says *which*, and that saying it more than once is a matter of
        // going round again rather than of rewriting any of it.
        for (declaration, service_index) in declarations.iter().zip(indexes) {
          let d0 = declaration.clone();
          let (
            service,
            service_custom_name,
            path_bind,
            hostname_bind,
            hostname_binds,
            max_concurrent,
            tcp,
            backend_healthy,
            backend_probed,
            priority,
            bandwidth_bps,
            public,
            visitor_auth,
            visitor_auth_methods,
            allowed_ips,
            tunnels,
            cache,
            resilience,
            no_capture,
            max_request_body,
            response_timeout,
            webhook_inbox,
            denied,
            scaling,
            connections,
            connections_min,
            connections_max,
            config_notes,
            metrics_labels,
          ) = (
            d0.service,
            d0.service_custom_name,
            d0.path_bind,
            d0.hostname_bind,
            d0.hostname_binds,
            d0.max_concurrent,
            d0.tcp,
            d0.backend_healthy,
            d0.backend_probed,
            d0.priority,
            d0.bandwidth_bps,
            d0.public,
            d0.visitor_auth,
            d0.visitor_auth_methods,
            d0.allowed_ips,
            d0.tunnels,
            d0.cache,
            d0.resilience,
            d0.no_capture,
            d0.max_request_body,
            d0.response_timeout,
            d0.webhook_inbox,
            d0.denied,
            d0.scaling,
            d0.connections,
            d0.connections_min,
            d0.connections_max,
            d0.config_notes,
            d0.metrics_labels,
          );
          let normalized_path = path_bind.and_then(|b| normalize_path_bind(&b));
          let normalized_host = hostname_bind.and_then(|h| normalize_hostname_bind(&h));

          // Declared binds must be permitted by the token used to connect.
          if let Some(p) = normalized_path {
            if handle.perms.path_allowed(&p) {
              handle.service_at_mut(service_index).declared_path = Some(p);
            } else {
              warn!(
                "Client {} declared path bind {} not permitted by its token; ignored",
                client_id, p
              );
            }
          }
          if let Some(h) = normalized_host {
            if handle.perms.hostname_allowed(&h) {
              handle.service_at_mut(service_index).declared_hostname = Some(h);
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
            handle.service_at_mut(service_index).declared_hostnames = admitted;
          }
          // Create the concurrency limiter on the first Ping that
          // announces a limit; the limit is fixed for the connection.
          if handle.service_at(service_index).inflight_limiter.is_none()
            && let Some(n) = max_concurrent
            && n > 0
          {
            handle.service_at_mut(service_index).max_concurrent = Some(n);
            // Clamp to the semaphore's permit ceiling: a client
            // announcing an absurd limit must not panic Semaphore::new
            // (its max is below u32::MAX on 32-bit targets).
            let permits = (n as usize).min(Semaphore::MAX_PERMITS);
            handle.service_at_mut(service_index).inflight_limiter =
              Some(Arc::new(Semaphore::new(permits)));
            info!(
              "Client {} announced concurrency limit: {}, excess requests will be queued",
              client_id, n
            );
          }
          handle.service_at_mut(service_index).tcp_enabled = tcp;
          if handle.service_at(service_index).cache != cache {
            handle.service_at_mut(service_index).cache = cache;
            if cache {
              info!(
                "Client {} opted into the server-side response cache",
                client_id
              );
            }
          }
          // The service asked to be cached but the server's cache is
          // off, so the opt-in silently does nothing, warn once so the
          // operator can enable APERIO_CACHE (or the owner can drop the
          // flag). Surfaced in the dashboard as a badge too.
          if cache
            && !state.config().cache_enabled
            && !handle.service_at(service_index).cache_ignored_warned
          {
            handle.service_at_mut(service_index).cache_ignored_warned = true;
            warn!(
              "Client {} requested response caching (cache: true) but the server cache is disabled (APERIO_CACHE off); the opt-in is ignored",
              client_id
            );
          }
          if handle.service_at(service_index).max_request_body != max_request_body {
            handle.service_at_mut(service_index).max_request_body = max_request_body;
            if let Some(limit) = max_request_body {
              info!(
                "Client {} declared a request body cap of {} bytes; bigger uploads are rejected with 413 before dispatch",
                client_id, limit
              );
            }
          }
          if handle.service_at(service_index).response_timeout != response_timeout {
            handle.service_at_mut(service_index).response_timeout = response_timeout;
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
          if handle.service_at(service_index).denied != denied {
            if let Some(url) = &denied {
              info!(
                "Client {} declares a denied-visitor redirect: {}",
                client_id, url
              );
            }
            handle.service_at_mut(service_index).denied = denied;
          }
          // Parallel-connection count and the client's own record of
          // what it resolved differently: display-only, for the
          // dashboard's per-connection config view.
          handle.service_at_mut(service_index).connections = connections;
          handle.service_at_mut(service_index).connections_min = connections_min;
          handle.service_at_mut(service_index).connections_max = connections_max;
          handle.service_at_mut(service_index).capture = !no_capture;
          // The declared id is `<base>-<service>` for the first
          // connection and `<base>-<service>-c<N>` for the rest, so it
          // names both the service and this connection's place in its
          // fan. Recorded here rather than trusted for anything else:
          // it is what lets the ceiling below be about *one service*
          // instead of the whole process.
          handle.declared_client_id = Some(cid.clone());
          // Counted after this block: `handle` is a mutable borrow of
          // the map the count has to walk.
          ceiling_ctx = Some((
            handle.instance_group.clone(),
            handle.perms.connection_ceiling(server_max_connections),
          ));
          if handle.service_at(service_index).config_notes != config_notes {
            handle.service_at_mut(service_index).config_notes = config_notes;
          }
          // Sanitized on arrival rather than on the way out: a series, once
          // scraped, is in the metrics backend whatever the server does later.
          let metrics_labels = crate::metrics_labels::sanitize(&metrics_labels);
          if handle.service_at(service_index).metrics_labels != metrics_labels {
            handle.service_at_mut(service_index).metrics_labels = metrics_labels;
          }
          handle.drain_secs = drain_secs;
          if handle.service_at(service_index).webhook_inbox != webhook_inbox {
            handle.service_at_mut(service_index).webhook_inbox = webhook_inbox;
            if webhook_inbox {
              info!(
                "Client {} opted into the webhook inbox: inbound POSTs are persisted for re-firing",
                client_id
              );
            }
          }
          if handle.service_at(service_index).resilience != resilience {
            handle.service_at_mut(service_index).resilience = resilience;
            if resilience {
              info!(
                "Client {} asked for serve-stale resilience: cached responses outlive its disconnects",
                client_id
              );
            }
          }
          if handle.service_at(service_index).tunnels != tunnels {
            info!(
              "Client {} declares {} bindable tunnel(s)",
              client_id,
              tunnels.len()
            );
            handle.service_at_mut(service_index).tunnels = tunnels;
          }
          // Log backend health transitions reported by the client's
          // own probe; the eligibility filter honours the flag.
          handle.service_at_mut(service_index).backend_probed = backend_probed;
          // Self-reported client health. Stored as sent, including absences: a
          // client that stops reporting a figure (an older build, or a platform
          // where it cannot be read) should show nothing rather than the last
          // value it happened to send, which would age silently.
          handle.cpu_percent = cpu_percent;
          handle.rss_bytes = rss_bytes;
          handle.rtt_ms = rtt_ms;
          handle.jitter_ms = jitter_ms;
          handle.reconnects = reconnects;
          if handle.service_at(service_index).backend_healthy != backend_healthy {
            handle.service_at_mut(service_index).backend_healthy = backend_healthy;
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
          if handle.service_at(service_index).priority != priority {
            info!(
              "Client {} announced load-balancing priority {}",
              client_id, priority
            );
            handle.service_at_mut(service_index).priority = priority;
          }
          // The self-reported instance ID is remembered (first value
          // wins) so failover `wait` mode can recognize this client
          // process when it reconnects under a new connection ID.
          if handle.reported_instance_id.is_none() && !cid.is_empty() {
            handle.reported_instance_id = Some(cid.clone());
          }
          // Announced link capacity feeds the writer task's shaper.
          let announced_bw = bandwidth_bps.unwrap_or(0);
          if handle
            .service_at(service_index)
            .bandwidth_bps
            .swap(announced_bw, Ordering::Relaxed)
            != announced_bw
            && announced_bw > 0
          {
            info!(
              "Client {} announced a bandwidth limit of {} bytes/s; pacing outgoing frames",
              client_id, announced_bw
            );
          }
          // The client's build, which belongs to the connection rather than
          // to any service, so it is read rather than consumed here.
          if let Some(v) = version.as_ref() {
            handle.client_version = Some(v.clone());
          }
          if service.is_some() {
            handle.service_at_mut(service_index).service_name = service;
            handle.service_at_mut(service_index).service_custom_name = service_custom_name;
          }
          // Public declaration: honored only when the token permits
          // publishing public services.
          let effective_public = public && handle.perms.allow_public;
          if public
            && !handle.perms.allow_public
            && !handle.service_at(service_index).public_denied_warned
          {
            handle.service_at_mut(service_index).public_denied_warned = true;
            warn!(
              "Client {} declared itself public but its token does not permit publishing public services; keeping the visitor auth gate",
              client_id
            );
          }
          if handle.service_at(service_index).public != effective_public {
            handle.service_at_mut(service_index).public = effective_public;
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
              if !handle.service_at(service_index).visitor_auth_denied_warned {
                handle
                  .service_at_mut(service_index)
                  .visitor_auth_denied_warned = true;
                warn!(
                  "Client {} declared a visitor password but its token does not permit controlling the visitor gate; ignoring it",
                  client_id
                );
              }
              None
            }
            Some(ref creds) if !crate::routing::valid_visitor_creds(creds) => {
              if !handle.service_at(service_index).visitor_auth_denied_warned {
                handle
                  .service_at_mut(service_index)
                  .visitor_auth_denied_warned = true;
                warn!(
                  "Client {} declared an invalid visitor password (expected user:password); ignoring it",
                  client_id
                );
              }
              None
            }
            other => other,
          };
          // The full policy, when the client sent one. Same permission as the
          // scalar and as `public`, and the same silent-drop rule: a method
          // this build does not know, or one a client has no business
          // declaring, is dropped rather than guessed at, and the methods
          // beside it stay in force.
          let declared_policy = visitor_auth_methods.as_ref().and_then(|specs| {
          if !handle.perms.allow_public {
            // Said out loud, like the scalar case beside it. The handshake
            // already refused this connection the right to declare a gate (it
            // announced an empty list), so a client that is up to date is not
            // serving this route at all; a client that sent one anyway is
            // reading an announcement it should have refused on, and the
            // reason belongs in the operator's log either way.
            if !handle.service_at(service_index).visitor_auth_denied_warned {
              handle.service_at_mut(service_index).visitor_auth_denied_warned = true;
              warn!(
                "Client {} declared a visitor-auth policy but its token does not permit controlling the visitor gate; ignoring it",
                client_id
              );
            }
            return None;
          }
          let usable: Vec<aperio_config::AuthMethodSpec> = specs
            .iter()
            .filter(|spec| {
              CLIENT_DECLARABLE_METHODS.contains(&spec.method.trim().to_ascii_lowercase().as_str())
            })
            .cloned()
            .collect();
          if usable.len() != specs.len() && !handle.service_at(service_index).visitor_auth_denied_warned {
            handle.service_at_mut(service_index).visitor_auth_denied_warned = true;
            warn!(
              "Client {} declared a visitor-auth method this server does not accept from a client; ignoring it",
              client_id
            );
          }
          (!usable.is_empty())
            .then(|| {
              // `true`: these specs arrived over the tunnel, so a `jwks_url`
              // among them is a destination a client chose for this server to
              // fetch, and is fenced as one.
              crate::visitor_auth::Policy::compile_from(
                &aperio_config::AuthSetting::Any(usable),
                true,
              )
            })
            .filter(|p| p.gates() || p.admits_everyone())
        });
          if handle.service_at(service_index).visitor_auth_policy != declared_policy {
            if let Some(ref p) = declared_policy {
              info!(
                "Client {} gates its service with method(s): {}",
                client_id,
                p.method_names().join(", ")
              );
            }
            handle.service_at_mut(service_index).visitor_auth_policy = declared_policy;
          }
          if handle.service_at(service_index).visitor_auth != effective_auth {
            let now_set = effective_auth.is_some();
            handle.service_at_mut(service_index).visitor_auth = effective_auth;
            if now_set {
              info!(
                "Client {} gates its service behind a client-set visitor login",
                client_id
              );
            }
          }
          // The service nothing gates. Said once per connection, and only
          // while the server is still open by default, because that is the
          // configuration where an ungated service is reachable by anyone and
          // nothing in the file says so: it is open because nothing closed it.
          // Under `default_access: deny` this is not a warning but the stated
          // policy, and the route is simply refused (planned_features #108).
          if !handle.service_at(service_index).ungated_warned
            && handle.service_at(service_index).visitor_auth.is_none()
            && handle
              .service_at(service_index)
              .visitor_auth_policy
              .is_none()
            && !handle.service_at(service_index).public
            && !state.config().visitor_auth.gates()
            && state.oidc.is_none()
          {
            handle.service_at_mut(service_index).ungated_warned = true;
            if state.config().default_access == crate::settings::DefaultAccess::Deny {
              // The one message that turns "the site went dark after we
              // upgraded" from an afternoon into a minute. It fires where the
              // cause is, on the connection whose service is being refused,
              // and it names the line to write rather than the state it found.
              warn!(
                "Client {} declares no gate and is not declared open, so its traffic is refused (`default_access` is `deny`). Write `public: true` on the service if it is meant to be reachable by anyone, or give it an `auth:`.",
                client_id
              );
            } else {
              warn!(
                "Client {} serves traffic that nothing gates: this server has no `auth:` and the service declares neither a gate nor `public: true`, so it is reachable by anyone. Declare it (`auth:` / `public: true`), or set `default_access: deny` to refuse what nothing declares.",
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
          if effective_ips.len() != before
            && !handle.service_at(service_index).allowed_ips_invalid_warned
          {
            handle
              .service_at_mut(service_index)
              .allowed_ips_invalid_warned = true;
            warn!(
              "Client {} declared allowed_ips with invalid entries; dropping them",
              client_id
            );
          }
          if handle.service_at(service_index).allowed_ips != effective_ips {
            if !effective_ips.is_empty() {
              info!(
                "Client {} restricts visitors to {:?}",
                client_id, effective_ips
              );
            }
            handle.service_at_mut(service_index).allowed_ips = effective_ips;
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
          if let Some(ref declared_scaling) = scaling {
            scaling_ctx = Some((
              declared_scaling.clone(),
              // The declaring service's own binds, not the connection's
              // union: an autoscaling record is armed per hostname of the
              // service that asked for it.
              handle
                .service_at(service_index)
                .effective_hostnames()
                .into_iter()
                .cloned()
                .collect::<Vec<String>>(),
              handle
                .service_at(service_index)
                .effective_path_bind()
                .cloned(),
              handle.perms.org_id.clone(),
              handle.perms.token_id.clone(),
            ));
          }
        }
      }
      if let Some((group, ceiling)) = ceiling_ctx {
        over_ceiling =
          service_connection_over_ceiling(&clients, client_id, group.as_deref(), &cid, ceiling);
      }
    }

    // A connection past what this token may hold for one service.
    // Refused here rather than at the handshake because the service
    // it belongs to is only known once the client says so, and said
    // out loud rather than dropped silently: a client that opened
    // more than it may is a config to fix, not a mystery. A current
    // client never reaches this, it reads the ceiling from the
    // handshake header and opens that many.
    if over_ceiling {
      let ceiling = perms.connection_ceiling(server_max_connections);
      warn!(
        "Client {} ({}) opened more parallel connections than permitted for one \
                   service; closing this one. Ceiling {} ({})",
        cid,
        client_ip,
        ceiling,
        match perms.max_connections {
          Some(_) => "the token's own, at or below the server's",
          None => "the server's max_connections_per_service",
        }
      );
      return false;
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
          None => Ok(crate::store::tokens::PinOutcome::Mismatch),
        }
      };
      match verdict {
        Ok(crate::store::tokens::PinOutcome::Mismatch) => {
          warn!(
            "Token pinning: client {} presented token '{}' without a matching device key, rejecting the connection",
            client_id, token_name
          );
          state
            .audit_in(
              "token_pin_mismatch",
              &token_name,
              client_ip,
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
          return false;
        }
        Ok(crate::store::tokens::PinOutcome::Pinned) => {
          info!(
            "Token pinning: pinned token '{}' to the connecting device",
            token_name
          );
        }
        // The pin was made and rolled back because it could not be saved.
        // **Refuse**, for the reason pinning exists: a pin that is not written
        // down does not bind the next connection to this device, so admitting
        // this one would leave the operator with a control that reports itself
        // enabled and holds nothing. The same argument as an auth gate that
        // opens when its check is unreachable.
        Err(crate::store::tokens::NotWritten::NotPersisted) => {
          warn!(
            "Token pinning: could not record the pin for token '{}', refusing the connection",
            token_name
          );
          return false;
        }
        // Match, or a token that disappeared between authorization and here.
        // Both leave the store as it was, and neither is this block's decision
        // to make.
        Ok(crate::store::tokens::PinOutcome::Match)
        | Err(crate::store::tokens::NotWritten::NoSuchRecord) => {}
      }
    }

    // Autoscaling: arm (or refresh) one record per hostname this
    // client serves. The record deliberately outlives the connection,
    // which is the whole point of `min: 0`: the server must be able
    // to call the endpoint when nothing is running. A fleet of
    // identical replicas converges on one record per bind, because
    // the store dedupes by a hash of the declaration.
    // The declaration travels with the context it was captured from. Reading
    // it back off the first entry, as this did, silently armed nothing when a
    // *later* service was the one that asked for scaling: the context was
    // set, the declaration was not, and the tuple pattern simply failed.
    if let (true, Some((decl, hostnames, path, org, token_id))) =
      (state.config().scaling_enabled, scaling_ctx)
    {
      for hostname in hostnames {
        let record =
          crate::api::scaling::record_from_decl(&decl, org.clone(), &hostname, path.as_deref());
        let record = match record {
          Ok(record) => record,
          Err(e) => {
            let warned = {
              let mut clients = state.clients.write().await;
              match clients.get_mut(client_id) {
                Some(handle) => {
                  let already = handle.sole().scaling_invalid_warned;
                  handle.sole_mut().scaling_invalid_warned = true;
                  already
                }
                None => true,
              }
            };
            if !warned {
              warn!(
                "Client {} declared an invalid scaling block: {}",
                client_id, e
              );
            }
            break;
          }
        };
        let id = record.id.clone();
        let outcome = {
          let mut store = state.scaling_store.lock().await;
          store.upsert(
            record,
            token_id.as_deref(),
            crate::store::tokens::now_secs(),
          )
        };
        match outcome {
          crate::store::scaling::Upsert::Unchanged => {}
          other => {
            // A changed declaration re-arms a record the breaker may
            // have disarmed: the operator just told us something new.
            state.scaling_runtime.lock().await.rearm(&id);
            info!(
              "Autoscaling record {} for {} ({:?})",
              if other == crate::store::scaling::Upsert::Created {
                "armed"
              } else {
                "updated"
              },
              hostname,
              other
            );
          }
        }
      }
    }
    let pong = TunnelMessage::Pong {
      timestamp,
      version: Some(env!("CARGO_PKG_VERSION").to_string()),
      protocol: Some(PROTOCOL_VERSION),
    };
    if let Ok(pong_str) = serde_json::to_string(&pong) {
      let _ = tx_write.send(Message::Text(pong_str.into())).await;
    }

    true
  }

  /// Handles the answer to a relayed WebSocket upgrade.
  async fn on_upgrade_response(&self, msg: TunnelMessage) {
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
  async fn on_ws_data(&self, msg: TunnelMessage) {
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
  async fn deliver_ws_frame(&self, stream_id: &str, ws_msg: Message) {
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

  async fn on_ws_close(&self, msg: TunnelMessage) {
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
  fn on_compression_ack(&self) {
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
  async fn on_otlp_export(&self, signal: String, data: String) {
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

  async fn on_draining(&self) {
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
  async fn cleanup(&self) {
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

/// WebSocket processing logic. Listens for client frame inputs (Responses/Pings).
pub(crate) async fn handle_socket(
  socket: WebSocket,
  client_ip: String,
  state: Arc<AppState>,
  perms: ClientPerms,
  instance_group: Option<String>,
) {
  let (mut ws_sender, mut ws_receiver) = socket.split();
  let client_id = uuid::Uuid::new_v4().to_string();
  // Read once: the ceiling is a server setting, and a live edit of it should
  // not retroactively evict connections that were within the number when they
  // were made.
  let server_max_connections = state.config().max_connections_per_service;

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
    let mut pacer = SendPacer::new(Instant::now());
    // Everything already queued behind a message rides the same flush
    // (planned_features #38, the mirror of what #24 did on the client's
    // writer). Each message used to pay its own flush, which is a syscall per
    // frame, and this is the busier side under fan-out. The messages are
    // whole frames by the time they arrive here, so batching them costs no
    // latency: a message is not held waiting for company, only joined by what
    // was already waiting.
    'writer: while let Some(msg) = rx_write.recv().await {
      let mut msg = writer_transform(msg, compress_out_writer.load(Ordering::SeqCst));
      loop {
        // The pacer is spent per frame even inside a batch: it exists to keep
        // the server from pushing faster than the client's link can drain,
        // and a batch that skipped it would do exactly that in bursts.
        let rate = bandwidth_writer.load(Ordering::Relaxed);
        if rate > 0 {
          let size = match &msg {
            Message::Text(t) => t.len(),
            Message::Binary(b) => b.len(),
            _ => 0,
          };
          if let Some(debt) = pacer.debt(size, rate, Instant::now()) {
            // Flush what is already fed before sleeping, so a paced
            // connection does not hold finished frames in the buffer for the
            // length of its debt.
            if let Err(e) = ws_sender.flush().await {
              error!(
                "Error flushing to websocket client {}: {:?}",
                writer_client_id, e
              );
              break 'writer;
            }
            tokio::time::sleep(debt).await;
          }
        }
        // Take the next one first: whether anything is waiting decides
        // between feeding this frame (more to come) and sending it (flush).
        match rx_write.try_recv() {
          Ok(next) => {
            if let Err(e) = ws_sender.feed(msg).await {
              error!(
                "Error writing to websocket client {}: {:?}",
                writer_client_id, e
              );
              break 'writer;
            }
            msg = writer_transform(next, compress_out_writer.load(Ordering::SeqCst));
          }
          Err(_) => {
            if let Err(e) = ws_sender.send(msg).await {
              error!(
                "Error writing to websocket client {}: {:?}",
                writer_client_id, e
              );
              break 'writer;
            }
            break;
          }
        }
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
  // any token-granted hostnames, the client serves both.
  let mut assigned_hostnames = perms.granted_hostnames();
  let random_hostname = state
    .config()
    .random_subdomain_suffix
    .as_ref()
    .map(|pattern| {
      // Derive the label deterministically from the instance group + declared
      // binds so every parallel connection of one process gets the *same* random
      // hostname (shared, not one per connection). Fall back to a fresh random
      // label when the client sends no instance group or declares no hostname.
      match &instance_group {
        Some(group) if !assigned_hostnames.is_empty() => {
          let seed = format!("{group}\0{}", assigned_hostnames.join(","));
          random_subdomain_hostname_seeded(pattern, &seed)
        }
        _ => random_subdomain_hostname(pattern),
      }
    });
  if let Some(ref h) = random_hostname {
    assigned_hostnames.push(h.clone());
  }

  // Signalled to force this connection's read loop to end (e.g. token revoke).
  let disconnect = Arc::new(Notify::new());

  // Register active client
  {
    let mut clients = state.clients.write().await;
    clients.insert(
      client_id.clone(),
      ClientHandle {
        tx: tx_write.clone(),
        disconnect: disconnect.clone(),
        connected_at: Instant::now(),
        client_ip: client_ip.clone(),
        declared_client_id: None,
        drain_secs: None,
        last_ping_at: None,
        perms: perms.clone(),
        draining: false,
        client_version: None,
        client_protocol: None,
        cpu_percent: None,
        rss_bytes: None,
        rtt_ms: None,
        jitter_ms: None,
        reconnects: None,
        reported_instance_id: None,
        instance_group: instance_group.clone(),
        subscriptions: Vec::new(),
        services: vec![crate::state::ServiceState {
          request_count: client_req_count.clone(),
          declared_path: None,
          assigned_path: perms.granted_path(),
          declared_hostname: None,
          declared_hostnames: Vec::new(),
          assigned_hostnames,
          random_hostname: random_hostname.clone(),
          override_path_bind: None,
          override_hostname_binds: Vec::new(),
          capture: true,
          connections: None,
          connections_min: None,
          connections_max: None,
          config_notes: Vec::new(),
          metrics_labels: Vec::new(),
          max_concurrent: None,
          inflight_limiter: None,
          admin_enabled: true,
          tcp_enabled: false,
          backend_healthy: true,
          backend_probed: true,
          priority: 0,
          bandwidth_bps: bandwidth_bps.clone(),
          service_name: None,
          service_custom_name: None,
          public: false,
          public_denied_warned: false,
          visitor_auth: None,
          visitor_auth_policy: None,
          visitor_auth_denied_warned: false,
          ungated_warned: false,
          allowed_ips: Vec::new(),
          allowed_ips_invalid_warned: false,
          scaling_invalid_warned: false,
          tunnels: Vec::new(),
          cache: false,
          cache_ignored_warned: false,
          resilience: false,
          max_request_body: None,
          response_timeout: None,
          webhook_inbox: false,
          denied: None,
          recent_failures: VecDeque::new(),
          ejected_until: None,
        }],
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
      let _ = tx_write.send(Message::Text(json.into())).await;
    }
  }

  // Offer tunnel compression; frames stay uncompressed until the client Acks.
  if state.config().tunnel_compression
    && let Ok(json) = serde_json::to_string(&TunnelMessage::CompressionStart {})
  {
    let _ = tx_write.send(Message::Text(json.into())).await;
  }

  // Cap for decompressed tunnel frames (defends against zlib bombs).
  let max_inflated = state
    .config()
    .max_body_size
    .saturating_mul(4)
    .max(8 * 1024 * 1024);

  let ctx = ConnCtx {
    state: state.clone(),
    client_id: client_id.clone(),
    client_ip: client_ip.clone(),
    tx_write: tx_write.clone(),
    compress_out: compress_out.clone(),
    perms,
    server_max_connections,
    max_inflated,
    stream_cache: std::sync::Mutex::new(HashMap::new()),
  };

  // Read loop. Ends on the client closing the socket, or when `disconnect` is
  // signalled (e.g. the token this client connected with was revoked), which
  // yields `None` so the loop falls through to the normal cleanup below. Only
  // decode-and-dispatch lives here; what each message means is its handler's
  // business.
  while let Some(result) = tokio::select! {
    msg = ws_receiver.next() => msg,
    _ = disconnect.notified() => {
      info!("Force-disconnecting tunnel client {} (server request, e.g. token revoked)", client_id);
      None
    }
  } {
    match result {
      Ok(msg) => {
        let (text_opt, mut full_body) = ctx.decode_incoming(msg).await;
        if let Some(text) = text_opt
          && let Ok(tunnel_msg) = serde_json::from_str::<TunnelMessage>(&text)
        {
          match tunnel_msg {
            m @ TunnelMessage::Response { .. } => ctx.on_response(m, full_body.take()).await,
            m @ TunnelMessage::ResponseStart { .. } => ctx.on_response_start(m).await,
            m @ TunnelMessage::ResponseChunk { .. } => ctx.on_response_chunk(m).await,
            m @ TunnelMessage::ResponseEnd { .. } => ctx.on_response_end(m).await,
            m @ TunnelMessage::ResponseAbort { .. } => ctx.on_response_abort(m).await,
            m @ TunnelMessage::TcpData { .. } => ctx.on_tcp_data(m).await,
            m @ TunnelMessage::TcpClose { .. } => ctx.on_tcp_close(m).await,
            m @ TunnelMessage::UdpDatagram { .. } => ctx.on_udp_datagram(m).await,
            m @ TunnelMessage::UdpClose { .. } => ctx.on_udp_close(m).await,
            TunnelMessage::CompressionAck {} => ctx.on_compression_ack(),
            m @ TunnelMessage::Subscribe { .. } => ctx.on_subscribe(m).await,
            m @ TunnelMessage::Unsubscribe { .. } => ctx.on_unsubscribe(m).await,
            m @ TunnelMessage::PublishAck { .. } => ctx.on_publish_ack(m).await,
            m @ TunnelMessage::Publish { .. } => ctx.on_publish(m).await,
            TunnelMessage::Draining {} => ctx.on_draining().await,
            // The one verdict a handler renders: a Ping revealing a
            // connection past its ceiling, or a failed token pin, ends the
            // connection.
            m @ TunnelMessage::Ping { .. } => {
              if !ctx.on_ping(m).await {
                break;
              }
            }
            m @ TunnelMessage::UpgradeResponse { .. } => ctx.on_upgrade_response(m).await,
            m @ TunnelMessage::WsData { .. } => ctx.on_ws_data(m).await,
            m @ TunnelMessage::WsClose { .. } => ctx.on_ws_close(m).await,
            TunnelMessage::OtlpExport { signal, data } => ctx.on_otlp_export(signal, data).await,
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

  // Client cleanup.
  writer_task.abort();
  ctx.cleanup().await;
}
