//! One tunnel service: a single outbound tunnel connection exposing one
//! local target, with its own reconnect loop, heartbeat, backend health
//! probe and forwarding state. The supervisor in `main` spawns one task per
//! service and respawns them (with freshly resolved settings) when the
//! configuration file changes, which is how every setting, not just a
//! subset, takes effect on hot-reload.

use base64::prelude::*;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio_tungstenite::tungstenite::{
  client::IntoClientRequest,
  http::HeaderValue,
  protocol::{Message, WebSocketConfig},
};
use tracing::{debug, error, info, warn};

use crate::protocol::{
  FRAME_REQUEST_CHUNK, FRAME_REQUEST_FULL, FRAME_REQUEST_FULL_ZLIB, FRAME_RESPONSE_FULL,
  FRAME_RESPONSE_FULL_ZLIB, PROTOCOL_VERSION, RequestBodyFeeder, TunnelDecl, TunnelMessage,
  compress_frame, decode_binary_frame, decompress_frame, encode_binary_frame, split_full_response,
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
      // `mode` on the open only applies to a file this call *creates*. A path
      // that already existed, an empty one left by a failed write, or one an
      // operator touched, keeps whatever mode it had, which is 0644 under the
      // usual umask: the secret would be written world-readable into a file
      // that looks like it was written owner-only. Tightening after the fact
      // covers both paths with one rule.
      #[cfg(unix)]
      if write_res.is_ok() {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
          warn!("Could not restrict the device key file {path} to 0600: {e}");
        }
      }
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

/// Longest the tunnel's read loop will wait for one stream's consumer, whether
/// that is an upload's backend request, a proxied WebSocket or a TCP relay.
///
/// The loop is shared by every request, every stream and the heartbeat on this
/// connection, so a blocking send here does not slow one stream down, it stops
/// the whole tunnel: no Pong goes out, and fifteen seconds later the liveness
/// check tears the connection down and every in-flight request with it. Two
/// seconds is generous for a backend that is merely slow and short enough that
/// two of them in a row still leave the heartbeat alive.
const STREAM_STALL_BUDGET: Duration = Duration::from_secs(2);

/// Hands one frame to a relay's consumer, waiting only as long as the tunnel
/// can afford. `false` means this stream is finished and its entry should go.
///
/// The alternative that was here, `try_send` and drop the stream the moment
/// its buffer is full, protected the read loop and turned *transient*
/// backpressure into stream death. WebSockets and TCP relays are lossless, so
/// a healthy consumer that is merely slower than a burst, a large file over a
/// tunneled socket whose peer applies flow control, was being killed for
/// keeping the tunnel waiting a few milliseconds. Waiting a bounded two
/// seconds first covers that; a consumer still not ready after it is stalled
/// rather than slow, and loses its own stream instead of the connection.
async fn deliver_to_relay<T>(tx: &mpsc::Sender<T>, kind: &str, stream_id: &str, item: T) -> bool {
  match tx.try_send(item) {
    Ok(()) => true,
    Err(mpsc::error::TrySendError::Closed(_)) => false,
    Err(mpsc::error::TrySendError::Full(item)) => {
      match tokio::time::timeout(STREAM_STALL_BUDGET, tx.send(item)).await {
        Ok(Ok(())) => true,
        _ => {
          warn!(
            "{} relay {} stalled: its consumer took no data for {}s, dropping that stream rather than the tunnel",
            kind,
            stream_id,
            STREAM_STALL_BUDGET.as_secs()
          );
          false
        }
      }
    }
  }
}

/// Delivers one relayed TCP chunk to its backend stream, however it arrived
/// (base64 in JSON from an older server, or a v7 binary frame).
async fn deliver_tcp_bytes(
  streams: &Arc<Mutex<HashMap<String, TcpStreamHandle>>>,
  stream_id: &str,
  bytes: bytes::Bytes,
) {
  let tx = {
    let map = streams.lock().await;
    map.get(stream_id).map(|h| h.tx.clone())
  };
  if let Some(tx) = tx
    && !deliver_to_relay(&tx, "TCP", stream_id, bytes).await
  {
    streams.lock().await.remove(stream_id);
  }
}

/// Delivers one relayed datagram. Best-effort by contract, unlike the WS and
/// TCP paths: a datagram relay that waits for a congested consumer is no
/// longer a datagram relay, so a full channel drops it and keeps the stream.
async fn deliver_udp_bytes(
  streams: &Arc<Mutex<HashMap<String, UdpStreamHandle>>>,
  stream_id: &str,
  bytes: bytes::Bytes,
) {
  let streams = streams.lock().await;
  if let Some(handle) = streams.get(stream_id) {
    let _ = handle.tx.try_send(bytes);
  }
}

/// Delivers one frame of a passed-through WebSocket to its backend stream.
async fn deliver_ws_frame(
  streams: &Arc<Mutex<HashMap<String, WsStreamHandle>>>,
  stream_id: &str,
  msg: Message,
) {
  let tx = {
    let map = streams.lock().await;
    map.get(stream_id).map(|h| h.tx.clone())
  };
  if let Some(tx) = tx
    && !deliver_to_relay(&tx, "WebSocket", stream_id, msg).await
  {
    streams.lock().await.remove(stream_id);
  }
}

