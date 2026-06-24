use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::{
  connect_async,
  tungstenite::{client::IntoClientRequest, http::HeaderValue, protocol::Message},
};
use tracing::{debug, error, info, warn};

/// Message structure exchanged over the WebSocket reverse tunnel.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum TunnelMessage {
  Ping {
    client_id: String,
    timestamp: u64,
    path_bind: Option<String>,
  },
  Pong {
    timestamp: u64,
  },
  Request {
    id: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
  },
  Response {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Option<String>, // Base64 encoded payload
  },
}

#[tokio::main]
/// Entry point for the Aperio client.
/// Loads configuration from environment variables, sets up logging, and initiates the reconnect loop.
async fn main() {
  // Initialize logging with structured JSON output (pino.js style)
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });

  tracing_subscriber::fmt()
    .json()
    .with_current_span(false)
    .with_span_list(false)
    .flatten_event(true)
    .with_env_filter(log_filter)
    .init();

  info!("Starting Aperio Client...");

  // Enforce APERIO_SERVER_TOKEN environment variable
  let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_else(|_| {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN environment variable must be set!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN cannot be empty!");
    std::process::exit(1);
  }

  let server_addr = std::env::var("APERIO_SERVER_URL").unwrap_or_else(|_| {
    error!("CRITICAL ERROR: APERIO_SERVER_URL environment variable must be set!");
    std::process::exit(1);
  });
  let target = std::env::var("APERIO_CLIENT_TARGET").unwrap_or_else(|_| {
    error!("CRITICAL ERROR: APERIO_CLIENT_TARGET environment variable must be set!");
    std::process::exit(1);
  });
  let pass_hostname_val =
    std::env::var("APERIO_CLIENT_PASS_HOSTNAME").unwrap_or_else(|_| "0".to_string());
  let pass_hostname = pass_hostname_val == "1";

  let path_bind = std::env::var("APERIO_PATH_BIND").ok();

  let trim_bind = if path_bind.is_some() {
    std::env::var("APERIO_CLIENT_TRIM_BIND").unwrap_or_else(|_| "1".to_string()) == "1"
  } else {
    false
  };

  // Maximum response body size (in bytes) accepted from the target backend.
  // Protects the client (and the tunnel) from OOM when a misbehaving backend
  // streams an unbounded response. Defaults to 50 MB.
  let max_response_body_size = std::env::var("APERIO_CLIENT_MAX_RESPONSE_BODY")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(50 * 1024 * 1024);

  // Per-request timeout (in seconds) for calls to the local target backend.
  // Prevents the client from hanging indefinitely if the backend stalls.
  // Defaults to 30 seconds.
  let client_timeout_secs = std::env::var("APERIO_CLIENT_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(30);

  let client_id = uuid::Uuid::new_v4().to_string();

  let ws_url = match build_ws_url(&server_addr) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to build WebSocket URL: {}", e);
      std::process::exit(1);
    }
  };

  info!("Configuration loaded:");
  info!("- Client ID: {}", client_id);
  info!("- Target: {}", target);
  info!("- Pass Hostname: {}", pass_hostname);
  if let Some(ref bind) = path_bind {
    info!("- Path Bind: {}", bind);
    info!("- Trim Bind: {}", trim_bind);
  }
  info!("- WebSocket URL: {}", ws_url);

  // Reconnection Loop
  loop {
    info!("Connecting to Aperio Server at: {}...", server_addr);

    let ws_req_result = ws_url.clone().into_client_request();
    let ws_req = match ws_req_result {
      Ok(mut req) => {
        // Set Authorization Token Header securely (avoids leaking token in query params / logs)
        match HeaderValue::from_str(&format!("Bearer {}", token)) {
          Ok(val) => {
            req.headers_mut().insert("Authorization", val);
            Ok(req)
          }
          Err(e) => Err(format!("Invalid token header format: {:?}", e)),
        }
      }
      Err(e) => Err(format!("Failed to construct connection request: {:?}", e)),
    };

    match ws_req {
      Ok(req) => {
        match connect_async(req).await {
          Ok((ws_stream, _)) => {
            info!("Successfully connected to Aperio Server!");
            let (mut ws_sender, mut ws_receiver) = ws_stream.split();

            // Channel to write messages to the WebSocket
            let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);

            // Abort channel for liveness failures
            let (abort_tx, mut abort_rx) = mpsc::channel::<()>(1);

            // Track connection liveness via Pong response time
            let last_pong_time = Arc::new(Mutex::new(Instant::now()));

            // Spawn task to handle WebSocket writes
            let writer_task = tokio::spawn(async move {
              while let Some(msg) = rx_write.recv().await {
                if let Err(e) = ws_sender.send(msg).await {
                  error!("Error writing to server socket: {:?}", e);
                  break;
                }
              }
            });

            // Spawn task for heartbeat (Ping every 5 seconds & liveness check)
            let tx_ping = tx_write.clone();
            let client_id_ping = client_id.clone();
            let path_bind_ping = path_bind.clone();
            let last_pong_time_ping = last_pong_time.clone();
            let abort_tx_ping = abort_tx.clone();

            let ping_task = tokio::spawn(async move {
              loop {
                tokio::time::sleep(Duration::from_secs(5)).await;

                // Check last Pong receipt time (max 15s limit)
                let elapsed = {
                  let lock = last_pong_time_ping.lock().await;
                  lock.elapsed()
                };
                if elapsed > Duration::from_secs(15) {
                  warn!(
                    "Liveness check failed: no Pong received for {} seconds. Resetting connection.",
                    elapsed.as_secs()
                  );
                  let _ = abort_tx_ping.send(()).await;
                  break;
                }

                let ping_msg = TunnelMessage::Ping {
                  client_id: client_id_ping.clone(),
                  timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                  path_bind: path_bind_ping.clone(),
                };
                if let Ok(ping_str) = serde_json::to_string(&ping_msg)
                  && tx_ping.send(Message::Text(ping_str)).await.is_err()
                {
                  break;
                }
              }
            });

            // Reqwest Client to make local forwarding requests
            let reqwest_client = reqwest::Client::builder()
              .redirect(reqwest::redirect::Policy::none()) // Let backend determine redirect loops
              .timeout(Duration::from_secs(client_timeout_secs))
              .build()
              .unwrap_or_default();

            // Read messages from Server
            loop {
              tokio::select! {
                  _ = abort_rx.recv() => {
                      warn!("Liveness timeout triggered. Aborting socket loop.");
                      break;
                  }
                  msg_res = ws_receiver.next() => {
                      match msg_res {
                          Some(Ok(msg)) => {
                              if let Message::Text(text) = msg
                                  && let Ok(tunnel_msg) = serde_json::from_str::<TunnelMessage>(&text)
                              {
                                  match tunnel_msg {
                                          TunnelMessage::Request {
                                              id,
                                              method,
                                              uri,
                                              headers,
                                              body,
                                          } => {
                                              let tx_resp = tx_write.clone();
                                              let req_client = reqwest_client.clone();
                                              let target_url = target.clone();
                                              let path_bind_val = path_bind.clone();
                                              let trim_bind_val = trim_bind;
                                              let max_resp_size = max_response_body_size;

                                              // Handle incoming request concurrently
                                              tokio::spawn(async move {
                                                  let response = handle_incoming_request(
                                                      req_client,
                                                      id,
                                                      method,
                                                      uri,
                                                      headers,
                                                      body,
                                                      &target_url,
                                                      pass_hostname,
                                                      path_bind_val,
                                                      trim_bind_val,
                                                      max_resp_size,
                                                  )
                                                  .await;

                                                  if let Ok(resp_str) = serde_json::to_string(&response) {
                                                      let _ = tx_resp.send(Message::Text(resp_str)).await;
                                                  }
                                              });
                                          }
                                          TunnelMessage::Pong { timestamp } => {
                                              debug!("Pong received: {}", timestamp);
                                              let mut lock = last_pong_time.lock().await;
                                              *lock = Instant::now();
                                          }
                                          _ => {}
                                      }
                                  }
                              }
                          Some(Err(e)) => {
                              error!("Error reading from server socket: {:?}", e);
                              break;
                          }
                          None => {
                              warn!("WebSocket stream closed by server.");
                              break;
                          }
                      }
                  }
              }
            }

            // Cleanup tasks on connection loss
            writer_task.abort();
            ping_task.abort();
            warn!("Connection to server lost.");
          }
          Err(e) => {
            error!("Failed to connect to server: {:?}.", e);
          }
        }
      }
      Err(e) => {
        error!("WebSocket configuration request building error: {}", e);
      }
    }

    info!("Retrying connection in 5 seconds...");
    tokio::time::sleep(Duration::from_secs(5)).await;
  }
}

