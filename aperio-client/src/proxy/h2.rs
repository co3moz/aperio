//! HTTP/2 forwarding for `h2c://` (prior-knowledge cleartext) and `h2://`
//! (TLS + ALPN) targets, the path gRPC backends need. Built directly on
//! hyper because reqwest does not expose response trailers, and gRPC carries
//! its status (`grpc-status`) in the trailers.

use crate::proxy::http::Failure;
use base64::prelude::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::protocol::TunnelMessage;
use crate::protocol::send_tunnel_msg;
use crate::proxy::http::{
  ChunkCoalescer, ForwardContext, ForwardRequest, STREAM_CHUNK_SIZE, STREAM_THRESHOLD,
  build_dest_url, make_error_response, send_response_chunk,
};
use futures_util::FutureExt;

/// Request body type sent to HTTP/2 backends.
type H2Body = BoxBody<Bytes, std::io::Error>;

/// HTTP/2 client for one service's backend: cleartext prior-knowledge for
/// `h2c://`, TLS with ALPN restricted to h2 for `h2://`.
pub(crate) enum H2Client {
  Cleartext(Client<HttpConnector, H2Body>),
  Tls(Client<hyper_rustls::HttpsConnector<HttpConnector>, H2Body>),
}

/// True when a normalized target URL uses one of the HTTP/2 schemes.
pub(crate) fn is_h2_target(target: &str) -> bool {
  target.starts_with("h2c://") || target.starts_with("h2://")
}

/// Builds the HTTP/2 client matching the target's scheme; None for plain
/// HTTP targets.
///
/// `min_tls_version` is honored here for the same reason it is on the HTTP/1
/// path. It used to be read only where the reqwest client is built, which
/// this target never reaches, so a config that asked for a TLS 1.3 floor got
/// it for every backend except the `h2://` ones: a setting that was refused
/// nowhere and applied nowhere, which is the worst state a security setting
/// can be in. The value is validated on the config path, so an unusable one
/// never reaches this.
pub(crate) fn build_h2_client(target: &str, min_tls_version: Option<&str>) -> Option<H2Client> {
  if target.starts_with("h2c://") {
    Some(H2Client::Cleartext(
      Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(HttpConnector::new()),
    ))
  } else if target.starts_with("h2://") {
    let builder = hyper_rustls::HttpsConnectorBuilder::new();
    let https = match tls_versions(min_tls_version) {
      Some(versions) => {
        let roots = rustls::RootCertStore {
          roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
          .with_root_certificates(roots)
          .with_no_client_auth();
        builder
          .with_tls_config(config)
          .https_only()
          .enable_http2()
          .build()
      }
      None => builder
        .with_webpki_roots()
        .https_only()
        .enable_http2()
        .build(),
    };
    Some(H2Client::Tls(
      Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(https),
    ))
  } else {
    None
  }
}

/// The protocol versions a floor allows, or `None` for "no floor asked for",
/// which leaves rustls' own default set.
fn tls_versions(
  min_tls_version: Option<&str>,
) -> Option<&'static [&'static rustls::SupportedProtocolVersion]> {
  const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
  const TLS12_UP: &[&rustls::SupportedProtocolVersion] =
    &[&rustls::version::TLS12, &rustls::version::TLS13];
  match crate::proxy::http::tls_floor(min_tls_version) {
    Ok(Some(reqwest::tls::Version::TLS_1_3)) => Some(TLS13_ONLY),
    Ok(Some(_)) => Some(TLS12_UP),
    // Unset, or a value the config path already refused.
    _ => None,
  }
}

impl H2Client {
  fn request(&self, req: hyper::Request<H2Body>) -> hyper_util::client::legacy::ResponseFuture {
    match self {
      H2Client::Cleartext(c) => c.request(req),
      H2Client::Tls(c) => c.request(req),
    }
  }
}