/// Hands one chunk of a streamed request body to the backend request it
/// belongs to, without letting a slow consumer stall the tunnel.
///
/// The lock is released before the send, and the send is bounded. A consumer
/// that cannot take the chunk in time has its upload *failed* rather than
/// silently truncated: the error travels down the same channel as the body, so
/// the backend request ends with an error instead of a body that looks
/// complete and is not.
async fn feed_request_chunk(
  streams: &Arc<Mutex<HashMap<String, RequestBodyFeeder>>>,
  id: &str,
  bytes: bytes::Bytes,
) {
  let feeder = {
    let map = streams.lock().await;
    match map.get(id) {
      Some(feeder) => feeder.clone(),
      None => return,
    }
  };
  // Fast path: room in the buffer, nothing to wait for.
  match feeder.try_send(Ok(bytes)) {
    Ok(()) => return,
    Err(mpsc::error::TrySendError::Closed(_)) => {
      streams.lock().await.remove(id);
      return;
    }
    Err(mpsc::error::TrySendError::Full(chunk)) => {
      if tokio::time::timeout(STREAM_STALL_BUDGET, feeder.send(chunk))
        .await
        .is_ok()
      {
        return;
      }
    }
  }
  warn!(
    "Upload {} stalled: the backend did not read it for {}s, failing that request rather than the tunnel",
    id,
    STREAM_STALL_BUDGET.as_secs()
  );
  // Best effort: the channel is full by definition here, so this only lands
  // once the consumer takes one more chunk. When it never does, dropping the
  // feeder below ends the body anyway, and the request fails on its own
  // content-length check.
  let _ = feeder.try_send(Err(std::io::Error::other(
    "upload abandoned: the backend stopped reading the request body",
  )));
  streams.lock().await.remove(id);
}

/// How busy one service's pool of connections is.
///
/// The peak matters rather than the instant reading: the supervisor ticks
/// every couple of seconds and a burst that fits entirely between two ticks is
/// exactly the burst worth growing for. `take_peak` reads and resets, so each
/// tick sees the window it is deciding about and nothing older.
#[derive(Default, Debug)]
pub(crate) struct PoolLoad {
  inflight: AtomicUsize,
  peak: AtomicUsize,
  /// Connections the elastic supervisor currently has open, `0` for a fixed
  /// pool that has no supervisor to report it.
  open: AtomicU32,
}

impl PoolLoad {
  /// Records the pool's size, for the announcement each connection makes.
  pub(crate) fn set_open(&self, n: u32) {
    self.open.store(n, Ordering::Relaxed);
  }

  /// The pool's size, or `None` when nothing is managing one.
  pub(crate) fn open(&self) -> Option<u32> {
    match self.open.load(Ordering::Relaxed) {
      0 => None,
      n => Some(n),
    }
  }

  /// Counts a request in, keeping the window's high-water mark.
  pub(crate) fn enter(&self) {
    let now = self.inflight.fetch_add(1, Ordering::Relaxed) + 1;
    self.peak.fetch_max(now, Ordering::Relaxed);
  }

  pub(crate) fn leave(&self) {
    self.inflight.fetch_sub(1, Ordering::Relaxed);
  }

  /// The window's high-water mark, resetting it to what is in flight right
  /// now. Not to zero: a request that has been running across the tick
  /// boundary is still occupying the pool, and starting the next window at
  /// zero would report an idle pool while it is anything but.
  pub(crate) fn take_peak(&self) -> usize {
    let current = self.inflight.load(Ordering::Relaxed);
    self.peak.swap(current, Ordering::Relaxed)
  }
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
  /// Backend resilience for this service: retry policy and circuit breaker,
  /// resolved from the entry with the top-level values as the fallback.
  /// Seconds a config reload gives this service's in-flight requests.
  pub(crate) reload_drain_secs: u64,
  pub(crate) retry_attempts: u32,
  pub(crate) retry_backoff_ms: u64,
  pub(crate) retry_all_methods: bool,
  pub(crate) breaker_failures: u32,
  pub(crate) breaker_open_for_secs: u64,
  /// Largest request body, in bytes, visitors may upload to this service
  /// (announced via Ping; the server answers bigger uploads with an early
  /// 413 before they enter the tunnel; None = only the server's limit).
  pub(crate) max_request_body: Option<u64>,
  /// Per-service override of the server's gateway response timeout, in seconds
  /// (announced via Ping; None = the server's global value applies).
  pub(crate) response_timeout: Option<u64>,
  pub(crate) timeout_secs: u64,
  pub(crate) max_concurrent: Option<u32>,
  /// Move the announced concurrency with backend pressure (#65).
  pub(crate) adaptive_concurrency: bool,
  /// Most parallel tunnel connections for this service. The supervisor spawns
  /// one service task per connection, each with a derived client id.
  pub(crate) connections: u32,
  /// Connections opened at startup and never retired. Equal to `connections`
  /// for a fixed pool; lower for an elastic one, where the supervisor opens
  /// this many and grows towards `connections` under load.
  pub(crate) connections_min: u32,
  /// Static Prometheus labels announced for this service's metric series.
  pub(crate) metrics_labels: std::collections::BTreeMap<String, String>,
  /// Seconds this service waits before opening its tunnel.
  pub(crate) startup_delay: u64,
  /// Service names that must have a live tunnel before this one opens its own.
  pub(crate) depends_on: Vec<String>,
  /// Seconds to wait for the TCP connection to this backend (None = only
  /// `timeout_secs` applies).
  pub(crate) connect_timeout: Option<u64>,
  /// Lowest TLS version accepted from an `https://` backend.
  pub(crate) min_tls_version: Option<String>,
  /// Requests in flight across this service's whole pool, shared by every one
  /// of its connections because `ServiceSpec` is cloned per connection and the
  /// `Arc` comes along. This is what the elastic supervisor reads; a config
  /// reload rebuilds the specs and so starts the measurement over, which is
  /// right, the pool it describes is a new one.
  pub(crate) pool_load: std::sync::Arc<PoolLoad>,
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
  /// False when this service asked not to be recorded for the dashboard's
  /// request inspector (`capture: false`). Announced in every heartbeat, so
  /// the server can skip the capture for this service's traffic.
  pub(crate) capture: bool,
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
  /// OTLP exports waiting to be carried to the server on a tunnel, when the
  /// bridge is configured with `transport: tunnel`. One queue for the
  /// process: any live connection can carry an export, and the first one to
  /// take it wins, which is what makes this survive a service reconnecting.
  pub(crate) otel_exports: Option<crate::otel_bridge::Queue>,
  /// Services in this process that currently have a live tunnel, for
  /// `depends_on`. A watch rather than a notify: a dependent that starts late
  /// has to see the state as it already is, not wait for the next change.
  pub(crate) ready_services: watch::Sender<std::collections::HashSet<String>>,
}

