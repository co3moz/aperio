use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_tungstenite::{
  connect_async, connect_async_with_config,
  tungstenite::{
    client::IntoClientRequest,
    http::{HeaderName as TungsteniteHeaderName, HeaderValue},
    protocol::{Message, WebSocketConfig},
  },
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
    #[serde(default)]
    hostname_bind: Option<String>,
    /// Maximum concurrent requests this client is willing to process.
    /// The server queues excess requests instead of dispatching them.
    #[serde(default)]
    max_concurrent: Option<u32>,
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
  /// Start of a streamed response: status and headers only. The body follows
  /// as `ResponseChunk` messages terminated by `ResponseEnd`. Used for large
  /// bodies so neither side buffers the full payload in memory.
  ResponseStart {
    id: String,
    status: u16,
    headers: Vec<(String, String)>,
  },
  /// A chunk of a streamed response body (Base64 encoded).
  ResponseChunk {
    id: String,
    data: String,
  },
  /// Marks the end of a streamed response body.
  ResponseEnd {
    id: String,
  },
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
}

/// Handle to an active WebSocket proxy stream connected to the local backend.
struct WsStreamHandle {
  /// Sender to forward tunnel WsData frames to the backend WebSocket writer task.
  tx: mpsc::Sender<Message>,
  /// Abort handle to stop the relay tasks.
  abort_tx: mpsc::Sender<()>,
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

  // Hostname this client wants to serve (e.g. "a.example.com"). The server
  // routes requests whose Host header matches this value to this client.
  let hostname_bind = std::env::var("APERIO_HOSTNAME_BIND")
    .ok()
    .map(|h| h.trim().to_ascii_lowercase())
    .filter(|h| !h.is_empty());

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

  // Maximum concurrent requests processed locally. Announced to the server so
  // it queues excess requests instead of flooding the backend. Also enforced
  // locally, since the client must not fully trust the server. 0 = unlimited.
  let max_concurrent = std::env::var("APERIO_CLIENT_MAX_CONCURRENT")
    .ok()
    .and_then(|val| val.parse::<u32>().ok())
    .filter(|n| *n > 0);

  let local_limiter: Option<Arc<Semaphore>> =
    max_concurrent.map(|n| Arc::new(Semaphore::new(n as usize)));

