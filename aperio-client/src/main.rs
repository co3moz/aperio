use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_tungstenite::{
  connect_async_with_config,
  tungstenite::{
    client::IntoClientRequest,
    http::HeaderValue,
    protocol::{Message, WebSocketConfig},
  },
};
use tracing::{debug, error, info, warn};

mod check;
mod config;
mod protocol;
mod proxy;
mod tcp;

use check::run_check;
use config::{
  CliMode, FileConfig, build_ws_url, load_file_config, load_home_config, parse_bandwidth,
  parse_cli, resolve_settings,
};
use protocol::{
  FRAME_REQUEST_CHUNK, PROTOCOL_VERSION, RequestBodyFeeder, TunnelMessage, compress_frame,
  decode_binary_frame, decompress_frame,
};
use proxy::http::{ForwardContext, ForwardRequest, handle_incoming_request};
use proxy::ws::{WsStreamHandle, handle_upgrade_request};
use tcp::{TcpStreamHandle, handle_tcp_open, run_tcp_bridge};

#[tokio::main]
/// Entry point for the Aperio client.
/// Loads configuration from environment variables, sets up logging, and initiates the reconnect loop.
async fn main() {
  // Parse CLI first so `--help` and argument errors never emit JSON logs.
  let cli = parse_cli();

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

  // Configuration layering: CLI > ./aperio.yaml > environment > ~/.aperio.yaml.
  let home_cfg = load_home_config();
  let file_cfg = load_file_config(cli.opts.config.as_deref());
  let settings = resolve_settings(&cli, &home_cfg, &file_cfg);

  // Diagnostics mode reports missing config instead of exiting on it.
  if let CliMode::Check = cli.mode {
    run_check(&settings).await;
  }

  let mut token = settings.token.unwrap_or_else(|| {
    error!("CRITICAL SECURITY ERROR: a tunnel token is required (--server-token, APERIO_SERVER_TOKEN, or yaml: server.token)!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: the tunnel token cannot be empty!");
    std::process::exit(1);
  }

  let mut server_addr = settings.server.unwrap_or_else(|| {
    error!(
      "CRITICAL ERROR: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)!"
    );
    std::process::exit(1);
  });

  // TCP bridge mode short-circuits the tunnel client entirely.
  if let CliMode::TcpBridge = cli.mode {
    let port = cli.local_port.unwrap_or(0);
    run_tcp_bridge(port, &server_addr, &token).await;
    return;
  }

  let mut target = settings.target.unwrap_or_else(|| {
    error!("CRITICAL ERROR: the target is required (positional argument, APERIO_TARGET, or yaml: target)!");
    std::process::exit(1);
  });
  let pass_hostname = settings.pass_hostname;

  let mut path_bind = settings.path;

  // Hostname this client wants to serve (e.g. "a.example.com"). The server
  // routes requests whose Host header matches this value to this client.
  let mut hostname_bind = settings.hostname;

  let trim_bind = if path_bind.is_some() {
    settings.trim_bind.unwrap_or(true)
  } else {
    false
  };

  // Maximum response body size (in bytes) accepted from the target backend.
  // Protects the client (and the tunnel) from OOM when a misbehaving backend
  // streams an unbounded response.
  let max_response_body_size = settings.max_response_body;

  // Per-request timeout (in seconds) for calls to the local target backend.
  let client_timeout_secs = settings.timeout_secs;

  // Maximum concurrent requests processed locally. Announced to the server so
  // it queues excess requests instead of flooding the backend. Also enforced
  // locally, since the client must not fully trust the server.
  let max_concurrent = settings.max_concurrent;

  let local_limiter: Option<Arc<Semaphore>> =
    max_concurrent.map(|n| Arc::new(Semaphore::new(n as usize)));

  // Load-balancing priority tier announced to the server: 0 = primary
  // (default), higher numbers are standbys that only receive traffic when
  // the server runs the primary-standby strategy and no lower tier is up.
  let mut priority = settings.priority;

  // Announced link capacity (bytes/second); the server paces its frames so
  // this client's network is never flooded (e.g. "8mbit" on a DSL uplink).
  let bandwidth_bps = settings.bandwidth.as_deref().and_then(|raw| {
    let parsed = parse_bandwidth(raw);
    if parsed.is_none() {
      warn!("Invalid bandwidth value '{}'; ignoring", raw);
    }
    parsed
  });

  // Backend redirects: same-host scheme upgrades (http → https) and hops
  // within the same root domain are followed transparently, up to this many
  // jumps. 0 = pass every redirect through to the visitor.
  let max_redirects = settings.max_redirects;

  // Cap on individual tunnel WebSocket messages accepted from the server.
  let max_message_size = settings.max_message_size;

  // Experimental raw TCP tunneling: when set (host:port), the server can open
  // TCP streams through this client to exactly this target. The client never
  // connects anywhere else, no matter what the server asks for.
  let tcp_target = settings.tcp_target;

  // Backend health probing: when a health endpoint is configured the client
  // probes it on its own schedule, independent of the server connection.
  let target_health = settings.target_health;
  let health_interval = settings.health_interval;
  let health_timeout = settings.health_timeout;
  let health_threshold = settings.health_threshold;

  let client_id = uuid::Uuid::new_v4().to_string();

  // Latest backend health verdict, reported to the server via heartbeats. An
  // unhealthy backend never tears the tunnel down: the server just takes
  // this client out of routing until the backend recovers.
  let backend_healthy = Arc::new(AtomicBool::new(true));
  if let Some(ref health_path) = target_health {
    let health_url = if health_path.starts_with("http://") || health_path.starts_with("https://") {
      health_path.clone()
    } else {
      format!(
        "{}/{}",
        target.trim_end_matches('/'),
        health_path.trim_start_matches('/')
      )
    };
    let flag = backend_healthy.clone();
    let probe_client = reqwest::Client::builder()
      .timeout(Duration::from_secs(health_timeout))
      .build()
      .unwrap_or_default();
    info!(
      "- Backend health check: {} (every {}s, timeout {}s, threshold {})",
      health_url, health_interval, health_timeout, health_threshold
    );
    tokio::spawn(async move {
      let mut consecutive_failures: u32 = 0;
      loop {
        tokio::time::sleep(Duration::from_secs(health_interval)).await;
        let ok = matches!(
          probe_client.get(&health_url).send().await,
          Ok(resp) if resp.status().is_success()
        );
        if ok {
          consecutive_failures = 0;
          if !flag.swap(true, Ordering::SeqCst) {
            info!("Backend health restored: {}", health_url);
          }
        } else {
          consecutive_failures = consecutive_failures.saturating_add(1);
          if consecutive_failures >= health_threshold && flag.swap(false, Ordering::SeqCst) {
            warn!(
              "Backend health check failed {} consecutive time(s): {} — reporting unhealthy to the server (tunnel stays connected)",
              consecutive_failures, health_url
            );
          }
        }
      }
    });
  }

  // Graceful shutdown state: a signal marks the client as draining, the
  // server is notified, and the process exits once in-flight work finishes.
  let shutting_down = Arc::new(AtomicBool::new(false));
  let inflight_requests = Arc::new(AtomicUsize::new(0));
  let shutdown_notify = Arc::new(tokio::sync::Notify::new());
  {
    let shutting_down = shutting_down.clone();
    let shutdown_notify = shutdown_notify.clone();
    tokio::spawn(async move {
      let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
      };
      #[cfg(unix)]
      let terminate = async {
        if let Ok(mut sig) =
          tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
          sig.recv().await;
        } else {
          std::future::pending::<()>().await;
        }
      };
      #[cfg(not(unix))]
      let terminate = std::future::pending::<()>();

      tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
      }
      info!("Shutdown signal received: draining before exit...");
      shutting_down.store(true, Ordering::SeqCst);
      shutdown_notify.notify_waiters();
    });
  }

  let mut ws_url = match build_ws_url(&server_addr) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to build WebSocket URL: {}", e);
      std::process::exit(1);
    }
  };

  // Config hot-reload: when the yaml config file changes on disk, the client
  // drops the current connection and reconnects with freshly resolved values
  // (token, server, target, binds, priority). CLI arguments and environment
  // variables keep their precedence over the file.
  let config_path = cli
    .opts
    .config
    .clone()
    .unwrap_or_else(|| "aperio.yaml".to_string());
  let config_dirty = Arc::new(AtomicBool::new(false));
  if std::path::Path::new(&config_path).exists() {
    let dirty = config_dirty.clone();
    let watch_path = config_path.clone();
    let mut last_mtime = std::fs::metadata(&watch_path)
      .ok()
      .and_then(|m| m.modified().ok());
    info!("- Watching {} for configuration changes", watch_path);
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mtime = std::fs::metadata(&watch_path)
          .ok()
          .and_then(|m| m.modified().ok());
        if mtime != last_mtime {
          last_mtime = mtime;
          info!(
            "Configuration file {} changed; reconnecting to apply it",
            watch_path
          );
          dirty.store(true, Ordering::SeqCst);
        }
      }
    });
  }

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
  if priority > 0 {
    info!("- Load Balancing Priority: {} (standby tier)", priority);
  }
  if let Some(bw) = bandwidth_bps {
    info!("- Announced Bandwidth: {} bytes/s", bw);
  }
  if let Some(ref t) = tcp_target {
    info!("- TCP Target: {}", t);
  }
  info!("- WebSocket URL: {}", ws_url);

  // Reconnection Loop. Retries use exponential backoff with jitter so that a
  // fleet of clients does not stampede the server after a restart; the
  // counter resets once a connection proves stable.
  let mut reconnect_attempt: u32 = 0;
  loop {
    // Apply a pending config file change before (re)connecting. A file that
    // no longer parses keeps the previous configuration.
    if config_dirty.swap(false, Ordering::SeqCst) {
      let reloaded = std::fs::read_to_string(&config_path)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_yaml::from_str::<FileConfig>(&raw).map_err(|e| e.to_string()));
      match reloaded {
        Ok(file_cfg) => {
          // Re-run the full layering with the fresh files; CLI arguments and
          // environment variables keep their precedence.
          let s = resolve_settings(&cli, &load_home_config(), &file_cfg);
          if let Some(t) = s.token {
            token = t;
          }
          if let Some(srv) = s.server {
            match build_ws_url(&srv) {
              Ok(url) => {
                server_addr = srv;
                ws_url = url;
              }
              Err(e) => warn!(
                "Reloaded server URL {} is invalid ({}); keeping previous",
                srv, e
              ),
            }
          }
          if let Some(t) = s.target {
            target = t;
          }
          path_bind = s.path;
          hostname_bind = s.hostname;
          priority = s.priority;
          reconnect_attempt = 0;
          info!(
            "Configuration reloaded from {} (target: {}, hostname bind: {:?}, path bind: {:?})",
            config_path, target, hostname_bind, path_bind
          );
        }
        Err(e) => warn!(
          "Config reload from {} failed ({}); keeping previous configuration",
          config_path, e
        ),
      }
    }

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
            let connected_at = Instant::now();
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

            // Active raw TCP tunnel streams: stream_id → handle
            let active_tcp_streams: Arc<Mutex<HashMap<String, TcpStreamHandle>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Outgoing compression is enabled after the server's offer is Acked.
            let compress_out = Arc::new(AtomicBool::new(false));

            // Spawn task to handle WebSocket writes
            let compress_out_writer = compress_out.clone();
            let writer_task = tokio::spawn(async move {
              while let Some(msg) = rx_write.recv().await {
                let msg = match msg {
                  Message::Text(t) if compress_out_writer.load(Ordering::SeqCst) => {
                    Message::Binary(compress_frame(&t))
                  }
                  other => other,
                };
                if let Err(e) = ws_sender.send(msg).await {
                  error!("Error writing to server socket: {:?}", e);
                  break;
                }
              }
            });

            // Spawn task for heartbeat (Ping every 5 seconds & liveness check)
            let tx_ping = tx_write.clone();
            let tcp_enabled_ping = tcp_target.is_some();
            let client_id_ping = client_id.clone();
            let path_bind_ping = path_bind.clone();
            let hostname_bind_ping = hostname_bind.clone();
            let last_pong_time_ping = last_pong_time.clone();
            let abort_tx_ping = abort_tx.clone();
            let backend_healthy_ping = backend_healthy.clone();
            let config_dirty_ping = config_dirty.clone();

            let ping_task = tokio::spawn(async move {
              // The first Ping goes out immediately: it announces the binds,
              // version/protocol, and health before any traffic is routed.
              loop {
                // A pending config change drops the connection so the
                // reconnect loop can re-resolve and apply it.
                if config_dirty_ping.load(Ordering::SeqCst) {
                  info!("Dropping connection to apply the configuration change...");
                  let _ = abort_tx_ping.send(()).await;
                  break;
                }

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
                  tcp: tcp_enabled_ping,
                  version: Some(env!("CARGO_PKG_VERSION").to_string()),
                  protocol: Some(PROTOCOL_VERSION),
                  backend_healthy: backend_healthy_ping.load(Ordering::SeqCst),
                  priority,
                  bandwidth_bps,
                };
                if let Ok(ping_str) = serde_json::to_string(&ping_msg)
                  && tx_ping.send(Message::Text(ping_str)).await.is_err()
                {
                  break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
              }
            });

            // Reqwest Client to make local forwarding requests. Same-site
            // backend redirects (http→https, same root domain) are followed
            // transparently; everything else passes through to the visitor.
            let reqwest_client = reqwest::Client::builder()
              .redirect(proxy::http::redirect_policy(max_redirects))
              .timeout(Duration::from_secs(client_timeout_secs))
              .build()
              .unwrap_or_default();

            // Per-connection forwarding constants shared by all request tasks.
            let forward_ctx = Arc::new(ForwardContext {
              client: reqwest_client.clone(),
              target: target.clone(),
              pass_hostname,
              path_bind: path_bind.clone(),
              trim_bind,
              max_response_body_size,
              tunnel_tx: tx_write.clone(),
            });

            // Protocol version the server announced via Pong; v2 enables
            // binary chunk frames and streamed request bodies.
            let server_protocol = Arc::new(std::sync::atomic::AtomicU32::new(1));

            // Streamed request bodies in flight: request id → chunk feeder.
            let active_request_streams: Arc<Mutex<HashMap<String, RequestBodyFeeder>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Read messages from Server
            let mut version_skew_warned = false;
            loop {
              tokio::select! {
                  _ = abort_rx.recv() => {
                      warn!("Liveness timeout triggered. Aborting socket loop.");
                      break;
                  }
                  _ = shutdown_notify.notified() => {
                      // Announce drain, let in-flight requests finish, then exit.
                      if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                          let _ = tx_write.send(Message::Text(json)).await;
                      }
                      let drain_deadline = Instant::now() + Duration::from_secs(30);
                      loop {
                          let inflight = inflight_requests.load(Ordering::SeqCst);
                          if inflight == 0 {
                              info!("Drain complete; exiting.");
                              break;
                          }
                          if Instant::now() >= drain_deadline {
                              warn!("Drain timeout with {} request(s) still in flight; exiting anyway.", inflight);
                              break;
                          }
                          info!("Draining: {} request(s) in flight...", inflight);
                          tokio::time::sleep(Duration::from_millis(500)).await;
                      }
                      // Give the Draining frame a moment to flush before closing.
                      tokio::time::sleep(Duration::from_millis(200)).await;
                      std::process::exit(0);
                  }
                  msg_res = ws_receiver.next() => {
                      match msg_res {
                          Some(Ok(msg)) => {
                              let text_opt = match msg {
                                  Message::Text(t) => Some(t),
                                  Message::Binary(b) => {
                                      // v2 binary chunk frames carry a tag byte that never
                                      // collides with zlib streams (0x78).
                                      if let Some((FRAME_REQUEST_CHUNK, fid, payload)) = decode_binary_frame(&b) {
                                          let streams = active_request_streams.lock().await;
                                          if let Some(feeder) = streams.get(fid) {
                                              let _ = feeder.send(Ok(payload.to_vec())).await;
                                          }
                                          None
                                      } else {
                                          decompress_frame(&b, max_message_size.saturating_mul(4))
                                      }
                                  }
                                  _ => None,
                              };
                              if let Some(text) = text_opt
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
                                              let ctx = forward_ctx.clone();
                                              let limiter = local_limiter.clone();
                                              let inflight = inflight_requests.clone();
                                              let proto = server_protocol.clone();
                                              inflight.fetch_add(1, Ordering::SeqCst);

                                              // Handle incoming request concurrently
                                              tokio::spawn(async move {
                                                  // Local concurrency guard: even a misbehaving server
                                                  // cannot push more parallel work onto the backend.
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
                                                  let binary = proto.load(Ordering::Relaxed) >= 2;
                                                  let response = handle_incoming_request(
                                                      &ctx,
                                                      ForwardRequest { id, method, uri, headers, body },
                                                      None,
                                                      binary,
                                                  )
                                                  .await;

                                                  // None = the response was streamed through the tunnel already.
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = ctx.tunnel_tx.send(Message::Text(resp_str)).await;
                                                  }
                                                  inflight.fetch_sub(1, Ordering::SeqCst);
                                              });
                                          }
                                          TunnelMessage::RequestStart {
                                              id,
                                              method,
                                              uri,
                                              headers,
                                          } => {
                                              // Streamed request body (protocol v2): the backend
                                              // request starts immediately and is fed chunk-by-chunk
                                              // as RequestChunk frames arrive.
                                              let (body_tx, body_rx) =
                                                  mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
                                              active_request_streams.lock().await.insert(id.clone(), body_tx);
                                              let ctx = forward_ctx.clone();
                                              let limiter = local_limiter.clone();
                                              let inflight = inflight_requests.clone();
                                              let streams = active_request_streams.clone();
                                              let proto = server_protocol.clone();
                                              inflight.fetch_add(1, Ordering::SeqCst);
                                              tokio::spawn(async move {
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
                                                  let binary = proto.load(Ordering::Relaxed) >= 2;
                                                  let response = handle_incoming_request(
                                                      &ctx,
                                                      ForwardRequest {
                                                          id: id.clone(),
                                                          method,
                                                          uri,
                                                          headers,
                                                          body: None,
                                                      },
                                                      Some(body_rx),
                                                      binary,
                                                  )
                                                  .await;
                                                  streams.lock().await.remove(&id);
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = ctx.tunnel_tx.send(Message::Text(resp_str)).await;
                                                  }
                                                  inflight.fetch_sub(1, Ordering::SeqCst);
                                              });
                                          }
                                          TunnelMessage::RequestChunk { id, data } => {
                                              // Base64 fallback path; v2 servers send binary frames.
                                              let streams = active_request_streams.lock().await;
                                              if let Some(feeder) = streams.get(&id) {
                                                  match BASE64_STANDARD.decode(&data) {
                                                      Ok(bytes) => {
                                                          let _ = feeder.send(Ok(bytes)).await;
                                                      }
                                                      Err(_) => warn!(
                                                          "Failed to decode Base64 RequestChunk for {}",
                                                          id
                                                      ),
                                                  }
                                              }
                                          }
                                          TunnelMessage::RequestEnd { id } => {
                                              // Dropping the feeder ends the streamed body.
                                              active_request_streams.lock().await.remove(&id);
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
                                          TunnelMessage::TcpOpen { stream_id } => {
                                              match tcp_target.clone() {
                                                  Some(target_addr) => {
                                                      let tx = tx_write.clone();
                                                      let streams = active_tcp_streams.clone();
                                                      tokio::spawn(async move {
                                                          handle_tcp_open(stream_id, target_addr, tx, streams).await;
                                                      });
                                                  }
                                                  None => {
                                                      warn!("TcpOpen received but APERIO_CLIENT_TCP_TARGET is not set; refusing");
                                                      let close = TunnelMessage::TcpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json)).await;
                                                      }
                                                  }
                                              }
                                          }
                                          TunnelMessage::TcpData { stream_id, data } => {
                                              let streams = active_tcp_streams.lock().await;
                                              if let Some(handle) = streams.get(&stream_id) {
                                                  match BASE64_STANDARD.decode(&data) {
                                                      Ok(bytes) => {
                                                          if handle.tx.send(bytes).await.is_err() {
                                                              debug!("TCP backend channel closed for stream {}", stream_id);
                                                          }
                                                      }
                                                      Err(_) => warn!("Failed to decode Base64 TcpData for stream {}", stream_id),
                                                  }
                                              }
                                          }
                                          TunnelMessage::TcpClose { stream_id } => {
                                              let mut streams = active_tcp_streams.lock().await;
                                              if let Some(handle) = streams.remove(&stream_id) {
                                                  let _ = handle.abort_tx.send(()).await;
                                                  debug!("Closed TCP stream {}", stream_id);
                                              }
                                          }
                                          TunnelMessage::CompressionStart {} => {
                                              info!("Server offered tunnel compression; enabling zlib frames");
                                              if let Ok(json) = serde_json::to_string(&TunnelMessage::CompressionAck {}) {
                                                  let _ = tx_write.send(Message::Text(json)).await;
                                              }
                                              compress_out.store(true, Ordering::SeqCst);
                                          }
                                          TunnelMessage::HostnameAssigned { hostname } => {
                                              info!("Server assigned hostname to this client: {}", hostname);
                                          }
                                          TunnelMessage::Pong { timestamp, version, protocol } => {
                                              debug!("Pong received: {}", timestamp);
                                              if let Some(p) = protocol {
                                                  server_protocol.store(p, Ordering::Relaxed);
                                              }
                                              // Log version skew once per connection, not per heartbeat.
                                              if !version_skew_warned
                                                && let Some(p) = protocol
                                                && p != PROTOCOL_VERSION
                                              {
                                                  version_skew_warned = true;
                                                  warn!(
                                                      "Server speaks tunnel protocol v{} (server version {}) but this client speaks v{}; update the older side",
                                                      p,
                                                      version.as_deref().unwrap_or("unknown"),
                                                      PROTOCOL_VERSION
                                                  );
                                              }
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

            // A connection that survived for a while counts as healthy:
            // start the next retry sequence from the base delay again.
            if connected_at.elapsed() >= Duration::from_secs(RECONNECT_STABLE_SECS) {
              reconnect_attempt = 0;
            }
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

    // A shutdown signal while disconnected exits immediately (nothing to drain).
    if shutting_down.load(Ordering::SeqCst) {
      info!("Shutdown requested while disconnected; exiting.");
      std::process::exit(0);
    }
    reconnect_attempt = reconnect_attempt.saturating_add(1);
    let delay = reconnect_delay(reconnect_attempt);
    info!(
      "Retrying connection in {:.1} seconds (attempt {})...",
      delay.as_secs_f64(),
      reconnect_attempt
    );
    tokio::time::sleep(delay).await;
  }
}

/// First retry delay of the reconnect backoff.
const RECONNECT_BASE_DELAY_MS: u64 = 1_000;
/// Upper bound for the reconnect backoff.
const RECONNECT_MAX_DELAY_MS: u64 = 60_000;
/// A connection lasting at least this long resets the backoff counter.
const RECONNECT_STABLE_SECS: u64 = 30;

/// Exponential reconnect backoff with jitter: the deterministic delay doubles
/// per attempt (1s, 2s, 4s, ... capped at 60s) and the returned value is
/// drawn from [cap/2, cap] so simultaneously disconnected clients spread out
/// instead of reconnecting in lockstep. The jitter is derived from the clock
/// to avoid pulling in a RNG dependency.
fn reconnect_delay(attempt: u32) -> Duration {
  let doublings = attempt.saturating_sub(1).min(6); // 2^6 * 1s covers the 60s cap
  let cap = (RECONNECT_BASE_DELAY_MS << doublings).min(RECONNECT_MAX_DELAY_MS);
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .subsec_nanos() as u64;
  let jitter = nanos % (cap / 2 + 1);
  Duration::from_millis(cap / 2 + jitter)
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