/// Longest a service waits for its `depends_on` before opening anyway.
///
/// A bound rather than a wait: a dependency that never arrives, because it is
/// misspelled, or removed, or itself waiting on something, must not keep a
/// service that could be serving traffic off the air forever. The wait is a
/// convenience for ordering, not a correctness mechanism.
pub(crate) const DEPENDS_ON_GRACE: Duration = Duration::from_secs(60);

/// Waits until every named service has a live tunnel, or the grace period
/// expires. Returns the names it gave up on, for the caller to report.
pub(crate) async fn await_dependencies(shared: &Shared, names: &[String]) -> Vec<String> {
  if names.is_empty() {
    return Vec::new();
  }
  let mut rx = shared.ready_services.subscribe();
  let deadline = tokio::time::Instant::now() + DEPENDS_ON_GRACE;
  loop {
    let missing: Vec<String> = {
      let ready = rx.borrow_and_update();
      names
        .iter()
        .filter(|n| !ready.contains(n.as_str()))
        .cloned()
        .collect()
    };
    if missing.is_empty() {
      return Vec::new();
    }
    if tokio::time::timeout_at(deadline, rx.changed())
      .await
      .is_err()
    {
      return missing;
    }
  }
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
  drain_inflight_for(shared, Duration::from_secs(30)).await
}

/// Waits for in-flight requests to finish, giving up after `budget`.
///
/// Used with a long budget by process shutdown and a short one by a config
/// reload, where the point is to finish what is in flight without holding the
/// new configuration back for a stalled request.
async fn drain_inflight_for(shared: &Shared, budget: Duration) {
  if budget.is_zero() {
    return;
  }
  let deadline = Instant::now() + budget;
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

/// Why the socket loop is being ended from outside it.
///
/// The channel used to carry `()`, and the receiving end logged every wake-up
/// as a liveness timeout. Three quite different things arrive on it, so a
/// configuration reload and an elastic pool giving a connection back both
/// reported a heartbeat failure that had not happened, in a warning, which is
/// the worst way to learn that something worked as designed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbortReason {
  /// The supervisor asked for this connection to end: a config reload, a
  /// shutdown, or an elastic pool retiring it because the load dropped.
  Requested,
  /// No Pong inside the liveness window; the link is presumed gone.
  Liveness,
}

/// Drains the outgoing queue onto the tunnel socket until the socket fails or
/// the connection is asked to finish.
///
/// Extracted from the connection loop so the one decision it makes can be
/// tested: what happens to messages that are already queued when the
/// connection ends. It used to be aborted, and a response reaches this queue
/// *before* the request task decrements the in-flight counter that a drain
/// waits on. So a configuration reload could pass its drain, abort the
/// writer, and drop a response the visitor was owed, which is precisely what
/// the drain was added to prevent.
///
/// `finish` asks for "send what is queued, then stop", not "stop now": the
/// select below is biased so a queued message always wins the race with it.
pub(crate) async fn run_writer<S>(
  mut sink: S,
  mut queue: mpsc::Receiver<Message>,
  finish: tokio::sync::oneshot::Receiver<()>,
  compress_out: Arc<AtomicBool>,
) where
  S: futures_util::SinkExt<Message> + Unpin,
  <S as futures_util::Sink<Message>>::Error: std::fmt::Debug,
{
  let mut finish = finish;
  let transform = |msg: Message| match msg {
    Message::Text(t) if compress_out.load(Ordering::SeqCst) => {
      Message::Binary(compress_frame(&t).into())
    }
    // A full-response frame carries a body that used to travel inside a text
    // frame and be compressed with it. Compressed here rather than where it
    // is built, so the negotiated flag stays in one place, and only when
    // deflating wins: for an already-compressed body it does not, and the
    // frame goes out as it is.
    Message::Binary(b)
      if compress_out.load(Ordering::SeqCst) && b.first() == Some(&FRAME_RESPONSE_FULL) =>
    {
      match decode_binary_frame(&b) {
        Some((_, id, payload)) => match crate::protocol::deflate_payload(payload) {
          Some(deflated) => match encode_binary_frame(FRAME_RESPONSE_FULL_ZLIB, id, &deflated) {
            Some(frame) => Message::Binary(frame.into()),
            None => Message::Binary(b),
          },
          None => Message::Binary(b),
        },
        None => Message::Binary(b),
      }
    }
    other => other,
  };
  // Everything already queued behind a message rides the same flush: at bulk
  // throughput each message used to pay its own (a syscall per frame), and
  // the messages are already whole frames, so batching them costs no latency.
  'writer: loop {
    let next_msg = tokio::select! {
      biased;
      msg = queue.recv() => msg,
      _ = &mut finish => None,
    };
    let Some(msg) = next_msg else {
      break 'writer;
    };
    let mut msg = transform(msg);
    while let Ok(next) = queue.try_recv() {
      if let Err(e) = sink.feed(msg).await {
        error!("Error writing to server socket: {:?}", e);
        break 'writer;
      }
      msg = transform(next);
    }
    if let Err(e) = sink.send(msg).await {
      error!("Error writing to server socket: {:?}", e);
      break 'writer;
    }
  }
  // Whatever the loop stopped for, the socket's own buffer may still hold
  // bytes that were fed but never flushed.
  let _ = sink.flush().await;
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

  /// What the server has announced so far, without waiting. `None` before the
  /// first connection has learned anything, or against a server too old to
  /// announce at all.
  pub(crate) fn permitted(&self) -> Option<u32> {
    *self.rx.borrow()
  }
}

