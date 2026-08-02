//! HTTP request forwarding: proxies tunnel requests to the local target,
//! streaming large response bodies back through the tunnel in chunks.

use base64::prelude::*;
use futures_util::FutureExt;
use futures_util::stream::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{error, info, warn};

use crate::protocol::{FRAME_RESPONSE_CHUNK, TunnelMessage, encode_binary_frame, send_tunnel_msg};

/// Response bodies larger than this are streamed through the tunnel in
/// chunks instead of being buffered and sent as one message. Used with a peer
/// that cannot take binary frames, where streaming means base64 chunks and is
/// therefore only worth it to bound memory.
pub(crate) const STREAM_THRESHOLD: usize = 256 * 1024;

/// The same threshold for a peer that takes binary frames, where streaming
/// also means the body stops being base64-encoded and JSON-escaped.
///
/// Measured on loopback, median of three interleaved runs, buffered against
/// streamed at the same body size:
///
/// | body   | buffered | streamed |
/// |--------|----------|----------|
/// | 8 KB   | **+18%** |          |
/// | 16 KB  | **+14%** |          |
/// | 32 KB  | tie      | tie      |
/// | 64 KB  |          | **+23%** |
/// | 128 KB |          | **+36%** |
///
/// Streaming has a fixed cost per response (a head message, a frame per
/// chunk, a tail, and a pause registration) that a small body cannot repay,
/// and base64 has a per-byte cost that a large one cannot escape. They cross
/// at 32 KB, so that is where the switch goes: everything above it is the
/// side that wins, everything below keeps the single message it wanted.
pub(crate) const BINARY_STREAM_THRESHOLD: usize = 32 * 1024;
/// Size of individual streamed body chunks.
pub(crate) const STREAM_CHUNK_SIZE: usize = 128 * 1024;

/// Batches backend body chunks into full `STREAM_CHUNK_SIZE` frames
/// (planned_features #24). A backend yields bytes in read-sized pieces well
/// under the frame size, and every frame pays its own allocation, client-side
/// mask pass over each byte, and writer flush; full frames cut that per-frame
/// cost several-fold. The caller flushes the remainder (`take`) the moment
/// the backend has nothing more ready, so a trickling stream (server-sent
/// events, long polling) is never held back waiting for a full frame.
pub(crate) struct ChunkCoalescer {
  buf: Vec<u8>,
}

impl ChunkCoalescer {
  pub(crate) fn new() -> Self {
    ChunkCoalescer { buf: Vec::new() }
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.buf.is_empty()
  }

  /// Appends one backend chunk.
  pub(crate) fn add(&mut self, data: &[u8]) {
    self.buf.extend_from_slice(data);
  }

  /// A full frame's worth, when one has accumulated.
  pub(crate) fn pop_full(&mut self) -> Option<Vec<u8>> {
    if self.buf.len() < STREAM_CHUNK_SIZE {
      return None;
    }
    let rest = self.buf.split_off(STREAM_CHUNK_SIZE);
    Some(std::mem::replace(&mut self.buf, rest))
  }

  /// Whatever is left, for the backend-quiet and end-of-stream flushes.
  pub(crate) fn take(&mut self) -> Option<Vec<u8>> {
    if self.buf.is_empty() {
      None
    } else {
      Some(std::mem::take(&mut self.buf))
    }
  }
}

/// Sends a whole buffered response as one v5 binary frame: the envelope and
/// the body in a single message, with the body as bytes.
///
/// The alternative is what every version before v5 did, base64 the body into
/// the JSON: a third more bytes on the wire, an encode pass here and a decode
/// pass on the server, and a String the size of the response held on both
/// sides. Returns whether it went out; a peer that cannot take the frame is
/// never offered one, so a failure here is a dead connection rather than a
/// version problem.
pub(crate) async fn send_full_response(
  tunnel_tx: &mpsc::Sender<Message>,
  id: &str,
  message: &TunnelMessage,
  body: &[u8],
) -> bool {
  let Ok(json) = serde_json::to_string(message) else {
    return false;
  };
  let Some(frame) = crate::protocol::encode_full_response_frame(id, &json, body) else {
    return false;
  };
  tunnel_tx.send(Message::Binary(frame)).await.is_ok()
}

