//! HTTP request forwarding: proxies tunnel requests to the local target,
//! streaming large response bodies back through the tunnel in chunks.

use base64::prelude::*;
use futures_util::FutureExt;
use futures_util::stream::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, error, info, warn};

// Split by what each part is for, leaving `handle_incoming_request` here
// because it *is* the request path: it holds the pause guard, the breaker
// permit and the streaming state that every one of its exits has to release.
pub(crate) mod client;
pub(crate) mod context;
pub(crate) mod resilience;
pub(crate) mod stream;

pub(crate) use client::*;
pub(crate) use context::*;
pub(crate) use resilience::*;
pub(crate) use stream::*;

use crate::protocol::{FRAME_RESPONSE_CHUNK, TunnelMessage, encode_binary_frame, send_tunnel_msg};

/// Headers this path never hands to the backend.
///
/// The shared half is `aperio_config::hop_by_hop::HOP_BY_HOP_CORE`, which
/// every path to a backend strips, and a test holds this to it. `trailer` is
/// this path's own addition: on HTTP/1 it is a hop-by-hop framing header a
/// visitor can write, which the h2 path does not need because HTTP/2 carries
/// trailers as a protocol concept instead.
///
/// `host` is deliberately absent. It is taken out of the header loop and put
/// back exactly once, only when `pass_hostname` is set, because adding it in
/// both places produced a duplicate Host header.
/// Lowercases its own input rather than trusting the caller to have done it.
/// The server's twin does the same, so all three paths answer the same
/// question whatever spelling reaches them.
pub(crate) fn is_hop_by_hop(name: &str) -> bool {
  let n = name.to_ascii_lowercase();
  aperio_config::hop_by_hop::HOP_BY_HOP_CORE.contains(&n.as_str())
    || n == "trailer"
    || n.starts_with(aperio_config::hop_by_hop::WEBSOCKET_PREFIX)
}

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

/// Backend resilience for one service: how many attempts a failed request
/// gets, and when to stop dialing a backend that keeps failing
/// (planned_features #29).
///
/// The server can already fail a request over to another *client* and eject
/// one that misbehaves. Neither helps the single client whose own backend is
/// refusing connections: without this, one refused connect is the visitor's
/// answer, and a dead backend is dialed once per request for as long as the
/// traffic lasts.
#[derive(Clone)]
pub(crate) struct BackendResilience {
  /// Total attempts including the first; 1 disables retrying.
  pub(crate) attempts: u32,
  /// Delay before the second attempt, doubled before each further one.
  pub(crate) backoff: std::time::Duration,
  /// Retry non-idempotent methods too. Off by default: a retried POST may
  /// reach the backend twice, the same trade the server's
  /// `failover.all_methods` names.
  pub(crate) all_methods: bool,
  /// Consecutive failures that open the breaker; 0 disables it.
  pub(crate) breaker_failures: u32,
  /// How long the breaker stays open before one request is let through.
  pub(crate) breaker_open_for: std::time::Duration,
  /// Shared state, because one `ForwardContext` serves every request of a
  /// service and the breaker is only useful if they see each other's failures.
  state: std::sync::Arc<std::sync::Mutex<BreakerState>>,
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

impl<E: std::fmt::Debug> std::fmt::Display for Failure<E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Failure::Timeout => write!(f, "timed out waiting for the response head"),
      Failure::Backend(e) => write!(f, "{e:?}"),
    }
  }
}

