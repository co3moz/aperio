//! One tunnel service: a single outbound tunnel connection exposing one
//! local target, with its own reconnect loop, heartbeat, backend health
//! probe and forwarding state. The supervisor in `main` spawns one task per
//! service and respawns them (with freshly resolved settings) when the
//! configuration file changes, which is how every setting, not just a
//! subset, takes effect on hot-reload.

use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio_tungstenite::tungstenite::{
  client::IntoClientRequest,
  http::HeaderValue,
  protocol::{Message, WebSocketConfig},
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

/// Resolves this client's trust-on-first-use device key for token pinning,
/// announced in the Ping. Opt-in: an explicit `key` is used as given;
/// otherwise `file` names a path whose contents are used, generating and
/// persisting a fresh random key there on first run. `None` (nothing
/// announced) when neither is set. Both come from the layered configuration
/// (yaml `device_key` / `device_key_file`, or their `APERIO_*` spellings).
fn resolve_device_key(key: Option<String>, file: Option<String>) -> Option<String> {
  if let Some(v) = key {
    let v = v.trim().to_string();
    if !v.is_empty() {
      return Some(v);
    }
  }
  let path = file
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty())?;
  match std::fs::read_to_string(&path) {
    Ok(k) if !k.trim().is_empty() => Some(k.trim().to_string()),
    _ => {
      let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
      );
      // The device key is a pinning secret; persist it owner-only (0600) on
      // Unix so a local user cannot read it and replay a leaked token.
      let write_res = {
        use std::io::Write;
        #[cfg(unix)]
        let opened = {
          use std::os::unix::fs::OpenOptionsExt;
          std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        };
        #[cfg(not(unix))]
        let opened = std::fs::OpenOptions::new()
          .write(true)
          .create(true)
          .truncate(true)
          .open(&path);
        opened.and_then(|mut f| f.write_all(key.as_bytes()))
      };
      match write_res {
        Ok(()) => info!("Generated a new device key at {path} for token pinning"),
        Err(e) => warn!(
          "Could not persist the device key to {path}: {e}. Running with an in-memory key that changes on every restart, if the server enforces token pinning it will reject this client after a restart. On a read-only or ephemeral filesystem, set a stable key via the APERIO_DEVICE_KEY environment variable instead of a file."
        ),
      }
      Some(key)
    }
  }
}

/// Where the device key comes from, resolved from the full configuration
/// layering (yaml `device_key`/`device_key_file`, or `APERIO_DEVICE_KEY` /
/// `APERIO_DEVICE_KEY_FILE`) and installed once at startup.
static DEVICE_KEY_SOURCES: std::sync::OnceLock<(Option<String>, Option<String>)> =
  std::sync::OnceLock::new();

/// Installs the device-key sources. Called once from `main` before any
/// service connects; a later call is ignored, so a config reload cannot swap
/// the identity of a running process out from under the server's pin.
pub(crate) fn set_device_key_sources(key: Option<String>, file: Option<String>) {
  let _ = DEVICE_KEY_SOURCES.set((key, file));
}

/// The process-wide device key, resolved once.
fn device_key() -> Option<String> {
  static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
  KEY
    .get_or_init(|| {
      let (key, file) = DEVICE_KEY_SOURCES.get().cloned().unwrap_or_default();
      resolve_device_key(key, file)
    })
    .clone()
}

