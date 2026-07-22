use serde::{Deserialize, Serialize};
use tracing::warn;

/// Version of the tunnel wire protocol. Bumped on breaking changes to
/// `TunnelMessage` so version skew between client and server is surfaced
/// (in logs and on the dashboard) instead of failing in obscure ways.
/// v2: streamed request bodies (RequestStart/Chunk/End) and raw binary
/// chunk frames instead of base64+JSON for body data.
pub const PROTOCOL_VERSION: u32 = 2;

// --- Protocol v2 binary frames: [tag][id_len][id bytes][payload] ---
// Data-heavy chunk messages skip the base64+JSON encoding entirely. The tag
// byte never collides with zlib-compressed JSON frames, which start with
// 0x78.

/// Binary frame tag for a streamed request-body chunk (server → client).
pub const FRAME_REQUEST_CHUNK: u8 = 1;
/// Binary frame tag for a streamed response-body chunk (client → server).
pub const FRAME_RESPONSE_CHUNK: u8 = 2;

/// Encodes a v2 binary chunk frame.
pub(crate) fn encode_binary_frame(tag: u8, id: &str, payload: &[u8]) -> Vec<u8> {
  // The length prefix is one byte: ids are request UUIDs (~36 bytes), always
  // well under 255. Assert the invariant so a future change that grows the id
  // is caught in tests rather than silently truncating on the wire.
  debug_assert!(
    id.len() <= u8::MAX as usize,
    "binary frame id length {} exceeds the u8 length prefix",
    id.len()
  );
  let mut out = Vec::with_capacity(2 + id.len() + payload.len());
  out.push(tag);
  out.push(id.len() as u8);
  out.extend_from_slice(id.as_bytes());
  out.extend_from_slice(payload);
  out
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
fn default_true() -> bool {
  true
}

/// Serde default protocol of a declared tunnel.
fn default_tcp() -> String {
  "tcp".to_string()
}

/// One tunnel declared by a client (`tunnels:` list in its aperio.yaml): a
/// normally unexposed local service that a peer client may reach through
/// the server with `--bind-tunnels` — same token, explicit client id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TunnelDecl {
  /// Local address the declaring client connects to, e.g. `127.0.0.1:27017`.
  pub target: String,
  /// Transport protocol: `tcp` or `udp` (best-effort datagram relay).
  #[serde(default = "default_tcp")]
  pub protocol: String,
  /// The tunnel is end-to-end encrypted between the two clients; the server
  /// only relays ciphertext. Relayed to binders via tunnel discovery.
  #[serde(default)]
  pub encrypt: bool,
  /// UDP only: seconds a relay may sit idle before it expires (default 60).
  /// Declared by the owning client, relayed to binders via tunnel discovery.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub idle_timeout: Option<u64>,
  /// Public-expose key (experimental): matches this tunnel to an `expose:`
  /// entry of aperio-server.yaml. A shared secret — deserialized from the
  /// client's Ping but never re-serialized (tunnel discovery must not leak
  /// it to same-token binders).
  #[serde(default, skip_serializing)]
  pub expose: Option<String>,
}

/// Client-side stage durations of one proxied request, in microseconds from
/// the moment the client received the tunnel request. Attached to buffered
/// `Response` messages so the server can assemble a request timeline;
/// additive — older peers simply omit it.
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