/// HTTP/2 counterpart of `handle_incoming_request`: forwards the request to
/// the backend over HTTP/2 and relays the response including its trailers
/// (`grpc-status` & friends). Small trailer-less responses are returned as a
/// buffered `Response`; everything else streams through the tunnel.
pub(crate) async fn handle_incoming_request_h2(
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
  info!(
    "Forwarding tunnel request ID {} over HTTP/2: {} {}",
    id, method_str, uri_str
  );
  let Some(h2_client) = ctx.h2_client.as_deref() else {
    error!("HTTP/2 path invoked without an HTTP/2 client (bug)");
    return Some(make_error_response(id, 500));
  };

  let dest_url = match build_dest_url(ctx, &id, &uri_str) {
    Ok(url) => url,
    Err(status) => return Some(make_error_response(id, status)),
  };
  // The h2c/h2 schemes are aperio config vocabulary; on the wire the dial is
  // plain TCP or TLS. (Reparse instead of set_scheme: the url crate refuses
  // to switch a non-special scheme to a special one in place.)
  let wire_url = if let Some(rest) = dest_url.as_str().strip_prefix("h2c://") {
    format!("http://{rest}")
  } else if let Some(rest) = dest_url.as_str().strip_prefix("h2://") {
    format!("https://{rest}")
  } else {
    dest_url.as_str().to_string()
  };
  let dest_url = match url::Url::parse(&wire_url) {
    Ok(u) => u,
    Err(e) => {
      error!("Failed to build wire URL for HTTP/2 target: {:?}", e);
      return Some(make_error_response(id, 502));
    }
  };

  let method = match hyper::Method::from_bytes(method_str.as_bytes()) {
    Ok(m) => m,
    Err(e) => {
      error!("Invalid HTTP method representation: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  let mut builder = hyper::Request::builder()
    .method(method)
    .uri(dest_url.as_str());
  let headers = ctx.request_headers.apply(headers);
  for (k, v) in headers.iter() {
    let k_lower = k.to_lowercase();
    // Connection-specific headers are forbidden in HTTP/2, except
    // `te: trailers`, which gRPC requires end-to-end.
    if k_lower == "connection"
      || k_lower == "keep-alive"
      || k_lower == "upgrade"
      || k_lower == "proxy-connection"
      || k_lower == "transfer-encoding"
      || k_lower == "accept-encoding"
      || k_lower == "host"
      || k_lower.starts_with("sec-websocket-")
    {
      continue;
    }
    if k_lower == "te" && !v.to_ascii_lowercase().contains("trailers") {
      continue;
    }
    if let (Ok(name), Ok(val)) = (
      hyper::header::HeaderName::from_bytes(k.as_bytes()),
      hyper::header::HeaderValue::from_str(v),
    ) {
      builder = builder.header(name, val);
    }
  }

  // A streamed body is consumed by its first attempt; a buffered one is kept
  // so the request can be built again. That is the same fence the HTTP/1 path
  // applies, and it is what makes retrying safe rather than a second delivery
  // of a body nobody has.
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

  // The head, kept apart from the body so an attempt can be rebuilt.
  let head = match builder.body(()) {
    Ok(r) => r.into_parts().0,
    Err(e) => {
      error!("Failed to build HTTP/2 backend request: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  // The breaker, then the attempts. Both used to be skipped entirely on this
  // path: `handle_incoming_request` dispatches to h2 before it reaches them,
  // so a service whose backend spoke HTTP/2 had `retry:` and
  // `circuit_breaker:` in its config doing nothing at all.
  if let crate::proxy::http::BreakerVerdict::Open(remaining) = ctx.resilience.check() {
    warn!(
      "Circuit breaker open for {}: request ID {} refused without dialing ({}s left)",
      ctx.target,
      id,
      remaining.as_secs()
    );
    return Some(make_error_response(id, 502));
  }
  let method_retryable = ctx.resilience.may_retry_method(head.method.as_str());
  let mut attempt = 1u32;
  let mut backoff = ctx.resilience.backoff;
  let mut redialed_stale = false;
  let res = loop {
    let body: H2Body = match (&replayable_body, streamed.take()) {
      (_, Some(rx)) => {
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
          rx.recv().await.map(|item| (item.map(Frame::data), rx))
        });
        BoxBody::new(StreamBody::new(stream))
      }
      (Some(bytes), None) => BoxBody::new(Full::new(bytes.clone()).map_err(|never| match never {})),
      (None, None) => {
        // The stream was spent by the previous attempt, which is why it is
        // not retried; reaching here would send an empty body in its place.
        break Err(None);
      }
    };
    let request = hyper::Request::from_parts(head.clone(), body);
    let outcome = tokio::time::timeout(
      std::time::Duration::from_secs(ctx.timeout_secs.max(1)),
      h2_client.request(request),
    )
    .await;
    // A timeout on the head is a failure before any response arrived, which
    // is exactly what `retry.attempts` is documented to cover, so it takes
    // the same path as a transport error rather than answering at once. A
    // stalled backend is the case an operator most expects a retry to cover,
    // and it was the one case that skipped the loop it was standing in.
    let failure = match outcome {
      Ok(Ok(res)) => break Ok(res),
      Ok(Err(e)) => Failure::Backend(e),
      Err(_) => Failure::Timeout,
    };
    let replayable = method_retryable && replayable_body.is_some();
    if replayable && attempt < ctx.resilience.attempts {
      warn!(
        "Backend attempt {}/{} failed for request ID {} (h2): {}; retrying in {}ms",
        attempt,
        ctx.resilience.attempts,
        id,
        failure,
        backoff.as_millis()
      );
      tokio::time::sleep(backoff).await;
      backoff = backoff.saturating_mul(2);
      attempt += 1;
      continue;
    }
    // The same silent re-dial the HTTP/1 path does for a connection the
    // backend had already closed: this client pools connections too. Never
    // for a timeout, which says the backend took the request and went quiet.
    if let Failure::Backend(e) = &failure
      && replayable
      && !redialed_stale
      && !e.is_connect()
      && crate::proxy::http::chain_says_connection_closed(e)
    {
      redialed_stale = true;
      continue;
    }
    break Err(Some(failure));
  };
  let res = match res {
    Ok(res) => res,
    Err(failure) => {
      ctx.resilience.record_failure();
      return Some(match failure {
        Some(Failure::Timeout) => {
          warn!("Tunnel request TIMEOUT (h2): ID={}", id);
          make_error_response(id, 504)
        }
        Some(Failure::Backend(e)) => {
          warn!("Tunnel request FAILURE (h2): ID={} Error={:?}", id, e);
          make_error_response(id, 502)
        }
        // The streamed body was spent by an earlier attempt.
        None => make_error_response(id, 502),
      });
    }
  };
  // A response head arrived, so the backend is reachable; its status is
  // deliberately not consulted, the same as on the HTTP/1 path.
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
  // chunked streaming beyond it. Trailers ride on ResponseEnd (streamed) or
  // Response (buffered).
  let threshold = STREAM_THRESHOLD.min(ctx.max_response_body_size);
  let mut body = res.into_body();
  let mut buf: Vec<u8> = Vec::new();
  let mut streaming = false;
  let mut pause_guard: Option<crate::flow::PauseGuard> = None;
  let mut coalescer = ChunkCoalescer::new();
  let mut aborted = false;
  let mut total: usize = 0;
  let mut trailers: Option<Vec<(String, String)>> = None;
  let read_budget = std::time::Duration::from_secs(ctx.timeout_secs.max(1));

  loop {
    // Bound each body read: the head timeout above does not cover the body, so
    // a backend that sends the head then stalls mid-body would otherwise hang
    // this task forever and leak the server's in-flight request slot.
    //
    // While bytes wait in the coalescer, poll rather than wait: a backend
    // gone quiet gets its bytes flushed now instead of held to the next read.
    let polled = if coalescer.is_empty() {
      tokio::time::timeout(read_budget, body.frame()).await
    } else {
      match tokio::time::timeout(read_budget, body.frame()).now_or_never() {
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
          tokio::time::timeout(read_budget, body.frame()).await
        }
      }
    };
    let frame_res = match polled {
      Ok(Some(fr)) => fr,
      Ok(None) => break,
      Err(_) => {
        warn!(
          "HTTP/2 body read timeout for request ID {}; aborting stream",
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
            "HTTP/2 body error from backend for request ID {}: {:?}; aborting stream",
            id, e
          );
          aborted = true;
          break;
        }
        error!("Failed to retrieve HTTP/2 response body: {:?}", e);
        return Some(make_error_response(id, 502));
      }
    };
    if frame.is_data() {
      let chunk = frame.into_data().unwrap_or_default();
      total += chunk.len();
      // Before the chunk is used for anything: the check used to sit in the
      // streaming arm only, so a single oversized chunk became the head of a
      // stream and went out unmeasured.
      if total > ctx.max_response_body_size {
        if streaming {
          warn!(
            "Streamed HTTP/2 response for request ID {} exceeded limit ({} bytes); aborting",
            id, ctx.max_response_body_size
          );
          aborted = true;
          break;
        }
        warn!(
          "HTTP/2 response for request ID {} exceeded max_response_body ({} bytes) before any of it was sent; refusing",
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
    } else if let Ok(map) = frame.into_trailers() {
      let list: Vec<(String, String)> = map
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
        .collect();
      if !list.is_empty() {
        trailers = Some(list);
      }
      break;
    }
  }

  if streaming {
    if aborted {
      let abort = TunnelMessage::ResponseAbort { id: id.clone() };
      let _ = send_tunnel_msg(tunnel_tx, &abort).await;
      warn!(
        "Tunnel request ABORTED (h2, streamed): ID={} Status={} Bytes={}",
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
      trailers,
    };
    let _ = send_tunnel_msg(tunnel_tx, &end).await;
    info!(
      "Tunnel request SUCCESS (h2, streamed): ID={} Status={} Bytes={}",
      id, status, total
    );
    return None;
  }

  let body_encoded = if buf.is_empty() {
    None
  } else {
    Some(BASE64_STANDARD.encode(&buf))
  };
  info!("Tunnel request SUCCESS (h2): ID={} Status={}", id, status);
  Some(TunnelMessage::Response {
    id,
    status,
    headers: res_headers,
    body: body_encoded,
    trailers,
    timings: None,
  })
}

#[cfg(test)]
#[path = "h2_tests.rs"]
mod tests;

// --- gRPC health checking (planned_features #35) ----------------------------

/// The standard health-checking method, from `grpc/health/v1/health.proto`.
const GRPC_HEALTH_METHOD: &str = "/grpc.health.v1.Health/Check";
/// `ServingStatus.SERVING`, the only status that means "route to me".
const GRPC_STATUS_SERVING: u64 = 1;

/// Encodes a `HealthCheckRequest{ service }` and wraps it in a gRPC length
/// prefix.
///
/// Both messages in this exchange are a single field, so they are written and
/// read by hand rather than by pulling in prost and a build step to generate
/// two structs. `service` is field 1, a length-delimited string (tag `0x0a`);
/// an empty name is the protocol's way of asking about the server as a whole,
/// and an empty message is how that is encoded.
fn health_request_frame(service: &str) -> Bytes {
  let mut msg: Vec<u8> = Vec::new();
  if !service.is_empty() {
    msg.push(0x0a);
    // A service name long enough to need a multi-byte varint is not a real
    // name; the length is written as one byte and longer names are refused
    // by the caller.
    msg.push(service.len() as u8);
    msg.extend_from_slice(service.as_bytes());
  }
  let mut framed = Vec::with_capacity(5 + msg.len());
  framed.push(0); // not compressed
  framed.extend_from_slice(&(msg.len() as u32).to_be_bytes());
  framed.extend_from_slice(&msg);
  Bytes::from(framed)
}

/// Reads `HealthCheckResponse.status` out of a gRPC response body.
///
/// The body is one length-prefixed message whose only field is `status`
/// (field 1, varint, tag `0x08`). Anything unreadable is reported as not
/// serving rather than guessed at: a health check that cannot be parsed has
/// not said the backend is healthy.
fn health_response_status(body: &[u8]) -> Option<u64> {
  // 5-byte gRPC frame header: one compression flag, four length bytes.
  let msg = body.get(5..)?;
  // `status` is the only field of the message, so it is the first one on the
  // wire: tag 0x08 (field 1, varint). Anything else is a shape this does not
  // understand, and guessing at it would be guessing about health.
  if *msg.first()? != 0x08 {
    return None;
  }
  let mut value: u64 = 0;
  let mut shift = 0u32;
  for byte in &msg[1..] {
    value |= u64::from(byte & 0x7f) << shift;
    if byte & 0x80 == 0 {
      return Some(value);
    }
    shift += 7;
    if shift > 63 {
      return None;
    }
  }
  // Ran off the end of the message mid-varint.
  None
}

/// Asks a gRPC backend's health service whether it is serving.
///
/// This is what a health probe against an `h2c://`/`h2://` target should do:
/// the plain GET the HTTP path uses cannot work against a server that speaks
/// HTTP/2 with prior knowledge and routes by gRPC method name, which is why
/// the documentation used to advise pointing the probe somewhere else
/// entirely.
///
/// Healthy means all three of: an HTTP 200, a `grpc-status` of 0 (in the
/// headers or the trailers, since a gRPC error may arrive either way), and a
/// `SERVING` status in the message. Anything else, including a timeout, is
/// unhealthy.
pub(crate) async fn grpc_health_check(
  client: &H2Client,
  target: &str,
  service: &str,
  timeout: std::time::Duration,
) -> bool {
  let wire = target
    .replacen("h2c://", "http://", 1)
    .replacen("h2://", "https://", 1);
  let uri = format!("{}{}", wire.trim_end_matches('/'), GRPC_HEALTH_METHOD);
  let body: H2Body = Full::new(health_request_frame(service))
    .map_err(|e| match e {})
    .boxed();
  let req = match hyper::Request::builder()
    .method(hyper::Method::POST)
    .uri(&uri)
    .header("content-type", "application/grpc")
    // Required by the gRPC HTTP/2 mapping, and what tells the server it may
    // answer with trailers.
    .header("te", "trailers")
    .body(body)
  {
    Ok(r) => r,
    Err(e) => {
      warn!("gRPC health check could not build a request for {uri}: {e}");
      return false;
    }
  };

  let Ok(Ok(resp)) = tokio::time::timeout(timeout, client.request(req)).await else {
    return false;
  };
  if resp.status() != hyper::StatusCode::OK {
    return false;
  }
  // A trailers-only error response carries grpc-status in the headers.
  let header_status = resp
    .headers()
    .get("grpc-status")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.parse::<i32>().ok());
  if header_status.is_some_and(|s| s != 0) {
    return false;
  }
  // Under the same budget as the head. A backend that answers with headers
  // and then never sends the body or the trailers used to hang this probe for
  // the life of the process, which left the backend marked with whatever
  // verdict it last had: a health check that cannot fail is not a health
  // check.
  let Ok(Ok(collected)) = tokio::time::timeout(timeout, resp.into_body().collect()).await else {
    return false;
  };
  let trailer_status = collected
    .trailers()
    .and_then(|t| t.get("grpc-status"))
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.parse::<i32>().ok());
  if trailer_status.is_some_and(|s| s != 0) {
    return false;
  }
  health_response_status(&collected.to_bytes()) == Some(GRPC_STATUS_SERVING)
}