/// Sends one streamed response chunk: a raw binary frame for v2 servers, or
/// the legacy base64+JSON message otherwise. Honors the stream's pause
/// switch first, so a `StreamPause` from the server stops the body read
/// loop right here and TCP backpressure reaches the backend.
pub(crate) async fn send_response_chunk(
  tunnel_tx: &mpsc::Sender<Message>,
  id: &str,
  part: &[u8],
  binary: bool,
  pause: &crate::flow::PauseSignal,
) -> Result<(), ()> {
  pause.wait_while_paused().await;
  if binary {
    // An id too long to frame is a broken peer, not a chunk to guess at.
    let Some(frame) = encode_binary_frame(FRAME_RESPONSE_CHUNK, id, part) else {
      tracing::warn!("Refusing to frame a response chunk: request id is too long to encode");
      return Err(());
    };
    tunnel_tx.send(Message::Binary(frame)).await.map_err(|_| ())
  } else {
    let msg = TunnelMessage::ResponseChunk {
      id: id.to_string(),
      data: BASE64_STANDARD.encode(part),
    };
    send_tunnel_msg(tunnel_tx, &msg).await
  }
}

/// True when two hosts belong to the same site for redirect purposes:
/// identical hosts, or hostnames sharing at least the last two DNS labels
/// (`example.com` ↔ `test.example.com`, `a.example.com` ↔ `b.example.com`).
/// IP addresses and single-label hosts (`localhost`) only match exactly.
pub(crate) fn same_site(a: &str, b: &str) -> bool {
  let a = a.trim_end_matches('.').to_ascii_lowercase();
  let b = b.trim_end_matches('.').to_ascii_lowercase();
  if a == b {
    return true;
  }
  // IP literals never match a different host.
  if a.parse::<std::net::IpAddr>().is_ok() || b.parse::<std::net::IpAddr>().is_ok() {
    return false;
  }
  let shared = a
    .rsplit('.')
    .zip(b.rsplit('.'))
    .take_while(|(x, y)| x == y)
    .count();
  shared >= 2
}

/// Redirect policy for backend requests: follows same-host scheme upgrades
/// (`http://x` → `https://x`) and redirects within the same root domain, up
/// to `max_hops` jumps and never downgrading https to http. Anything else,
/// including the hop after the limit, is passed through to the visitor as a
/// regular redirect response, exactly like `Policy::none`.
pub(crate) fn redirect_policy(max_hops: usize) -> reqwest::redirect::Policy {
  if max_hops == 0 {
    return reqwest::redirect::Policy::none();
  }
  reqwest::redirect::Policy::custom(move |attempt| {
    if attempt.previous().len() > max_hops {
      return attempt.stop();
    }
    let orig = match attempt.previous().first() {
      Some(u) => u.clone(),
      None => return attempt.stop(),
    };
    let next = attempt.url();
    // https → http downgrades and non-http schemes are never followed.
    let scheme_ok = matches!(
      (orig.scheme(), next.scheme()),
      ("http", "http") | ("http", "https") | ("https", "https")
    );
    let host_ok = match (orig.host_str(), next.host_str()) {
      (Some(a), Some(b)) => same_site(a, b),
      _ => false,
    };
    if scheme_ok && host_ok {
      attempt.follow()
    } else {
      attempt.stop()
    }
  })
}

/// Compiled form of the config `headers:` add/remove rules for one traffic
/// direction: removals are matched case-insensitively, additions replace any
/// existing header of the same name.
#[derive(Default)]
pub(crate) struct HeaderTransform {
  /// Headers to set (original-case name, value).
  add: Vec<(String, String)>,
  /// Lowercased names to strip (includes the names being re-added).
  remove: std::collections::HashSet<String>,
}

