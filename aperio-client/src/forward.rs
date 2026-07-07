//! HTTP request forwarding: proxies tunnel requests to the local target,
//! streaming large response bodies back through the tunnel in chunks.

use base64::prelude::*;
use futures_util::stream::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{error, info, warn};

use crate::protocol::{FRAME_RESPONSE_CHUNK, TunnelMessage, encode_binary_frame, send_tunnel_msg};

/// Response bodies larger than this are streamed through the tunnel in
/// chunks instead of being buffered and sent as one message.
const STREAM_THRESHOLD: usize = 256 * 1024;
/// Size of individual streamed body chunks.
const STREAM_CHUNK_SIZE: usize = 128 * 1024;

/// Sends one streamed response chunk: a raw binary frame for v2 servers, or
/// the legacy base64+JSON message otherwise.
async fn send_response_chunk(
  tunnel_tx: &mpsc::Sender<Message>,
  id: &str,
  part: &[u8],
  binary: bool,
) -> Result<(), ()> {
  if binary {
    tunnel_tx
      .send(Message::Binary(encode_binary_frame(
        FRAME_RESPONSE_CHUNK,
        id,
        part,
      )))
      .await
      .map_err(|_| ())
  } else {
    let msg = TunnelMessage::ResponseChunk {
      id: id.to_string(),
      data: BASE64_STANDARD.encode(part),
    };
    send_tunnel_msg(tunnel_tx, &msg).await
  }
}

/// Per-connection constants for request forwarding, so per-request calls
/// only carry the request itself.
pub(crate) struct ForwardContext {
  /// HTTP client used for all backend calls on this connection.
  pub(crate) client: reqwest::Client,
  /// Local backend base URL.
  pub(crate) target: String,
  /// Forward the original `Host` header instead of the target's.
  pub(crate) pass_hostname: bool,
  /// Path bind of this client, stripped from incoming paths when `trim_bind`.
  pub(crate) path_bind: Option<String>,
  pub(crate) trim_bind: bool,
  /// Cap on response bodies read from the backend.
  pub(crate) max_response_body_size: usize,
  /// Write half of the tunnel, used for streamed responses.
  pub(crate) tunnel_tx: mpsc::Sender<Message>,
}

/// One proxied request as received from the tunnel.
pub(crate) struct ForwardRequest {
  pub(crate) id: String,
  pub(crate) method: String,
  pub(crate) uri: String,
  pub(crate) headers: Vec<(String, String)>,
  /// Base64-encoded buffered body (None when absent or streamed).
  pub(crate) body: Option<String>,
}

/// Forwards a proxied HTTP request from the websocket tunnel to the local target server.
/// Sanitizes sensitive/upgrade headers, rewrites URLs, routes the HTTP request, and returns
/// the response mapped back into a `TunnelMessage`.
///
/// Small responses are returned as `Some(TunnelMessage::Response)` for the
/// caller to send. Large responses are streamed directly through the tunnel
/// (ResponseStart/Chunk/End) and `None` is returned.
pub(crate) async fn handle_incoming_request(
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
    "Forwarding tunnel request ID {}: {} {}",
    id, method_str, uri_str
  );
  let target_parsed = match url::Url::parse(&ctx.target) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse local target URL: {:?}", e);
      return Some(make_error_response(id, 502));
    }
  };

  // Parse the incoming URI to extract path and query params
  let incoming_parsed = match url::Url::parse(&format!("http://localhost{}", uri_str)) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse incoming proxy URI path: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  let mut dest_url = target_parsed.clone();

  // Map URI path, optionally stripping the path_bind prefix
  let target_path = target_parsed.path().trim_end_matches('/');
  let mut incoming_path = incoming_parsed.path().trim_start_matches('/').to_string();

  if ctx.trim_bind && let Some(ref bind) = ctx.path_bind {
    let bind_trimmed = bind.trim_matches('/');
    if incoming_path.starts_with(bind_trimmed) {
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
  dest_url.set_query(incoming_parsed.query());

  // SSRF defence-in-depth: verify the constructed URL still resolves to the
  // original target host and port. This guards against subtle URL-parsing
  // edge-cases that could redirect tunnel traffic to unintended internal services.
  if dest_url.scheme() != target_parsed.scheme()
    || dest_url.host_str() != target_parsed.host_str()
    || dest_url.port_or_known_default() != target_parsed.port_or_known_default()
  {
    error!(
      "SSRF protection triggered: constructed URL diverges from target for request ID {}",
      id
    );
    return Some(make_error_response(id, 400));
  }

  let method = match reqwest::Method::from_bytes(method_str.as_bytes()) {
    Ok(m) => m,
    Err(e) => {
      error!("Invalid HTTP method representation: {:?}", e);
      return Some(make_error_response(id, 400));
    }
  };

  let mut builder = ctx.client.request(method, dest_url);

  // Map Headers
  let mut host_header_val = None;
  for (k, v) in headers.iter() {
    let k_lower = k.to_lowercase();

    // CRITICAL: Strip connection control, upgrade, and websocket headers
    if k_lower == "connection"
      || k_lower == "keep-alive"
      || k_lower == "upgrade"
      || k_lower == "proxy-connection"
      || k_lower == "accept-encoding"
      || k_lower.starts_with("sec-websocket-")
    {
      continue;
    }

    if k_lower == "host" {
      host_header_val = Some(v.clone());
      if !ctx.pass_hostname {
        // Ignore Host header if pass_hostname is disabled (use target authority)
        continue;
      }
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
  match builder.send().await {
    Ok(res) => {
      let status = res.status().as_u16();

      let mut res_headers: Vec<(String, String)> = Vec::new();
      for (k, v) in res.headers().iter() {
        if let Ok(v_str) = v.to_str() {
          res_headers.push((k.to_string(), v_str.to_string()));
        }
      }

      // Read the body incrementally. Bodies up to the stream threshold are
      // buffered and returned as a single Response message; larger bodies
      // switch to chunked streaming so memory usage stays bounded.
      let threshold = STREAM_THRESHOLD.min(ctx.max_response_body_size);
      let mut stream = res.bytes_stream();
      let mut buf: Vec<u8> = Vec::new();
      let mut streaming = false;
      let mut total: usize = 0;

      loop {
        match stream.next().await {
          Some(Ok(chunk)) => {
            total += chunk.len();
            if !streaming {
              buf.extend_from_slice(&chunk);
              if buf.len() > threshold {
                // Switch to streaming: send head + buffered data as chunks.
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
                  "Streamed response for request ID {} exceeded limit ({} bytes); truncating",
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
          }
          Some(Err(e)) => {
            if streaming {
              error!(
                "Body stream error from backend for request ID {}: {:?}; ending stream",
                id, e
              );
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

      if streaming {
        let end = TunnelMessage::ResponseEnd { id: id.clone() };
        let _ = send_tunnel_msg(tunnel_tx, &end).await;
        info!(
          "Tunnel request SUCCESS (streamed): ID={} Status={} Bytes={}",
          id, status, total
        );
        return None;
      }

      let body_encoded = if buf.is_empty() {
        None
      } else {
        Some(BASE64_STANDARD.encode(&buf))
      };

      info!("Tunnel request SUCCESS: ID={} Status={}", id, status);

      Some(TunnelMessage::Response {
        id,
        status,
        headers: res_headers,
        body: body_encoded,
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
  }
}

#[cfg(test)]
#[path = "forward_tests.rs"]
mod tests;
