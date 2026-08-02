//! Tunnel wire protocol: message schema, binary chunk frames, and optional
//! zlib frame compression.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::warn;

/// Version of the tunnel wire protocol. Must match the constant in
/// aperio-server; bumped on breaking changes to `TunnelMessage`.
/// v2: streamed request bodies (RequestStart/Chunk/End) and raw binary
/// chunk frames instead of base64+JSON for body data.
/// v3: per-stream flow control (StreamPause/StreamResume), the server
/// pauses a producer whose visitor reads slower than it sends.
/// v5: a buffered response travels as one binary frame (envelope + body)
/// instead of base64 inside JSON.
/// v6: the same for a buffered *request* body, server to client, which is
/// the other direction of the same cost (an upload was still base64 in JSON).
/// v7: TCP/UDP/WS relay payloads travel as raw binary frames instead of
/// base64 inside JSON, closing the last base64 leg of the tunnel.
pub(crate) const PROTOCOL_VERSION: u32 = 7;

// --- Protocol v2 binary frames: [tag][id_len][id bytes][payload] ---
// Data-heavy chunk messages skip the base64+JSON encoding entirely. The tag
// byte never collides with zlib-compressed JSON frames, which start with
// 0x78.

/// Binary frame tag for a streamed request-body chunk (server → client).
pub(crate) const FRAME_REQUEST_CHUNK: u8 = 1;
/// Binary frame tag for a streamed response-body chunk (client → server).
pub(crate) const FRAME_RESPONSE_CHUNK: u8 = 2;

/// Binary frame tag for a whole buffered response (client → server), v5.
///
/// The `Response` message and its body in one frame, so a body that is not
/// streamed still travels as bytes instead of base64 inside JSON. The payload
/// is `[json_len: u32 LE][json][body]`: the JSON is the `Response` with
/// `body: None`, the rest is the body itself.
///
/// Why a frame rather than lowering the streaming threshold: streaming costs
/// a head message, a frame per chunk and a tail, which a small body cannot
/// repay. This costs one message, the same as the JSON it replaces.
pub(crate) const FRAME_RESPONSE_FULL: u8 = 3;

/// Binary frame tag for a whole buffered response whose payload is zlib
/// compressed (client → server), v5.
///
/// The uncompressed frame bypasses the tunnel's compression entirely, because
/// compression only ever applied to text frames. For a compressible body that
/// is a large regression against what the base64-in-JSON path used to send:
/// 32 KB of HTML went out as a few hundred bytes and would have gone out
/// whole. This is the same frame with a deflated payload, sent only when the
/// peer negotiated compression and only when deflating actually made it
/// smaller.
pub(crate) const FRAME_RESPONSE_FULL_ZLIB: u8 = 4;

/// Binary frame tag for a whole buffered request (server → client), v6.
///
/// The mirror of `FRAME_RESPONSE_FULL`, in the direction an upload travels.
/// A buffered request body under the streaming threshold was still base64
/// inside the `Request` JSON: a third more bytes on the wire, an encode on
/// the server and a decode here, per POST. Same layout,
/// `[json_len: u32 LE][json][body]`, with the JSON being the `Request`
/// message carrying `body: None`.
pub(crate) const FRAME_REQUEST_FULL: u8 = 5;

/// The same frame with a zlib-deflated payload (server → client), v6.
pub(crate) const FRAME_REQUEST_FULL_ZLIB: u8 = 6;

/// Binary frame tag for one chunk of a relayed TCP stream (either
/// direction), v7. The payload is the raw bytes; the id is the stream id.
///
/// Until v7 a `TcpData` chunk travelled base64-encoded inside JSON, a third
/// more bytes on the wire plus an encode, a JSON parse and a decode per
/// 16 KB chunk, in both directions. The relay payload is an opaque byte
/// stream (often TLS or an AEAD-sealed tunnel already), which is also why
/// these frames have no zlib sibling: the bytes rarely deflate, and the
/// win here is the per-byte codec cost, not the wire size.
///
/// **Known trade-off, deliberate.** These payloads used to ride inside a
/// *text* frame, which the writer deflated whole when the connection
/// negotiated `tunnel_compression`. A binary frame skips that path, so a
/// deployment with compression on and a *compressible* protocol tunnelled
/// over TCP (a plain-text wire protocol, say) sends more bytes than it did
/// before v7, while paying less CPU per byte. Adding zlib siblings here
/// would recover it; that is a deliberate non-goal, since the payload that
/// motivates the relay path is the opaque kind.
pub(crate) const FRAME_TCP_DATA: u8 = 7;

