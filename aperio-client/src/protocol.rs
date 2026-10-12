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
/// v8: a Ping may describe its work as a `services` list instead of the
/// singular per-service fields; when present it is authoritative and they
/// are ignored, when absent nothing changes. Additive in both directions.
/// v9: a service entry may carry `server_side_target`, which asks the server
/// to reach that address itself instead of dispatching over the tunnel. The
/// field is additive, but honouring it is not: a server that ignores it
/// relays the request instead, and a client that asked usually cannot reach
/// the target at all, so the ask is negotiated on the handshake rather than
/// sent hopefully. A Ping may also carry `name`, which needs no negotiation
/// because a server that ignores it shows the id, as it always did.
pub(crate) const PROTOCOL_VERSION: u32 = 10;

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

/// One relay payload as a tagged binary frame carrying the raw bytes. The
/// JSON message is only the shape for a stream id too long for the frame
/// header, which no id the server mints is.
pub(crate) fn relay_frame(
  tag: u8,
  stream_id: &str,
  bytes: &[u8],
  json_fallback: impl FnOnce(String) -> TunnelMessage,
) -> Option<Message> {
  if let Some(frame) = encode_binary_frame(tag, stream_id, bytes) {
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

// The tunnel declaration is shared with the `aperio.yaml` schema crate so the
// same type serves both the config file and the wire (Ping) form.
pub(crate) use aperio_config::{ScalingDecl, TunnelDecl};

/// Client-side stage durations of one proxied request, in microseconds from
/// the moment the client received the tunnel request. Attached to `Response`
/// and to the `ResponseStart` head of a streamed one, so the server can
/// assemble a request timeline; additive, older peers simply omit it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct ClientTimings {
  /// The backend request left the client.
  pub backend_sent_us: u64,
  /// The backend's response headers (first byte) arrived.
  pub backend_first_byte_us: u64,
  /// The backend body was fully read. `None` on the head of a streamed
  /// response, where the body is still arriving: the stage exists, it has
  /// simply not happened yet, and inventing an end for it would be the one
  /// thing a timeline must never do. A peer that only ever sends buffered
  /// responses always fills it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub backend_done_us: Option<u64>,
  /// The response frame, or the head of a streamed response, was handed to
  /// the tunnel.
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
/// One service a client serves, as the Ping declares it (`planned_features.md`
/// #46).
///
/// **The unit the protocol is growing towards.** Until v8 a Ping described
/// exactly one service in its own top-level fields, which made a connection
/// and a service the same thing: five services meant five sockets, five
/// readers, five writers and five heartbeats. This is that same description
/// lifted into a value, so a Ping can eventually carry a list of them.
///
/// Every field is optional or defaulted, because a client that names only a
/// hostname is the ordinary case and the wire should not carry thirty nulls
/// to say so.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ServiceDecl {
  /// What the client calls this service, and what names it in the dashboard
  /// and in per-service statistics.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub service: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub service_custom_name: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub path_bind: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hostname_bind: Option<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub hostname_binds: Vec<String>,
  /// True when the service has an HTTP target, so the server can tell it
  /// when nothing routes to it (a carrier of tunnels or messages has none).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub http: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_concurrent: Option<u32>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub tcp: bool,
  #[serde(default)]
  pub backend_healthy: bool,
  #[serde(default)]
  pub backend_probed: bool,
  #[serde(default, skip_serializing_if = "is_zero_u32")]
  pub priority: u32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub bandwidth_bps: Option<u64>,
  /// The address the server should reach for this service itself, instead of
  /// dispatching over the tunnel (`server_side: true` in the client's file).
  ///
  /// Sent only when the client asked for it, which is what keeps a relayed
  /// service's backend address off the wire: the server has never needed to
  /// know where a client connects, and it still does not unless the client is
  /// asking it to go there. Honored only when the token allows it and the
  /// address matches the server's `server_side_targets:`; a service that asks
  /// and is not permitted is refused rather than quietly relayed, because
  /// asking usually means this client cannot reach the target at all.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub server_side_target: Option<String>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub public: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub visitor_auth: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub visitor_auth_methods: Option<Vec<aperio_config::AuthMethodSpec>>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub allowed_ips: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub tunnels: Vec<TunnelDecl>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub cache: bool,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub resilience: bool,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub no_capture: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_request_body: Option<u64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub response_timeout: Option<u64>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub webhook_inbox: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub denied: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub scaling: Option<ScalingDecl>,
  /// The connection pool this service asked for, the three spellings the
  /// Ping has always carried: a fixed `connections: n`, or an elastic pool's
  /// floor and ceiling. Per-service in `aperio.yaml` (`ServiceEntry`), so it
  /// belongs to the entry rather than to the connection, even though what it
  /// counts is connections. How a per-service pool composes with several
  /// services on one socket is the open question in `planned_features.md`
  /// #48, and leaving the field out here would have hidden it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub connections: Option<u32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub connections_min: Option<u32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub connections_max: Option<u32>,
  /// Settings this service resolved to something other than its config asked
  /// for. Built per service on the client, so it is reported per service.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub config_notes: Vec<ConfigNote>,
  /// Extra Prometheus labels for this service's metrics, per-service in
  /// `aperio.yaml`. Still bounded by the server on arrival, for the reason
  /// the singular field gives: label cardinality is how a metrics backend
  /// dies, and these come from clients.
  #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
  pub metrics_labels: std::collections::BTreeMap<String, String>,
}

fn is_zero_u32(v: &u32) -> bool {
  *v == 0
}

#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub(crate) enum TunnelMessage {
  Ping {
    /// Every service this connection serves, at least one. A connection
    /// carrying one service sends a list of one; there is no second spelling
    /// in top-level fields, so the declaration has one shape to read.
    #[serde(default)]
    services: Vec<ServiceDecl>,
    client_id: String,
    /// What to call this client, from `name:` in its file. A label the server
    /// shows instead of an id, never something it addresses: `client_id`
    /// above stays the identity, because failover and `bind_tunnels:` need
    /// one value per process and a name is shared by replicas on purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    timestamp: u64,
    /// Client build version (CARGO_PKG_VERSION), for display/diagnostics.
    #[serde(default)]
    version: Option<String>,
    /// Tunnel wire protocol version the client speaks, for display.
    #[serde(default)]
    protocol: Option<u32>,
    /// Trust-on-first-use device key for token pinning. The server pins the
    /// first key seen for the token and rejects a later connection announcing
    /// a different one (None = not announced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_key: Option<String>,
    /// Seconds this client gives its own in-flight requests when it is asked
    /// to stand down (`reload_drain:`). Announced so the server can size its
    /// shutdown drain from what its clients actually expect, rather than from
    /// a number nobody revisits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drain_secs: Option<u64>,
    /// What the client observes about itself: CPU as a percentage of one core
    /// and resident memory of the client process, and round-trip time, jitter
    /// and reconnect count of this tunnel connection. The process figures are
    /// absent on platforms where they cannot be read without guessing.
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
    /// Which of the connection's services this is for, by the name the
    /// client gave it (protocol v8). `None` on a connection carrying one,
    /// which is every client before v8 and every ordinary one after it.
    ///
    /// Only the frames that *open* something carry it. Everything that
    /// follows is addressed by a stream id the server minted here, so the
    /// client learns the service once and routes the rest by id; putting a
    /// selector on every frame would have meant changing the binary framing
    /// for a value that never changes mid-stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
  },
  /// Start of a streamed request body (protocol v2): method/uri/headers
  /// only; the body follows as RequestChunk frames ended by RequestEnd.
  RequestStart {
    id: String,
    /// Which of the connection's services this is for, by the name the
    /// client gave it (protocol v8). `None` on a connection carrying one,
    /// which is every client before v8 and every ordinary one after it.
    ///
    /// Only the frames that *open* something carry it. Everything that
    /// follows is addressed by a stream id the server minted here, so the
    /// client learns the service once and routes the rest by id; putting a
    /// selector on every frame would have meant changing the binary framing
    /// for a value that never changes mid-stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
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
    /// Stage durations up to the head. A streamed body ends long after this
    /// message, so `backend_done_us` is absent here while every earlier
    /// stage is measured exactly as it is for a buffered response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timings: Option<ClientTimings>,
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
    /// Which of the connection's services this is for, by the name the
    /// client gave it (protocol v8). `None` on a connection carrying one,
    /// which is every client before v8 and every ordinary one after it.
    ///
    /// Only the frames that *open* something carry it. Everything that
    /// follows is addressed by a stream id the server minted here, so the
    /// client learns the service once and routes the rest by id; putting a
    /// selector on every frame would have meant changing the binary framing
    /// for a value that never changes mid-stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
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
  /// Server → client: a sentence about this client's own configuration that
  /// the server acted on (a gate it did not apply, a route it refuses), so the
  /// client's log says why rather than only the server's.
  Notice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    message: String,
  },
  /// Client → server: the client received a shutdown signal and is draining.
  Draining {},
  /// Server → client: ask the endpoint on the client's own network about a
  /// request (`forward` with `via: client`, planned_features #157). The
  /// headers are the ones the server already composed and allow-listed,
  /// `X-Forwarded-*` included; `response_headers` names what may come back.
  AuthAsk {
    id: String,
    url: String,
    method: String,
    uri: String,
    #[serde(default)]
    host: Option<String>,
    visitor_ip: String,
    #[serde(default)]
    request_headers: Vec<(String, String)>,
    #[serde(default)]
    response_headers: Vec<String>,
    timeout_ms: u64,
  },
  /// Client → server: the endpoint's answer, a status and the headers a
  /// browser or the allowlist needs, or why it could not be asked.
  AuthVerdict {
    id: String,
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
  },
  /// Server → client: open a raw TCP connection for this stream. `target`
  /// selects one of the client's declared tunnels; when absent the legacy
  /// `tcp_target` is used. The client only ever connects to addresses it
  /// itself declared, regardless of what the server asks.
  TcpOpen {
    stream_id: String,
    /// Which of the connection's services this is for, by the name the
    /// client gave it (protocol v8). `None` on a connection carrying one,
    /// which is every client before v8 and every ordinary one after it.
    ///
    /// Only the frames that *open* something carry it. Everything that
    /// follows is addressed by a stream id the server minted here, so the
    /// client learns the service once and routes the rest by id; putting a
    /// selector on every frame would have meant changing the binary framing
    /// for a value that never changes mid-stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    #[serde(default)]
    target: Option<String>,
    /// The visitor's address, `ip:port`, as the server observed it. Carried so
    /// a tunnel with `proxy_protocol: true` can tell its backend who is
    /// really calling, which is otherwise lost at the last hop: the backend
    /// sees a connection from the client process. Additive; an older server
    /// omits it and the client then writes no header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visitor: Option<String>,
  },
  /// Raw TCP bytes relayed through the tunnel (Base64).
  TcpData { stream_id: String, data: String },
  /// Client → server: an OpenTelemetry export to forward to the collector
  /// (the OTel bridge). `signal` is `traces`, `metrics` or `logs`; the payload
  /// is an OTLP protobuf, Base64 for the same reason every other relay
  /// payload is on this JSON channel.
  ///
  /// On the tunnel rather than over HTTPS because the point of the bridge is
  /// an edge host with exactly one outbound connection; sending it here keeps
  /// that property.
  OtlpExport { signal: String, data: String },
  /// Signals that a TCP stream has been closed (either side).
  TcpClose { stream_id: String },
  /// Protocol v10, server to client: drain queued input and write EOF while
  /// continuing to relay backend responses. TcpClose still aborts immediately.
  TcpEof { stream_id: String },
  /// Server → client: open a UDP relay for this stream toward one of the
  /// client's declared `protocol: udp` tunnels. The client only ever sends
  /// to addresses it itself declared, regardless of what the server asks.
  UdpOpen {
    stream_id: String,
    target: String,
    /// Which of the connection's services this is for, by the name the
    /// client gave it (protocol v8). `None` on a connection carrying one,
    /// which is every client before v8 and every ordinary one after it.
    ///
    /// Only the frames that *open* something carry it. Everything that
    /// follows is addressed by a stream id the server minted here, so the
    /// client learns the service once and routes the rest by id; putting a
    /// selector on every frame would have meant changing the binary framing
    /// for a value that never changes mid-stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
  },
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

/// Reads a frame this client sent as the message it stands for.
///
/// Responses, response chunks and relay payloads travel as tagged binary
/// frames; a test inspecting the tunnel reads both shapes through here, a
/// binary frame coming back as its JSON variant with the bytes base64-encoded.
#[cfg(test)]
pub(crate) fn decode_sent(msg: &tokio_tungstenite::tungstenite::Message) -> Option<TunnelMessage> {
  use base64::prelude::*;
  use tokio_tungstenite::tungstenite::Message;
  match msg {
    Message::Text(text) => serde_json::from_str(text).ok(),
    Message::Binary(bytes) => {
      let (tag, id, payload) = decode_binary_frame(bytes)?;
      let encoded = BASE64_STANDARD.encode(payload);
      match tag {
        FRAME_RESPONSE_CHUNK => Some(TunnelMessage::ResponseChunk {
          id: id.to_string(),
          data: encoded,
        }),
        FRAME_TCP_DATA => Some(TunnelMessage::TcpData {
          stream_id: id.to_string(),
          data: encoded,
        }),
        FRAME_UDP_DATAGRAM => Some(TunnelMessage::UdpDatagram {
          stream_id: id.to_string(),
          data: encoded,
        }),
        FRAME_WS_DATA_BIN => Some(TunnelMessage::WsData {
          stream_id: id.to_string(),
          data: encoded,
          is_text: false,
        }),
        FRAME_RESPONSE_FULL | FRAME_RESPONSE_FULL_ZLIB => {
          let inflated;
          let payload = if tag == FRAME_RESPONSE_FULL_ZLIB {
            inflated = inflate_payload(payload, 64 << 20)?;
            &inflated[..]
          } else {
            payload
          };
          let (json, body) = split_full_response(payload)?;
          match serde_json::from_str(json).ok()? {
            TunnelMessage::Response {
              id,
              status,
              headers,
              trailers,
              timings,
              ..
            } => Some(TunnelMessage::Response {
              id,
              status,
              headers,
              body: Some(BASE64_STANDARD.encode(body)),
              trailers,
              timings,
            }),
            other => Some(other),
          }
        }
        _ => None,
      }
    }
    _ => None,
  }
}