/// Forwards a proxied HTTP request from the websocket tunnel to the local
/// target server.
///
/// Sanitizes sensitive/upgrade headers, rewrites URLs, routes the HTTP request,
/// and returns the response mapped back into a `TunnelMessage`.
///
/// Small responses are returned as `Some(TunnelMessage::Response)` for the
/// caller to send. Large responses are streamed directly through the tunnel
/// (ResponseStart/Chunk/End) and `None` is returned, and so is a buffered
/// response sent as a v5 full-body frame, which this sends itself for the
/// same reason: the body does not fit through a `TunnelMessage`.
pub(crate) async fn handle_incoming_request(
  ctx: &ForwardContext,
  req: ForwardRequest,
  streamed_body: Option<mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>>,
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
    if is_hop_by_hop(&k_lower) {
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

  // The breaker first: while it is open the backend is not dialed at all,
  // which is the whole point, and the visitor gets its 502 immediately
  // instead of waiting out a connect that is not going to succeed.
  if let BreakerVerdict::Open(remaining) = ctx.resilience.check() {
    warn!(
      "Circuit breaker open for {}: request ID {} refused without dialing ({}s left)",
      ctx.target,
      id,
      remaining.as_secs()
    );
    return Some(make_error_response(id, 502));
  }

  // Execute Request, retrying a failure that happened before any response.
  //
  // Two fences on what may be retried. The method must be idempotent (unless
  // the operator opted in), and the request must be replayable at all:
  // `try_clone` returns None for a streamed body, which the first attempt has
  // already consumed. Once a response head arrives we are past this point, so
  // an error later in the body is never retried here.
  let backend_sent_us = received_at.elapsed().as_micros() as u64;
  let method_retryable = ctx.resilience.may_retry_method(&method_str);
  let retries_allowed = ctx.resilience.attempts > 1 && method_retryable;
  let mut attempt = 1u32;
  let mut backoff = ctx.resilience.backoff;
  // One immediate re-dial for a connection the backend had already closed,
  // available even with `retry.attempts: 1`. That is not the retry policy the
  // operator turned off: the policy is about a backend that failed, and this
  // is about the client's own connection pool racing the backend's idle
  // timeout. It costs one extra dial, only for a request the same fences
  // already allow to be replayed, and only when no response head arrived.
  let mut redialed_stale = false;
  let outcome = loop {
    let policy_retry = retries_allowed && attempt < ctx.resilience.attempts;
    let stale_retry_available = !redialed_stale && method_retryable;
    let replay = if policy_retry || stale_retry_available {
      builder.try_clone()
    } else {
      None
    };
    let result = builder.send().await;
    let Err(ref e) = result else {
      break result;
    };
    let Some(next) = replay else {
      break result;
    };
    if policy_retry {
      warn!(
        "Backend attempt {}/{} failed for request ID {}: {}; retrying in {}ms",
        attempt,
        ctx.resilience.attempts,
        id,
        e,
        backoff.as_millis()
      );
      tokio::time::sleep(backoff).await;
      backoff = backoff.saturating_mul(2);
      attempt += 1;
    } else {
      if !is_stale_connection_error(e) {
        break result;
      }
      // No backoff and no warning: this is the pool cleaning up after itself,
      // not an incident. A second one on the same request is a backend that
      // really is closing on us, and that reaches the visitor.
      redialed_stale = true;
      debug!(
        "Backend closed a pooled connection for request ID {} ({}); dialing again once",
        id, e
      );
    }
    builder = next;
  };

  match outcome {
    Ok(res) => {
      // A response head arrived, so the backend is reachable and the breaker
      // resets. Its status is deliberately not consulted: a 500 is a backend
      // that is up and answering, and refusing to dial it would turn an
      // application error into an outage.
      ctx.resilience.record_success();
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
            // Before the chunk is used for anything. The check used to live
            // in the streaming arm only, so a single chunk that on its own
            // passed the limit was buffered, turned into the head of a
            // stream and sent: the limit was enforced from the *second*
            // chunk onwards, and a one-chunk body escaped it entirely.
            if total > ctx.max_response_body_size {
              if pause_guard.is_some() {
                warn!(
                  "Streamed response for request ID {} exceeded limit ({} bytes); aborting",
                  id, ctx.max_response_body_size
                );
                aborted = true;
                break;
              }
              // Nothing has left yet, so this can still be a clean failure
              // rather than a truncated success.
              warn!(
                "Response for request ID {} exceeded max_response_body ({} bytes) before any of it was sent; refusing",
                id, ctx.max_response_body_size
              );
              return Some(make_error_response(id, 502));
            }
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
                    // The head of a stream is the same milestone a buffered
                    // response reports at, reached with the same two backend
                    // stages behind it. Without this a streamed response told
                    // the server nothing at all about where its time went,
                    // which is precisely backwards: the big responses are the
                    // ones worth profiling.
                    timings: Some(crate::protocol::ClientTimings {
                      backend_sent_us,
                      backend_first_byte_us,
                      backend_done_us: None,
                      respond_us: received_at.elapsed().as_micros() as u64,
                    }),
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
        backend_done_us: Some(backend_done_us),
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
      if ctx.resilience.record_failure() {
        warn!(
          "Circuit breaker opened for {}: {} consecutive backend failures; not dialing again for {}s",
          ctx.target,
          ctx.resilience.breaker_failures,
          ctx.resilience.breaker_open_for.as_secs()
        );
      }
      Some(make_error_response(id, 502))
    }
  }
}

#[cfg(test)]
#[path = "http_tests.rs"]
pub(crate) mod http_tests;