/// Binary frame tag for one relayed UDP datagram (either direction), v7.
/// Same layout and reasoning as `FRAME_TCP_DATA`; one frame is one datagram.
pub(crate) const FRAME_UDP_DATAGRAM: u8 = 8;

/// Binary frame tag for one binary frame of a passed-through WebSocket
/// (either direction), v7. Text WS frames keep the JSON `WsData` shape:
/// they were never base64-encoded, so there is nothing to save.
pub(crate) const FRAME_WS_DATA_BIN: u8 = 9;

/// Deflates a frame payload. Returns `None` when the result is not smaller,
/// which is the normal answer for an already-compressed or random body: there
/// is no point paying for the bytes twice, and the reader takes either tag.
pub(crate) fn deflate_payload(payload: &[u8]) -> Option<Vec<u8>> {
  use flate2::{Compression, write::ZlibEncoder};
  use std::io::Write;
  let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
  enc.write_all(payload).ok()?;
  let out = enc.finish().ok()?;
  (out.len() < payload.len()).then_some(out)
}

/// Inflates a frame payload, bounded like every other decompression on this
/// path so a small frame cannot ask for an unbounded allocation.
pub(crate) fn inflate_payload(data: &[u8], max_out: usize) -> Option<Vec<u8>> {
  use flate2::read::ZlibDecoder;
  use std::io::Read;
  let mut out = Vec::new();
  let mut dec = ZlibDecoder::new(data).take(max_out as u64 + 1);
  dec.read_to_end(&mut out).ok()?;
  if out.len() > max_out {
    warn!("Dropped tunnel frame: decompressed payload exceeds limit");
    return None;
  }
  Some(out)
}

/// Builds the whole `FRAME_RESPONSE_FULL` frame in one allocation:
/// `[tag][id_len][id][json_len][json][body]`.
///
/// The two halves used to be built separately, which copied the body twice:
/// once into the payload and once again into the frame around it. The wire
/// format is the same either way; this is the same bytes with one pass over
/// the body instead of two. `None` when the id will not fit its one-byte
/// length, the same refusal `encode_binary_frame` makes and for the same
/// reason.
pub(crate) fn encode_full_response_frame(id: &str, json: &str, body: &[u8]) -> Option<Vec<u8>> {
  if id.len() > u8::MAX as usize {
    return None;
  }
  let mut out = Vec::with_capacity(2 + id.len() + 4 + json.len() + body.len());
  out.push(FRAME_RESPONSE_FULL);
  out.push(id.len() as u8);
  out.extend_from_slice(id.as_bytes());
  out.extend_from_slice(&(json.len() as u32).to_le_bytes());
  out.extend_from_slice(json.as_bytes());
  out.extend_from_slice(body);
  Some(out)
}

/// Splits a full-envelope frame payload (`FRAME_RESPONSE_FULL` in the test
/// that proves both sides agree, `FRAME_REQUEST_FULL` in production) into its
/// JSON envelope and the body that follows it. `None` when the length prefix
/// does not describe the frame, which is a corrupt or truncated message
/// rather than an old peer: the tag is only sent to a peer that announced the
/// version that has it.
pub(crate) fn split_full_response(payload: &[u8]) -> Option<(&str, &[u8])> {
  let (len_bytes, rest) = payload.split_at_checked(4)?;
  let json_len = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
  let (json, body) = rest.split_at_checked(json_len)?;
  Some((std::str::from_utf8(json).ok()?, body))
}

/// Builds a `FRAME_RESPONSE_FULL` payload: the envelope's length, the
/// envelope, then the body verbatim.
#[cfg(test)]
pub(crate) fn join_full_response(json: &str, body: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(4 + json.len() + body.len());
  out.extend_from_slice(&(json.len() as u32).to_le_bytes());
  out.extend_from_slice(json.as_bytes());
  out.extend_from_slice(body);
  out
}

/// Encodes a v2 binary chunk frame, or `None` when the id will not fit the
/// one-byte length prefix.
///
/// Checked in every build rather than only in tests: the id is not always
/// ours (a peer's request id is echoed back), and `id.len() as u8` on a
/// longer one wraps, writing a length that does not describe the frame and
/// putting every frame after it out of step. Refusing costs one chunk;
/// a wrong length costs the connection. Ids are UUIDs, so this is a guard
/// against a peer that is broken or hostile, not a case the code produces.
pub(crate) fn encode_binary_frame(tag: u8, id: &str, payload: &[u8]) -> Option<Vec<u8>> {
  if id.len() > u8::MAX as usize {
    return None;
  }
  let mut out = Vec::with_capacity(2 + id.len() + payload.len());
  out.push(tag);
  out.push(id.len() as u8);
  out.extend_from_slice(id.as_bytes());
  out.extend_from_slice(payload);
  Some(out)
}