/// Everything a service needs to run, fully resolved. Built by `main` from
/// the layered configuration; rebuilt (and the service respawned) on
/// config hot-reload.
#[derive(Clone, Debug)]
pub(crate) struct ServiceSpec {
  /// Handle from the `services:` list (None for the single default service).
  /// An identifier: a-z, 0-9 and `_`.
  pub(crate) name: Option<String>,
  /// What to call it on screen, when the file said something friendlier.
  pub(crate) custom_name: Option<String>,
  /// Stable instance id announced to the server. Kept across reconnects
  /// and config respawns so the server's failover `wait` mode keeps
  /// recognizing this client.
  pub(crate) client_id: String,
  pub(crate) token: String,
  /// Process-wide instance group id (the raw `client_id` base, shared by every
  /// service and every parallel connection of this process). Announced to the
  /// server via the `x-aperio-instance` handshake header so the dashboard can
  /// group a process's connections and the server can share one random hostname
  /// across them. Unlike `client_id`, this is never suffixed per connection.
  pub(crate) instance_group: String,
  pub(crate) server_addr: String,
  pub(crate) ws_url: String,
  /// All candidate server WebSocket URLs, primary first (from
  /// `APERIO_SERVER_URLS`). The reconnect loop rotates to the next one after a
  /// failed connection, so a client can fail over across a server fleet.
  pub(crate) ws_urls: Vec<String>,
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
  /// Per-service override of the server's gateway response timeout, in seconds
  /// (announced via Ping; None = the server's global value applies).
  pub(crate) response_timeout: Option<u64>,
  pub(crate) timeout_secs: u64,
  pub(crate) max_concurrent: Option<u32>,
  /// Parallel tunnel connections for this service (1–16). The supervisor
  /// spawns one service task per connection, each with a derived client id.
  pub(crate) connections: u32,
  pub(crate) priority: u32,
  /// Rate a single connection of this service announces, in bytes/second
  /// (None = unlimited). Already settled against the client-wide budget and
  /// divided across `connections` by `allocate_bandwidth`.
  pub(crate) bandwidth_bps: Option<u64>,
  /// The `bandwidth:` value as written in the config, kept so the client can
  /// report how it differs from what it ended up announcing.
  pub(crate) bandwidth_declared: Option<String>,
  /// Settings resolved to something other than the config asked for,
  /// announced via Ping and surfaced in the dashboard's config view.
  pub(crate) config_notes: Vec<crate::protocol::ConfigNote>,
  pub(crate) max_message_size: usize,
  pub(crate) max_redirects: usize,
  pub(crate) tcp_target: Option<String>,
  pub(crate) target_health: Option<String>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection (no-op when `target_health` is set, that gates startup too).
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
  /// Ask the server to persist inbound POSTs to this service into its
  /// webhook inbox (announced via Ping).
  pub(crate) webhook_inbox: bool,
  /// Redirect URL for visitors this service's `allowed_ips` rejects
  /// (announced via Ping; None = stealth).
  pub(crate) denied: Option<String>,
  /// Autoscaling declaration announced via Ping: the endpoint the server
  /// calls when this service needs capacity (None = not managed).
  pub(crate) scaling: Option<crate::protocol::ScalingDecl>,
}

