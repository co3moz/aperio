use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::{info, warn};

use super::*;

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
  /// This service asked to share a connection with the others that did
  /// (`multiplex: true`). What it asked for, not what it got: whether it
  /// actually shares one is `multiplex_group`, since sharing needs somebody to
  /// share with.
  pub(crate) multiplex: bool,
  /// The group of services this one is carried on a single connection with,
  /// settled by `build_specs` because that is the only place that sees every
  /// service at once.
  ///
  /// `None` covers both a service that never asked and one that asked and is
  /// alone in what it asked for, and those two collapse on purpose: a group of
  /// one is a connection carrying one service, which is what the ordinary
  /// shape already is. Announcing a one-entry list instead would change
  /// nothing on the wire except which servers can read it.
  pub(crate) multiplex_group: Option<usize>,
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
  /// The full `auth:` policy for this service, when it says more than the
  /// scalar above can carry. Announced only to a server that said it
  /// understands the methods in it (`planned_features.md` #111).
  pub(crate) visitor_auth_policy: Option<aperio_config::AuthSetting>,
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
  /// `depends_on`, counted by how many of their connections are up. A watch
  /// rather than a notify: a dependent that starts late has to see the state
  /// as it already is, not wait for the next change.
  ///
  /// Counted rather than a set of names, for two reasons that are really the
  /// same one. A service with `connections: N` announces one name from N
  /// connections, so "is it up" is "does it have any", and it was previously
  /// a set that nothing ever removed from: a service that connected once and
  /// then went away stayed ready forever, so a dependent starting after that,
  /// after a reload, say, was told its dependency was up when it was not.
  pub(crate) ready_services: watch::Sender<std::collections::HashMap<String, usize>>,
}

/// Longest a service waits for its `depends_on` before opening anyway.
///
/// A bound rather than a wait: a dependency that never arrives, because it is
/// misspelled, or removed, or itself waiting on something, must not keep a
/// service that could be serving traffic off the air forever. It orders
/// startup, and nothing more: once a service is past its gate it stays up
/// whatever its dependency does afterwards, because taking a healthy service
/// off the air over someone else's outage turns one failure into two.
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
        .filter(|n| !ready.contains_key(n.as_str()))
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
pub(crate) async fn drain_inflight(shared: &Shared) {
  drain_inflight_for(shared, Duration::from_secs(30)).await
}

/// Waits for in-flight requests to finish, giving up after `budget`.
///
/// Used with a long budget by process shutdown and a short one by a config
/// reload, where the point is to finish what is in flight without holding the
/// new configuration back for a stalled request.
pub(crate) async fn drain_inflight_for(shared: &Shared, budget: Duration) {
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
pub(crate) async fn exit_if_shutting_down(shared: &Shared) {
  if !shared.shutting_down.load(Ordering::SeqCst) {
    return;
  }
  info!("Shutdown requested while disconnected; exiting.");
  drain_inflight(shared).await;
  crate::remove_pid_file();
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

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;

/// One service, and everything this connection needs to serve it.
///
/// These were five `Vec`s walked in lockstep by the same service index:
/// `specs`, `healths`, `local_limiters`, `adaptives` and
/// `visitor_auth_policies`. Nothing said they were the same length, so
/// `run_service` opened with a runtime check that they were, and a caller that
/// got it wrong would otherwise have surfaced as an out-of-range panic six
/// hundred lines in.
///
/// One list of one struct says it instead, and says it to the compiler. The
/// three derived fields are built here from the spec, so they cannot be built
/// for a different service than the one they end up beside.
pub(crate) struct ServiceRuntime {
  pub(crate) spec: ServiceSpec,
  /// Shared with this service's other parallel connections: the backend is
  /// probed once per service, not once per connection.
  pub(crate) health: BackendHealth,
  /// Per service rather than per connection, because `max_concurrent:` is what
  /// a *backend* will take: one service's slow backend must not hold up
  /// permits another service's requests are waiting for.
  pub(crate) limiter: Option<std::sync::Arc<tokio::sync::Semaphore>>,
  /// The controller that moves `limiter` with backend pressure, when this
  /// service asked for one (#65).
  pub(crate) adaptive: Option<std::sync::Arc<crate::adaptive::Adaptive>>,
  /// The `auth:` this service was written with, negotiated against each server
  /// separately on every connect (#111).
  pub(crate) visitor_auth_policy: Option<aperio_config::AuthSetting>,
}

impl ServiceRuntime {
  /// Builds the derived state for one service. `health` comes from the
  /// supervisor rather than from here, because it is shared across the
  /// service's parallel connections and this is called once per connection.
  pub(crate) fn new(spec: ServiceSpec, health: BackendHealth) -> Self {
    let limiter = spec
      .max_concurrent
      .map(|n| std::sync::Arc::new(tokio::sync::Semaphore::new(n as usize)));
    let adaptive = match (spec.adaptive_concurrency, &limiter, spec.max_concurrent) {
      (true, Some(limiter), Some(configured)) => {
        let adaptive =
          std::sync::Arc::new(crate::adaptive::Adaptive::new(configured, limiter.clone()));
        crate::adaptive::spawn(adaptive.clone(), spec.label());
        Some(adaptive)
      }
      (true, _, _) => {
        warn!(
          "[{}] adaptive_concurrency needs max_concurrent to be set; there is no number to move",
          spec.label()
        );
        None
      }
      _ => None,
    };
    let visitor_auth_policy = spec.visitor_auth_policy.clone();
    Self {
      spec,
      health,
      limiter,
      adaptive,
      visitor_auth_policy,
    }
  }
}