/// Chunk feeder for one streamed request body in flight.
pub(crate) type RequestBodyFeeder = mpsc::Sender<Result<bytes::Bytes, std::io::Error>>;

/// One relay payload, in the shape the peer negotiated: a v7 server takes
/// the raw bytes in a tagged binary frame, anything older takes the JSON
/// message with the payload base64-encoded. The server has the mirror of
/// this; see its doc for why the decision lives in one function.
pub(crate) fn relay_frame(
  protocol: u32,
  tag: u8,
  stream_id: &str,
  bytes: &[u8],
  json_fallback: impl FnOnce(String) -> TunnelMessage,
) -> Option<Message> {
  if protocol >= 7
    && let Some(frame) = encode_binary_frame(tag, stream_id, bytes)
  {
    return Some(Message::Binary(frame.into()));
  }
  use base64::prelude::*;
  let msg = json_fallback(BASE64_STANDARD.encode(bytes));
  serde_json::to_string(&msg)
    .ok()
    .map(|json| Message::Text(json.into()))
}

/// Decodes a v2 binary chunk frame into (tag, id, payload).
pub(crate) fn decode_binary_frame(data: &[u8]) -> Option<(u8, &str, &[u8])> {
  if data.len() < 2 {
    return None;
  }
  let id_len = data[1] as usize;
  if data.len() < 2 + id_len {
    return None;
  }
  let id = std::str::from_utf8(&data[2..2 + id_len]).ok()?;
  Some((data[0], id, &data[2 + id_len..]))
}

/// Serde default for fields that must be true when absent (older peers).
/// Keeps `qos: 0` off the wire, which is what every message that does not
/// ask for more is.
fn is_zero(value: &u8) -> bool {
  *value == 0
}

fn default_true() -> bool {
  true
}

// The tunnel declaration is shared with the `aperio.yaml` schema crate so the
// same type serves both the config file and the wire (Ping) form.
pub(crate) use aperio_config::{ScalingDecl, TunnelDecl};

/// Client-side stage durations of one proxied request, in microseconds from
/// the moment the client received the tunnel request. Attached to buffered
/// `Response` messages so the server can assemble a request timeline;
/// additive, older peers simply omit it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct ClientTimings {
  /// The backend request left the client.
  pub backend_sent_us: u64,
  /// The backend's response headers (first byte) arrived.
  pub backend_first_byte_us: u64,
  /// The backend body was fully read.
  pub backend_done_us: u64,
  /// The response frame was handed to the tunnel.
  pub respond_us: u64,
}

/// A setting whose effective value differs from what the config asked for,
/// announced in the Ping so the dashboard can show the difference next to the
/// value instead of leaving the operator to find it in a startup log line.
///
/// Only the client knows both sides: by the time a value reaches the server it
/// is already the resolved one (an announced `bandwidth`, for instance, is the
/// per-connection share, with no trace of the budget it was cut from).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ConfigNote {
  /// Config key the note is about (`bandwidth`, `connections`, …).
  pub field: String,
  /// What the config asked for, as the operator wrote it.
  pub declared: String,
  /// What the client resolved it to.
  pub effective: String,
  /// Why the two differ, one sentence.
  pub reason: String,
}

