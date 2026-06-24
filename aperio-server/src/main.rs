use axum::{
  Json, Router,
  body::Body,
  extract::{
    ConnectInfo, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
  response::{Html, IntoResponse, Response},
  routing::{any, get},
};
use chrono::Local;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

/// Message structure exchanged over the WebSocket reverse tunnel.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum TunnelMessage {
  Ping {
    client_id: String,
    timestamp: u64,
  },
  Pong {
    timestamp: u64,
  },
  Request {
    id: String,
    method: String,
    uri: String,
    headers: HashMap<String, String>,
    body: Option<String>, // Base64 encoded payload
  },
  Response {
    id: String,
    status: u16,
    headers: HashMap<String, String>,
    body: Option<String>, // Base64 encoded payload
  },
}

/// Configuration settings for the Aperio server.
#[derive(Clone)]
struct ServerConfig {
  token: String,
  gateway_timeout: Duration,
  gateway_response_timeout: Duration,
  max_body_size: usize,
  max_tunnels: usize,
  ip_limit_max: f64,
  ip_limit_refill: f64,
  basic_auth: Option<String>,
}

/// In-memory server-wide traffic statistics.
#[derive(Serialize, Clone)]
struct ServerStats {
  /// Total count of incoming proxied requests.
  total_requests: u64,
  /// Count of successful request forwards.
  successful_requests: u64,
  /// Count of failed request forwards.
  failed_requests: u64,
  /// Total bytes of payloads transferred through the server.
  total_bytes_transferred: u64,
}

/// Details of an active tunnel client connection.
#[derive(Serialize, Clone)]
struct ClientDetail {
  /// Unique client UUID.
  id: String,
  /// Remote socket IP address of the client connection.
  ip: String,
  /// Number of seconds elapsed since connection establishment.
  connected_for_seconds: u64,
  /// Total request count processed by this client connection.
  request_count: u64,
}

/// Enhanced metrics stats combined with active client details.
#[derive(Serialize, Clone)]
struct EnhancedServerStats {
  /// Total incoming request count.
  total_requests: u64,
  /// Successful requests count.
  successful_requests: u64,
  /// Failed requests count.
  failed_requests: u64,
  /// Total bytes transferred.
  total_bytes_transferred: u64,
  /// Current count of connected tunnel clients.
  connected_clients_count: usize,
  /// Uptime in seconds.
  uptime_seconds: u64,
  /// Request count waiting in the reconnection buffer.
  pending_requests_count: usize,
  /// List of client connection details.
  active_clients: Vec<ClientDetail>,
}

/// Structure representing a logged HTTP transaction.
#[derive(Serialize, Clone)]
struct RequestLog {
  /// Request UUID.
  id: String,
  /// Timestamp formatted as string.
  timestamp: String,
  /// HTTP method (GET, POST, etc.).
  method: String,
  /// Request URI path.
  uri: String,
  /// Status code returned.
  status: Option<u16>,
  /// Duration of processing in milliseconds.
  duration_ms: u128,
  /// Reason string if request failed.
  error: Option<String>,
}

/// Handle tracking active WebSocket sender channel and metadata.
struct ClientHandle {
  /// Sender channel to push messages to the client.
  tx: mpsc::Sender<Message>,
  /// Instant when client connection was established.
  connected_at: Instant,
  /// Client remote IP address.
  client_ip: String,
  /// Total request count processed by this specific client connection.
  request_count: Arc<AtomicU64>,
}

/// Standard response payload returned by tunnel client.
struct TunnelResponse {
  /// HTTP status code.
  status: u16,
  /// Map of response headers.
  headers: HashMap<String, String>,
  /// Base64 encoded payload body.
  body: Option<String>,
}

/// Structure tracking requests waiting for client execution.
struct PendingRequest {
  /// Oneshot channel sender to return client response to proxy handler thread.
  tx: oneshot::Sender<TunnelResponse>,
  /// Target client UUID.
  client_id: String,
}

/// Bucket tracking current tokens and refill state for rate limiting.
struct RateLimitState {
  /// Current token balance.
  tokens: f64,
  /// Last instant when tokens were updated.
  last_updated: Instant,
}

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
struct AppState {
  clients: Mutex<HashMap<String, ClientHandle>>,
  client_connected: watch::Sender<bool>,
  last_disconnect_time: Mutex<Option<Instant>>,
  server_start_time: Instant,
  pending_requests: Mutex<HashMap<String, PendingRequest>>,
  stats: Mutex<ServerStats>,
  recent_logs: Mutex<VecDeque<RequestLog>>,
  config: ServerConfig,
  concurrency_semaphore: Semaphore,
  request_counter: AtomicU64,
  rate_limiter: Mutex<HashMap<IpAddr, RateLimitState>>,
}

