//! One tunnel service: a single outbound tunnel connection exposing one
//! local target, with its own reconnect loop, heartbeat, backend health
//! probe and forwarding state. The supervisor in `main` spawns one task per
//! service and respawns them (with freshly resolved settings) when the
//! configuration file changes — which is how every setting, not just a
//! subset, takes effect on hot-reload.

use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio_tungstenite::{
  connect_async_with_config,
  tungstenite::{
    client::IntoClientRequest,
    http::HeaderValue,
    protocol::{Message, WebSocketConfig},
  },
};
use tracing::{debug, error, info, warn};

use crate::protocol::{
  FRAME_REQUEST_CHUNK, PROTOCOL_VERSION, RequestBodyFeeder, TunnelDecl, TunnelMessage,
  compress_frame, decode_binary_frame, decompress_frame,
};
use crate::proxy::http::{
  ForwardContext, ForwardRequest, HeaderTransform, handle_incoming_request,
};
use crate::proxy::ws::{WsStreamHandle, handle_upgrade_request};
use crate::tcp::{TcpStreamHandle, handle_tcp_open};
use crate::udp::{UdpStreamHandle, handle_udp_open};

/// Everything a service needs to run, fully resolved. Built by `main` from
/// the layered configuration; rebuilt (and the service respawned) on
/// config hot-reload.
#[derive(Clone, Debug)]
pub(crate) struct ServiceSpec {
  /// Display name from the `services:` list (None for the single default
  /// service).
  pub(crate) name: Option<String>,
  /// Stable instance id announced to the server. Kept across reconnects
  /// and config respawns so the server's failover `wait` mode keeps
  /// recognizing this client.
  pub(crate) client_id: String,
  pub(crate) token: String,
  pub(crate) server_addr: String,
  pub(crate) ws_url: String,
  pub(crate) target: String,
  /// Public hostname(s) claimed for this service (first is the primary).
  pub(crate) hostnames: Vec<String>,
  pub(crate) path: Option<String>,
  pub(crate) trim_bind: bool,
  pub(crate) pass_hostname: bool,
  pub(crate) max_response_body: usize,
  /// Largest request body, in bytes, visitors may upload to this service
  /// (announced via Ping; the server answers bigger uploads with an early
  /// 413 before they enter the tunnel; None = only the server's limit).
  pub(crate) max_request_body: Option<u64>,
  pub(crate) timeout_secs: u64,
  pub(crate) max_concurrent: Option<u32>,
  /// Parallel tunnel connections for this service (1–16). The supervisor
  /// spawns one service task per connection, each with a derived client id.
  pub(crate) connections: u32,
  pub(crate) priority: u32,
  pub(crate) bandwidth_bps: Option<u64>,
  pub(crate) max_message_size: usize,
  pub(crate) max_redirects: usize,
  pub(crate) tcp_target: Option<String>,
  pub(crate) target_health: Option<String>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection (no-op when `target_health` is set — that gates startup too).
  pub(crate) wait_for_backend: bool,
  pub(crate) health_interval: u64,
  pub(crate) health_timeout: u64,
  pub(crate) health_threshold: u32,
  /// Ask the server to skip its visitor auth gate for this service.
  pub(crate) public: bool,
  /// Per-service visitor login (`user:password`) the server should gate this
  /// service behind, overriding its own APERIO_SERVER_AUTH (None = no override).
  pub(crate) visitor_auth: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach this service (empty = everyone);
  /// announced via Ping and enforced by the server before dispatch.
  pub(crate) allowed_ips: Vec<String>,
  /// Tunnels declared by this client process (`tunnels:` list): normally
  /// unexposed local services a peer client may bind with `--bind-tunnels`.
  /// Announced via Ping on every connection of the process.
  pub(crate) tunnels: Vec<TunnelDecl>,
  /// Header add/remove rules for this service's proxied HTTP traffic
  /// (config `headers:`; None = pass through untouched).
  pub(crate) headers: Option<crate::config::HeaderRules>,
  /// Opt this service into the server-side response cache (announced via
  /// Ping; effective only when the server enables APERIO_CACHE).
  pub(crate) cache: bool,
  /// Ask the server to keep serving this service's cached responses while
  /// no healthy client is connected (announced via Ping; needs `cache`).
  pub(crate) resilience: bool,
}

