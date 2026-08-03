//! HTTP forwarding to Unix domain socket targets (`unix:///var/run/app.sock`).
//!
//! Built directly on hyper: reqwest cannot dial Unix sockets. Each request
//! opens a fresh connection to the socket and speaks HTTP/1.1 over it,
//! matching how local backends behind a socket (gunicorn, php-fpm-style
//! bridges, systemd socket activation) expect to be driven. Unix targets are
//! HTTP-only: WebSocket upgrades are answered with 502.

use base64::prelude::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::protocol::TunnelMessage;
use crate::protocol::send_tunnel_msg;
use crate::proxy::http::{
  ChunkCoalescer, ForwardContext, ForwardRequest, STREAM_CHUNK_SIZE, STREAM_THRESHOLD,
  make_error_response, send_response_chunk,
};
use futures_util::FutureExt;

/// Request body type sent to the socket backend.
type UnixBody = BoxBody<Bytes, std::io::Error>;

/// True when a normalized target uses the Unix socket scheme.
pub(crate) fn is_unix_target(target: &str) -> bool {
  target.starts_with("unix://")
}

/// Extracts the filesystem path of a `unix://` target.
pub(crate) fn unix_socket_path(target: &str) -> Option<String> {
  let path = target.strip_prefix("unix://")?;
  // `unix:///var/run/app.sock` → `/var/run/app.sock`; a relative form like
  // `unix://./app.sock` is kept as written.
  let path = path.trim();
  if path.is_empty() {
    None
  } else {
    Some(path.to_string())
  }
}

/// Maps the incoming request path onto the backend origin-form URI,
/// honouring `trim_bind` exactly like the URL-based targets.
fn build_origin_uri(ctx: &ForwardContext, uri_str: &str) -> String {
  let (path, query) = match uri_str.split_once('?') {
    Some((p, q)) => (p, Some(q)),
    None => (uri_str, None),
  };
  let mut path = path.trim_start_matches('/').to_string();
  if ctx.trim_bind
    && let Some(ref bind) = ctx.path_bind
  {
    let bind_trimmed = bind.trim_matches('/');
    // Match only at a path-segment boundary (see http.rs): bind `/api` must not
    // match `/apiv2/x`.
    let matches_bind = match path.strip_prefix(bind_trimmed) {
      Some(rest) => rest.is_empty() || rest.starts_with('/'),
      None => false,
    };
    if matches_bind {
      path = path[bind_trimmed.len()..]
        .trim_start_matches('/')
        .to_string();
    }
  }
  match query {
    Some(q) => format!("/{path}?{q}"),
    None => format!("/{path}"),
  }
}