/// Message structure exchanged over the WebSocket reverse tunnel.
// The `Ping` variant is intentionally wide (it announces the client's full
// per-service configuration); boxing its many small fields would only obscure
// the protocol for no real memory win, since Pings are short-lived.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub(crate) enum TunnelMessage {
  Ping {
    client_id: String,
    timestamp: u64,
    path_bind: Option<String>,
    #[serde(default)]
    hostname_bind: Option<String>,
    /// Additional hostname binds beyond `hostname_bind` (multi-hostname
    /// services). Additive; older peers omit it and send only the single
    /// `hostname_bind`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hostname_binds: Vec<String>,
    /// Maximum concurrent requests this client is willing to process.
    /// The server queues excess requests instead of dispatching them.
    #[serde(default)]
    max_concurrent: Option<u32>,
    /// True when the client has a TCP target configured (APERIO_TCP_TARGET).
    #[serde(default)]
    tcp: bool,
    /// Client build version (CARGO_PKG_VERSION), for display/diagnostics.
    #[serde(default)]
    version: Option<String>,
    /// Tunnel wire protocol version this client speaks.
    #[serde(default)]
    protocol: Option<u32>,
    /// Result of the client's own backend health probe (APERIO_TARGET_HEALTH).
    /// False takes this client out of routing without dropping the tunnel.
    #[serde(default = "default_true")]
    backend_healthy: bool,
    /// False only while a configured health check has not completed its first
    /// probe yet (UI shows "checking" vs "down"). Older peers omit it → true.
    #[serde(default = "default_true")]
    backend_probed: bool,
    /// Load-balancing priority tier: 0 = primary (default), higher numbers
    /// are standbys (used with the server's primary-standby strategy).
    #[serde(default)]
    priority: u32,
    /// Announced downstream link capacity in bytes/second; the server paces
    /// tunnel frames so this client is never pushed faster than its network.
    #[serde(default)]
    bandwidth_bps: Option<u64>,
    /// Handle of the service this connection exposes (from the client's
    /// `services:` list), for the dashboard.
    #[serde(default)]
    service: Option<String>,
    /// What that service is called on screen (`custom_name:`), when the file
    /// gave it one. Free text; the handle above is what anything addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_custom_name: Option<String>,
    /// The client declares its service public: the server skips the visitor
    /// auth gate for traffic routed here (honored only when the token
    /// permits publishing public services).
    #[serde(default)]
    public: bool,
    /// Per-service visitor credentials ("user:password") declared by the
    /// client: the server gates traffic routed here behind a login with these
    /// credentials (honored only when the token may control the visitor gate,
    /// same permission as `public`, and the server has not set
    /// APERIO_IGNORE_CLIENT_AUTH). None = no override.
    #[serde(default)]
    visitor_auth: Option<String>,
    /// Visitor IPs/CIDRs allowed to reach this service (empty = everyone).
    /// The server rejects other visitors with 403 before dispatching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_ips: Vec<String>,
    /// Tunnels declared by this client (`tunnels:` list): normally
    /// unexposed local services reachable by a peer client via
    /// `--bind-tunnels` with the same token and this client's id.
    #[serde(default)]
    tunnels: Vec<TunnelDecl>,
    /// Opt this service into the server-side response cache (effective only
    /// when the server enables APERIO_CACHE).
    #[serde(default)]
    cache: bool,
    /// The client asks the server to keep serving this service's cached
    /// responses (marked, even expired) while no healthy client is
    /// connected, instead of failing with 504. Needs `cache`.
    #[serde(default)]
    resilience: bool,
    /// The client asks the server not to record this service's transactions
    /// for the request inspector. Absent on older clients, which means the
    /// default: record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    no_capture: bool,
    /// Largest request body, in bytes, visitors may upload to this service;
    /// the server answers bigger uploads with an early 413 before they enter
    /// the tunnel (None = only the server's global limit applies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_request_body: Option<u64>,
    /// How long, in seconds, the server should wait for this service to answer
    /// a dispatched request before failing it, a per-service override of the
    /// server's global gateway response timeout (None = use the global value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response_timeout: Option<u64>,
    /// Trust-on-first-use device key for token pinning (`APERIO_DEVICE_KEY`).
    /// The server pins the first key seen for the token and rejects later
    /// connections announcing a different key (None = not announced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_key: Option<String>,
    /// Ask the server to persist inbound POSTs to this service into its
    /// webhook inbox (browse & re-fire from the dashboard).
    #[serde(default)]
    webhook_inbox: bool,
    /// Redirect URL the server should answer to visitors rejected by this
    /// service's `allowed_ips` when no route candidate admits them
    /// (None = stealth: same answer as an unclaimed route).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    denied: Option<String>,
    /// Autoscaling declaration: the endpoint the server calls when this
    /// service needs capacity it does not have. Persisted server-side against
    /// this client's binds and deliberately outliving the connection, so a
    /// scale-to-zero service can be woken when nothing is running. Honored
    /// only when the token permits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scaling: Option<ScalingDecl>,
    /// Parallel tunnel connections this service runs (`connections:`). Every
    /// one of them announces the same count; the dashboard needs it to explain
    /// per-connection values such as the bandwidth share.
    #[serde(default)]
    connections: Option<u32>,
    /// Settings this client resolved to something other than the config asked
    /// for. Additive; older peers omit it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    config_notes: Vec<ConfigNote>,
    /// Static Prometheus labels this client announces (`metrics_labels:`),
    /// attached to its own series so one Prometheus can serve several
    /// environments without relabelling rules. Validated and capped by the
    /// server on arrival: label cardinality is how a metrics backend dies,
    /// and these come from clients.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    metrics_labels: std::collections::BTreeMap<String, String>,
    /// Seconds this client gives its own in-flight requests when it is asked
    /// to stand down (`reload_drain:`). Announced so the server can size its
    /// shutdown drain from what its clients actually expect, rather than from
    /// a number nobody revisits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drain_secs: Option<u64>,
    /// What the client observes about itself (planned_features #37): CPU as a
    /// percentage of one core and resident memory of the client process, and
    /// round-trip time, jitter and reconnect count of this tunnel connection.
    /// All additive and all optional: an older client omits them, and the
    /// process figures are absent on platforms where they cannot be read
    /// without guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cpu_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rtt_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jitter_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reconnects: Option<u32>,
  },
  Pong {
    timestamp: u64,
    /// Server build version, for logging version skew.
    #[serde(default)]
    version: Option<String>,
    /// Tunnel wire protocol version the server speaks.
    #[serde(default)]
    protocol: Option<u32>,
  },
  Request {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
  },
  /// Start of a streamed request body (protocol v2): method/uri/headers
  /// only; the body follows as RequestChunk frames ended by RequestEnd.
  RequestStart {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
  },
  /// A chunk of a streamed request body (Base64; v2 peers use raw binary
  /// frames instead).
  RequestChunk { id: String, data: String },
  /// Marks the end of a streamed request body.
  RequestEnd { id: String },
  Response {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
    /// HTTP trailers of the backend response (e.g. `grpc-status` for gRPC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trailers: Option<Vec<(String, String)>>,
    /// Client-side stage durations for the request timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timings: Option<ClientTimings>,
  },
  /// Start of a streamed response: status and headers only. The body follows
  /// as `ResponseChunk` messages terminated by `ResponseEnd`. Used for large
  /// bodies so neither side buffers the full payload in memory.
  ResponseStart {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
  },
  /// A chunk of a streamed response body (Base64 encoded).
  ResponseChunk { id: String, data: String },
  /// Marks the end of a streamed response body, optionally carrying the
  /// backend's HTTP trailers (e.g. `grpc-status` for gRPC).
  ResponseEnd {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trailers: Option<Vec<(String, String)>>,
  },
  /// Abnormal end of a streamed response (e.g. the body exceeded
  /// `max_response_body_size`, or the backend errored mid-stream). Unlike
  /// `ResponseEnd` this must NOT look successful: the server drops the visitor's
  /// body stream with an error so the visitor sees an incomplete/aborted
  /// response instead of a silently truncated 200.
  ResponseAbort { id: String },
  /// Server instructs the client to open a WebSocket connection to the local backend.
  UpgradeRequest {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
  },
  /// Client response after the backend WebSocket upgrade handshake completes (or fails).
  UpgradeResponse {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
  },
  /// Bidirectional WebSocket frame relayed through the tunnel.
  WsData {
    stream_id: String,
    data: String, // Base64 for binary frames, plain text for text frames
    is_text: bool,
  },
  /// Signals that a WebSocket stream has been closed.
  WsClose {
    stream_id: String,
    code: u16,
    reason: String,
  },
  /// Server → client: informs the client of a hostname automatically
  /// assigned to it (random subdomain feature).
  HostnameAssigned { hostname: String },
  /// Client → server: the client received a shutdown signal and is draining.
  Draining {},
  /// Server → client: open a raw TCP connection for this stream. `target`
  /// selects one of the client's declared tunnels; when absent the legacy
  /// `tcp_target` is used. The client only ever connects to addresses it
  /// itself declared, regardless of what the server asks.
  TcpOpen {
    stream_id: String,
    #[serde(default)]
    target: Option<String>,
  },
  /// Raw TCP bytes relayed through the tunnel (Base64).
  TcpData { stream_id: String, data: String },
  /// Signals that a TCP stream has been closed (either side).
  TcpClose { stream_id: String },
  /// Server → client: open a UDP relay for this stream toward one of the
  /// client's declared `protocol: udp` tunnels. The client only ever sends
  /// to addresses it itself declared, regardless of what the server asks.
  UdpOpen { stream_id: String, target: String },
  /// One UDP datagram relayed through the tunnel (Base64). Best-effort:
  /// datagrams are dropped, never queued unboundedly, when a hop is slow.
  UdpDatagram { stream_id: String, data: String },
  /// Tears down a UDP relay (either side; also sent on idle expiry).
  UdpClose { stream_id: String },
  /// Server → client: the server is shutting down gracefully and the tunnel
  /// is about to drop. The client switches to aggressive (no-backoff)
  /// reconnect so downtime is limited to the actual restart window.
  ServerShutdown {},
  /// Server → client: offers zlib compression for subsequent tunnel frames.
  CompressionStart {},
  /// Client → server: compression accepted; both sides may now send
  /// compressed binary frames.
  CompressionAck {},
  /// Server → client (v3): too much of stream `id` is backed up server-side
  /// because its visitor reads slower than this client produces. Producing
  /// pauses (the backend read waits) until the matching `StreamResume`, so
  /// ordinary TCP backpressure reaches the backend instead of the server
  /// buffering or dropping the stream. `id` is a request id (streamed
  /// response) or a stream id (WS/TCP relay).
  StreamPause { id: String },
  /// Server → client (v3): stream `id`'s backlog drained; resume producing.
  StreamResume { id: String },
  /// Client → server: subscribe this client *process* to topic filters. The
  /// server keys subscriptions on the process (`instance_group`), not on the
  /// connection, so a client running several services does not receive one
  /// copy per connection. Re-sent on every reconnect; the server holds no
  /// subscription for a connection that is gone.
  Subscribe { topics: Vec<String> },
  /// Client → server: drop these filters again.
  Unsubscribe { topics: Vec<String> },
  /// Server → client: a `Subscribe` filter was not accepted, and why.
  ///
  /// Without it the refusal is only in the server's log, and the operator of
  /// the client sees a subscription that silently never delivers, which is
  /// indistinguishable from a topic nobody publishes on.
  SubscribeRefused { topic: String, reason: String },
  /// Server → client: this publish went nowhere, and why.
  ///
  /// The same reasoning as `SubscribeRefused`, for the other direction. A
  /// publish the token's topics do not cover is dropped, and the local
  /// application has already been told the message was accepted; without this
  /// the only trace is a line in the server's log, on a machine the person
  /// debugging usually cannot read.
  PublishRefused { topic: String, reason: String },
  /// Either direction: one message on `topic`.
  ///
  /// Client → server publishes it to the organization; server → client is a
  /// delivery to a matching subscriber. `id` is assigned by the server and is
  /// the same across every delivery of one publish, so a subscriber can tell
  /// a redelivery from a new message.
  Publish {
    topic: String,
    /// Base64, like every other payload on this wire: a message is bytes, and
    /// the tunnel frame is text.
    payload: String,
    #[serde(default)]
    id: Option<String>,
    /// 0 = send once and forget. 1 = the server keeps it until the subscriber
    /// acknowledges it, and resends it meanwhile. Absent = 0, which is what a
    /// peer that predates the field means.
    #[serde(default, skip_serializing_if = "is_zero")]
    qos: u8,
  },
  /// Client → server: message `id` arrived. Only sent for a delivery that
  /// carried `qos: 1`; without it the server keeps resending until the
  /// message ages out.
  PublishAck { id: String },
}