impl ServiceSpec {
  /// Short label used to attribute log lines to this service.
  pub(crate) fn label(&self) -> String {
    self.name.clone().unwrap_or_else(|| {
      if self.target.is_empty() {
        "(tunnels only)".to_string()
      } else {
        self.target.clone()
      }
    })
  }
}

/// Process-wide state shared by every service task.
#[derive(Clone)]
pub(crate) struct Shared {
  /// Set once a shutdown signal arrived; services exit instead of
  /// reconnecting.
  pub(crate) shutting_down: Arc<AtomicBool>,
  /// Woken by the signal handler to start draining.
  pub(crate) shutdown_notify: Arc<tokio::sync::Notify>,
  /// In-flight proxied requests across all services (drain waits on it).
  pub(crate) inflight_requests: Arc<AtomicUsize>,
}

/// Runs one tunnel service until the process shuts down or `cancel` fires
/// (config reload → the supervisor respawns with a fresh spec).
pub(crate) async fn run_service(
  spec: ServiceSpec,
  shared: Shared,
  mut cancel: watch::Receiver<bool>,
) {
  let label = spec.label();

  // Latest backend health verdict, reported to the server via heartbeats. An
  // unhealthy backend never tears the tunnel down: the server just takes
  // this client out of routing until the backend recovers.
  //
  // Initial verdict: healthy only when no health check is configured (there
  // is nothing to prove). With a `target_health` check the backend starts
  // *unhealthy* and stays out of routing until the first probe succeeds, so a
  // client never claims a backend is up before it has actually checked it.
  let backend_healthy = Arc::new(AtomicBool::new(spec.target_health.is_none()));
  // False until the first probe of a configured health check completes, so
  // the dashboard can show "checking" instead of "backend down" before the
  // backend has actually been probed. Always true when no check is set.
  let backend_probed = Arc::new(AtomicBool::new(spec.target_health.is_none()));
  // Fired whenever the health verdict flips, so the heartbeat loop can push an
  // immediate Ping instead of waiting out its interval — the first successful
  // probe makes a healthy backend routable within a probe, not a ping cycle.
  let health_changed = Arc::new(tokio::sync::Notify::new());
  let probe_task = spec.target_health.as_ref().map(|health_path| {
    let health_changed = health_changed.clone();
    let probed = backend_probed.clone();
    let health_url = if health_path.starts_with("http://") || health_path.starts_with("https://") {
      health_path.clone()
    } else {
      // Health probes speak plain HTTP(S) even against h2c/h2 targets:
      // reqwest negotiates the version; gRPC servers usually expose gRPC
      // health checking elsewhere, so an explicit URL is recommended there.
      let base = spec
        .target
        .replacen("h2c://", "http://", 1)
        .replacen("h2://", "https://", 1);
      format!(
        "{}/{}",
        base.trim_end_matches('/'),
        health_path.trim_start_matches('/')
      )
    };
    let flag = backend_healthy.clone();
    // Health checks never follow redirects: a 3xx to some other page must
    // not let a broken backend look healthy via the redirect target.
    let probe_client = reqwest::Client::builder()
      .timeout(Duration::from_secs(spec.health_timeout))
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .unwrap_or_default();
    let (interval, threshold) = (spec.health_interval, spec.health_threshold);
    info!(
      "[{}] Backend health check: {} (every {}s, timeout {}s, threshold {})",
      label, health_url, interval, spec.health_timeout, threshold
    );
    tokio::spawn(async move {
      let mut consecutive_failures: u32 = 0;
      let mut first_result = true;
      // Probe immediately, then on the interval: a backend that is already
      // down when the client starts is reported after threshold probes
      // instead of sitting falsely healthy for a full extra interval. The
      // client also starts out-of-routing (unhealthy) until this first probe
      // lands, so the very first success is what makes the backend routable.
      loop {
        let ok = matches!(
          probe_client.get(&health_url).send().await,
          Ok(resp) if resp.status().is_success()
        );
        if ok {
          consecutive_failures = 0;
          if !flag.swap(true, Ordering::SeqCst) {
            health_changed.notify_one();
            if first_result {
              info!("Backend healthy: {} — now routable", health_url);
            } else {
              info!("Backend health restored: {}", health_url);
            }
          }
        } else {
          consecutive_failures = consecutive_failures.saturating_add(1);
          if consecutive_failures >= threshold && flag.swap(false, Ordering::SeqCst) {
            health_changed.notify_one();
            warn!(
              "Backend health check failed {} consecutive time(s): {} — reporting unhealthy to the server (tunnel stays connected)",
              consecutive_failures, health_url
            );
          } else if first_result {
            // Started unhealthy and the first probe also failed: make it clear
            // why the backend is not yet routable (the threshold warning above
            // only fires on a healthy→unhealthy transition).
            info!(
              "Backend not healthy yet: {} — staying out of routing until a probe passes",
              health_url
            );
          }
        }
        if first_result {
          probed.store(true, Ordering::SeqCst);
          health_changed.notify_one();
        }
        first_result = false;
        tokio::time::sleep(Duration::from_secs(interval)).await;
      }
    })
  });

  // Wait-for-backend startup gate (`wait_for_backend: true`): without a
  // configured health check the service normally claims a healthy backend
  // immediately, which yields connection-refused errors while a slow dev
  // server is still booting. The gate starts the service out of routing and
  // a lightweight connect-probe loop marks it routable the first time the
  // backend accepts a connection; after that the gate never re-engages
  // (`target_health` is the tool for continuous health tracking, and it
  // supersedes this gate entirely when configured).
  let wait_task = if spec.wait_for_backend
    && spec.target_health.is_none()
    && !spec.target.is_empty()
  {
    backend_healthy.store(false, Ordering::SeqCst);
    backend_probed.store(false, Ordering::SeqCst);
    let flag = backend_healthy.clone();
    let probed = backend_probed.clone();
    let health_changed = health_changed.clone();
    let target = spec.target.clone();
    let label = label.clone();
    info!(
      "[{}] Waiting for the backend to accept connections before joining routing ({})",
      label, target
    );
    Some(tokio::spawn(async move {
      loop {
        if backend_accepts_connections(&target).await {
          flag.store(true, Ordering::SeqCst);
          probed.store(true, Ordering::SeqCst);
          health_changed.notify_one();
          info!("[{}] Backend is up ({}) — now routable", label, target);
          break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
      }
    }))
  } else {
    if spec.wait_for_backend && spec.target_health.is_some() {
      info!(
        "[{}] wait_for_backend is implied by target_health; the health check already gates startup",
        label
      );
    }
    None
  };

  // Local concurrency guard, shared across reconnects of this service.
  let local_limiter: Option<Arc<Semaphore>> = spec
    .max_concurrent
    .map(|n| Arc::new(Semaphore::new(n as usize)));

  // Reconnection Loop. Retries use exponential backoff with jitter so that a
  // fleet of clients does not stampede the server after a restart; the
  // counter resets once a connection proves stable.
  let mut reconnect_attempt: u32 = 0;
  // Set when the server announces a graceful shutdown: the next reconnect
  // skips the exponential backoff (one short jittered delay instead).
  let mut fast_reconnect = false;
  'outer: loop {
    if *cancel.borrow() {
      break;
    }

    info!(
      "[{}] Connecting to Aperio Server at: {}...",
      label, spec.server_addr
    );

    let ws_req_result = spec.ws_url.clone().into_client_request();
    let ws_req = match ws_req_result {
      Ok(mut req) => {
        // Set Authorization Token Header securely (avoids leaking token in query params / logs)
        match HeaderValue::from_str(&format!("Bearer {}", spec.token)) {
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
          max_message_size: Some(spec.max_message_size),
          max_frame_size: Some(spec.max_message_size),
          ..Default::default()
        };
        match connect_async_with_config(req, Some(ws_config), false).await {
          Ok((ws_stream, _)) => {
            info!("[{}] Successfully connected to Aperio Server!", label);
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

            // Active UDP relay streams: stream_id → handle
            let active_udp_streams: Arc<Mutex<HashMap<String, UdpStreamHandle>>> =
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
            let tcp_enabled_ping = spec.tcp_target.is_some();
            let client_id_ping = spec.client_id.clone();
            let path_bind_ping = spec.path.clone();
            let hostnames_ping = spec.hostnames.clone();
            let hostname_bind_ping = spec.hostnames.first().cloned();
            let last_pong_time_ping = last_pong_time.clone();
            let abort_tx_ping = abort_tx.clone();
            let backend_healthy_ping = backend_healthy.clone();
            let backend_probed_ping = backend_probed.clone();
            let health_changed_ping = health_changed.clone();
            let cancel_ping = cancel.clone();
            let service_name_ping = spec.name.clone();
            let tunnels_ping = spec.tunnels.clone();
            let visitor_auth_ping = spec.visitor_auth.clone();
            let allowed_ips_ping = spec.allowed_ips.clone();
            let (max_concurrent, priority, bandwidth_bps, public, cache, resilience) = (
              spec.max_concurrent,
              spec.priority,
              spec.bandwidth_bps,
              spec.public,
              spec.cache,
              spec.resilience,
            );
            let max_request_body_ping = spec.max_request_body;

            let ping_task = tokio::spawn(async move {
              // The first Ping goes out immediately: it announces the binds,
              // version/protocol, and health before any traffic is routed.
              loop {
                // A pending config change drops the connection so the
                // supervisor can respawn the service with fresh settings.
                if *cancel_ping.borrow() {
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
                  hostname_binds: hostnames_ping.clone(),
                  max_concurrent,
                  tcp: tcp_enabled_ping,
                  version: Some(env!("CARGO_PKG_VERSION").to_string()),
                  protocol: Some(PROTOCOL_VERSION),
                  backend_healthy: backend_healthy_ping.load(Ordering::SeqCst),
                  backend_probed: backend_probed_ping.load(Ordering::SeqCst),
                  priority,
                  bandwidth_bps,
                  service: service_name_ping.clone(),
                  public,
                  visitor_auth: visitor_auth_ping.clone(),
                  allowed_ips: allowed_ips_ping.clone(),
                  tunnels: tunnels_ping.clone(),
                  cache,
                  resilience,
                  max_request_body: max_request_body_ping,
                };
                if let Ok(ping_str) = serde_json::to_string(&ping_msg)
                  && tx_ping.send(Message::Text(ping_str)).await.is_err()
                {
                  break;
                }
                // Wake early when the backend health verdict flips, so a
                // change is reported at once rather than up to 5s later.
                tokio::select! {
                  _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                  _ = health_changed_ping.notified() => {}
                }
              }
            });

            // Reqwest Client to make local forwarding requests. Same-site
            // backend redirects (http→https, same root domain) are followed
            // transparently; everything else passes through to the visitor.
            let reqwest_client = reqwest::Client::builder()
              .redirect(crate::proxy::http::redirect_policy(spec.max_redirects))
              .timeout(Duration::from_secs(spec.timeout_secs))
              .build()
              .unwrap_or_default();

            // Per-connection forwarding constants shared by all request tasks.
            if crate::proxy::h2::is_h2_target(&spec.target) && spec.pass_hostname {
              warn!(
                "pass_hostname is ignored for HTTP/2 targets ({}): the backend sees the target authority",
                spec.target
              );
            }
            let forward_ctx = Arc::new(ForwardContext {
              client: reqwest_client.clone(),
              h2_client: crate::proxy::h2::build_h2_client(&spec.target).map(Arc::new),
              unix_socket: crate::proxy::unix::unix_socket_path(&spec.target),
              timeout_secs: spec.timeout_secs,
              target: spec.target.clone(),
              pass_hostname: spec.pass_hostname,
              path_bind: spec.path.clone(),
              trim_bind: spec.trim_bind,
              max_response_body_size: spec.max_response_body,
              tunnel_tx: tx_write.clone(),
              request_headers: HeaderTransform::compile(
                spec.headers.as_ref().and_then(|h| h.request.as_ref()),
              ),
              response_headers: HeaderTransform::compile(
                spec.headers.as_ref().and_then(|h| h.response.as_ref()),
              ),
            });

            // Protocol version the server announced via Pong; v2 enables
            // binary chunk frames and streamed request bodies.
            let server_protocol = Arc::new(std::sync::atomic::AtomicU32::new(1));

            // Streamed request bodies in flight: request id → chunk feeder.
            let active_request_streams: Arc<Mutex<HashMap<String, RequestBodyFeeder>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Read messages from Server
            let mut version_skew_warned = false;
            let mut server_announced_shutdown = false;
            loop {
              tokio::select! {
                  _ = abort_rx.recv() => {
                      warn!("Liveness timeout triggered. Aborting socket loop.");
                      break;
                  }
                  _ = shared.shutdown_notify.notified() => {
                      // Announce drain, let in-flight requests finish, then exit.
                      if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                          let _ = tx_write.send(Message::Text(json)).await;
                      }
                      let drain_deadline = Instant::now() + Duration::from_secs(30);
                      loop {
                          let inflight = shared.inflight_requests.load(Ordering::SeqCst);
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
                                          decompress_frame(&b, spec.max_message_size.saturating_mul(4))
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
                                              let inflight = shared.inflight_requests.clone();
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
                                              let inflight = shared.inflight_requests.clone();
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
                                              let target_url = spec.target.clone();
                                              let path_bind_val = spec.path.clone();
                                              let trim_bind_val = spec.trim_bind;
                                              let active_streams = active_ws_streams.clone();
                                              let client_timeout = spec.timeout_secs;

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
                                          TunnelMessage::TcpOpen { stream_id, target } => {
                                              // SSRF guard: only addresses this client itself
                                              // declared are ever dialed — a named target must be
                                              // in the tunnels: list, no target means the legacy
                                              // tcp_target.
                                              let resolved = match &target {
                                                  Some(t) => spec
                                                      .tunnels
                                                      .iter()
                                                      .find(|d| d.target == *t && d.protocol == "tcp")
                                                      .map(|d| (d.target.clone(), d.encrypt, d.psk.clone())),
                                                  None => spec.tcp_target.clone().map(|t| (t, false, None)),
                                              };
                                              match resolved {
                                                  Some((target_addr, encrypt, psk)) => {
                                                      // Register the stream handle synchronously, BEFORE
                                                      // spawning: TcpData for this stream can arrive on the
                                                      // very next tunnel frame and would be dropped if the
                                                      // spawned task had not registered yet. The channel
                                                      // buffers bytes until the backend connect completes.
                                                      let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(64);
                                                      let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
                                                      active_tcp_streams.lock().await.insert(
                                                          stream_id.clone(),
                                                          TcpStreamHandle { tx: bytes_tx, abort_tx },
                                                      );
                                                      let tx = tx_write.clone();
                                                      let streams = active_tcp_streams.clone();
                                                      tokio::spawn(async move {
                                                          let e2e = encrypt.then_some(crate::e2e::E2eParams { psk });
                                                          handle_tcp_open(stream_id, target_addr, tx, streams, bytes_rx, abort_rx, e2e).await;
                                                      });
                                                  }
                                                  None => {
                                                      match target {
                                                          Some(t) => warn!("TcpOpen for undeclared target {}; refusing", t),
                                                          None => warn!("TcpOpen received but no TCP target is configured; refusing"),
                                                      }
                                                      let close = TunnelMessage::TcpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json)).await;
                                                      }
                                                  }
                                              }
                                          }
                                          TunnelMessage::UdpOpen { stream_id, target } => {
                                              // SSRF guard: only declared protocol: udp targets
                                              // are ever dialed, mirroring TcpOpen.
                                              let resolved = spec
                                                  .tunnels
                                                  .iter()
                                                  .find(|d| d.target == target && d.protocol == "udp")
                                                  .map(|d| (d.target.clone(), crate::udp::effective_idle_timeout(d.idle_timeout)));
                                              match resolved {
                                                  Some((target_addr, idle_timeout)) => {
                                                      // Register synchronously, like TcpOpen: datagrams
                                                      // can arrive on the very next tunnel frame.
                                                      let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(64);
                                                      let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
                                                      active_udp_streams.lock().await.insert(
                                                          stream_id.clone(),
                                                          UdpStreamHandle { tx: dg_tx, abort_tx },
                                                      );
                                                      let tx = tx_write.clone();
                                                      let streams = active_udp_streams.clone();
                                                      tokio::spawn(async move {
                                                          handle_udp_open(stream_id, target_addr, tx, streams, dg_rx, abort_rx, idle_timeout).await;
                                                      });
                                                  }
                                                  None => {
                                                      warn!("UdpOpen for undeclared target {}; refusing", target);
                                                      let close = TunnelMessage::UdpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json)).await;
                                                      }
                                                  }
                                              }
                                          }
                                          TunnelMessage::UdpDatagram { stream_id, data } => {
                                              let streams = active_udp_streams.lock().await;
                                              if let Some(handle) = streams.get(&stream_id) {
                                                  match BASE64_STANDARD.decode(&data) {
                                                      // Best-effort: drop when the relay is congested.
                                                      Ok(bytes) => { let _ = handle.tx.try_send(bytes); }
                                                      Err(_) => warn!("Failed to decode Base64 UdpDatagram for stream {}", stream_id),
                                                  }
                                              }
                                          }
                                          TunnelMessage::UdpClose { stream_id } => {
                                              let mut streams = active_udp_streams.lock().await;
                                              if let Some(handle) = streams.remove(&stream_id) {
                                                  let _ = handle.abort_tx.send(()).await;
                                                  debug!("Closed UDP relay {}", stream_id);
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
                                              info!("[{}] Server assigned hostname to this client: {}", label, hostname);
                                          }
                                          TunnelMessage::ServerShutdown {} => {
                                              // The server is restarting: skip the reconnect backoff
                                              // once the socket drops so downtime stays minimal.
                                              info!("[{}] Server announced a graceful shutdown; will reconnect aggressively.", label);
                                              server_announced_shutdown = true;
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
            warn!("[{}] Connection to server lost.", label);

            // A connection that survived for a while counts as healthy:
            // start the next retry sequence from the base delay again.
            if connected_at.elapsed() >= Duration::from_secs(RECONNECT_STABLE_SECS) {
              reconnect_attempt = 0;
            }
            fast_reconnect = server_announced_shutdown;
          }
          Err(e) => {
            use tokio_tungstenite::tungstenite::Error as WsError;
            if let WsError::Http(resp) = &e {
              let code = resp.status().as_u16();
              if code == 401 || code == 403 {
                error!(
                  "[{}] Authentication failed (HTTP {}): the server rejected the tunnel token. Check --server-token / APERIO_SERVER_TOKEN / yaml server.token — it may be wrong, expired, or revoked.",
                  label, code
                );
              } else {
                error!(
                  "[{}] Server rejected the connection with HTTP {}.",
                  label, code
                );
              }
            } else {
              error!("[{}] Failed to connect to server: {}.", label, e);
            }
          }
        }
      }
      Err(e) => {
        error!("WebSocket configuration request building error: {}", e);
      }
    }

    // A shutdown signal while disconnected exits immediately (nothing to drain).
    if shared.shutting_down.load(Ordering::SeqCst) {
      info!("Shutdown requested while disconnected; exiting.");
      std::process::exit(0);
    }
    if *cancel.borrow() {
      break 'outer;
    }
    let delay = if fast_reconnect {
      // The server told us it is restarting: come back right away (with a
      // little jitter so a fleet does not stampede), and reset the backoff
      // so a slow restart falls back to the normal schedule from the start.
      fast_reconnect = false;
      reconnect_attempt = 0;
      let d = fast_reconnect_delay();
      info!(
        "[{}] Server shutdown announced; reconnecting in {:.2} seconds...",
        label,
        d.as_secs_f64()
      );
      d
    } else {
      reconnect_attempt = reconnect_attempt.saturating_add(1);
      let d = reconnect_delay(reconnect_attempt);
      info!(
        "[{}] Retrying connection in {:.1} seconds (attempt {})...",
        label,
        d.as_secs_f64(),
        reconnect_attempt
      );
      d
    };
    tokio::select! {
      _ = tokio::time::sleep(delay) => {}
      _ = cancel.changed() => break 'outer,
    }
  }

  if let Some(t) = probe_task {
    t.abort();
  }
  if let Some(t) = wait_task {
    t.abort();
  }
  info!("[{}] Service stopped.", label);
}

