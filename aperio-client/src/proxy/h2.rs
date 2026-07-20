//! HTTP/2 forwarding for `h2c://` (prior-knowledge cleartext) and `h2://`
//! (TLS + ALPN) targets — the path gRPC backends need. Built directly on
//! hyper because reqwest does not expose response trailers, and gRPC carries
//! its status (`grpc-status`) in the trailers.

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
  ForwardContext, ForwardRequest, STREAM_CHUNK_SIZE, STREAM_THRESHOLD, build_dest_url,
  make_error_response, send_response_chunk,
};

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
pub(crate) fn build_h2_client(target: &str) -> Option<H2Client> {
  if target.starts_with("h2c://") {
    Some(H2Client::Cleartext(
      Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(HttpConnector::new()),
    ))
  } else if target.starts_with("h2://") {
    let https = hyper_rustls::HttpsConnectorBuilder::new()
      .with_webpki_roots()
      .https_only()
      .enable_http2()
      .build();
    Some(H2Client::Tls(
      Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(https),
    ))
  } else {
    None
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
  streamed_body: Option<mpsc::Receiver<Result<Vec<u8>, std::io::Error>>>,
  binary_chunks: bool,
) -> Option<TunnelMessage> {
  let ForwardRequest {
    id,
    method: method_str,
    uri: uri_str,
    headers,
    body: body_base64,
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
    // Connection-specific headers are forbidden in HTTP/2 — except
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

  let body: H2Body = if let Some(rx) = streamed_body {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
      rx.recv()
        .await
        .map(|item| (item.map(|bytes| Frame::data(Bytes::from(bytes))), rx))
    });
    BoxBody::new(StreamBody::new(stream))
  } else if let Some(encoded_body) = body_base64 {
    match BASE64_STANDARD.decode(encoded_body) {
      Ok(bytes) => BoxBody::new(Full::new(Bytes::from(bytes)).map_err(|never| match never {})),
      Err(e) => {
        error!("Base64 decoding failed for request body payload: {:?}", e);
        return Some(make_error_response(id, 400));
      }
    }
  } else {
    BoxBody::new(http_body_util::Empty::new().map_err(|never| match never {}))
  };

  let request = match builder.body(body) {
    Ok(r) => r,
    Err(e) => {
      error!("Failed to build HTTP/2 backend request: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  // The timeout covers reaching the backend and receiving the response head;
  // the body may stream for as long as the RPC lives.
  let res = match tokio::time::timeout(
    std::time::Duration::from_secs(ctx.timeout_secs.max(1)),
    h2_client.request(request),
  )
  .await
  {
    Ok(Ok(res)) => res,
    Ok(Err(e)) => {
      warn!("Tunnel request FAILURE (h2): ID={} Error={:?}", id, e);
      return Some(make_error_response(id, 502));
    }
    Err(_) => {
      warn!("Tunnel request TIMEOUT (h2): ID={}", id);
      return Some(make_error_response(id, 504));
    }
  };

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
  let mut total: usize = 0;
  let mut trailers: Option<Vec<(String, String)>> = None;

  loop {
    // Bound each body read: the head timeout above does not cover the body, so
    // a backend that sends the head then stalls mid-body would otherwise hang
    // this task forever and leak the server's in-flight request slot.
    let frame_res = match tokio::time::timeout(
      std::time::Duration::from_secs(ctx.timeout_secs.max(1)),
      body.frame(),
    )
    .await
    {
      Ok(Some(fr)) => fr,
      Ok(None) => break,
      Err(_) => {
        warn!(
          "HTTP/2 body read timeout for request ID {}; ending stream",
          id
        );
        if streaming {
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
            "HTTP/2 body error from backend for request ID {}: {:?}; ending stream",
            id, e
          );
          break;
        }
        error!("Failed to retrieve HTTP/2 response body: {:?}", e);
        return Some(make_error_response(id, 502));
      }
    };
    if frame.is_data() {
      let chunk = frame.into_data().unwrap_or_default();
      total += chunk.len();
      if !streaming {
        buf.extend_from_slice(&chunk);
        if buf.len() > threshold {
          let start = TunnelMessage::ResponseStart {
            id: id.clone(),
            status,
            headers: res_headers.clone(),
          };
          if send_tunnel_msg(tunnel_tx, &start).await.is_err() {
            return None;
          }
          for part in buf.chunks(STREAM_CHUNK_SIZE) {
            if send_response_chunk(tunnel_tx, &id, part, binary_chunks)
              .await
              .is_err()
            {
              return None;
            }
          }
          buf = Vec::new();
          streaming = true;
        }
      } else {
        if total > ctx.max_response_body_size {
          warn!(
            "Streamed HTTP/2 response for request ID {} exceeded limit ({} bytes); truncating",
            id, ctx.max_response_body_size
          );
          break;
        }
        if send_response_chunk(tunnel_tx, &id, &chunk, binary_chunks)
          .await
          .is_err()
        {
          return None;
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