impl AppState {
  /// In-memory thread-safe Per-IP Token Bucket Rate Limiter.
  /// Returns `true` if request is allowed, `false` if rate-limited.
  async fn check_rate_limit(&self, ip: IpAddr) -> bool {
    let mut limit_map = self.rate_limiter.lock().await;
    let now = Instant::now();

    // Inline garbage collection to avoid memory leak if client IPs grow indefinitely
    if limit_map.len() > 2000 {
      limit_map.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(3600));
    }

    let max_tokens = self.config.ip_limit_max;
    let refill_rate = self.config.ip_limit_refill;

    let state = limit_map.entry(ip).or_insert_with(|| RateLimitState {
      tokens: max_tokens,
      last_updated: now,
    });

    let elapsed = now.duration_since(state.last_updated).as_secs_f64();
    state.tokens = (state.tokens + elapsed * refill_rate).min(max_tokens);
    state.last_updated = now;

    if state.tokens >= 1.0 {
      state.tokens -= 1.0;
      true
    } else {
      false
    }
  }
}

#[tokio::main]
/// Entry point for the Aperio server.
/// Sets up logging, reads env config, registers paths/middleware, and binds the TCP listener.
async fn main() {
  // Initialize tracing with structured JSON output (pino.js style)
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

  info!("Starting Aperio Server...");

  // Enforce APERIO_SERVER_TOKEN environment variable
  let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_else(|_| {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN environment variable must be set!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN cannot be empty!");
    std::process::exit(1);
  }

  let gateway_timeout_secs = std::env::var("APERIO_SERVER_GATEWAY_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(10);

  let gateway_response_timeout_secs = std::env::var("APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(30);

  // Limit on max request body size (default: 10MB)
  let max_body_size = std::env::var("APERIO_MAX_BODY_SIZE")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10 * 1024 * 1024);

  // Concurrency limit on tunnel requests (default: 100 concurrent)
  let max_concurrent_requests = std::env::var("APERIO_MAX_CONCURRENT_REQUESTS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(100);

  // Max connected tunnel clients limit (default: 10 active clients)
  let max_tunnels = std::env::var("APERIO_MAX_TUNNELS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10);

  // Max IP token bucket capacity burst (default: 100 requests)
  let ip_limit_max = std::env::var("APERIO_IP_LIMIT_MAX")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .unwrap_or(100.0);

  // IP token bucket refill rate per second (default: 5.0 requests/sec, which is 300 req/min)
  let ip_limit_refill = std::env::var("APERIO_IP_LIMIT_REFILL")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .unwrap_or(5.0);

  // Optional Basic Auth credentials for proxied requests ("username:password")
  let basic_auth = std::env::var("APERIO_SERVER_BASIC_AUTH").ok();

  let config = ServerConfig {
    token: token.clone(),
    gateway_timeout: Duration::from_secs(gateway_timeout_secs),
    gateway_response_timeout: Duration::from_secs(gateway_response_timeout_secs),
    max_body_size,
    max_tunnels,
    ip_limit_max,
    ip_limit_refill,
    basic_auth,
  };

  let (client_connected_tx, _) = watch::channel(false);

  let state = Arc::new(AppState {
    clients: Mutex::new(HashMap::new()),
    client_connected: client_connected_tx,
    last_disconnect_time: Mutex::new(None),
    server_start_time: Instant::now(),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 0,
      successful_requests: 0,
      failed_requests: 0,
      total_bytes_transferred: 0,
    }),
    recent_logs: Mutex::new(VecDeque::with_capacity(100)),
    config,
    concurrency_semaphore: Semaphore::new(max_concurrent_requests),
    request_counter: AtomicU64::new(0),
    rate_limiter: Mutex::new(HashMap::new()),
  });

  let mut app = Router::new().fallback(any(proxy_handler));

  // Enable dashboard endpoints conditionally based on environment variable
  let dashboard_enabled = std::env::var("APERIO_DASHBOARD")
    .map(|val| val == "1" || val.to_lowercase() == "true")
    .unwrap_or(false);

  if dashboard_enabled {
    let mut dash_router = Router::new()
      .route("/", get(dashboard_handler))
      .route("/api/stats", get(stats_handler))
      .route("/api/logs", get(logs_handler))
      .route("/health", get(health_handler));

    let token_arc = Arc::new(token);
    dash_router = dash_router.layer(axum::middleware::from_fn(
      move |req: axum::extract::Request, next: axum::middleware::Next| {
        let token = token_arc.clone();
        async move {
          let headers: &HeaderMap = req.headers();
          let mut authenticated = false;
          if let Some(auth_header) = headers.get("authorization")
            && let Ok(auth_str) = auth_header.to_str()
            && let Some(stripped) = auth_str.strip_prefix("Basic ")
          {
            use base64::prelude::*;
            if let Ok(decoded) = BASE64_STANDARD.decode(stripped)
              && let Ok(decoded_str) = String::from_utf8(decoded)
              && let Some((user, pass)) = decoded_str.split_once(':')
              && user == "aperio"
              && pass == *token
            {
              authenticated = true;
            }
          }

          if authenticated {
            next.run(req).await
          } else {
            Response::builder()
              .status(StatusCode::UNAUTHORIZED)
              .header("WWW-Authenticate", "Basic realm=\"Aperio Dashboard\"")
              .body(Body::from(
                "401 Unauthorized - Basic authentication required",
              ))
              .unwrap()
          }
        }
      },
    ));

    app = app.nest("/aperio", dash_router);
  }

  app = app.route("/aperio/ws", get(ws_handler));
  let app = app.with_state(state);

  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

  let port = std::env::var("PORT")
    .ok()
    .and_then(|p| p.parse::<u16>().ok())
    .unwrap_or(8080);

  let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
    .await
    .unwrap();

  info!(
    "Server listening on {}:{} with connection info tracing enabled",
    host, port
  );

  axum::serve(
    listener,
    app.into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal())
  .await
  .unwrap();
}

/// Graceful shutdown listener for receiving SIGINT or SIGTERM signals.
async fn shutdown_signal() {
  let ctrl_c = async {
    tokio::signal::ctrl_c()
      .await
      .expect("Failed to install Ctrl+C handler");
  };

  #[cfg(unix)]
  let terminate = async {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      .expect("Failed to install SIGTERM handler")
      .recv()
      .await;
  };

  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
      _ = ctrl_c => {},
      _ = terminate => {},
  }

  info!("Shutdown signal received, closing Aperio Server connections...");
}

/// Handler serving the embedded HTML dashboard dashboard.
async fn dashboard_handler() -> Html<&'static str> {
  Html(include_str!("dashboard.html"))
}