impl HeaderTransform {
  /// Compiles the config directives for one direction (None = no edits).
  pub(crate) fn compile(directives: Option<&aperio_config::HeaderDirectives>) -> Self {
    let Some(d) = directives else {
      return HeaderTransform::default();
    };
    let mut remove: std::collections::HashSet<String> =
      d.remove.iter().map(|n| n.to_ascii_lowercase()).collect();
    let add: Vec<(String, String)> = d.add.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (name, _) in &add {
      remove.insert(name.to_ascii_lowercase());
    }
    HeaderTransform { add, remove }
  }

  /// True when there is nothing to do (the fast path for most services).
  fn is_empty(&self) -> bool {
    self.add.is_empty() && self.remove.is_empty()
  }

  /// Applies the rules to a header list: strips removals (and old values of
  /// re-added names), then appends the additions.
  pub(crate) fn apply(&self, mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
    if self.is_empty() {
      return headers;
    }
    headers.retain(|(k, _)| !self.remove.contains(&k.to_ascii_lowercase()));
    headers.extend(self.add.iter().cloned());
    headers
  }
}

/// Per-connection constants for request forwarding, so per-request calls
/// only carry the request itself.
pub(crate) struct ForwardContext {
  /// HTTP client used for all backend calls on this connection.
  pub(crate) client: reqwest::Client,
  /// Local backend base URL.
  pub(crate) target: String,
  /// The same URL, parsed once. It does not change for the life of the
  /// connection, and parsing it per request showed up in a profile as its own
  /// line: `url::Url::parse` is not cheap, and this one had the same answer
  /// every time. `None` for a target that is not a URL at all, which is a
  /// configuration error and answers 502, the same as when the parse lived in
  /// the request path.
  pub(crate) target_url: Option<url::Url>,
  /// Forward the original `Host` header instead of the target's.
  pub(crate) pass_hostname: bool,
  /// Path bind of this client, stripped from incoming paths when `trim_bind`.
  pub(crate) path_bind: Option<String>,
  pub(crate) trim_bind: bool,
  /// Cap on response bodies read from the backend.
  pub(crate) max_response_body_size: usize,
  /// Write half of the tunnel, used for streamed responses.
  pub(crate) tunnel_tx: mpsc::Sender<Message>,
  /// Header edits applied to forwarded requests (config `headers.request`).
  pub(crate) request_headers: HeaderTransform,
  /// Header edits applied to backend responses (config `headers.response`).
  pub(crate) response_headers: HeaderTransform,
  /// HTTP/2 client for `h2c://` / `h2://` targets (None = plain HTTP target).
  pub(crate) h2_client: Option<std::sync::Arc<crate::proxy::h2::H2Client>>,
  /// Filesystem path of a `unix://` target's socket (None = TCP target).
  pub(crate) unix_socket: Option<String>,
  /// Seconds to wait for the backend's response head on the HTTP/2 path
  /// (the reqwest path carries its timeout inside `client`).
  pub(crate) timeout_secs: u64,
  /// Pause switches for the streams this connection produces (server flow
  /// control, protocol v3); streamed response bodies register here.
  pub(crate) stream_pauses: crate::flow::PauseRegistry,
}