/// Compresses a tunnel text frame into a zlib binary frame.
pub(crate) fn compress_frame(text: &str) -> Vec<u8> {
  use flate2::{Compression, write::ZlibEncoder};
  use std::io::Write;
  let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
  let _ = enc.write_all(text.as_bytes());
  enc.finish().unwrap_or_default()
}

/// Inflates a zlib binary frame back into a text frame, bounding the output
/// size to protect against decompression bombs from a misbehaving server.
pub(crate) fn decompress_frame(data: &[u8], max_out: usize) -> Option<String> {
  use flate2::read::ZlibDecoder;
  use std::io::Read;
  let mut out = String::new();
  let mut dec = ZlibDecoder::new(data).take(max_out as u64 + 1);
  dec.read_to_string(&mut out).ok()?;
  if out.len() > max_out {
    warn!("Dropped tunnel frame: decompressed size exceeds limit");
    return None;
  }
  Some(out)
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

/// Serializes and sends a tunnel message; returns Err(()) when the tunnel
/// write channel is closed.
pub(crate) async fn send_tunnel_msg(
  tx: &mpsc::Sender<Message>,
  msg: &TunnelMessage,
) -> Result<(), ()> {
  match serde_json::to_string(msg) {
    Ok(json) => tx.send(Message::Text(json.into())).await.map_err(|_| ()),
    Err(_) => Err(()),
  }
}