/// Handler returning live statistics and active connections detail in JSON.
async fn stats_handler(State(state): State<Arc<AppState>>) -> Json<EnhancedServerStats> {
  let raw_stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;

  let active_clients = clients
    .iter()
    .map(|(id, handle)| ClientDetail {
      id: id.clone(),
      ip: handle.client_ip.clone(),
      connected_for_seconds: handle.connected_at.elapsed().as_secs(),
      request_count: handle.request_count.load(Ordering::SeqCst),
    })
    .collect();

  let pending_count = state.pending_requests.lock().await.len();

  Json(EnhancedServerStats {
    total_requests: raw_stats.total_requests,
    successful_requests: raw_stats.successful_requests,
    failed_requests: raw_stats.failed_requests,
    total_bytes_transferred: raw_stats.total_bytes_transferred,
    connected_clients_count: clients.len(),
    uptime_seconds: state.server_start_time.elapsed().as_secs(),
    pending_requests_count: pending_count,
    active_clients,
  })
}

/// Handler returning the list of recent HTTP logs in JSON.
async fn logs_handler(State(state): State<Arc<AppState>>) -> Json<Vec<RequestLog>> {
  let logs = state.recent_logs.lock().await;
  Json(logs.iter().cloned().collect())
}

/// Health check endpoint returning status, active connection counts, and uptime.
async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
  let clients_count = state.clients.lock().await.len();
  let stats = state.stats.lock().await;
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut health_info = HashMap::new();
  health_info.insert("status", serde_json::json!("healthy"));
  health_info.insert("connected_clients", serde_json::json!(clients_count));
  health_info.insert("uptime_seconds", serde_json::json!(uptime));
  health_info.insert("total_requests", serde_json::json!(stats.total_requests));

  (StatusCode::OK, Json(health_info))
}