/// Message structure exchanged over the WebSocket reverse tunnel.
// The `Ping` variant is intentionally wide (it announces the client's full
// per-service configuration); boxing its many small fields would only obscure
// the protocol for no real memory win, since Pings are short-lived.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum TunnelMessage {
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
    /// Maximum concurrent requests the client is willing to process.
    /// The server queues excess requests instead of dispatching them.
    #[serde(default)]
    max_concurrent: Option<u32>,
    /// True when the client has a TCP target configured (APERIO_TCP_TARGET).
    #[serde(default)]
    tcp: bool,
    /// Client build version (CARGO_PKG_VERSION), for display/diagnostics.
    #[serde(default)]
    version: Option<String>,
    /// Tunnel wire protocol version the client speaks.
    #[serde(default)]
    protocol: Option<u32>,
    /// Result of the client's own backend health probe. False takes the
    /// client out of routing without dropping the tunnel connection.
    #[serde(default = "default_true")]
    backend_healthy: bool,
    /// False only while a configured health check has not completed its first
    /// probe yet (UI shows "checking" vs "down"). Older peers omit it → true.
    #[serde(default = "default_true")]
    backend_probed: bool,
    /// Load-balancing priority tier: 0 = primary (default), higher numbers
    /// are standbys. Only used with APERIO_LB_STRATEGY=primary-standby.
    #[serde(default)]
    priority: u32,
    /// Announced downstream link capacity in bytes/second. The server paces
    /// tunnel frames so this client is never pushed faster than its network.
    #[serde(default)]
    bandwidth_bps: Option<u64>,
    /// Display name of the service this connection exposes (from the
    /// client's `services:` list), for the dashboard.
    #[serde(default)]
    service: Option<String>,
    /// The client declares its service public: skip the visitor auth gate
    /// for traffic routed here (honored only when the token permits it).
    #[serde(default)]
    public: bool,
    /// Per-service visitor credentials ("user:password") declared by the
    /// client: the server gates traffic routed here behind a login with these
    /// credentials, overriding (or, when the server has none, introducing) the
    /// visitor auth gate. Honored only when the token may control the visitor
    /// gate (same permission as `public`) and the server has not set
    /// APERIO_IGNORE_CLIENT_AUTH. None = no override.
    #[serde(default)]
    visitor_auth: Option<String>,
    /// Visitor IPs/CIDRs allowed to reach this service (empty = everyone).
    /// Enforced against every visitor request before dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_ips: Vec<String>,
    /// Tunnels declared by the client (`tunnels:` list): normally
    /// unexposed local services reachable by a peer client via
    /// `--bind-tunnels` with the same token and this client's id.
    #[serde(default)]
    tunnels: Vec<TunnelDecl>,
    /// The client opts this service into the server-side response cache
    /// (effective only when the server enables APERIO_CACHE).
    #[serde(default)]
    cache: bool,
    /// The client asks the server to keep serving this service's cached
    /// responses (marked, even expired) while no healthy client is
    /// connected, instead of failing with 504. Needs `cache`.
    #[serde(default)]
    resilience: bool,
    /// Largest request body, in bytes, visitors may upload to this service.
    /// The server answers bigger uploads with an early 413 before they enter
    /// the tunnel (None = only the global APERIO_MAX_BODY_SIZE applies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_request_body: Option<u64>,
    /// Per-service override of the global gateway response timeout, in seconds:
    /// how long the server waits for this service to answer a dispatched
    /// request before failing it (None = use the global value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response_timeout: Option<u64>,
    /// Trust-on-first-use device key for token pinning. The server pins the
    /// first key seen for the token and rejects a later connection announcing
    /// a different one (None = not announced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_key: Option<String>,
    /// The client asks the server to persist inbound POSTs to this service
    /// into the webhook inbox (browse & re-fire from the dashboard).
    #[serde(default)]
    webhook_inbox: bool,
    /// Redirect URL answered to visitors rejected by `allowed_ips` when no
    /// candidate of the route admits them (None = stealth).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    denied: Option<String>,
  },
  Pong {
    timestamp: u64,
    /// Server build version, echoed so the client can log mismatches.
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
  /// as `ResponseChunk` messages terminated by `ResponseEnd`. Used by clients
  /// for large bodies so neither side buffers the full payload in memory.
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
  /// Abnormal end of a streamed response (body exceeded the size limit, or the
  /// backend errored mid-stream). The server drops the visitor's body stream
  /// with an error so the visitor sees an aborted response, not a silently
  /// truncated success.
  ResponseAbort { id: String },
  /// Sent by server to instruct a client to open a WebSocket connection to the local backend.
  UpgradeRequest {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
  },
  /// Sent by client after the backend WebSocket upgrade handshake completes (or fails).
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
  /// The server stops routing new requests to it; in-flight requests finish.
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
  /// is about to drop. Clients switch to aggressive (no-backoff) reconnect so
  /// downtime is limited to the actual restart window. Older clients ignore
  /// the unknown message and reconnect on their normal backoff.
  ServerShutdown {},
  /// Server → client: offers zlib compression for subsequent tunnel frames.
  CompressionStart {},
  /// Client → server: compression accepted; both sides may now send
  /// compressed binary frames.
  CompressionAck {},
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
/// size to protect against decompression bombs.
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
