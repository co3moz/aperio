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

/// Most services one connection may declare in a Ping.
///
/// A sanity bound, not a policy, and deliberately far above anything a real
/// deployment writes: multiplexing exists for the operator with forty services
/// and this is six times that. What it bounds is the cost of the declaration
/// itself, which is paid under the `clients` write lock and is not all linear,
/// so an unbounded list is a way for one authenticated client to stall every
/// other one and then exhaust memory.
///
/// Refused rather than truncated. Serving the first 256 of a longer list is
/// the connection that establishes and then serves less than it was told to,
/// which is the failure the empty-list refusal beside it exists to prevent.
pub(crate) const MAX_DECLARED_SERVICES: usize = 256;

/// The tunnel protocol version this server speaks, announced on the upgrade
/// response (`planned_features.md` #120).
///
/// The Pong already carries it, and for everything the protocol has added so
/// far that was early enough: a client learns the peer's version and picks its
/// frame encoding from then on. Multiplexing is the first capability that
/// changes what the *first Ping* is allowed to say, and by the time a Pong
/// arrives that Ping has been sent. So the number is put where the client can
/// read it before it declares anything, next to the visitor-gate announcement,
/// which is on the upgrade response for the same reason.
pub(crate) const PROTOCOL_HEADER: &str = "x-aperio-protocol";

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
use tokio::sync::{Notify, mpsc};
use tracing::{error, info, warn};

use crate::auth::authorize_tunnel_token;
use crate::protocol::{
  FRAME_RESPONSE_CHUNK, FRAME_RESPONSE_FULL, FRAME_RESPONSE_FULL_ZLIB, PROTOCOL_VERSION,
  TunnelMessage, compress_frame, decode_binary_frame, decompress_frame,
};
use crate::routing::{
  extract_client_ip, random_subdomain_hostname, random_subdomain_hostname_seeded,
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
  // Which of the connection's services declared it, so the warn-once flag
  // that reports a bad block is that service's. Shared, a second service's
  // mistake would be silenced by the first service having already warned
  // about its own, which is the failure the per-service flags exist for.
  usize,
);

// The frame handlers, split by what arrives. Each is an `impl ConnCtx` block
// of its own: the connection is one thing, but what a client sends it falls
// into groups that share nothing except the socket.
//
// `ping` is one function and stays that way. A Ping declares everything a
// connection serves, and every field is admitted or refused against the token
// before any of it takes effect; cutting it up would mean carrying the
// half-applied state between the pieces.
mod declare;
mod messaging;
mod ping;
mod relay;
mod response;
mod upgrade;

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
  // What this server can be told, before the client has told it anything.
  if let Ok(value) = axum::http::HeaderValue::from_str(&PROTOCOL_VERSION.to_string()) {
    response.headers_mut().insert(PROTOCOL_HEADER, value);
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

impl ConnCtx {}

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
        declared_name: None,
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
          server_side_target: None,
          server_side_refused: None,
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
          max_concurrent_ceiling: None,
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