/// Helper function to extract Bearer token or `x-auth-token` from header values
/// and verify if it matches the configured server security token.
fn extract_and_verify_token(headers: &HeaderMap, server_token: &str) -> bool {
  let mut token_opt = None;
  if let Some(auth_header) = headers.get("authorization")
    && let Ok(auth_str) = auth_header.to_str()
    && let Some(stripped) = auth_str.strip_prefix("Bearer ")
  {
    token_opt = Some(stripped.to_string());
  }
  if token_opt.is_none()
    && let Some(x_token) = headers.get("x-auth-token")
    && let Ok(x_token_str) = x_token.to_str()
  {
    token_opt = Some(x_token_str.to_string());
  }

  match token_opt {
    Some(tok) => tok == server_token,
    None => false,
  }
}

/// Upgrade WebSocket endpoint. Extracts and verifies security tokens.
async fn ws_handler(
  ws: WebSocketUpgrade,
  headers: HeaderMap,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  State(state): State<Arc<AppState>>,
) -> Response {
  let authenticated = extract_and_verify_token(&headers, &state.config.token);

  if !authenticated {
    info!("Unauthorized connection attempt blocked.");
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  }

  // Validate maximum active tunnels limit (protects against file descriptor exhaustion)
  {
    let current_clients = state.clients.lock().await.len();
    if current_clients >= state.config.max_tunnels {
      warn!(
        "WebSocket upgrade connection rejected from {}: Maximum tunnels count reached ({}/{})",
        addr, current_clients, state.config.max_tunnels
      );
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable - Maximum active tunnels limit reached",
      )
        .into_response();
    }
  }

  ws.on_upgrade(move |socket| handle_socket(socket, addr.to_string(), state))
}

/// WebSocket processing logic. Listens for client frame inputs (Responses/Pings).
async fn handle_socket(socket: WebSocket, client_ip: String, state: Arc<AppState>) {
  let (mut ws_sender, mut ws_receiver) = socket.split();
  let client_id = uuid::Uuid::new_v4().to_string();

  // Create channel to handle writes asynchronously
  let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);

  // Spawn a writer task for this connection
  let writer_client_id = client_id.clone();
  let writer_task = tokio::spawn(async move {
    while let Some(msg) = rx_write.recv().await {
      if let Err(e) = ws_sender.send(msg).await {
        error!(
          "Error writing to websocket client {}: {:?}",
          writer_client_id, e
        );
        break;
      }
    }
  });

  info!("Tunnel client connected: {} (IP: {})", client_id, client_ip);

  let client_req_count = Arc::new(AtomicU64::new(0));

  // Register active client
  {
    let mut clients = state.clients.lock().await;
    clients.insert(
      client_id.clone(),
      ClientHandle {
        tx: tx_write.clone(),
        connected_at: Instant::now(),
        client_ip: client_ip.clone(),
        request_count: client_req_count.clone(),
      },
    );
    state.client_connected.send_replace(true);
  }

  // Read loop
  while let Some(result) = ws_receiver.next().await {
    match result {
      Ok(msg) => {
        if let Message::Text(text) = msg
          && let Ok(tunnel_msg) = serde_json::from_str::<TunnelMessage>(&text)
        {
          match tunnel_msg {
            TunnelMessage::Response {
              id,
              status,
              headers,
              body,
            } => {
              let mut pending = state.pending_requests.lock().await;
              if let Some(req) = pending.remove(&id)
                && req
                  .tx
                  .send(TunnelResponse {
                    status,
                    headers,
                    body,
                  })
                  .is_err()
              {
                warn!(
                  "Pending request oneshot receiver was dropped for request ID: {}",
                  id
                );
              }
            }
            TunnelMessage::Ping {
              client_id: cid,
              timestamp,
            } => {
              debug!("Heartbeat from client {}: {}", cid, timestamp);
              let pong = TunnelMessage::Pong { timestamp };
              if let Ok(pong_str) = serde_json::to_string(&pong) {
                let _ = tx_write.send(Message::Text(pong_str)).await;
              }
            }
            _ => {}
          }
        }
      }
      Err(e) => {
        error!("WebSocket reading error for client {}: {:?}", client_id, e);
        break;
      }
    }
  }

  // Client cleanup
  writer_task.abort();
  info!("Tunnel client disconnected: {}", client_id);
  {
    let mut clients = state.clients.lock().await;
    clients.remove(&client_id);

    if clients.is_empty() {
      state.client_connected.send_replace(false);
      let mut last_disc = state.last_disconnect_time.lock().await;
      *last_disc = Some(Instant::now());
    }
  }

  // Instantly abort pending requests that were routed to the disconnected client
  {
    let mut pending = state.pending_requests.lock().await;
    let keys_to_remove: Vec<String> = pending
      .iter()
      .filter(|(_, req)| req.client_id == client_id)
      .map(|(k, _)| k.clone())
      .collect();

    for k in keys_to_remove {
      if let Some(_req) = pending.remove(&k) {
        // Drop the sender channel, triggering an immediate channel cancellation / 502 Bad Gateway
        debug!(
          "Aborted pending request ID {} due to active client connection loss",
          k
        );
        // The oneshot channel dropping will wake the handler thread to reply immediately.
      }
    }
  }
}