/// Builds the backend URL for an incoming proxied request: maps the path
/// (optionally stripping the path bind), copies the query, and verifies the
/// result still points at the configured target (SSRF defence-in-depth).
/// Errors are the HTTP status to answer with.
pub(crate) fn build_dest_url(
  ctx: &ForwardContext,
  id: &str,
  uri_str: &str,
) -> Result<url::Url, u16> {
  let Some(target_parsed) = ctx.target_url.as_ref() else {
    error!("Failed to parse local target URL: {:?}", ctx.target);
    return Err(502);
  };
  // The path and the query. Origin-form (`/a/b?c`) is every HTTP/1.1 visitor
  // and splits without allocating; parsing `http://localhost{uri}` for it ran
  // a full URL parse per request to reach two `&str`s, and `set_path` below
  // normalizes what that parse normalized, which
  // `splitting_the_uri_agrees_with_parsing_it_as_a_url` holds to.
  //
  // Absolute-form (`http://host/a`) arrives from HTTP/2 visitors, where the
  // URI is rebuilt from `:scheme` and `:authority`. It is parsed, once, for
  // the shape that needs it. That also fixes what the old expression did with
  // it: prefixing `http://localhost` made the whole URI the *path*, so the
  // backend received `/127.0.0.1:8080/echo` instead of `/echo`.
  let uri_str = if uri_str.is_empty() { "/" } else { uri_str };
  let absolute;
  let (incoming_path_raw, incoming_query) = if uri_str.starts_with('/') {
    match uri_str.split_once('?') {
      Some((path, query)) => (path, Some(query)),
      None => (uri_str, None),
    }
  } else {
    absolute = match url::Url::parse(uri_str) {
      Ok(url) => url,
      Err(e) => {
        error!("Failed to parse incoming proxy URI {:?}: {:?}", uri_str, e);
        return Err(400);
      }
    };
    (absolute.path(), absolute.query())
  };

  let mut dest_url = target_parsed.clone();
  let target_path = target_parsed.path().trim_end_matches('/');
  let mut incoming_path = incoming_path_raw.trim_start_matches('/').to_string();
  if ctx.trim_bind
    && let Some(ref bind) = ctx.path_bind
  {
    let bind_trimmed = bind.trim_matches('/');
    // Match only at a path-segment boundary: bind `/api` trims `/api` and
    // `/api/x` but must NOT match `/apiv2/x` (which is a different route).
    let matches_bind = match incoming_path.strip_prefix(bind_trimmed) {
      Some(rest) => rest.is_empty() || rest.starts_with('/'),
      None => false,
    };
    if matches_bind {
      incoming_path = incoming_path[bind_trimmed.len()..]
        .trim_start_matches('/')
        .to_string();
    }
  }
  let combined_path = if target_path.is_empty() {
    format!("/{}", incoming_path)
  } else {
    format!("{}/{}", target_path, incoming_path)
  };
  dest_url.set_path(&combined_path);
  dest_url.set_query(incoming_query);

  if dest_url.scheme() != target_parsed.scheme()
    || dest_url.host_str() != target_parsed.host_str()
    || dest_url.port_or_known_default() != target_parsed.port_or_known_default()
  {
    error!(
      "SSRF protection triggered: constructed URL diverges from target for request ID {}",
      id
    );
    return Err(400);
  }
  Ok(dest_url)
}

/// One proxied request as received from the tunnel.
pub(crate) struct ForwardRequest {
  pub(crate) id: String,
  pub(crate) method: String,
  pub(crate) uri: String,
  pub(crate) headers: Vec<(String, String)>,
  /// Base64-encoded buffered body (None when absent, streamed, or carried as
  /// bytes in a v6 frame).
  pub(crate) body: Option<String>,
  /// The buffered body as bytes, from a v6 full-request frame. Takes
  /// precedence over `body`: only one of the two is ever set, and this one
  /// costs no decode.
  pub(crate) raw_body: Option<Vec<u8>>,
}