/// One connect-probe of the wait-for-backend gate: true when the backend
/// accepts a TCP (or unix-socket) connection. Deliberately connection-level
/// only — the gate answers "is anything listening yet", not "is it healthy"
/// (that is `target_health`'s job).
async fn backend_accepts_connections(target: &str) -> bool {
  let attempt = async {
    #[cfg(unix)]
    if let Some(path) = crate::proxy::unix::unix_socket_path(target) {
      return tokio::net::UnixStream::connect(path).await.is_ok();
    }
    let wire = target
      .replacen("h2c://", "http://", 1)
      .replacen("h2://", "https://", 1);
    let Ok(url) = url::Url::parse(&wire) else {
      return false;
    };
    let Some(host) = url.host_str() else {
      return false;
    };
    let Some(port) = url.port_or_known_default() else {
      return false;
    };
    tokio::net::TcpStream::connect((host, port)).await.is_ok()
  };
  tokio::time::timeout(Duration::from_secs(3), attempt)
    .await
    .unwrap_or(false)
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

/// Reconnect delay used after the server announces a graceful shutdown:
/// 100–500 ms of clock-derived jitter, no exponential backoff. Short enough
/// that a rolling restart is barely visible, jittered enough that a fleet of
/// clients does not stampede the returning server.
fn fast_reconnect_delay() -> Duration {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .subsec_nanos() as u64;
  Duration::from_millis(100 + nanos % 401)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