/// Proxy handler for forwarding all incoming HTTP requests to active client.
/// Validates rate-limits, handles connection buffering and timeout limits, load-balances requests,
/// and maps response formats.
async fn proxy_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  method: Method,
  uri: Uri,
  headers: HeaderMap,
  body: Body,
) -> Response {
  let start_time = Instant::now();
  let method_str = method.to_string();
  let uri_str = uri.to_string();
  let caller_ip = addr.ip();

  // 1. Per-IP Rate Limiting (Token Bucket)
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("Rate Limit Exceeded for IP {}", caller_ip)),
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - IP rate limit exceeded",
    )
      .into_response();
  }

  // 2. Basic Auth check (if configured)
  if let Some(ref credentials) = state.config.basic_auth {
    let mut authenticated = false;
    if let Some(auth_header) = headers.get("authorization")
      && let Ok(auth_str) = auth_header.to_str()
      && let Some(stripped) = auth_str.strip_prefix("Basic ")
    {
      use base64::prelude::*;
      if let Ok(decoded) = BASE64_STANDARD.decode(stripped)
        && let Ok(decoded_str) = String::from_utf8(decoded)
      {
        authenticated = decoded_str == *credentials;

        if !authenticated {
          // Also allow auth using the token directly as password (e.g. for the dashboard)
          authenticated = decoded_str == format!("aperio:{}", state.config.token);
        }
      }
    }

    if !authenticated {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        401,
        start_time.elapsed(),
        Some("Basic auth required"),
      )
      .await;
      return Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Basic realm=\"Aperio\"")
        .body(Body::from("401 Unauthorized"))
        .unwrap();
    }
  }

  // 3. Wait for connection if client is disconnected
  let is_connected = *state.client_connected.borrow();
  if !is_connected {
    let last_disc = *state.last_disconnect_time.lock().await;
    let uptime = state.server_start_time.elapsed();

    let should_timeout = match last_disc {
      Some(disc_time) => disc_time.elapsed() > Duration::from_secs(60),
      None => uptime > Duration::from_secs(60),
    };

    if should_timeout {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("No active tunnel client (offline > 1m)"),
      )
      .await;
      return (
        StatusCode::GATEWAY_TIMEOUT,
        "504 Gateway Timeout - Tunnel client is offline for more than 1 minute",
      )
        .into_response();
    }

    // Wait for client to reconnect
    let mut rx = state.client_connected.subscribe();
    let timeout_fut = tokio::time::sleep(state.config.gateway_timeout);
    tokio::pin!(timeout_fut);

    let mut reconnected = false;
    loop {
      tokio::select! {
          _ = &mut timeout_fut => {
              break;
          }
          res = rx.changed() => {
              if res.is_ok() && *rx.borrow() {
                  reconnected = true;
                  break;
              }
          }
      }
    }

    if !reconnected {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("Gateway Timeout - Reconnect wait expired"),
      )
      .await;
      return (
        StatusCode::GATEWAY_TIMEOUT,
        "504 Gateway Timeout - No client connected in time",
      )
        .into_response();
    }
  }

  // 3. Limit concurrency to prevent resource starvation / DoS
  let _permit = match state.concurrency_semaphore.try_acquire() {
    Ok(p) => p,
    Err(_) => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        429,
        start_time.elapsed(),
        Some("Concurrency limit exceeded"),
      )
      .await;
      return (
        StatusCode::TOO_MANY_REQUESTS,
        "429 Too Many Requests - Concurrency limit reached on tunnel server",
      )
        .into_response();
    }
  };

  // 4. Get an active client using Round-Robin load-balancing
  let client_info = {
    let clients = state.clients.lock().await;
    let keys: Vec<String> = clients.keys().cloned().collect();
    if keys.is_empty() {
      None
    } else {
      let idx = state.request_counter.fetch_add(1, Ordering::SeqCst) as usize % keys.len();
      let chosen_id = &keys[idx];
      clients
        .get(chosen_id)
        .map(|c| (chosen_id.clone(), c.tx.clone(), c.request_count.clone()))
    }
  };

  let (chosen_client_id, client_tx, client_req_counter) = match client_info {
    Some(info) => info,
    None => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("No active client connection available"),
      )
      .await;
      return (
        StatusCode::GATEWAY_TIMEOUT,
        "504 Gateway Timeout - Client disconnected before request dispatch",
      )
        .into_response();
    }
  };

  // Increment request stats for client
  client_req_counter.fetch_add(1, Ordering::SeqCst);

  // 5. Read body with limit to prevent OOM / DoS
  let body_bytes = match axum::body::to_bytes(body, state.config.max_body_size).await {
    Ok(bytes) => bytes,
    Err(e) => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        413,
        start_time.elapsed(),
        Some(&format!("Payload too large or read failure: {}", e)),
      )
      .await;
      return (
        StatusCode::PAYLOAD_TOO_LARGE,
        "413 Payload Too Large - Request body size exceeds limit",
      )
        .into_response();
    }
  };

  let base64_body = if body_bytes.is_empty() {
    None
  } else {
    use base64::prelude::*;
    Some(BASE64_STANDARD.encode(&body_bytes))
  };

  // Map headers
  let mut serialized_headers = HashMap::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      serialized_headers.insert(k.to_string(), val_str.to_string());
    }
  }

  let request_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();

  // Insert oneshot receiver to await response mapping
  {
    let mut pending = state.pending_requests.lock().await;
    pending.insert(
      request_id.clone(),
      PendingRequest {
        tx: tx_response,
        client_id: chosen_client_id,
      },
    );
  }

  let tunnel_req = TunnelMessage::Request {
    id: request_id.clone(),
    method: method_str.clone(),
    uri: uri_str.clone(),
    headers: serialized_headers,
    body: base64_body,
  };

  let req_json = match serde_json::to_string(&tunnel_req) {
    Ok(json) => json,
    Err(e) => {
      state.pending_requests.lock().await.remove(&request_id);
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        500,
        start_time.elapsed(),
        Some(&format!("Request serialization failed: {}", e)),
      )
      .await;
      return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
    }
  };

  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_requests.lock().await.remove(&request_id);
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      502,
      start_time.elapsed(),
      Some("Failed to send tunnel frame to websocket client"),
    )
    .await;
    return (
      StatusCode::BAD_GATEWAY,
      "502 Bad Gateway - Client socket error",
    )
      .into_response();
  }

  // Update traffic metrics
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
    stats.total_bytes_transferred += body_bytes.len() as u64;
  }

  // 6. Await response from client with response timeout
  let timeout_fut = tokio::time::sleep(state.config.gateway_response_timeout);
  tokio::pin!(timeout_fut);

  tokio::select! {
      _ = &mut timeout_fut => {
          state.pending_requests.lock().await.remove(&request_id);
          log_request_failure(
              &state,
              &method_str,
              &uri_str,
              504,
              start_time.elapsed(),
              Some("Client response timeout expired"),
          )
          .await;
          (StatusCode::GATEWAY_TIMEOUT, "504 Gateway Timeout - Gateway response timeout expired").into_response()
      }
      res_opt = rx_response => {
          let duration = start_time.elapsed();
          match res_opt {
              Ok(tunnel_res) => {
                  let status_code = StatusCode::from_u16(tunnel_res.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

                  let res_bytes = if let Some(ref encoded_body) = tunnel_res.body {
                      use base64::prelude::*;
                      BASE64_STANDARD.decode(encoded_body).unwrap_or_default()
                  } else {
                      Vec::new()
                  };

                  let body_len = res_bytes.len() as u64;

                  let mut response_builder = Response::builder().status(status_code);

                  for (k, v) in tunnel_res.headers.iter() {
                      let k_lower = k.to_lowercase();
                      // Strip connection management headers
                      if k_lower == "connection" || k_lower == "keep-alive" || k_lower == "transfer-encoding" {
                          continue;
                      }
                      if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                          response_builder = response_builder.header(name, value);
                      }
                  }

                  {
                      let mut stats = state.stats.lock().await;
                      if status_code.is_success() {
                          stats.successful_requests += 1;
                      } else {
                          stats.failed_requests += 1;
                      }
                      stats.total_bytes_transferred += body_len;
                  }

                  log_request_success(&state, request_id, &method_str, &uri_str, tunnel_res.status, duration).await;

                  match response_builder.body(Body::from(res_bytes)) {
                      Ok(r) => r,
                      Err(e) => {
                          error!("Error constructing response: {:?}", e);
                          (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
                      }
                  }
              }
              Err(_) => {
                  log_request_failure(
                      &state,
                      &method_str,
                      &uri_str,
                      502,
                      duration,
                      Some("Communication channel with client closed abruptly"),
                  )
                  .await;
                  (StatusCode::BAD_GATEWAY, "502 Bad Gateway - Client connection lost in flight").into_response()
              }
          }
      }
  }
}