  // Cap on individual tunnel WebSocket messages accepted from the server.
  // The client must not fully trust the server, so bound memory per frame.
  // Defaults to 32 MB (requests bodies are limited server-side anyway).
  let max_message_size = std::env::var("APERIO_CLIENT_MAX_MESSAGE_SIZE")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(32 * 1024 * 1024);

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
  if let Some(ref host) = hostname_bind {
    info!("- Hostname Bind: {}", host);
  }
  if let Some(n) = max_concurrent {
    info!("- Max Concurrent Requests: {}", n);
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
        let ws_config = WebSocketConfig {
          max_message_size: Some(max_message_size),
          max_frame_size: Some(max_message_size),
          ..Default::default()
        };
        match connect_async_with_config(req, Some(ws_config), false).await {
          Ok((ws_stream, _)) => {
            info!("Successfully connected to Aperio Server!");
            let (mut ws_sender, mut ws_receiver) = ws_stream.split();

            // Channel to write messages to the WebSocket
            let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);

            // Abort channel for liveness failures
            let (abort_tx, mut abort_rx) = mpsc::channel::<()>(1);

            // Track connection liveness via Pong response time
            let last_pong_time = Arc::new(Mutex::new(Instant::now()));

            // Active WebSocket proxy streams: stream_id → handle
            let active_ws_streams: Arc<Mutex<HashMap<String, WsStreamHandle>>> =
              Arc::new(Mutex::new(HashMap::new()));

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
            let hostname_bind_ping = hostname_bind.clone();
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
                  hostname_bind: hostname_bind_ping.clone(),
                  max_concurrent,
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
                                              let limiter = local_limiter.clone();

                                              // Handle incoming request concurrently
                                              tokio::spawn(async move {
                                                  // Local concurrency guard: even a misbehaving server
                                                  // cannot push more parallel work onto the backend.
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
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
                                                      tx_resp.clone(),
                                                  )
                                                  .await;

                                                  // None = the response was streamed through the tunnel already.
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = tx_resp.send(Message::Text(resp_str)).await;
                                                  }
                                              });
                                          }
                                          TunnelMessage::UpgradeRequest {
                                              id,
                                              method,
                                              uri,
                                              headers,
                                          } => {
                                              let tx_resp = tx_write.clone();
                                              let target_url = target.clone();
                                              let path_bind_val = path_bind.clone();
                                              let trim_bind_val = trim_bind;
                                              let active_streams = active_ws_streams.clone();
                                              let client_timeout = client_timeout_secs;

                                              tokio::spawn(async move {
                                                  handle_upgrade_request(
                                                      id,
                                                      method,
                                                      uri,
                                                      headers,
                                                      &target_url,
                                                      path_bind_val,
                                                      trim_bind_val,
                                                      tx_resp,
                                                      active_streams,
                                                      client_timeout,
                                                  )
                                                  .await;
                                              });
                                          }
                                          TunnelMessage::WsData {
                                              stream_id,
                                              data,
                                              is_text,
                                          } => {
                                              // Forward from tunnel → backend WS
                                              let streams = active_ws_streams.lock().await;
                                              if let Some(handle) = streams.get(&stream_id) {
                                                  let ws_msg = if is_text {
                                                      Message::Text(data)
                                                  } else {
                                                      match BASE64_STANDARD.decode(&data) {
                                                          Ok(bytes) => Message::Binary(bytes),
                                                          Err(_) => {
                                                              warn!("Failed to decode Base64 WsData for stream {}", stream_id);
                                                              continue;
                                                          }
                                                      }
                                                  };
                                                  if handle.tx.send(ws_msg).await.is_err() {
                                                      debug!("Backend WS channel closed for stream {}", stream_id);
                                                  }
                                              }
                                          }
                                          TunnelMessage::WsClose {
                                              stream_id,
                                              code: _,
                                              reason: _,
                                          } => {
                                              // Close the backend WS stream
                                              let mut streams = active_ws_streams.lock().await;
                                              if let Some(handle) = streams.remove(&stream_id) {
                                                  let _ = handle.abort_tx.send(()).await;
                                                  debug!("Closed WebSocket stream {}", stream_id);
                                              }
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

/// Serializes and sends a tunnel message; returns Err(()) when the tunnel
/// write channel is closed.
async fn send_tunnel_msg(tx: &mpsc::Sender<Message>, msg: &TunnelMessage) -> Result<(), ()> {
  match serde_json::to_string(msg) {
    Ok(json) => tx.send(Message::Text(json)).await.map_err(|_| ()),
    Err(_) => Err(()),
  }
}

/// Response bodies larger than this are streamed through the tunnel in
/// chunks instead of being buffered and sent as one message.
const STREAM_THRESHOLD: usize = 256 * 1024;
/// Size of individual streamed body chunks.
const STREAM_CHUNK_SIZE: usize = 128 * 1024;

/// Forwards a proxied HTTP request from the websocket tunnel to the local target server.
/// Sanitizes sensitive/upgrade headers, rewrites URLs, routes the HTTP request, and returns
/// the response mapped back into a `TunnelMessage`.
///
/// Small responses are returned as `Some(TunnelMessage::Response)` for the
/// caller to send. Large responses are streamed directly through `tunnel_tx`
/// (ResponseStart/Chunk/End) and `None` is returned.
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
  tunnel_tx: mpsc::Sender<Message>,
) -> Option<TunnelMessage> {
  info!(
    "Forwarding tunnel request ID {}: {} {}",
    id, method_str, uri_str
  );
  let target_parsed = match url::Url::parse(target) {
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
    return Some(make_error_response(id, 400));
  }

  let method = match reqwest::Method::from_bytes(method_str.as_bytes()) {
    Ok(m) => m,
    Err(e) => {
      error!("Invalid HTTP method representation: {:?}", e);
      return Some(make_error_response(id, 400));
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
      let threshold = STREAM_THRESHOLD.min(max_response_body_size);
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
                if send_tunnel_msg(&tunnel_tx, &start).await.is_err() {
                  return None;
                }
                for part in buf.chunks(STREAM_CHUNK_SIZE) {
                  let msg = TunnelMessage::ResponseChunk {
                    id: id.clone(),
                    data: BASE64_STANDARD.encode(part),
                  };
                  if send_tunnel_msg(&tunnel_tx, &msg).await.is_err() {
                    return None;
                  }
                }
                buf = Vec::new();
                streaming = true;
              }
            } else {
              if total > max_response_body_size {
                warn!(
                  "Streamed response for request ID {} exceeded limit ({} bytes); truncating",
                  id, max_response_body_size
                );
                break;
              }
              let msg = TunnelMessage::ResponseChunk {
                id: id.clone(),
                data: BASE64_STANDARD.encode(&chunk),
              };
              if send_tunnel_msg(&tunnel_tx, &msg).await.is_err() {
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
        let _ = send_tunnel_msg(&tunnel_tx, &end).await;
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

/// Handles a WebSocket upgrade request from the server.
/// Connects to the local backend via WebSocket, sends the upgrade response,
/// and spawns relay tasks for bidirectional frame forwarding.
#[allow(clippy::too_many_arguments)]
async fn handle_upgrade_request(
  stream_id: String,
  _method: String,
  uri_str: String,
  headers: Vec<(String, String)>,
  target: &str,
  path_bind: Option<String>,
  trim_bind: bool,
  tunnel_tx: mpsc::Sender<Message>,
  active_streams: Arc<Mutex<HashMap<String, WsStreamHandle>>>,
  client_timeout_secs: u64,
) {
  info!("Handling WebSocket upgrade for stream {}", stream_id);

  let target_parsed = match url::Url::parse(target) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse local target URL for WS upgrade: {:?}", e);
      send_upgrade_error(&stream_id, &tunnel_tx, 502).await;
      return;
    }
  };

  // Build the WebSocket URL from the target
  let incoming_parsed = match url::Url::parse(&format!("http://localhost{}", uri_str)) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse incoming URI for WS upgrade: {:?}", e);
      send_upgrade_error(&stream_id, &tunnel_tx, 400).await;
      return;
    }
  };

  let mut ws_dest_str = target.to_string();
  // Build the full path for the WebSocket URL
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

  // Reconstruct the target as a ws:// URL for tokio-tungstenite
  let ws_scheme = match target_parsed.scheme() {
    "https" => "wss",
    _ => "ws",
  };

  let combined_path = if target_path.is_empty() {
    format!("/{}", incoming_path)
  } else {
    format!("{}/{}", target_path, incoming_path)
  };

  if let Ok(mut parsed) = url::Url::parse(&ws_dest_str) {
    let _ = parsed.set_scheme(ws_scheme);
    parsed.set_path(&combined_path);
    parsed.set_query(incoming_parsed.query());
    ws_dest_str = parsed.to_string();
  }

  // Build the WebSocket request with the original headers (KEEP upgrade headers here)
  let ws_req = match ws_dest_str.clone().into_client_request() {
    Ok(mut req) => {
      // Map relevant headers from the upgrade request (keep Sec-WebSocket-*, etc.)
      for (k, v) in headers.iter() {
        let k_lower = k.to_lowercase();
        // Skip headers that tokio-tungstenite manages or that cause issues
        if k_lower == "host"
          || k_lower == "connection"
          || k_lower == "accept-encoding"
          || k_lower == "content-length"
          || k_lower == "content-type"
        {
          continue;
        }
        // Keep upgrade-related headers for the backend WS handshake
        let is_upgrade_header =
          k_lower == "upgrade" || k_lower.starts_with("sec-websocket-") || k_lower == "origin";

        if is_upgrade_header
          && let (Ok(name), Ok(val)) = (
            TungsteniteHeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
          )
        {
          req.headers_mut().insert(name, val);
        }
      }
      Ok(req)
    }
    Err(e) => Err(format!("Failed to construct WS request: {:?}", e)),
  };

  let ws_req = match ws_req {
    Ok(r) => r,
    Err(e) => {
      error!(
        "WebSocket request building error for stream {}: {}",
        stream_id, e
      );
      send_upgrade_error(&stream_id, &tunnel_tx, 400).await;
      return;
    }
  };

  // Connect with timeout
  let connect_fut = connect_async(ws_req);
  let timeout_fut = tokio::time::sleep(Duration::from_secs(client_timeout_secs));
  tokio::pin!(timeout_fut);

  let (backend_ws, _resp) = tokio::select! {
      _ = &mut timeout_fut => {
          error!("WebSocket connection to backend timed out for stream {}", stream_id);
          send_upgrade_error(&stream_id, &tunnel_tx, 504).await;
          return;
      }
      result = connect_fut => {
          match result {
              Ok(ws) => ws,
              Err(e) => {
                  error!("Failed to connect WebSocket to backend for stream {}: {:?}", stream_id, e);
                  send_upgrade_error(&stream_id, &tunnel_tx, 502).await;
                  return;
              }
          }
      }
  };

  // Send UpgradeResponse (101) to server
  let upgrade_resp = TunnelMessage::UpgradeResponse {
    id: stream_id.clone(),
    status: 101,
    headers: vec![
      ("upgrade".to_string(), "websocket".to_string()),
      ("connection".to_string(), "Upgrade".to_string()),
    ],
  };

  if let Ok(json) = serde_json::to_string(&upgrade_resp)
    && tunnel_tx.send(Message::Text(json)).await.is_err()
  {
    error!("Failed to send UpgradeResponse for stream {}", stream_id);
    return;
  }

  // Split the backend WebSocket
  let (mut backend_sender, mut backend_receiver) = backend_ws.split();

  // Channel to relay tunnel WsData → backend WS
  let (relay_tx, mut relay_rx) = mpsc::channel::<Message>(64);
  // Abort channel
  let (abort_tx, mut abort_rx) = mpsc::channel::<()>(1);

  // Register the stream
  {
    let mut streams = active_streams.lock().await;
    streams.insert(
      stream_id.clone(),
      WsStreamHandle {
        tx: relay_tx,
        abort_tx: abort_tx.clone(),
      },
    );
  }

  let tunnel_tx_clone = tunnel_tx.clone();
  let stream_id_clone = stream_id.clone();

  // Task: read from backend WS → send WsData through tunnel
  let backend_to_tunnel = tokio::spawn(async move {
    while let Some(result) = backend_receiver.next().await {
      match result {
        Ok(msg) => {
          let tunnel_msg = match msg {
            Message::Text(text) => TunnelMessage::WsData {
              stream_id: stream_id_clone.clone(),
              data: text.to_string(),
              is_text: true,
            },
            Message::Binary(data) => {
              use base64::prelude::*;
              TunnelMessage::WsData {
                stream_id: stream_id_clone.clone(),
                data: BASE64_STANDARD.encode(&data),
                is_text: false,
              }
            }
            Message::Close(frame) => TunnelMessage::WsClose {
              stream_id: stream_id_clone.clone(),
              code: frame.as_ref().map(|f| f.code.into()).unwrap_or(1000),
              reason: frame
                .as_ref()
                .map(|f| f.reason.to_string())
                .unwrap_or_default(),
            },
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
              // Auto-handled at transport level
              continue;
            }
          };

          if let Ok(json) = serde_json::to_string(&tunnel_msg)
            && tunnel_tx_clone.send(Message::Text(json)).await.is_err()
          {
            break;
          }
        }
        Err(e) => {
          debug!(
            "Backend WS read error for stream {}: {:?}",
            stream_id_clone, e
          );
          break;
        }
      }
    }

    // Backend WS disconnected — send WsClose to server
    let close_msg = TunnelMessage::WsClose {
      stream_id: stream_id_clone.clone(),
      code: 1000,
      reason: String::new(),
    };
    if let Ok(json) = serde_json::to_string(&close_msg) {
      let _ = tunnel_tx_clone.send(Message::Text(json)).await;
    }
  });

  // Task: read from relay channel (tunnel → backend WS) → write to backend WS
  let stream_id_writer = stream_id.clone();
  let backend_writer = tokio::spawn(async move {
    loop {
      tokio::select! {
          _ = abort_rx.recv() => {
              // Stream is being closed
              let _ = backend_sender.send(Message::Close(None)).await;
              break;
          }
          msg_opt = relay_rx.recv() => {
              match msg_opt {
                  Some(msg) => {
                      if backend_sender.send(msg).await.is_err() {
                          break;
                      }
                  }
                  None => break,
              }
          }
      }
    }
    debug!("Backend WS writer closed for stream {}", stream_id_writer);
  });

  let backend_to_tunnel_abort = backend_to_tunnel.abort_handle();
  let backend_writer_abort = backend_writer.abort_handle();

  // Wait for either task to finish; abort the other
  tokio::select! {
      _ = backend_to_tunnel => {
          backend_writer_abort.abort();
      }
      _ = backend_writer => {
          backend_to_tunnel_abort.abort();
      }
  }

  {
    let mut streams = active_streams.lock().await;
    streams.remove(&stream_id);
  }

  info!("WebSocket stream {} closed", stream_id);
}