/// Forwards a proxied HTTP request from the websocket tunnel to the local target server.
/// Sanitizes sensitive/upgrade headers, rewrites URLs, routes the HTTP request, and returns
/// the response mapped back into a `TunnelMessage`.
///
/// Small responses are returned as `Some(TunnelMessage::Response)` for the
/// caller to send. Large responses are streamed directly through the tunnel
/// (ResponseStart/Chunk/End) and `None` is returned, and so is a buffered
/// response sent as a v5 full-body frame, which this sends itself for the
/// same reason: the body does not fit through a `TunnelMessage`.
pub(crate) async fn handle_incoming_request(
  ctx: &ForwardContext,
  req: ForwardRequest,
  streamed_body: Option<mpsc::Receiver<Result<Vec<u8>, std::io::Error>>>,
  binary_chunks: bool,
  full_body_frames: bool,
) -> Option<TunnelMessage> {
  // HTTP/2 targets (h2c:// / h2://, e.g. gRPC backends) take the hyper-based
  // path, which speaks HTTP/2 to the backend and relays trailers.
  if ctx.h2_client.is_some() {
    return crate::proxy::h2::handle_incoming_request_h2(ctx, req, streamed_body, binary_chunks)
      .await;
  }
  // Unix socket targets (unix:///path.sock) take the hyper-based path that
  // dials the socket directly; reqwest cannot.
  if ctx.unix_socket.is_some() {
    return crate::proxy::unix::handle_incoming_request_unix(
      ctx,
      req,
      streamed_body,
      binary_chunks,
    )
    .await;
  }
  // Timeline anchor: everything below is measured from receipt of the
  // tunnel request, in microseconds, and reported with buffered responses.
  let received_at = std::time::Instant::now();
  let ForwardRequest {
    id,
    method: method_str,
    uri: uri_str,
    headers,
    body: body_base64,
    raw_body,
  } = req;
  let tunnel_tx = &ctx.tunnel_tx;
  info!(
    "Forwarding tunnel request ID {}: {} {}",
    id, method_str, uri_str
  );
  let dest_url = match build_dest_url(ctx, &id, &uri_str) {
    Ok(url) => url,
    Err(status) => return Some(make_error_response(id, status)),
  };

  let method = match reqwest::Method::from_bytes(method_str.as_bytes()) {
    Ok(m) => m,
    Err(e) => {
      error!("Invalid HTTP method representation: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  let mut builder = ctx.client.request(method, dest_url);

  // Config header rules first; the critical strips below still apply to
  // anything they add, so tunnel-managed headers cannot be smuggled in.
  let headers = ctx.request_headers.apply(headers);

  // Map Headers
  let mut host_header_val = None;
  for (k, v) in headers.iter() {
    let k_lower = k.to_lowercase();

    // CRITICAL: Strip connection control, upgrade, and websocket headers.
    // transfer-encoding / trailer are hop-by-hop framing headers: forwarding a
    // visitor-supplied `transfer-encoding: chunked` would collide with
    // reqwest's own body framing and open an HTTP desync / request-smuggling
    // surface. Dropping transfer-encoding leaves content-length as the single
    // framing signal (mirrors the h2.rs path, which also drops only TE).
    // content-length is intentionally kept: the streamed-upload path below
    // relies on it so reqwest frames with content-length instead of falling
    // back to chunked, which content-length-only backends cannot read.
    if k_lower == "connection"
      || k_lower == "keep-alive"
      || k_lower == "upgrade"
      || k_lower == "proxy-connection"
      || k_lower == "accept-encoding"
      || k_lower == "transfer-encoding"
      || k_lower == "trailer"
      || k_lower.starts_with("sec-websocket-")
    {
      continue;
    }

    if k_lower == "host" {
      // Never forwarded from here, the pass_hostname block below adds it
      // exactly once (reqwest's .header() appends, so adding it in both
      // places produced a duplicate Host header). Without pass_hostname the
      // target authority is used instead.
      host_header_val = Some(v.clone());
      continue;
    }

    if let (Ok(name), Ok(val)) = (
      reqwest::header::HeaderName::from_bytes(k.as_bytes()),
      reqwest::header::HeaderValue::from_str(v),
    ) {
      builder = builder.header(name, val);
    }
  }

  if ctx.pass_hostname
    && let Some(host) = host_header_val
    && let Ok(val) = reqwest::header::HeaderValue::from_str(&host)
  {
    builder = builder.header(reqwest::header::HOST, val);
  }

  // Map Body: either the buffered base64 payload, or a protocol v2 streamed
  // body fed chunk-by-chunk from the tunnel read loop.
  if let Some(rx) = streamed_body {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
      rx.recv().await.map(|item| (item, rx))
    });
    builder = builder.body(reqwest::Body::wrap_stream(stream));
  } else if let Some(bytes) = raw_body {
    // v6: the body arrived as bytes in the dispatch frame, nothing to decode.
    builder = builder.body(bytes);
  } else if let Some(encoded_body) = body_base64 {
    match BASE64_STANDARD.decode(encoded_body) {
      Ok(bytes) => {
        builder = builder.body(bytes);
      }
      Err(e) => {
        error!("Base64 decoding failed for request body payload: {:?}", e);
        return Some(make_error_response(id, 400));
      }
    }
  }

  // Execute Request
  let backend_sent_us = received_at.elapsed().as_micros() as u64;
  match builder.send().await {
    Ok(res) => {
      let backend_first_byte_us = received_at.elapsed().as_micros() as u64;
      let status = res.status().as_u16();

      // Sized from the header count, for the same reason as the body buffer
      // below: a dozen pushes into an empty Vec is a handful of reallocations
      // per response, each copying what it had.
      let mut res_headers: Vec<(String, String)> = Vec::with_capacity(res.headers().len());
      for (k, v) in res.headers().iter() {
        if let Ok(v_str) = v.to_str() {
          res_headers.push((k.to_string(), v_str.to_string()));
        }
      }
      let res_headers = ctx.response_headers.apply(res_headers);

      // Read the body incrementally. Bodies up to the stream threshold are
      // buffered and returned as a single Response message; larger bodies
      // switch to chunked streaming so memory usage stays bounded.
      let threshold = if binary_chunks {
        BINARY_STREAM_THRESHOLD
      } else {
        STREAM_THRESHOLD
      }
      .min(ctx.max_response_body_size);
      // Sized up front from the backend's own Content-Length, capped at the
      // point where this switches to streaming anyway. Growing into an empty
      // Vec was the single heaviest thing in a profile of the client under
      // load: a 32 KB body arriving in chunks reallocated its way there,
      // copying what it had accumulated each time.
      let reserve = res
        .content_length()
        .map(|len| len.min(threshold as u64 + 1) as usize)
        .unwrap_or(0);
      let mut stream = res.bytes_stream();
      let mut buf: Vec<u8> = Vec::with_capacity(reserve);
      let mut pause_guard: Option<crate::flow::PauseGuard> = None;
      let mut coalescer = ChunkCoalescer::new();
      let mut aborted = false;
      let mut total: usize = 0;

      loop {
        // While bytes wait in the coalescer, poll rather than wait: a backend
        // with more data ready keeps filling the frame, and one gone quiet
        // gets its bytes flushed now instead of held to the next read.
        let item = if coalescer.is_empty() {
          stream.next().await
        } else {
          match stream.next().now_or_never() {
            Some(item) => item,
            None => {
              let guard = pause_guard
                .as_ref()
                .expect("bytes are only held while streaming");
              if let Some(part) = coalescer.take()
                && send_response_chunk(tunnel_tx, &id, &part, binary_chunks, guard.signal())
                  .await
                  .is_err()
              {
                return None;
              }
              stream.next().await
            }
          }
        };
        match item {
          Some(Ok(chunk)) => {
            total += chunk.len();
            match &pause_guard {
              None => {
                buf.extend_from_slice(&chunk);
                if buf.len() > threshold {
                  // Switch to streaming: send head + buffered data as chunks.
                  // Registering for pause first, so the server can throttle
                  // this stream from its very first chunk.
                  let guard = ctx.stream_pauses.register(&id);
                  let start = TunnelMessage::ResponseStart {
                    id: id.clone(),
                    status,
                    headers: res_headers.clone(),
                  };
                  if send_tunnel_msg(tunnel_tx, &start).await.is_err() {
                    return None;
                  }
                  for part in buf.chunks(STREAM_CHUNK_SIZE) {
                    if send_response_chunk(tunnel_tx, &id, part, binary_chunks, guard.signal())
                      .await
                      .is_err()
                    {
                      return None;
                    }
                  }
                  buf = Vec::new();
                  pause_guard = Some(guard);
                }
              }
              Some(guard) => {
                if total > ctx.max_response_body_size {
                  warn!(
                    "Streamed response for request ID {} exceeded limit ({} bytes); aborting",
                    id, ctx.max_response_body_size
                  );
                  aborted = true;
                  break;
                }
                coalescer.add(&chunk);
                while let Some(part) = coalescer.pop_full() {
                  if send_response_chunk(tunnel_tx, &id, &part, binary_chunks, guard.signal())
                    .await
                    .is_err()
                  {
                    return None;
                  }
                }
              }
            }
          }
          Some(Err(e)) => {
            if pause_guard.is_some() {
              error!(
                "Body stream error from backend for request ID {}: {:?}; aborting stream",
                id, e
              );
              aborted = true;
              break;
            }
            error!(
              "Failed to retrieve response body from target backend: {:?}",
              e
            );
            return Some(make_error_response(id, 502));
          }
          None => break,
        }
      }

      if let Some(guard) = &pause_guard {
        if aborted {
          // Abnormal end: the visitor must see an aborted response, not a
          // silently truncated success.
          let abort = TunnelMessage::ResponseAbort { id: id.clone() };
          let _ = send_tunnel_msg(tunnel_tx, &abort).await;
          warn!(
            "Tunnel request ABORTED (streamed): ID={} Status={} Bytes={}",
            id, status, total
          );
          return None;
        }
        // The body ended with a partial frame still held back: it goes out
        // before the End that closes the stream.
        if let Some(part) = coalescer.take()
          && send_response_chunk(tunnel_tx, &id, &part, binary_chunks, guard.signal())
            .await
            .is_err()
        {
          return None;
        }
        let end = TunnelMessage::ResponseEnd {
          id: id.clone(),
          trailers: None,
        };
        let _ = send_tunnel_msg(tunnel_tx, &end).await;
        info!(
          "Tunnel request SUCCESS (streamed): ID={} Status={} Bytes={}",
          id, status, total
        );
        return None;
      }

      let backend_done_us = received_at.elapsed().as_micros() as u64;

      info!("Tunnel request SUCCESS: ID={} Status={}", id, status);

      let timings = Some(crate::protocol::ClientTimings {
        backend_sent_us,
        backend_first_byte_us,
        backend_done_us,
        respond_us: received_at.elapsed().as_micros() as u64,
      });

      // A v5 peer takes the body as bytes in the same frame as the envelope.
      // Anything older gets it base64-encoded inside the JSON, which is what
      // every version before this did.
      if full_body_frames && !buf.is_empty() {
        let envelope = TunnelMessage::Response {
          id: id.clone(),
          status,
          headers: res_headers,
          body: None,
          trailers: None,
          timings,
        };
        if send_full_response(tunnel_tx, &id, &envelope, &buf).await {
          return None;
        }
        // The send failed, which means the connection is going away. Falling
        // through would encode the body a second time for a socket that is
        // not there.
        return None;
      }

      Some(TunnelMessage::Response {
        id,
        status,
        headers: res_headers,
        body: (!buf.is_empty()).then(|| BASE64_STANDARD.encode(&buf)),
        trailers: None,
        timings,
      })
    }
    Err(e) => {
      warn!("Tunnel request FAILURE: ID={} Error={:?}", id, e);
      Some(make_error_response(id, 502))
    }
  }
}

/// Formats a generic masked error response, avoiding leaking raw socket error details.
pub(crate) fn make_error_response(id: String, status: u16) -> TunnelMessage {
  let headers = vec![("content-type".to_string(), "text/plain".to_string())];

  let user_error = match status {
    502 => "502 Bad Gateway - Target server connection failed.",
    400 => "400 Bad Request - Invalid request payload data.",
    _ => "500 Internal Server Error - Tunnel client failed to process request.",
  };

  let body = BASE64_STANDARD.encode(user_error.as_bytes());

  TunnelMessage::Response {
    id,
    status,
    headers,
    body: Some(body),
    trailers: None,
    timings: None,
  }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