async fn log_request_success(
  state: &Arc<AppState>,
  id: String,
  method: &str,
  uri: &str,
  status: u16,
  duration: Duration,
) {
  let mut logs = state.recent_logs.lock().await;
  if logs.len() >= 100 {
    logs.pop_front();
  }
  let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
  logs.push_back(RequestLog {
    id: id.clone(),
    timestamp,
    method: method.to_string(),
    uri: uri.to_string(),
    status: Some(status),
    duration_ms: duration.as_millis(),
    error: None,
  });
  info!(
    "Proxy SUCCESS: ID={} Method={} URI={} Status={} Duration={}ms",
    id,
    method,
    uri,
    status,
    duration.as_millis()
  );
}

async fn log_request_failure(
  state: &Arc<AppState>,
  method: &str,
  uri: &str,
  status: u16,
  duration: Duration,
  error: Option<&str>,
) {
  let mut logs = state.recent_logs.lock().await;
  if logs.len() >= 100 {
    logs.pop_front();
  }
  let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
  let id = uuid::Uuid::new_v4().to_string();
  logs.push_back(RequestLog {
    id: id.clone(),
    timestamp,
    method: method.to_string(),
    uri: uri.to_string(),
    status: Some(status),
    duration_ms: duration.as_millis(),
    error: error.map(|s| s.to_string()),
  });
  warn!(
    "Proxy FAILURE: ID={} Method={} URI={} Status={} Duration={}ms Error={:?}",
    id,
    method,
    uri,
    status,
    duration.as_millis(),
    error
  );
}