/// Unix-socket counterpart of `handle_incoming_request`: dials the socket,
/// forwards the request over HTTP/1.1, and relays the response (buffered
/// under the stream threshold, chunk-streamed beyond it).
pub(crate) async fn handle_incoming_request_unix(
  ctx: &ForwardContext,
  req: ForwardRequest,
  streamed_body: Option<mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>>,
  binary_chunks: bool,
) -> Option<TunnelMessage> {
  let ForwardRequest {
    id,
    method: method_str,
    uri: uri_str,
    headers,
    body: body_base64,
    raw_body,
  } = req;
  let tunnel_tx = &ctx.tunnel_tx;
  let Some(socket_path) = ctx.unix_socket.as_deref() else {
    error!("Unix socket path invoked without a socket path (bug)");
    return Some(make_error_response(id, 500));
  };
  info!(
    "Forwarding tunnel request ID {} to unix socket {}: {} {}",
    id, socket_path, method_str, uri_str
  );

  let method = match hyper::Method::from_bytes(method_str.as_bytes()) {
    Ok(m) => m,
    Err(e) => {
      error!("Invalid HTTP method representation: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  let mut builder = hyper::Request::builder()
    .method(method)
    .uri(build_origin_uri(ctx, &uri_str));
  let headers = ctx.request_headers.apply(headers);
  let mut host_header_val = None;
  for (k, v) in headers.iter() {
    let k_lower = k.to_lowercase();
    // Strip the hop-by-hop framing headers transfer-encoding / trailer so a
    // visitor-supplied `transfer-encoding: chunked` cannot collide with hyper's
    // own http1 framing, the same request-smuggling guard as http.rs and
    // h2.rs. content-length is kept so content-length-only backends still get a
    // framed body (dropping it would force chunked on streamed uploads).
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
      host_header_val = Some(v.clone());
      continue;
    }
    if let (Ok(name), Ok(val)) = (
      hyper::header::HeaderName::from_bytes(k.as_bytes()),
      hyper::header::HeaderValue::from_str(v),
    ) {
      builder = builder.header(name, val);
    }
  }
  // HTTP/1.1 needs a Host header; sockets have no authority, so `localhost`
  // stands in unless the visitor's Host is passed through.
  let host = if ctx.pass_hostname {
    host_header_val.unwrap_or_else(|| "localhost".to_string())
  } else {
    "localhost".to_string()
  };
  if let Ok(val) = hyper::header::HeaderValue::from_str(&host) {
    builder = builder.header(hyper::header::HOST, val);
  }

  // A streamed body is consumed by its first attempt; a buffered one is kept
  // so the request can be built again, the same fence the HTTP/1 path uses.
  let mut streamed = streamed_body;
  let replayable_body: Option<Bytes> = if streamed.is_some() {
    None
  } else if let Some(bytes) = raw_body {
    // v6: the body arrived as bytes in the dispatch frame, nothing to decode.
    Some(Bytes::from(bytes))
  } else if let Some(encoded_body) = body_base64 {
    match BASE64_STANDARD.decode(encoded_body) {
      Ok(bytes) => Some(Bytes::from(bytes)),
      Err(e) => {
        error!("Base64 decoding failed for request body payload: {:?}", e);
        return Some(make_error_response(id, 400));
      }
    }
  } else {
    Some(Bytes::new())
  };

  let head = match builder.body(()) {
    Ok(r) => r.into_parts().0,
    Err(e) => {
      error!("Failed to build unix-socket backend request: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  // The breaker and the attempts, both of which this path used to skip: the
  // dispatch to it happens before either, so a service on a unix socket had
  // `retry:` and `circuit_breaker:` in its config doing nothing.
  if let crate::proxy::http::BreakerVerdict::Open(remaining) = ctx.resilience.check() {
    warn!(
      "Circuit breaker open for {}: request ID {} refused without dialing ({}s left)",
      ctx.target,
      id,
      remaining.as_secs()
    );
    return Some(make_error_response(id, 502));
  }
  // Dial + handshake + response head, all under the backend timeout; the
  // body may stream for as long as the response lives.
  let timeout = std::time::Duration::from_secs(ctx.timeout_secs.max(1));
  let method_retryable = ctx.resilience.may_retry_method(head.method.as_str());
  let mut attempt = 1u32;
  let mut backoff = ctx.resilience.backoff;
  let res = loop {
    let body: UnixBody = match (&replayable_body, streamed.take()) {
      (_, Some(rx)) => {
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
          rx.recv().await.map(|item| (item.map(Frame::data), rx))
        });
        BoxBody::new(StreamBody::new(stream))
      }
      (Some(bytes), None) => BoxBody::new(Full::new(bytes.clone()).map_err(|never| match never {})),
      // The stream was spent by the previous attempt, which is why it is not
      // retried; reaching here would send an empty body in its place.
      (None, None) => break Err(None),
    };
    let request = hyper::Request::from_parts(head.clone(), body);
    let outcome = tokio::time::timeout(timeout, dial_and_send(socket_path, request)).await;
    let e = match outcome {
      Ok(Ok(res)) => break Ok(res),
      Ok(Err(e)) => e,
      Err(_) => {
        warn!("Tunnel request TIMEOUT (unix): ID={}", id);
        ctx.resilience.record_failure();
        return Some(make_error_response(id, 504));
      }
    };
    // A fresh socket per request here, so there is no pooled connection to
    // find closed: only the configured policy retries.
    if method_retryable && replayable_body.is_some() && attempt < ctx.resilience.attempts {
      warn!(
        "Backend attempt {}/{} failed for request ID {} (unix): {}; retrying in {}ms",
        attempt,
        ctx.resilience.attempts,
        id,
        e,
        backoff.as_millis()
      );
      tokio::time::sleep(backoff).await;
      backoff = backoff.saturating_mul(2);
      attempt += 1;
      continue;
    }
    break Err(Some(e));
  };
  let res = match res {
    Ok(res) => res,
    Err(e) => {
      if let Some(e) = e {
        warn!("Tunnel request FAILURE (unix): ID={} Error={}", id, e);
      }
      ctx.resilience.record_failure();
      return Some(make_error_response(id, 502));
    }
  };
  ctx.resilience.record_success();

  let status = res.status().as_u16();
  let mut res_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in res.headers().iter() {
    if let Ok(v_str) = v.to_str() {
      res_headers.push((k.to_string(), v_str.to_string()));
    }
  }
  let res_headers = ctx.response_headers.apply(res_headers);

  // Mirror the HTTP/1 path: buffer up to the stream threshold, switch to
  // chunked streaming beyond it.
  let threshold = STREAM_THRESHOLD.min(ctx.max_response_body_size);
  let mut body = res.into_body();
  let mut buf: Vec<u8> = Vec::new();
  let mut streaming = false;
  let mut pause_guard: Option<crate::flow::PauseGuard> = None;
  let mut coalescer = ChunkCoalescer::new();
  let mut aborted = false;
  let mut total: usize = 0;

  loop {
    // Bound each body read: the dial/head `timeout` above does not cover the
    // body, so a backend that stalls mid-body would hang this task forever and
    // leak the server's in-flight request slot.
    //
    // While bytes wait in the coalescer, poll rather than wait: a backend
    // gone quiet gets its bytes flushed now instead of held to the next read.
    let polled = if coalescer.is_empty() {
      tokio::time::timeout(timeout, body.frame()).await
    } else {
      match tokio::time::timeout(timeout, body.frame()).now_or_never() {
        Some(r) => r,
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
          tokio::time::timeout(timeout, body.frame()).await
        }
      }
    };
    let frame_res = match polled {
      Ok(Some(fr)) => fr,
      Ok(None) => break,
      Err(_) => {
        warn!(
          "Unix-socket body read timeout for request ID {}; aborting stream",
          id
        );
        if streaming {
          aborted = true;
          break;
        } else {
          return Some(make_error_response(id, 504));
        }
      }
    };
    let frame = match frame_res {
      Ok(f) => f,
      Err(e) => {
        if streaming {
          error!(
            "Unix-socket body error from backend for request ID {}: {:?}; aborting stream",
            id, e
          );
          aborted = true;
          break;
        }
        error!("Failed to retrieve unix-socket response body: {:?}", e);
        return Some(make_error_response(id, 502));
      }
    };
    let Some(chunk) = frame.into_data().ok() else {
      continue;
    };
    total += chunk.len();
    // Before the chunk is used for anything: the check used to sit in the
    // streaming arm only, so a single oversized chunk became the head of a
    // stream and went out unmeasured.
    if total > ctx.max_response_body_size {
      if streaming {
        warn!(
          "Streamed unix-socket response for request ID {} exceeded limit ({} bytes); aborting",
          id, ctx.max_response_body_size
        );
        aborted = true;
        break;
      }
      warn!(
        "Unix-socket response for request ID {} exceeded max_response_body ({} bytes) before any of it was sent; refusing",
        id, ctx.max_response_body_size
      );
      return Some(make_error_response(id, 502));
    }
    if !streaming {
      buf.extend_from_slice(&chunk);
      if buf.len() > threshold {
        // Register for server flow control before the first chunk goes out.
        let guard = ctx.stream_pauses.register(&id);
        let start = TunnelMessage::ResponseStart {
          id: id.clone(),
          status,
          headers: res_headers.clone(),
          timings: None,
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
        streaming = true;
      }
    } else {
      let pause = pause_guard
        .as_ref()
        .expect("streaming implies a pause guard");
      coalescer.add(&chunk);
      while let Some(part) = coalescer.pop_full() {
        if send_response_chunk(tunnel_tx, &id, &part, binary_chunks, pause.signal())
          .await
          .is_err()
        {
          return None;
        }
      }
    }
  }

  if streaming {
    if aborted {
      let abort = TunnelMessage::ResponseAbort { id: id.clone() };
      let _ = send_tunnel_msg(tunnel_tx, &abort).await;
      warn!(
        "Tunnel request ABORTED (unix, streamed): ID={} Status={} Bytes={}",
        id, status, total
      );
      return None;
    }
    // The body ended with a partial frame still held back: it goes out
    // before the End that closes the stream.
    if let Some(part) = coalescer.take() {
      let guard = pause_guard
        .as_ref()
        .expect("streaming implies a pause guard");
      if send_response_chunk(tunnel_tx, &id, &part, binary_chunks, guard.signal())
        .await
        .is_err()
      {
        return None;
      }
    }
    let end = TunnelMessage::ResponseEnd {
      id: id.clone(),
      trailers: None,
    };
    let _ = send_tunnel_msg(tunnel_tx, &end).await;
    info!(
      "Tunnel request SUCCESS (unix, streamed): ID={} Status={} Bytes={}",
      id, status, total
    );
    return None;
  }

  let body_encoded = if buf.is_empty() {
    None
  } else {
    Some(BASE64_STANDARD.encode(&buf))
  };
  info!("Tunnel request SUCCESS (unix): ID={} Status={}", id, status);
  Some(TunnelMessage::Response {
    id,
    status,
    headers: res_headers,
    body: body_encoded,
    trailers: None,
    timings: None,
  })
}

/// Connects to the socket, performs the HTTP/1.1 handshake, and sends the
/// request. Unix-only; on other platforms unix targets are rejected at
/// startup, so this is never reached.
#[cfg(unix)]
async fn dial_and_send(
  socket_path: &str,
  request: hyper::Request<UnixBody>,
) -> Result<hyper::Response<hyper::body::Incoming>, String> {
  let stream = tokio::net::UnixStream::connect(socket_path)
    .await
    .map_err(|e| format!("cannot connect to {socket_path}: {e}"))?;
  let io = hyper_util::rt::TokioIo::new(stream);
  let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
    .await
    .map_err(|e| format!("HTTP handshake on {socket_path} failed: {e}"))?;
  // The connection task drives the socket until the response (and body) is
  // done; it ends by itself when either side closes.
  tokio::spawn(async move {
    let _ = conn.await;
  });
  sender
    .send_request(request)
    .await
    .map_err(|e| format!("request on {socket_path} failed: {e}"))
}

#[cfg(not(unix))]
async fn dial_and_send(
  socket_path: &str,
  _request: hyper::Request<UnixBody>,
) -> Result<hyper::Response<hyper::body::Incoming>, String> {
  Err(format!(
    "unix socket targets ({socket_path}) are not supported on this platform"
  ))
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