/// Builds the correct WebSocket connection URL from an HTTP or WS address.
/// Ensures the scheme is set to `ws` or `wss` and appends the tunnel path `/aperio/ws`.
fn build_ws_url(server: &str) -> Result<String, String> {
  let mut server_clean = server.to_string();
  if !server_clean.contains("://") {
    server_clean = format!("http://{}", server_clean);
  }

  let mut parsed = url::Url::parse(&server_clean).map_err(|e| e.to_string())?;

  let ws_scheme = match parsed.scheme() {
    "https" | "wss" => "wss",
    "http" | "ws" => "ws",
    other => return Err(format!("Unsupported scheme: {}", other)),
  };

  parsed
    .set_scheme(ws_scheme)
    .map_err(|_| "Failed to set WebSocket scheme".to_string())?;
  parsed.set_path("/aperio/ws");

  Ok(parsed.to_string())
}

/// Forwards a proxied HTTP request from the websocket tunnel to the local target server.
/// Sanitizes sensitive/upgrade headers, rewrites URLs, routes the HTTP request, and returns
/// the response mapped back into a `TunnelMessage`.
#[allow(clippy::too_many_arguments)]
async fn handle_incoming_request(
  client: reqwest::Client,
  id: String,
  method_str: String,
  uri_str: String,
  headers: Vec<(String, String)>,
  body_base64: Option<String>,
  target: &str,
  pass_hostname: bool,
  path_bind: Option<String>,
  trim_bind: bool,
  max_response_body_size: usize,
) -> TunnelMessage {
  info!(
    "Forwarding tunnel request ID {}: {} {}",
    id, method_str, uri_str
  );
  let target_parsed = match url::Url::parse(target) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse local target URL: {:?}", e);
      return make_error_response(id, 502);
    }
  };

  // Parse the incoming URI to extract path and query params
  let incoming_parsed = match url::Url::parse(&format!("http://localhost{}", uri_str)) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse incoming proxy URI path: {:?}", e);
      return make_error_response(id, 400);
    }
  };

  let mut dest_url = target_parsed.clone();

  // Map URI path, optionally stripping the path_bind prefix
  let target_path = target_parsed.path().trim_end_matches('/');
  let mut incoming_path = incoming_parsed.path().trim_start_matches('/').to_string();

  if trim_bind && let Some(ref bind) = path_bind {
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
    return make_error_response(id, 400);
  }

  let method = match reqwest::Method::from_bytes(method_str.as_bytes()) {
    Ok(m) => m,
    Err(e) => {
      error!("Invalid HTTP method representation: {:?}", e);
      return make_error_response(id, 400);
    }
  };

  let mut builder = client.request(method, dest_url);

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
      if !pass_hostname {
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

  if pass_hostname
    && let Some(host) = host_header_val
    && let Ok(val) = reqwest::header::HeaderValue::from_str(&host)
  {
    builder = builder.header(reqwest::header::HOST, val);
  }

  // Map Body
  if let Some(encoded_body) = body_base64 {
    match BASE64_STANDARD.decode(encoded_body) {
      Ok(bytes) => {
        builder = builder.body(bytes);
      }
      Err(e) => {
        error!("Base64 decoding failed for request body payload: {:?}", e);
        return make_error_response(id, 400);
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

      let body_bytes = match collect_body_limited(res, max_response_body_size).await {
        Ok(bytes) => bytes,
        Err(BodyError::TooLarge) => {
          warn!(
            "Target backend response body exceeded limit ({} bytes) for request ID {}",
            max_response_body_size, id
          );
          return make_error_response(id, 502);
        }
        Err(BodyError::Io(e)) => {
          error!(
            "Failed to retrieve response body from target backend: {:?}",
            e
          );
          return make_error_response(id, 502);
        }
      };

      let body_encoded = if body_bytes.is_empty() {
        None
      } else {
        Some(BASE64_STANDARD.encode(&body_bytes))
      };

      info!("Tunnel request SUCCESS: ID={} Status={}", id, status);

      TunnelMessage::Response {
        id,
        status,
        headers: res_headers,
        body: body_encoded,
      }
    }
    Err(e) => {
      warn!("Tunnel request FAILURE: ID={} Error={:?}", id, e);
      make_error_response(id, 502)
    }
  }
}

/// Error returned by [`collect_body_limited`].
enum BodyError {
  /// Response body exceeded the configured maximum size.
  TooLarge,
  /// Underlying transport error while streaming the body.
  Io(reqwest::Error),
}

/// Reads a response body fully into a `Vec<u8>`, aborting once it exceeds
/// `max_size` bytes to protect against OOM from unbounded backend responses.
async fn collect_body_limited(
  res: reqwest::Response,
  max_size: usize,
) -> Result<Vec<u8>, BodyError> {
  use futures_util::StreamExt;
  let mut stream = res.bytes_stream();
  let mut buf: Vec<u8> = Vec::new();
  while let Some(chunk) = stream.next().await {
    let chunk = chunk.map_err(BodyError::Io)?;
    if buf.len() + chunk.len() > max_size {
      return Err(BodyError::TooLarge);
    }
    buf.extend_from_slice(&chunk);
  }
  Ok(buf)
}

/// Formats a generic masked error response, avoiding leaking raw socket error details.
fn make_error_response(id: String, status: u16) -> TunnelMessage {
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
mod tests {
  use super::*;

  #[test]
  fn test_build_ws_url() {
    assert_eq!(
      build_ws_url("http://localhost:8080").unwrap(),
      "ws://localhost:8080/aperio/ws"
    );
    assert_eq!(
      build_ws_url("https://example.com").unwrap(),
      "wss://example.com/aperio/ws"
    );
    assert_eq!(
      build_ws_url("ws://localhost:8080").unwrap(),
      "ws://localhost:8080/aperio/ws"
    );
    assert_eq!(
      build_ws_url("localhost:8080").unwrap(),
      "ws://localhost:8080/aperio/ws"
    );
    assert!(build_ws_url("ftp://localhost").is_err());
  }

  #[tokio::test]
  async fn test_make_error_response() {
    let response = make_error_response("req-123".to_string(), 502);
    if let TunnelMessage::Response {
      id,
      status,
      headers,
      body,
    } = response
    {
      assert_eq!(id, "req-123");
      assert_eq!(status, 502);
      let ct = headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v)
        .unwrap();
      assert_eq!(ct, "text/plain");
      let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
      let decoded_str = String::from_utf8(decoded).unwrap();
      assert!(decoded_str.contains("502 Bad Gateway"));
    } else {
      panic!("Expected Response variant");
    }
  }

  #[tokio::test]
  async fn test_handle_incoming_request() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target_url = format!("http://127.0.0.1:{}", port);

    // Spawn a mock target server
    tokio::spawn(async move {
      if let Ok((mut socket, _)) = listener.accept().await {
        let mut buf = [0; 1024];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]);

        // Check that request contains original path and custom header
        assert!(req_str.contains("GET /test-path"));
        assert!(req_str.contains("x-custom-header: custom-value"));

        // Write back a simple HTTP response
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nContent-Type: text/plain\r\n\r\nhello from local";
        socket.write_all(response.as_bytes()).await.unwrap();
      }
    });

    let client = reqwest::Client::new();
    let headers = vec![("x-custom-header".to_string(), "custom-value".to_string())];

    let result = handle_incoming_request(
      client,
      "req-id-123".to_string(),
      "GET".to_string(),
      "/test-path".to_string(),
      headers,
      None,
      &target_url,
      false,
      None,
      false,
      1024 * 1024,
    )
    .await;

    if let TunnelMessage::Response {
      id,
      status,
      headers,
      body,
    } = result
    {
      assert_eq!(id, "req-id-123");
      assert_eq!(status, 200);
      let ct = headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v)
        .unwrap();
      assert_eq!(ct, "text/plain");
      let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
      assert_eq!(String::from_utf8(decoded).unwrap(), "hello from local");
    } else {
      panic!("Expected response variant");
    }
  }

  #[tokio::test]
  async fn test_handle_incoming_request_trim_bind() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target_url = format!("http://127.0.0.1:{}", port);

    // Channel to receive the observed request line from the mock server.
    let (tx, rx) = oneshot::channel::<String>();

    tokio::spawn(async move {
      let _tx = tx;
      if let Ok((mut socket, _)) = listener.accept().await {
        let mut buf = [0; 1024];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let request_line = req_str.lines().next().unwrap_or("").to_string();
        // Send the observed request line back, then write a minimal response.
        let _ = _tx.send(request_line);
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let _ = socket.write_all(response.as_bytes()).await;
      }
    });

    let client = reqwest::Client::new();
    // path_bind = "/api", trim_bind = true → /api/hello should become /hello
    let result = handle_incoming_request(
      client,
      "req-trim-1".to_string(),
      "GET".to_string(),
      "/api/hello".to_string(),
      vec![],
      None,
      &target_url,
      false,
      Some("/api".to_string()),
      true,
      1024 * 1024,
    )
    .await;

    let observed = rx.await.unwrap();
    // The mock server should have received the trimmed path "/hello".
    assert!(
      observed.contains("GET /hello"),
      "expected trimmed path '/hello' in request line, got: {}",
      observed
    );

    if let TunnelMessage::Response { status, .. } = result {
      assert_eq!(status, 200);
    } else {
      panic!("Expected response variant");
    }
  }

  #[tokio::test]
  async fn test_handle_incoming_request_trim_bind_disabled() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target_url = format!("http://127.0.0.1:{}", port);

    let (tx, rx) = oneshot::channel::<String>();

    tokio::spawn(async move {
      let _tx = tx;
      if let Ok((mut socket, _)) = listener.accept().await {
        let mut buf = [0; 1024];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let request_line = req_str.lines().next().unwrap_or("").to_string();
        let _ = _tx.send(request_line);
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let _ = socket.write_all(response.as_bytes()).await;
      }
    });

    let client = reqwest::Client::new();
    // path_bind = "/api", trim_bind = false → path should NOT be stripped
    let _result = handle_incoming_request(
      client,
      "req-trim-2".to_string(),
      "GET".to_string(),
      "/api/hello".to_string(),
      vec![],
      None,
      &target_url,
      false,
      Some("/api".to_string()),
      false,
      1024 * 1024,
    )
    .await;

    let observed = rx.await.unwrap();
    assert!(
      observed.contains("GET /api/hello"),
      "expected untrimmed path '/api/hello' in request line, got: {}",
      observed
    );
  }
}