/// Sends an error UpgradeResponse to the server.
async fn send_upgrade_error(stream_id: &str, tunnel_tx: &mpsc::Sender<Message>, status: u16) {
  let resp = TunnelMessage::UpgradeResponse {
    id: stream_id.to_string(),
    status,
    headers: vec![("content-type".to_string(), "text/plain".to_string())],
  };
  if let Ok(json) = serde_json::to_string(&resp) {
    let _ = tunnel_tx.send(Message::Text(json)).await;
  }
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

  /// Tunnel sender whose receiver is drained in the background, for tests
  /// that exercise the buffered (non-streaming) response path.
  fn test_tunnel_tx() -> mpsc::Sender<Message> {
    let (tx, mut rx) = mpsc::channel::<Message>(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
  }

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
      test_tunnel_tx(),
    )
    .await
    .expect("expected buffered response");

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
  async fn test_handle_incoming_request_streams_large_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target_url = format!("http://127.0.0.1:{}", port);

    // Body larger than STREAM_THRESHOLD (256 KB) → must be streamed.
    let body_size = 600 * 1024;

    tokio::spawn(async move {
      if let Ok((mut socket, _)) = listener.accept().await {
        let mut buf = [0; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let header = format!(
          "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
          body_size
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        let payload = vec![0xABu8; body_size];
        socket.write_all(&payload).await.unwrap();
      }
    });

    let (tx, mut rx) = mpsc::channel::<Message>(256);
    let client = reqwest::Client::new();
    let result = handle_incoming_request(
      client,
      "req-stream-1".to_string(),
      "GET".to_string(),
      "/big".to_string(),
      vec![],
      None,
      &target_url,
      false,
      None,
      false,
      10 * 1024 * 1024,
      tx,
    )
    .await;

    // Streamed responses return None; the messages went through the channel.
    assert!(result.is_none(), "large body should be streamed");

    let mut got_start = false;
    let mut got_end = false;
    let mut total_bytes = 0usize;
    while let Some(Message::Text(json)) = rx.recv().await {
      match serde_json::from_str::<TunnelMessage>(&json).unwrap() {
        TunnelMessage::ResponseStart { id, status, .. } => {
          assert_eq!(id, "req-stream-1");
          assert_eq!(status, 200);
          got_start = true;
        }
        TunnelMessage::ResponseChunk { data, .. } => {
          assert!(got_start, "chunk before start");
          total_bytes += BASE64_STANDARD.decode(data).unwrap().len();
        }
        TunnelMessage::ResponseEnd { .. } => {
          got_end = true;
          break;
        }
        other => panic!("unexpected message: {:?}", other),
      }
    }
    assert!(got_start && got_end);
    assert_eq!(total_bytes, body_size);
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
      test_tunnel_tx(),
    )
    .await
    .expect("expected buffered response");

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
      test_tunnel_tx(),
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