/// Ceiling on a service's candidate server list, configured plus learned.
///
/// A fence rather than a policy: the list is tried in rotation, so a server
/// announcing a hundred alternates would turn every reconnect into a long walk
/// through addresses nobody chose.
const MAX_SERVER_URLS: usize = 16;

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

  // Lifecycle gates, before anything is dialed. Only the first connection of a
  // pool waits: the others are the same service, and making each of them sit
  // through the same delay would turn a five-second stagger into a
  // five-second-per-connection one.
  if connection_index == 1 {
    if !spec.depends_on.is_empty() {
      let missing = await_dependencies(&shared, &spec.depends_on).await;
      if !missing.is_empty() {
        warn!(
          "[{}] depends_on: {} did not come up within {}s; starting anyway",
          label,
          missing.join(", "),
          DEPENDS_ON_GRACE.as_secs()
        );
      }
    }
    if spec.startup_delay > 0 {
      info!(
        "[{}] startup_delay: waiting {}s before opening the tunnel",
        label, spec.startup_delay
      );
      tokio::time::sleep(Duration::from_secs(spec.startup_delay)).await;
    }
  }

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
    let absolute = health_path.starts_with("http://") || health_path.starts_with("https://");
    // An h2c/h2 target speaks HTTP/2 with prior knowledge and routes by gRPC
    // method name, so the plain GET below cannot reach it: the probe uses the
    // standard `grpc.health.v1.Health/Check` RPC instead, and the configured
    // value names the gRPC service to ask about (`/` = the server as a
    // whole). An absolute URL still means "probe this over ordinary HTTP",
    // which is the escape hatch for a backend exposing a health endpoint on a
    // separate port.
    let grpc_service = (!absolute && crate::proxy::h2::is_h2_target(&spec.target))
      .then(|| health_path.trim_matches('/').to_string());
    let health_url = if absolute {
      health_path.clone()
    } else {
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
    // Built once, outside the loop, like the HTTP probe client.
    let grpc_client = grpc_service
      .is_some()
      .then(|| crate::proxy::h2::build_h2_client(&spec.target, spec.min_tls_version.as_deref()))
      .flatten();
    let grpc_target = spec.target.clone();
    let flag = backend_healthy.clone();
    // Health checks never follow redirects: a 3xx to some other page must
    // not let a broken backend look healthy via the redirect target.
    let probe_client = reqwest::Client::builder()
      .tcp_nodelay(true)
      .timeout(Duration::from_secs(spec.health_timeout))
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .unwrap_or_else(|e| {
        error!("Failed to build the health-probe HTTP client: {e}; using a client without a timeout");
        reqwest::Client::new()
      });
    let (interval, threshold) = (spec.health_interval, spec.health_threshold);
    let probe_timeout = Duration::from_secs(spec.health_timeout);
    let what = match grpc_service.as_deref() {
      Some("") => format!("gRPC health of {} (whole server)", grpc_target),
      Some(svc) => format!("gRPC health of {} service {}", grpc_target, svc),
      None => health_url.clone(),
    };
    info!(
      "[{}] Backend health check: {} (every {}s, timeout {}s, threshold {})",
      label, what, interval, spec.health_timeout, threshold
    );
    let health_url_log = what;
    tokio::spawn(async move {
      let mut consecutive_failures: u32 = 0;
      let mut first_result = true;
      // Probe immediately, then on the interval: a backend that is already
      // down when the client starts is reported after threshold probes
      // instead of sitting falsely healthy for a full extra interval. The
      // client also starts out-of-routing (unhealthy) until this first probe
      // lands, so the very first success is what makes the backend routable.
      loop {
        let ok = match (&grpc_client, &grpc_service) {
          (Some(client), Some(service)) => {
            crate::proxy::h2::grpc_health_check(client, &grpc_target, service, probe_timeout).await
          }
          // An h2 target whose client could not be built cannot be probed;
          // reporting it healthy would route traffic at a backend nothing has
          // checked, so it stays unhealthy and says so through the log line
          // the failure branch already writes.
          (None, Some(_)) => false,
          _ => matches!(
            probe_client.get(&health_url).send().await,
            Ok(resp) if resp.status().is_success()
          ),
        };
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
              info!("Backend healthy: {}, now routable", health_url_log);
            } else {
              info!("Backend health restored: {}", health_url_log);
            }
          }
        } else {
          consecutive_failures = consecutive_failures.saturating_add(1);
          if consecutive_failures >= threshold && flag.swap(false, Ordering::SeqCst) {
            health_changed.notify_waiters();
            warn!(
              "Backend health check failed {} consecutive time(s): {}, reporting unhealthy to the server (tunnel stays connected)",
              consecutive_failures, health_url_log
            );
          } else if first_result {
            // Started unhealthy and the first probe also failed: make it clear
            // why the backend is not yet routable (the threshold warning above
            // only fires on a healthy→unhealthy transition).
            info!(
              "Backend not healthy yet: {}, staying out of routing until a probe passes",
              health_url_log
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

  // Adaptive concurrency (#65): the announced number follows backend
  // pressure. It needs the local limiter, because the evidence is how long
  // requests wait for one of its permits, and it is that number being moved.
  let adaptive = match (
    spec.adaptive_concurrency,
    &local_limiter,
    spec.max_concurrent,
  ) {
    (true, Some(limiter), Some(configured)) => {
      let adaptive = Arc::new(crate::adaptive::Adaptive::new(configured, limiter.clone()));
      crate::adaptive::spawn(adaptive.clone(), label.clone());
      Some(adaptive)
    }
    (true, _, _) => {
      warn!(
        "[{}] adaptive_concurrency needs max_concurrent to be set; there is no number to move",
        label
      );
      None
    }
    _ => None,
  };

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
  // This connection's candidate servers: the configured list, plus whatever
  // the servers on it announce. Owned here rather than on the spec because it
  // grows at runtime and a config reload rebuilds the spec, which is the right
  // moment to forget what was learned.
  let mut ws_urls: Vec<String> = spec.ws_urls.clone();
  // Self-reported health for this connection: the ping task fills it in, the
  // read loop times the pongs, and the reconnect counter lives across
  // attempts, which is the point of it.
  let health_report = Arc::new(crate::health_report::HealthReport::default());
  let mut connected_once = false;
  'outer: loop {
    if *cancel.borrow() {
      break;
    }
    exit_if_shutting_down(&shared).await;

    let current_ws = ws_urls
      .get(server_idx % ws_urls.len().max(1))
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
        // Built from the default rather than as a literal: the config struct
        // is non-exhaustive, so its future fields keep their own defaults.
        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(spec.max_message_size);
        ws_config.max_frame_size = Some(spec.max_message_size);
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
            // Servers this one says a client may fall back to
            // (planned_features #52). Appended after the ones this client's
            // own config names, never replacing them: the operator's list
            // decides the order, and this is advice from one of the servers on
            // it. Learned per connection, so a migration set up on the server
            // reaches a running client without a restart.
            //
            // The rotation is round-robin and wraps, so an alternate is never
            // a one-way door: a client that failed over keeps coming back to
            // try the primary, and a server that was briefly restarting gets
            // its clients back on the next pass.
            if let Some(learned) = response
              .headers()
              .get("x-aperio-alternate-servers")
              .and_then(|v| v.to_str().ok())
            {
              for url in learned.split(',').map(str::trim) {
                if (url.starts_with("ws://") || url.starts_with("wss://"))
                  && !ws_urls.iter().any(|u| u == url)
                  && ws_urls.len() < MAX_SERVER_URLS
                {
                  info!("[{}] Server announced an alternate: {}", label, url);
                  ws_urls.push(url.to_string());
                }
              }
            }
            let connected_at = Instant::now();
            // Announce this service to anything waiting on it via
            // `depends_on`. Keyed by service name, so every connection of a
            // parallel pool announces the same name and the first one to
            // connect is enough.
            if let Some(name) = spec.name.as_deref() {
              shared
                .ready_services
                .send_if_modified(|set| set.insert(name.to_string()));
            }
            // Every established connection after the first is a reconnect,
            // and the count is what tells a flapping link from a quiet one:
            // two clients both answering pings look identical otherwise.
            if connected_once {
              health_report.reconnected();
            }
            connected_once = true;
            let (ws_sender, mut ws_receiver) = ws_stream.split();

            // Channel to write messages to the WebSocket
            let (tx_write, rx_write) = mpsc::channel::<Message>(100);

            // OTel bridge, tunnel transport: one task per connection drains the
            // process-wide queue onto this socket. The queue is behind a mutex,
            // so exactly one live connection holds it; when this one ends the
            // lock is released and the next connection picks the queue up where
            // it was left, which is what makes exports survive a reconnect.
            let otel_task = shared.otel_exports.clone().map(|queue| {
              let tx = tx_write.clone();
              tokio::spawn(async move {
                use base64::Engine;
                let mut rx = queue.lock().await;
                while let Some(export) = rx.recv().await {
                  let msg = TunnelMessage::OtlpExport {
                    signal: export.signal.to_string(),
                    data: base64::engine::general_purpose::STANDARD.encode(&export.payload),
                  };
                  let Ok(json) = serde_json::to_string(&msg) else {
                    continue;
                  };
                  if tx.send(Message::Text(json.into())).await.is_err() {
                    break;
                  }
                }
              })
            });

            // Ends the socket loop from outside it, saying why.
            let (abort_tx, mut abort_rx) = mpsc::channel::<AbortReason>(1);

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

            // Spawn task to handle WebSocket writes.
            let compress_out_writer = compress_out.clone();
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
            let mut writer_task = tokio::spawn(run_writer(
              ws_sender,
              rx_write,
              finish_rx,
              compress_out_writer,
            ));

            // Spawn task for heartbeat (Ping every 5 seconds & liveness check)
            // Every connection subscribes with the full filter set. The
            // server collapses the copies to one per client process, and a
            // connection that never subscribed would leave the process deaf
            // the moment its sibling dropped.
            {
              let bus = shared.messages.clone();
              let tx = tx_write.clone();
              // This connection's own id, not the service label: a service
              // with `connections: N` shares one label across N connections,
              // and the bus keys its writers by connection.
              let connection_id = spec.client_id.clone();
              tokio::spawn(async move {
                bus.attach(&connection_id, tx.clone()).await;
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
            let self_health_ping = health_report.clone();
            let shared_ping = shared.clone();
            let reload_drain_ping = Duration::from_secs(spec.reload_drain_secs);
            let service_name_ping = spec.name.clone();
            let service_custom_name_ping = spec.custom_name.clone();
            let tunnels_ping = spec.tunnels.clone();
            let visitor_auth_ping = spec.visitor_auth.clone();
            let allowed_ips_ping = spec.allowed_ips.clone();
            let no_capture = !spec.capture;
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
            // What the pool is running right now, not what it may grow to: the
            // dashboard reads this as "this service has N connections", and an
            // elastic pool sitting at its floor would otherwise claim its
            // ceiling and look like connections had gone missing. Read inside
            // the heartbeat below, because an elastic pool moves: taking it
            // once here reported the size the pool happened to be when this
            // connection opened, for as long as the connection lived.
            let pool_load_ping = spec.pool_load.clone();
            let connections_configured = spec.connections;
            // Only meaningful as a range; a fixed `connections: N` announces
            // nothing rather than a min and max that are the same number.
            let (connections_min_ping, connections_max_ping) =
              if spec.connections_min < spec.connections {
                (Some(spec.connections_min), Some(spec.connections))
              } else {
                (None, None)
              };
            let metrics_labels_ping = spec.metrics_labels.clone();
            // What this service will actually take right now, which is the
            // configured number until adaptive concurrency lowers it.
            let adaptive_ping = adaptive.clone();
            let drain_secs_ping = Some(spec.reload_drain_secs);
            let config_notes_ping = spec.config_notes.clone();
            let scaling_ping = spec.scaling.clone();

            let ping_task = tokio::spawn(async move {
              // The first Ping goes out immediately: it announces the binds,
              // version/protocol, and health before any traffic is routed.
              loop {
                // The supervisor asked for this connection to end: a config
                // reload, a shutdown, or an elastic pool giving it back. The
                // cancel signal does not say which, and neither do these
                // lines: whichever of the three it was has already logged its
                // own reason, and guessing here is how a pool retirement came
                // to announce a configuration change that never happened.
                if *cancel_ping.borrow() {
                  // Announce the drain before dropping the socket. Without
                  // this, ending a connection killed whatever was in flight:
                  // the visitor saw a failure caused by a change that was
                  // meant to be invisible to them. `Draining` stops the
                  // server dispatching anything new here, which is what makes
                  // the wait below terminate rather than chase a moving
                  // target.
                  if reload_drain_ping.is_zero() {
                    info!("Closing this connection...");
                  } else {
                    info!("Draining before closing this connection...");
                    if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                      let _ = tx_ping.send(Message::Text(json.into())).await;
                    }
                    drain_inflight_for(&shared_ping, reload_drain_ping).await;
                  }
                  let _ = abort_tx_ping.send(AbortReason::Requested).await;
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
                  let _ = abort_tx_ping.send(AbortReason::Liveness).await;
                  break;
                }

                // One read, so the pair in this heartbeat is one observation.
                let reported_health = health_ping.report();
                // Likewise for the self-reported figures: everything in this
                // heartbeat describes the same moment.
                let (rtt_ms, jitter_ms, reconnects) = self_health_ping.link();
                let ping_msg = TunnelMessage::Ping {
                  cpu_percent: self_health_ping.cpu_percent(),
                  rss_bytes: crate::health_report::rss_bytes(),
                  rtt_ms,
                  jitter_ms,
                  reconnects: Some(reconnects),
                  client_id: client_id_ping.clone(),
                  timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                  path_bind: path_bind_ping.clone(),
                  hostname_bind: hostname_bind_ping.clone(),
                  hostname_binds: hostnames_ping.clone(),
                  // Not the configured number: what this service will take
                  // right now, which adaptive concurrency may have lowered.
                  max_concurrent: adaptive_ping
                    .as_ref()
                    .map(|a| a.announced())
                    .or(max_concurrent),
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
                  no_capture,
                  max_request_body: max_request_body_ping,
                  response_timeout: response_timeout_ping,
                  client_key: client_key_ping.clone(),
                  webhook_inbox: webhook_inbox_ping,
                  denied: denied_ping.clone(),
                  scaling: scaling_ping.clone(),
                  connections: Some(pool_load_ping.open().unwrap_or(connections_configured)),
                  connections_min: connections_min_ping,
                  connections_max: connections_max_ping,
                  metrics_labels: metrics_labels_ping.clone(),
                  drain_secs: drain_secs_ping,
                  config_notes: config_notes_ping.clone(),
                };
                if let Ok(ping_str) = serde_json::to_string(&ping_msg) {
                  // Timed from the moment it is queued, which is the same
                  // queue every other frame waits in: a round trip that
                  // excluded the writer's backlog would report the link as
                  // healthy while the connection was the thing falling behind.
                  self_health_ping.ping_sent();
                  if tx_ping.send(Message::Text(ping_str.into())).await.is_err() {
                    break;
                  }
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
            let mut builder = reqwest::Client::builder()
              .redirect(crate::proxy::http::redirect_policy(spec.max_redirects))
              .timeout(Duration::from_secs(spec.timeout_secs));
            // Connect and whole-request budgets are different questions: one
            // is "is this host reachable", the other "is this backend slow".
            // Unset leaves the single budget covering both, which is what this
            // always did.
            if let Some(secs) = spec.connect_timeout {
              builder = builder.connect_timeout(Duration::from_secs(secs));
            }
            // Validated by `build_specs` before any service is spawned, on
            // the first load and on every reload, so an unusable value never
            // reaches here. If one somehow does, the floor is dropped rather
            // than the process: killing every other service of this client
            // over one field is the failure a reload is meant to prevent.
            match crate::proxy::http::tls_floor(spec.min_tls_version.as_deref()) {
              Ok(Some(floor)) => builder = builder.min_tls_version(floor),
              Ok(None) => {}
              Err(e) => error!("{e}; continuing without a TLS floor for this backend"),
            }
            let reqwest_client = builder
              // Same reasoning as the tunnel socket: these are request and
              // response messages on a loopback or LAN hop, and holding one
              // back for Nagle is latency on a request a visitor is waiting
              // for.
              .tcp_nodelay(true)
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
              h2_client: crate::proxy::h2::build_h2_client(
                &spec.target,
                spec.min_tls_version.as_deref(),
              )
              .map(Arc::new),
              unix_socket: crate::proxy::unix::unix_socket_path(&spec.target),
              timeout_secs: spec.timeout_secs,
              // One breaker per service connection, shared by every request
              // it serves: a breaker that could not see the other requests'
              // failures would never trip.
              resilience: crate::proxy::http::BackendResilience::new(
                spec.retry_attempts,
                spec.retry_backoff_ms,
                spec.retry_all_methods,
                spec.breaker_failures,
                spec.breaker_open_for_secs,
              ),
              target: spec.target.clone(),
              // Parsed once here rather than per request. `None` keeps the
              // answer the request path used to give for a target that is not
              // a URL: 502, a configuration error, not the visitor's fault.
              target_url: url::Url::parse(&spec.target).ok(),
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
            // Set when this connection is ended deliberately, so the line
            // below the loop reports a close rather than a loss.
            let mut closed_on_request = false;
            loop {
              tokio::select! {
                  reason = abort_rx.recv() => {
                      match reason {
                          Some(AbortReason::Liveness) => {
                              warn!("Liveness timeout triggered. Aborting socket loop.");
                          }
                          // A reload, a shutdown or an elastic pool giving
                          // this connection back. Nothing failed.
                          _ => {
                              closed_on_request = true;
                              debug!("[{}] Closing the socket loop on request.", label);
                          }
                      }
                      break;
                  }
                  _ = shutdown_requested(&shared) => {
                      // Announce drain, let in-flight requests finish, then exit.
                      if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                          let _ = tx_write.send(Message::Text(json.into())).await;
                      }
                      drain_inflight(&shared).await;
                      // Give the Draining frame a moment to flush before closing.
                      tokio::time::sleep(Duration::from_millis(200)).await;
                      std::process::exit(0);
                  }
                  msg_res = ws_receiver.next() => {
                      match msg_res {
                          Some(Ok(msg)) => {
                              // A frame yields the envelope text and, for a v6
                              // full-request frame, the body that travelled with it
                              // as bytes rather than base64.
                              let mut frame_body: Option<Vec<u8>> = None;
                              let text_opt = match msg {
                                  Message::Text(t) => Some(t.to_string()),
                                  Message::Binary(b) => {
                                      // v2 binary chunk frames carry a tag byte that never
                                      // collides with zlib streams (0x78).
                                      // Payloads are the tail of the frame and
                                      // the frame is refcounted, so each of
                                      // these is a slice rather than a copy
                                      // (planned_features #42).
                                      match decode_binary_frame(&b) {
                                          Some((FRAME_REQUEST_CHUNK, fid, payload)) => {
                                              let payload = b.slice(b.len() - payload.len()..);
                                              feed_request_chunk(&active_request_streams, fid, payload).await;
                                              None
                                          }
                                          // v7: relay payloads as raw bytes, the same
                                          // deliveries their JSON shapes make below.
                                          Some((crate::protocol::FRAME_TCP_DATA, sid, payload)) => {
                                              deliver_tcp_bytes(&active_tcp_streams, sid, b.slice(b.len() - payload.len()..)).await;
                                              None
                                          }
                                          Some((crate::protocol::FRAME_UDP_DATAGRAM, sid, payload)) => {
                                              deliver_udp_bytes(&active_udp_streams, sid, b.slice(b.len() - payload.len()..)).await;
                                              None
                                          }
                                          Some((crate::protocol::FRAME_WS_DATA_BIN, sid, payload)) => {
                                              deliver_ws_frame(&active_ws_streams, sid, Message::Binary(b.slice(b.len() - payload.len()..))).await;
                                              None
                                          }
                                          // v6: envelope and buffered body in one frame,
                                          // deflated by the server's writer when this
                                          // connection negotiated compression.
                                          Some((tag @ (FRAME_REQUEST_FULL | FRAME_REQUEST_FULL_ZLIB), _, payload)) => {
                                              let max = spec.max_message_size.saturating_mul(4);
                                              let inflated = if tag == FRAME_REQUEST_FULL_ZLIB {
                                                  crate::protocol::inflate_payload(payload, max)
                                              } else {
                                                  None
                                              };
                                              let payload = inflated.as_deref().unwrap_or(payload);
                                              match split_full_response(payload) {
                                                  Some((json, body)) => {
                                                      frame_body = Some(body.to_vec());
                                                      Some(json.to_string())
                                                  }
                                                  None => {
                                                      warn!("Dropped a malformed full-request frame");
                                                      None
                                                  }
                                              }
                                          }
                                          _ => decompress_frame(&b, spec.max_message_size.saturating_mul(4)),
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
                                              let raw_body = frame_body.take();
                                              let pool = spec.pool_load.clone();
                                              inflight.fetch_add(1, Ordering::SeqCst);
                                              pool.enter();
                                              shared.mark_request_activity();

                                              // Handle incoming request concurrently
                                              let adaptive_for_task = adaptive.clone();
                                              tokio::spawn(async move {
                                                  // Local concurrency guard: even a misbehaving server
                                                  // cannot push more parallel work onto the backend.
                                                  // How long this waits is the evidence adaptive
                                                  // concurrency reads: a queue here means the backend
                                                  // is behind, whatever the host's CPU says.
                                                  let waiting = Instant::now();
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
                                                  if let Some(a) = &adaptive_for_task {
                                                      a.record_wait(waiting.elapsed());
                                                  }
                                                  let peer = proto.load(Ordering::Relaxed);
                                                  let binary = peer >= 2;
                                                  // v5: a buffered response goes out as one frame,
                                                  // envelope and body, instead of base64 in JSON.
                                                  let full_body = peer >= 5;
                                                  let response = handle_incoming_request(
                                                      &ctx,
                                                      ForwardRequest { id, method, uri, headers, body, raw_body },
                                                      None,
                                                      binary,
                                                      full_body,
                                                  )
                                                  .await;

                                                  // None = the response was streamed through the tunnel already.
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = ctx.tunnel_tx.send(Message::Text(resp_str.into())).await;
                                                  }
                                                  inflight.fetch_sub(1, Ordering::SeqCst);
                                                  pool.leave();
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
                                                  mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
                                              active_request_streams.lock().await.insert(id.clone(), body_tx);
                                              let ctx = forward_ctx.clone();
                                              let limiter = local_limiter.clone();
                                              let inflight = shared.inflight_requests.clone();
                                              let streams = active_request_streams.clone();
                                              let proto = server_protocol.clone();
                                              let pool = spec.pool_load.clone();
                                              inflight.fetch_add(1, Ordering::SeqCst);
                                              pool.enter();
                                              let adaptive_for_task = adaptive.clone();
                                              tokio::spawn(async move {
                                                  let waiting = Instant::now();
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
                                                  if let Some(a) = &adaptive_for_task {
                                                      a.record_wait(waiting.elapsed());
                                                  }
                                                  let peer = proto.load(Ordering::Relaxed);
                                                  let binary = peer >= 2;
                                                  // v5: a buffered response goes out as one frame,
                                                  // envelope and body, instead of base64 in JSON.
                                                  let full_body = peer >= 5;
                                                  let response = handle_incoming_request(
                                                      &ctx,
                                                      ForwardRequest {
                                                          id: id.clone(),
                                                          method,
                                                          uri,
                                                          headers,
                                                          body: None,
                                                          raw_body: None,
                                                      },
                                                      Some(body_rx),
                                                      binary,
                                                      full_body,
                                                  )
                                                  .await;
                                                  streams.lock().await.remove(&id);
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = ctx.tunnel_tx.send(Message::Text(resp_str.into())).await;
                                                  }
                                                  inflight.fetch_sub(1, Ordering::SeqCst);
                                                  pool.leave();
                                              });
                                          }
                                          TunnelMessage::RequestChunk { id, data } => {
                                              // Base64 fallback path; v2 servers send binary frames.
                                              match BASE64_STANDARD.decode(&data) {
                                                  Ok(bytes) => {
                                                      feed_request_chunk(&active_request_streams, &id, bytes.into()).await;
                                                  }
                                                  Err(_) => warn!(
                                                      "Failed to decode Base64 RequestChunk for {}",
                                                      id
                                                  ),
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
                                              // The peer's version decides how this stream's binary
                                              // frames travel back.
                                              let peer = server_protocol.load(Ordering::Relaxed);

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
                                                      peer,
                                                  )
                                                  .await;
                                              });
                                          }
                                          TunnelMessage::WsData {
                                              stream_id,
                                              data,
                                              is_text,
                                          } => {
                                              // Forward from tunnel → backend WS with the bounded
                                              // hand-off: the map is released first, and a consumer
                                              // that cannot take the frame within the budget loses its
                                              // own stream. Awaiting it without a bound would let one
                                              // backend that stopped reading wedge the read loop, which
                                              // also carries Pong, and take every stream on this
                                              // connection down with it.
                                              let ws_msg = if is_text {
                                                  Message::Text(data.into())
                                              } else {
                                                  // Base64 fallback; a v7 server sends
                                                  // FRAME_WS_DATA_BIN frames.
                                                  match BASE64_STANDARD.decode(&data) {
                                                      Ok(bytes) => Message::Binary(bytes.into()),
                                                      Err(_) => {
                                                          warn!("Failed to decode Base64 WsData for stream {}", stream_id);
                                                          continue;
                                                      }
                                                  }
                                              };
                                              deliver_ws_frame(&active_ws_streams, &stream_id, ws_msg).await;
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
                                          TunnelMessage::TcpOpen { stream_id, target, visitor } => {
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
                                                      .map(|d| (d.target.clone(), d.encrypt, d.psk.clone(), d.proxy_protocol)),
                                                  None => spec.tcp_target.clone().map(|t| (t, false, None, false)),
                                              };
                                              match resolved {
                                                  Some((target_addr, encrypt, psk, proxy_protocol)) => {
                                                      // Register the stream handle synchronously, BEFORE
                                                      // spawning: TcpData for this stream can arrive on the
                                                      // very next tunnel frame and would be dropped if the
                                                      // spawned task had not registered yet. The channel
                                                      // buffers bytes until the backend connect completes.
                                                      let (bytes_tx, bytes_rx) = mpsc::channel::<bytes::Bytes>(64);
                                                      let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
                                                      active_tcp_streams.lock().await.insert(
                                                          stream_id.clone(),
                                                          TcpStreamHandle { tx: bytes_tx, abort_tx },
                                                      );
                                                      let tx = tx_write.clone();
                                                      let streams = active_tcp_streams.clone();
                                                      let activity = shared.activity_clock();
                                                      let pauses = stream_pauses.clone();
                                                      // The peer's version, read when the stream opens:
                                                      // it decides whether this relay's payloads travel
                                                      // as v7 binary frames or base64 in JSON.
                                                      let peer = server_protocol.load(Ordering::Relaxed);
                                                      tokio::spawn(async move {
                                                          let e2e = encrypt.then_some(crate::e2e::E2eParams { psk });
                                                          let announce = proxy_protocol.then_some(visitor).flatten();
                                                          handle_tcp_open(stream_id, target_addr, tx, streams, bytes_rx, abort_rx, e2e, activity, pauses, peer, announce).await;
                                                      });
                                                  }
                                                  None => {
                                                      match target {
                                                          Some(t) => warn!("TcpOpen for undeclared target {}; refusing", t),
                                                          None => warn!("TcpOpen received but no TCP target is configured; refusing"),
                                                      }
                                                      let close = TunnelMessage::TcpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json.into())).await;
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
                                                      let (dg_tx, dg_rx) = mpsc::channel::<bytes::Bytes>(64);
                                                      let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
                                                      active_udp_streams.lock().await.insert(
                                                          stream_id.clone(),
                                                          UdpStreamHandle { tx: dg_tx, abort_tx },
                                                      );
                                                      let tx = tx_write.clone();
                                                      let streams = active_udp_streams.clone();
                                                      let activity = shared.activity_clock();
                                                      let peer = server_protocol.load(Ordering::Relaxed);
                                                      tokio::spawn(async move {
                                                          handle_udp_open(stream_id, target_addr, tx, streams, dg_rx, abort_rx, idle_timeout, activity, peer).await;
                                                      });
                                                  }
                                                  None => {
                                                      warn!("UdpOpen for undeclared target {}; refusing", target);
                                                      let close = TunnelMessage::UdpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json.into())).await;
                                                      }
                                                  }
                                              }
                                          }
                                          TunnelMessage::UdpDatagram { stream_id, data } => {
                                              // Base64 fallback; a v7 server sends
                                              // FRAME_UDP_DATAGRAM frames.
                                              match BASE64_STANDARD.decode(&data) {
                                                  Ok(bytes) => deliver_udp_bytes(&active_udp_streams, &stream_id, bytes.into()).await,
                                                  Err(_) => warn!("Failed to decode Base64 UdpDatagram for stream {}", stream_id),
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
                                              // The bounded hand-off (see the WsData arm): a backend
                                              // that accepts the connection and then stops reading must
                                              // never wedge the tunnel read loop and starve the liveness
                                              // watchdog, but a merely slow one keeps its stream.
                                              // Base64 fallback; a v7 server sends
                                              // FRAME_TCP_DATA frames.
                                              match BASE64_STANDARD.decode(&data) {
                                                  Ok(bytes) => deliver_tcp_bytes(&active_tcp_streams, &stream_id, bytes.into()).await,
                                                  Err(_) => warn!("Failed to decode Base64 TcpData for stream {}", stream_id),
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
                                                  let _ = tx_write.send(Message::Text(json.into())).await;
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
                                              health_report.pong_received();
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

            // Cleanup tasks on connection loss.
            //
            // Asked to finish rather than aborted, so anything already queued
            // reaches the socket. Bounded, because a connection that is gone
            // will never accept the writes and this must not become the thing
            // that holds a shutdown open.
            let _ = finish_tx.send(());
            if tokio::time::timeout(Duration::from_secs(2), &mut writer_task)
              .await
              .is_err()
            {
              writer_task.abort();
            }
            // Releases the export queue for the next connection to pick up.
            if let Some(task) = otel_task {
              task.abort();
            }
            ping_task.abort();
            if closed_on_request {
              info!("[{}] Connection closed.", label);
            } else {
              warn!("[{}] Connection to server lost.", label);
            }

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
    shared.messages.detach(&spec.client_id).await;

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
    if ws_urls.len() > 1 {
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