#[cfg(test)]
mod tests {
  use super::*;
  use base64::Engine;
  use std::net::Ipv4Addr;

  #[test]
  fn test_token_authentication() {
    let mut headers = HeaderMap::new();
    assert!(!extract_and_verify_token(&headers, "secret"));

    headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
    assert!(extract_and_verify_token(&headers, "secret"));
    assert!(!extract_and_verify_token(&headers, "wrong_secret"));

    headers.clear();
    headers.insert("x-auth-token", HeaderValue::from_static("secret"));
    assert!(extract_and_verify_token(&headers, "secret"));
    assert!(!extract_and_verify_token(&headers, "wrong_secret"));
  }

  #[tokio::test]
  async fn test_rate_limiting() {
    let config = ServerConfig {
      token: "test".to_string(),
      gateway_timeout: Duration::from_secs(1),
      gateway_response_timeout: Duration::from_secs(1),
      max_body_size: 1024,
      max_tunnels: 1,
      ip_limit_max: 2.0,
      ip_limit_refill: 0.0, // No refill for testing strict burst limit
      basic_auth: None,
    };

    let (client_connected_tx, _) = watch::channel(false);
    let state = AppState {
      clients: Mutex::new(HashMap::new()),
      client_connected: client_connected_tx,
      last_disconnect_time: Mutex::new(None),
      server_start_time: Instant::now(),
      pending_requests: Mutex::new(HashMap::new()),
      stats: Mutex::new(ServerStats {
        total_requests: 0,
        successful_requests: 0,
        failed_requests: 0,
        total_bytes_transferred: 0,
      }),
      recent_logs: Mutex::new(VecDeque::new()),
      config,
      concurrency_semaphore: Semaphore::new(10),
      request_counter: AtomicU64::new(0),
      rate_limiter: Mutex::new(HashMap::new()),
    };

    let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

    // First request should pass
    assert!(state.check_rate_limit(ip).await);
    // Second request should pass
    assert!(state.check_rate_limit(ip).await);
    // Third request should be rate limited (max burst is 2.0)
    assert!(!state.check_rate_limit(ip).await);
  }