impl ServiceSpec {
  /// Short label used to attribute log lines to this service.
  pub(crate) fn label(&self) -> String {
    self.name.clone().unwrap_or_else(|| {
      if self.target.is_empty() {
        // A connection that serves no HTTP target. It exists for the tunnels
        // a peer binds, for the messages this client carries, or both;
        // naming one of them would be a guess in the log line where the
        // reader is trying to work out what this connection is for.
        if self.tunnels.is_empty() {
          "(no service)".to_string()
        } else {
          "(tunnels only)".to_string()
        }
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
  /// Unix seconds of the last request this process started serving, and
  /// whether it has ever served one. Together they drive `idle_timeout`: the
  /// idle clock only starts after the first request, so a client that was
  /// just cold-started cannot retire before it is ever used.
  pub(crate) last_request_at: Arc<AtomicU64>,
  /// Process-wide message bus: the topic filters this client subscribes to,
  /// the live connections a publish can go out on, and the fan-out to
  /// whatever is attached locally.
  pub(crate) messages: Arc<crate::pubsub::MessageBus>,
}

impl Shared {
  /// Records that the server just handed this process work to do, which is
  /// what `idle_timeout` measures the absence of.
  ///
  /// Every kind of inbound work counts, not only buffered HTTP requests:
  /// streamed uploads, WebSocket upgrades and raw TCP/UDP sessions all mean
  /// the client is in use. Marking only the buffered kind let a busy client
  /// conclude it was idle and retire in the middle of live traffic, cutting
  /// long-running streams outright.
  pub(crate) fn mark_request_activity(&self) {
    self.activity_clock().stamp();
  }

  /// The idle clock as a handle the long-lived stream relays can stamp.
  pub(crate) fn activity_clock(&self) -> ActivityClock {
    ActivityClock(self.last_request_at.clone())
  }
}

/// Handle to the idle clock (`Shared::last_request_at`), passed into the
/// WebSocket/TCP/UDP relays so a long-lived stream keeps resetting it with
/// every relayed frame, in both directions. Stamping only the frame that
/// *opens* a stream let a session outlasting `idle_timeout` be retired in
/// the middle of live traffic.
#[derive(Clone, Default)]
pub(crate) struct ActivityClock(Arc<AtomicU64>);

impl ActivityClock {
  /// Records proxied work happening right now.
  pub(crate) fn stamp(&self) {
    self.0.store(
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs(),
      Ordering::SeqCst,
    );
  }

  /// Unix seconds of the last stamp; 0 when nothing was ever served.
  #[cfg(test)]
  pub(crate) fn secs(&self) -> u64 {
    self.0.load(Ordering::SeqCst)
  }
}

/// Whether the idle watcher should retire the process: only once it has
/// served something, nothing is in flight any more, and the clock has then
/// stayed quiet for the full window. The in-flight guard covers work that
/// produces no tunnel frames for long stretches (a backend taking minutes to
/// answer, a response streaming for longer than the window), which would
/// otherwise read as idleness and get cut by the drain deadline.
pub(crate) fn should_retire_idle(
  last_secs: u64,
  now_secs: u64,
  idle_secs: u64,
  inflight: usize,
) -> bool {
  last_secs != 0 && inflight == 0 && now_secs.saturating_sub(last_secs) >= idle_secs
}

/// Resolves once a shutdown has been requested, whether the request arrived
/// before or after this call.
///
/// `Notify::notify_waiters` wakes only the tasks already waiting, so the flag
/// is the source of truth and the notification is just what makes the wake-up
/// prompt. Waiting on the notification alone loses every signal that lands
/// while a service is elsewhere (sitting in its reconnect backoff, dialing),
/// and the service would then wait forever for a notification that already
/// happened.
pub(crate) async fn shutdown_requested(shared: &Shared) {
  let notified = shared.shutdown_notify.notified();
  tokio::pin!(notified);
  // Register as a waiter before reading the flag, so a signal landing between
  // the two is still delivered instead of falling into the gap.
  notified.as_mut().enable();
  if shared.shutting_down.load(Ordering::SeqCst) {
    return;
  }
  notified.await;
}

/// Waits for this process's in-flight requests to finish, bounded by a
/// deadline.
///
/// Shared by both shutdown paths: whichever service notices the signal first
/// must not tear the process down while a sibling service is still answering
/// a visitor.
async fn drain_inflight(shared: &Shared) {
  let deadline = Instant::now() + Duration::from_secs(30);
  loop {
    let inflight = shared.inflight_requests.load(Ordering::SeqCst);
    if inflight == 0 {
      info!("Drain complete; exiting.");
      return;
    }
    if Instant::now() >= deadline {
      warn!(
        "Drain timeout with {} request(s) still in flight; exiting anyway.",
        inflight
      );
      return;
    }
    info!("Draining: {} request(s) in flight...", inflight);
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
}

/// Ends the process when a shutdown was requested while this service has no
/// connection: there is no server to announce the drain to and nothing of its
/// own left in flight, but a sibling service may still be answering, so it
/// waits for the process-wide drain first.
async fn exit_if_shutting_down(shared: &Shared) {
  if !shared.shutting_down.load(Ordering::SeqCst) {
    return;
  }
  info!("Shutdown requested while disconnected; exiting.");
  drain_inflight(shared).await;
  std::process::exit(0);
}

/// Per-service backend-health state, shared by every parallel connection of a
/// service (`connections: N`) so the backend is probed once per service, not
/// once per connection. Every connection reports `healthy`/`probed` in its
/// heartbeat; only the probe-owning connection drives the probe/gate that
/// writes them, and `changed` wakes all connections when the verdict flips.
#[derive(Clone)]
pub(crate) struct BackendHealth {
  pub(crate) healthy: Arc<AtomicBool>,
  pub(crate) probed: Arc<AtomicBool>,
  pub(crate) changed: Arc<tokio::sync::Notify>,
}

impl BackendHealth {
  /// Initial state for `spec`: a service with a `target_health` check or a
  /// `wait_for_backend` gate starts out of routing (unhealthy, unprobed) so no
  /// connection reports the backend up before it has been checked; otherwise it
  /// is healthy immediately.
  pub(crate) fn for_spec(spec: &ServiceSpec) -> Self {
    let gated = spec.target_health.is_some() || (spec.wait_for_backend && !spec.target.is_empty());
    Self {
      healthy: Arc::new(AtomicBool::new(!gated)),
      probed: Arc::new(AtomicBool::new(!gated)),
      changed: Arc::new(tokio::sync::Notify::new()),
    }
  }

  /// The pair a heartbeat reports, read through one place so the two can never
  /// be sampled apart.
  ///
  /// `healthy` implies `probed`: the gated service starts unhealthy and only a
  /// probe that passed, or a backend that accepted a connection, ever makes it
  /// healthy, so being up *is* evidence something looked. Deriving it here
  /// rather than trusting the write order removes the window where a heartbeat
  /// woken between the two stores said "up, and nobody has checked", which is
  /// not a state that exists and which the dashboard renders as CHECKING for a
  /// backend that is already serving.
  pub(crate) fn report(&self) -> (bool, bool) {
    let healthy = self.healthy.load(Ordering::SeqCst);
    (healthy, healthy || self.probed.load(Ordering::SeqCst))
  }
}

/// Runs one tunnel service until the process shuts down or `cancel` fires
/// (config reload → the supervisor respawns with a fresh spec). `health` is the
/// service's shared backend-health state; `run_probe` is true only for the one
/// connection that owns the health probe/gate (the others just report it).
/// What the server said this token may open for one service, shared across a
/// service's parallel connections.
///
/// The first connection learns it from the handshake and publishes it here;
/// the others wait for it before opening a socket, so a `connections:` larger
/// than the server permits costs one refused connection instead of a fan of
/// them. `None` = not learned yet, or a server too old to announce it.
#[derive(Clone)]
pub(crate) struct ConnectionCeiling {
  pub(crate) tx: Arc<watch::Sender<Option<u32>>>,
  pub(crate) rx: watch::Receiver<Option<u32>>,
}

impl ConnectionCeiling {
  pub(crate) fn new() -> Self {
    let (tx, rx) = watch::channel(None);
    ConnectionCeiling {
      tx: Arc::new(tx),
      rx,
    }
  }

  /// Waits up to `grace` for the first connection to report the ceiling.
  /// Returns what it learned, or `None` when nothing arrived: an old server
  /// does not announce, and a connection that waited must still be allowed to
  /// try rather than hang for the life of the process.
  pub(crate) async fn learned(&self, grace: Duration) -> Option<u32> {
    let mut rx = self.rx.clone();
    if let Some(v) = *rx.borrow_and_update() {
      return Some(v);
    }
    let _ = tokio::time::timeout(grace, rx.changed()).await;
    *rx.borrow()
  }
}

pub(crate) async fn run_service(
  spec: ServiceSpec,
  shared: Shared,
  mut cancel: watch::Receiver<bool>,
  health: BackendHealth,
  run_probe: bool,
  connection_index: u32,
  ceiling: ConnectionCeiling,
) {
  let label = spec.label();

  // Connections beyond the first wait for the server's announced ceiling
  // before opening a socket. Five seconds is the whole budget: past that the
  // server is either old (no announcement) or slow, and in both cases trying
  // is better than a connection that never happens.
  if connection_index > 1
    && let Some(permitted) = ceiling.learned(Duration::from_secs(5)).await
    && connection_index > permitted
  {
    warn!(
      "[{}] The server permits {} parallel connection(s) for this service; \
       connection {} stands down. Raise max_connections_per_service on the server \
       (or the token's max_connections) to use more.",
      label, permitted, connection_index
    );
    return;
  }

  // Backend health is shared across the service's parallel connections (created
  // once by the supervisor). This connection reports it in every heartbeat and,
  // when it owns the probe, drives the probe/gate that updates it.
  let BackendHealth {
    healthy: backend_healthy,
    probed: backend_probed,
    changed: health_changed,
  } = health;
  let probe_task = if run_probe {
    spec.target_health.as_ref().map(|health_path| {
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
      .unwrap_or_else(|e| {
        error!("Failed to build the health-probe HTTP client: {e}; using a client without a timeout");
        reqwest::Client::new()
      });
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
        // Before anything is announced. The heartbeat reads both flags
        // together, and the healthy-transition notify below wakes it: with
        // the store left until after, that heartbeat carried "healthy, never
        // probed", a pair that describes nothing, and the one the dashboard
        // renders as CHECKING for a backend already probed and up. It
        // corrected itself on the next notify, which is exactly why it took a
        // one-in-many e2e run to see it.
        if first_result {
          probed.store(true, Ordering::SeqCst);
        }
        if ok {
          consecutive_failures = 0;
          if !flag.swap(true, Ordering::SeqCst) {
            health_changed.notify_waiters();
            if first_result {
              info!("Backend healthy: {}, now routable", health_url);
            } else {
              info!("Backend health restored: {}", health_url);
            }
          }
        } else {
          consecutive_failures = consecutive_failures.saturating_add(1);
          if consecutive_failures >= threshold && flag.swap(false, Ordering::SeqCst) {
            health_changed.notify_waiters();
            warn!(
              "Backend health check failed {} consecutive time(s): {}, reporting unhealthy to the server (tunnel stays connected)",
              consecutive_failures, health_url
            );
          } else if first_result {
            // Started unhealthy and the first probe also failed: make it clear
            // why the backend is not yet routable (the threshold warning above
            // only fires on a healthy→unhealthy transition).
            info!(
              "Backend not healthy yet: {}, staying out of routing until a probe passes",
              health_url
            );
          }
        }
        if first_result {
          health_changed.notify_waiters();
        }
        first_result = false;
        tokio::time::sleep(Duration::from_secs(interval)).await;
      }
    })
  })
  } else {
    None
  };

  // Wait-for-backend startup gate (`wait_for_backend: true`): without a
  // configured health check the service normally claims a healthy backend
  // immediately, which yields connection-refused errors while a slow dev
  // server is still booting. The gate starts the service out of routing and
  // a lightweight connect-probe loop marks it routable the first time the
  // backend accepts a connection; after that the gate never re-engages
  // (`target_health` is the tool for continuous health tracking, and it
  // supersedes this gate entirely when configured).
  let wait_task = if run_probe
    && spec.wait_for_backend
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
          health_changed.notify_waiters();
          info!("[{}] Backend is up ({}), now routable", label, target);
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
  // Index into `spec.ws_urls` for cross-server failover: advanced after each
  // failed/dropped connection so the client rotates across the server fleet.
  let mut server_idx = 0usize;
  'outer: loop {
    if *cancel.borrow() {
      break;
    }
    exit_if_shutting_down(&shared).await;

    let current_ws = spec
      .ws_urls
      .get(server_idx % spec.ws_urls.len().max(1))
      .cloned()
      .unwrap_or_else(|| spec.ws_url.clone());
    info!(
      "[{}] Connecting to Aperio Server at: {}...",
      label, current_ws
    );

    let ws_req_result = current_ws.into_client_request();
    let ws_req = match ws_req_result {
      Ok(mut req) => {
        // Set Authorization Token Header securely (avoids leaking token in query params / logs)
        match HeaderValue::from_str(&format!("Bearer {}", spec.token)) {
          Ok(val) => {
            req.headers_mut().insert("Authorization", val);
            // Announce the process-wide instance group so the server can group
            // this process's connections and share one random hostname across
            // them. Non-secret; safe as a plain header.
            if let Ok(g) = HeaderValue::from_str(&spec.instance_group) {
              req.headers_mut().insert("x-aperio-instance", g);
            }
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
        // Dial under the cancel signal so a shutdown aborts an in-progress
        // connect/handshake immediately instead of waiting for it to finish
        // (a half-open server can otherwise stall the handshake with no
        // timeout, keeping the service alive past cancel).
        let connect_fut = crate::dial::connect_ws(req, Some(ws_config));
        tokio::pin!(connect_fut);
        let connect_result = tokio::select! {
          _ = cancel.changed() => break 'outer,
          r = &mut connect_fut => r,
        };
        match connect_result {
          Ok((ws_stream, response)) => {
            info!("[{}] Successfully connected to Aperio Server!", label);
            // The server announces what this token may open for one service.
            // Published for the siblings waiting on it above; refreshed on
            // every reconnect, so raising the number on the server reaches a
            // running client without restarting it.
            if let Some(permitted) = response
              .headers()
              .get("x-aperio-max-connections")
              .and_then(|v| v.to_str().ok())
              .and_then(|v| v.trim().parse::<u32>().ok())
              .filter(|v| *v > 0)
            {
              ceiling.tx.send_replace(Some(permitted));
            }
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
            // Every connection subscribes with the full filter set. The
            // server collapses the copies to one per client process, and a
            // connection that never subscribed would leave the process deaf
            // the moment its sibling dropped.
            {
              let bus = shared.messages.clone();
              let tx = tx_write.clone();
              let label = label.clone();
              tokio::spawn(async move {
                bus.attach(&label, tx.clone()).await;
                bus.subscribe_on(&tx).await;
              });
            }
            let tx_ping = tx_write.clone();
            let tcp_enabled_ping = spec.tcp_target.is_some();
            let client_id_ping = spec.client_id.clone();
            let path_bind_ping = spec.path.clone();
            let hostnames_ping = spec.hostnames.clone();
            let hostname_bind_ping = spec.hostnames.first().cloned();
            let last_pong_time_ping = last_pong_time.clone();
            let abort_tx_ping = abort_tx.clone();
            let health_ping = BackendHealth {
              healthy: backend_healthy.clone(),
              probed: backend_probed.clone(),
              changed: health_changed.clone(),
            };
            let health_changed_ping = health_changed.clone();
            let cancel_ping = cancel.clone();
            let service_name_ping = spec.name.clone();
            let service_custom_name_ping = spec.custom_name.clone();
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
            let response_timeout_ping = spec.response_timeout;
            let client_key_ping = device_key();
            let webhook_inbox_ping = spec.webhook_inbox;
            let denied_ping = spec.denied.clone();
            let connections_ping = spec.connections;
            let config_notes_ping = spec.config_notes.clone();
            let scaling_ping = spec.scaling.clone();

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

                // One read, so the pair in this heartbeat is one observation.
                let reported_health = health_ping.report();
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
                  backend_healthy: reported_health.0,
                  backend_probed: reported_health.1,
                  priority,
                  bandwidth_bps,
                  service: service_name_ping.clone(),
                  service_custom_name: service_custom_name_ping.clone(),
                  public,
                  visitor_auth: visitor_auth_ping.clone(),
                  allowed_ips: allowed_ips_ping.clone(),
                  tunnels: tunnels_ping.clone(),
                  cache,
                  resilience,
                  max_request_body: max_request_body_ping,
                  response_timeout: response_timeout_ping,
                  client_key: client_key_ping.clone(),
                  webhook_inbox: webhook_inbox_ping,
                  denied: denied_ping.clone(),
                  scaling: scaling_ping.clone(),
                  connections: Some(connections_ping),
                  config_notes: config_notes_ping.clone(),
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
              .unwrap_or_else(|e| {
                error!("Failed to build the forwarding HTTP client: {e}; using a client without a timeout");
                reqwest::Client::new()
              });

            // Per-connection forwarding constants shared by all request tasks.
            if crate::proxy::h2::is_h2_target(&spec.target) && spec.pass_hostname {
              warn!(
                "pass_hostname is ignored for HTTP/2 targets ({}): the backend sees the target authority",
                spec.target
              );
            }
            // Pause switches for the streams this connection produces
            // (server flow control, protocol v3). Per connection: stream ids
            // do not survive a reconnect.
            let stream_pauses = crate::flow::PauseRegistry::default();

            let forward_ctx = Arc::new(ForwardContext {
              client: reqwest_client.clone(),
              stream_pauses: stream_pauses.clone(),
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
                  _ = shutdown_requested(&shared) => {
                      // Announce drain, let in-flight requests finish, then exit.
                      if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                          let _ = tx_write.send(Message::Text(json)).await;
                      }
                      drain_inflight(&shared).await;
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
                                              shared.mark_request_activity();

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
                                              shared.mark_request_activity();
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
                                              shared.mark_request_activity();
                                              let tx_resp = tx_write.clone();
                                              let target_url = spec.target.clone();
                                              let path_bind_val = spec.path.clone();
                                              let trim_bind_val = spec.trim_bind;
                                              let active_streams = active_ws_streams.clone();
                                              let client_timeout = spec.timeout_secs;
                                              let activity = shared.activity_clock();
                                              let pauses = stream_pauses.clone();

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
                                                      activity,
                                                      pauses,
                                                  )
                                                  .await;
                                              });
                                          }
                                          TunnelMessage::WsData {
                                              stream_id,
                                              data,
                                              is_text,
                                          } => {
                                              // Forward from tunnel → backend WS with a NON-BLOCKING
                                              // try_send: awaiting the send here would let one backend
                                              // that stopped reading wedge the entire tunnel read loop,
                                              // which also carries Pong, so a stall would starve the
                                              // liveness watchdog and drop every stream on this
                                              // connection. Instead drop just this backed-up stream (its
                                              // 64-slot channel is already a generous buffer).
                                              let tx = {
                                                  let streams = active_ws_streams.lock().await;
                                                  streams.get(&stream_id).map(|h| h.tx.clone())
                                              };
                                              if let Some(tx) = tx {
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
                                                  if tx.try_send(ws_msg).is_err() {
                                                      debug!("Backend WS channel full/closed for stream {}; dropping", stream_id);
                                                      active_ws_streams.lock().await.remove(&stream_id);
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
                                              shared.mark_request_activity();
                                              // SSRF guard: only addresses this client itself
                                              // declared are ever dialed, a named target must be
                                              // in the tunnels: list, no target means the legacy
                                              // tcp_target.
                                              let resolved = match &target {
                                                  Some(t) => spec
                                                      .tunnels
                                                      .iter()
                                                      .find(|d| {
                                                          d.target == *t
                                                              && aperio_config::protocol_serves(&d.protocol, "tcp")
                                                      })
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
                                                      let activity = shared.activity_clock();
                                                      let pauses = stream_pauses.clone();
                                                      tokio::spawn(async move {
                                                          let e2e = encrypt.then_some(crate::e2e::E2eParams { psk });
                                                          handle_tcp_open(stream_id, target_addr, tx, streams, bytes_rx, abort_rx, e2e, activity, pauses).await;
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
                                              shared.mark_request_activity();
                                              // SSRF guard: only declared protocol: udp targets
                                              // are ever dialed, mirroring TcpOpen.
                                              let resolved = spec
                                                  .tunnels
                                                  .iter()
                                                  .find(|d| {
                                                      d.target == target
                                                          && aperio_config::protocol_serves(&d.protocol, "udp")
                                                  })
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
                                                      let activity = shared.activity_clock();
                                                      tokio::spawn(async move {
                                                          handle_udp_open(stream_id, target_addr, tx, streams, dg_rx, abort_rx, idle_timeout, activity).await;
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
                                              // Non-blocking try_send (see the WsData arm): a backend that
                                              // accepts the connection but stops reading must never wedge
                                              // the tunnel read loop and starve the liveness watchdog.
                                              // Drop just this stalled stream instead.
                                              let tx = {
                                                  let streams = active_tcp_streams.lock().await;
                                                  streams.get(&stream_id).map(|h| h.tx.clone())
                                              };
                                              if let Some(tx) = tx {
                                                  match BASE64_STANDARD.decode(&data) {
                                                      Ok(bytes) => {
                                                          if tx.try_send(bytes).is_err() {
                                                              debug!("TCP backend channel full/closed for stream {}; dropping", stream_id);
                                                              active_tcp_streams.lock().await.remove(&stream_id);
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
                                          TunnelMessage::SubscribeRefused { topic, reason } => {
                                              warn!(
                                                  "[{}] Not subscribed to '{}': {}",
                                                  label, topic, reason
                                              );
                                          }
                                          TunnelMessage::PublishRefused { topic, reason } => {
                                              warn!(
                                                  "[{}] The message published on '{}' went nowhere: {}",
                                                  label, topic, reason
                                              );
                                          }
                                          TunnelMessage::Publish { topic, payload, id, qos } => {
                                              use base64::prelude::*;
                                              match BASE64_STANDARD.decode(&payload) {
                                                  Ok(bytes) => {
                                                      // Acknowledged before anything else, and
                                                      // whether or not this is a duplicate: the
                                                      // server resends until it hears back, and a
                                                      // redelivery it already sent needs answering
                                                      // too or it comes round again.
                                                      if qos >= 1 && let Some(id) = &id {
                                                          shared.messages.acknowledge(id).await;
                                                      }
                                                      // At-least-once means the same message can
                                                      // arrive twice when an acknowledgement is
                                                      // lost. Acting on a deploy trigger twice is
                                                      // worse than acting on it late.
                                                      let duplicate = match &id {
                                                          Some(id) => shared.messages.is_duplicate(id).await,
                                                          None => false,
                                                      };
                                                      // A filter removed since the server was told
                                                      // still delivers for a moment; dropping here
                                                      // keeps a local subscriber from seeing a
                                                      // topic it no longer asked for.
                                                      if !duplicate && shared.messages.wants(&topic).await {
                                                          shared.messages.deliver(crate::pubsub::Delivery {
                                                              topic,
                                                              payload: bytes,
                                                              id,
                                                          });
                                                      }
                                                  }
                                                  Err(e) => warn!("Undecodable message payload on '{}': {}", topic, e),
                                              }
                                          }
                                          TunnelMessage::StreamPause { id } => {
                                              // Server flow control (v3): the visitor of this
                                              // stream reads slower than we produce. An unknown
                                              // id (stream already finished) is a no-op.
                                              stream_pauses.pause(&id);
                                          }
                                          TunnelMessage::StreamResume { id } => {
                                              stream_pauses.resume(&id);
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
                  "[{}] Authentication failed (HTTP {}): the server rejected the tunnel token. Check --server-token / APERIO_SERVER_TOKEN / yaml server.token, it may be wrong, expired, or revoked.",
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

    // This connection's writer is gone: take it out of the bus so a publish
    // is not handed to a dead channel, and so "no tunnel connection is up"
    // stays a true statement when every one of them has dropped.
    shared.messages.detach(&label).await;

    exit_if_shutting_down(&shared).await;
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
    // Cross-server failover: after a failed/dropped connection, try the next
    // server on the next attempt (no-op with a single server).
    if spec.ws_urls.len() > 1 {
      server_idx = server_idx.wrapping_add(1);
    }
    tokio::select! {
      _ = tokio::time::sleep(delay) => {}
      _ = cancel.changed() => break 'outer,
      // The loop head does the exiting; this arm only cuts the wait short.
      _ = shutdown_requested(&shared) => {}
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
/// only, the gate answers "is anything listening yet", not "is it healthy"
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