  #[tokio::test]
  async fn test_proxy_handler_gateway_timeout_offline() {
    let config = ServerConfig {
      token: "test".to_string(),
      gateway_timeout: Duration::from_millis(100),
      gateway_response_timeout: Duration::from_millis(100),
      max_body_size: 1024,
      max_tunnels: 1,
      ip_limit_max: 100.0,
      ip_limit_refill: 10.0,
      basic_auth: None,
    };

    let (client_connected_tx, _) = watch::channel(false);
    let state = Arc::new(AppState {
      clients: Mutex::new(HashMap::new()),
      client_connected: client_connected_tx,
      last_disconnect_time: Mutex::new(None),
      // Set start time to 2 minutes ago to trigger immediate timeout
      server_start_time: Instant::now() - Duration::from_secs(120),
      pending_requests: Mutex::new(HashMap::new()),
      stats: Mutex::new(ServerStats {
        total_requests: 0,
        successful_requests: 0,
        failed_requests: 0,
        total_bytes_transferred: 0,
      }),
      recent_logs: Mutex::new(VecDeque::new()),
      config,
      concurrency_semaphore: Semaphore::new(10),
      request_counter: AtomicU64::new(0),
      rate_limiter: Mutex::new(HashMap::new()),
    });

    let response = proxy_handler(
      State(state),
      ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
      Method::GET,
      Uri::from_static("/test-path"),
      HeaderMap::new(),
      Body::empty(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
  }

  #[tokio::test]
  async fn test_proxy_handler_success() {
    let config = ServerConfig {
      token: "test".to_string(),
      gateway_timeout: Duration::from_millis(200),
      gateway_response_timeout: Duration::from_millis(500),
      max_body_size: 1024,
      max_tunnels: 2,
      ip_limit_max: 100.0,
      ip_limit_refill: 10.0,
      basic_auth: None,
    };

    let (client_connected_tx, _) = watch::channel(true);
    let state = Arc::new(AppState {
      clients: Mutex::new(HashMap::new()),
      client_connected: client_connected_tx,
      last_disconnect_time: Mutex::new(None),
      server_start_time: Instant::now(),
      pending_requests: Mutex::new(HashMap::new()),
      stats: Mutex::new(ServerStats {
        total_requests: 0,
        successful_requests: 0,
        failed_requests: 0,
        total_bytes_transferred: 0,
      }),
      recent_logs: Mutex::new(VecDeque::new()),
      config,
      concurrency_semaphore: Semaphore::new(10),
      request_counter: AtomicU64::new(0),
      rate_limiter: Mutex::new(HashMap::new()),
    });

    let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);
    let client_req_count = Arc::new(AtomicU64::new(0));

    state.clients.lock().await.insert(
      "mock-client-1".to_string(),
      ClientHandle {
        tx: tx_write,
        connected_at: Instant::now(),
        client_ip: "127.0.0.1".to_string(),
        request_count: client_req_count,
      },
    );

    let state_clone = state.clone();
    tokio::spawn(async move {
      if let Some(Message::Text(text)) = rx_write.recv().await
        && let Ok(TunnelMessage::Request { id, .. }) = serde_json::from_str::<TunnelMessage>(&text)
      {
        let mut pending = state_clone.pending_requests.lock().await;
        if let Some(req) = pending.remove(&id) {
          let mut headers = HashMap::new();
          headers.insert("content-type".to_string(), "application/json".to_string());
          let _ = req.tx.send(TunnelResponse {
            status: 200,
            headers,
            body: Some(base64::prelude::BASE64_STANDARD.encode(r#"{"status":"ok"}"#)),
          });
        }
      }
    });

    let response = proxy_handler(
      State(state),
      ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
      Method::GET,
      Uri::from_static("/test-path"),
      HeaderMap::new(),
      Body::empty(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
      response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap(),
      "application/json"
    );
  }
}
