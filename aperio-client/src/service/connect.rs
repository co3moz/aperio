use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::*;

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

/// The parts of a service's heartbeat declaration that move while the
/// connection is up, so the loop that sends it knows exactly what to re-read.
///
/// Everything else a `ServiceDecl` carries is settled by the config, and a
/// config change respawns the connection rather than editing it underneath.
/// Keeping the three that do move in one value beside the templates is what
/// stops a heartbeat mixing a fresh reading of one with a stale one of another.
pub(crate) struct LiveDecl {
  /// Written by this service's backend probe, read as a pair.
  pub(crate) health: BackendHealth,
  /// The number adaptive concurrency has arrived at, when it is running.
  pub(crate) adaptive: Option<Arc<crate::adaptive::Adaptive>>,
  /// How deep this service's connection pool is right now.
  pub(crate) pool: std::sync::Arc<PoolLoad>,
  /// What the file asked for, which is what a pool with no supervisor reports.
  pub(crate) connections_configured: u32,
}

/// Lowest tunnel protocol version that can carry several services on one
/// connection.
///
/// v8 is where the Ping's `services` list became something a server reads and
/// acts on; 0.9.0 shipped protocol 7, so no released server before 0.10.0
/// announces it. Compared against what the server announces on the handshake
/// rather than against a release number, because it is the wire format that has
/// to agree, and a fork or a pre-release is honest about its protocol in a way
/// its version string need not be.
pub(crate) const MIN_MULTIPLEX_PROTOCOL: u32 = 8;

/// Most services this client will put on one connection.
///
/// The server's own ceiling, mirrored so the refusal happens where the message
/// can name the config file. A server answers a longer list by dropping the
/// connection, which from the operator's side is a client that connects and
/// disconnects with the reason in somebody else's log.
pub(crate) const MAX_MULTIPLEXED_SERVICES: usize = 256;

/// Ceiling on a service's candidate server list, configured plus learned.
///
/// A fence rather than a policy: the list is tried in rotation, so a server
/// announcing a hundred alternates would turn every reconnect into a long walk
/// through addresses nobody chose.
pub(crate) const MAX_SERVER_URLS: usize = 16;

/// What a server's capability announcement means for the `auth:` this client
/// wants to declare (`planned_features.md` #111).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GateNegotiation {
  /// Declare nothing beyond the fields that always travelled. Either there is
  /// no policy, or it is one the scalar `visitor_auth` (or `public`) already
  /// carries, in which case even a server that has never heard of the grammar
  /// gates the route exactly as it did.
  Scalar,
  /// Declare the full policy: this server said it understands these methods.
  Methods(Vec<aperio_config::AuthMethodSpec>),
  /// This server cannot carry the gate that was written, so the service is
  /// not served at all.
  Unsupported {
    /// The methods it does not accept, for the message.
    wanted: Vec<String>,
    /// The methods it does, so the message names the way out.
    accepted: Vec<String>,
  },
  /// This server announced nothing, so it predates the field a policy travels
  /// in, and this policy cannot be said in the scalar it does read. Named
  /// apart from [`Self::Unsupported`] because the method is not the problem:
  /// an old server understands `basic` perfectly, it just has nowhere to put
  /// two of them, and a message saying it "does not accept basic" would send
  /// its reader looking for the wrong thing.
  TooOldForPolicy {
    /// The methods written, for the message.
    wanted: Vec<String>,
  },
}

/// Decides what to announce, given what the server said it accepts.
///
/// **An absent announcement is the important case.** A server too old to send
/// the header sends nothing, and nothing has to read as "only the two methods
/// that always travelled", never as "anything goes": such a server would
/// ignore a policy it does not understand, read this client as declaring *no*
/// gate, and bring the route up open. That is the failure this whole
/// negotiation exists to prevent, and it is the one path no integration test
/// can reach without an old binary, which is why it is a function with tests
/// rather than eight lines inside a connect loop.
pub(crate) fn negotiate_visitor_gate(
  announced: Option<&str>,
  policy: Option<&aperio_config::AuthSetting>,
) -> GateNegotiation {
  let accepted: Vec<String> = match announced {
    Some(raw) => raw
      .split(',')
      .map(|m| m.trim().to_ascii_lowercase())
      .filter(|m| !m.is_empty())
      .collect(),
    None => vec!["none".to_string(), "basic".to_string()],
  };
  let Some(policy) = policy else {
    return GateNegotiation::Scalar;
  };
  let specs = policy.methods();
  let wanted: Vec<String> = specs
    .iter()
    .map(|m| m.method.trim().to_ascii_lowercase())
    .collect();
  // A policy that gates nobody is not a gate to lose, so it never refuses a
  // connection: `method: none` says "serve this to anyone", which travels as
  // `public` and is the one declaration a server may safely disagree with. If
  // it does not permit this token to declare it, the route keeps whatever gate
  // is already in front of it, which is narrower than what was asked for
  // rather than wider.
  if wanted.iter().all(|m| m.eq_ignore_ascii_case("none")) {
    return GateNegotiation::Scalar;
  }
  let unsupported: Vec<String> = wanted
    .iter()
    .filter(|m| !accepted.contains(m))
    .cloned()
    .collect();
  if !unsupported.is_empty() {
    return GateNegotiation::Unsupported {
      wanted: unsupported,
      accepted,
    };
  }
  // The richer field is sent only where the scalar cannot say the same thing.
  // A policy that is one `basic` credential, or nothing but `none`, already
  // travels as `visitor_auth` and `public`, and sending it twice would be two
  // sources for one answer.
  let carried_by_scalar = policy.as_single_credential().is_some()
    || specs
      .iter()
      .all(|m| m.method.trim().eq_ignore_ascii_case("none"));
  if carried_by_scalar {
    return GateNegotiation::Scalar;
  }
  // Past here the policy can only travel in the field an old server does not
  // read, so an absent announcement refuses, even though every method named is
  // in the fallback list. Checking the names alone is not enough: `basic` is
  // one an old server understands, but two credentials under it have nowhere
  // to go, the scalar holds one. Sending the rich field anyway is precisely
  // the silent open route this negotiation exists to prevent, and it is the
  // shape that looks safest, since nothing in the policy is exotic.
  if announced.is_none() {
    return GateNegotiation::TooOldForPolicy { wanted };
  }
  GateNegotiation::Methods(specs)
}

/// The service a server-named dispatch is for, as an index into `specs`.
///
/// An index rather than the spec itself because the spec is only one of the
/// things a request needs from its service: the concurrency limiter it waits
/// on, the adaptive controller that reads that wait, and the pool counter it
/// is counted in are all kept in lists beside `specs`, and one lookup that
/// answers for all of them cannot disagree with itself.
///
/// The server matched a route to a service and put its name in the frame, so
/// this is a lookup rather than a decision. A name this client does not carry
/// falls back to the first service it *announced*, which is the only answer
/// that keeps a connection serving: the alternative is dropping a request the
/// server has already committed to, and the pairing that could produce it (a
/// server naming a service the client withdrew in the same instant) resolves
/// itself on the next heartbeat.
///
/// Announced, not simply first, because the two differ: a service whose visitor
/// gate this server could not carry is held back, and falling back onto it
/// would forward a request to a backend this connection deliberately did not
/// offer.
///
/// `None` is every client before v8 and every connection carrying one service,
/// where there is nothing to choose.
pub(crate) fn service_for(
  specs: &[ServiceSpec],
  announced: &[usize],
  named: &Option<String>,
) -> usize {
  let fallback = announced.first().copied().unwrap_or(0);
  match named {
    Some(name) => specs
      .iter()
      .position(|s| s.name.as_deref() == Some(name.as_str()))
      .filter(|i| announced.contains(i))
      .unwrap_or(fallback),
    None => fallback,
  }
}

/// Everything one service needs to forward a request to its backend.
///
/// Built per service and per connection: per service because every value in it
/// comes from that service's own config, and per connection because the
/// circuit breaker inside it is state, and a breaker that outlived the socket
/// would carry one connection's failures into the next.
pub(crate) fn forward_context(
  spec: &ServiceSpec,
  tunnel_tx: &mpsc::Sender<Message>,
  stream_pauses: &crate::flow::PauseRegistry,
) -> ForwardContext {
  // Reqwest Client to make local forwarding requests. Same-site backend
  // redirects (http→https, same root domain) are followed transparently;
  // everything else passes through to the visitor.
  let mut builder = crate::proxy::http::backend_client_builder()
    .redirect(crate::proxy::http::redirect_policy(spec.max_redirects))
    .timeout(Duration::from_secs(spec.timeout_secs));
  // Connect and whole-request budgets are different questions: one is "is this
  // host reachable", the other "is this backend slow". Unset leaves the single
  // budget covering both, which is what this always did.
  if let Some(secs) = spec.connect_timeout {
    builder = builder.connect_timeout(Duration::from_secs(secs));
  }
  // Validated by `build_specs` before any service is spawned, on the first
  // load and on every reload, so an unusable value never reaches here. If one
  // somehow does, the floor is dropped rather than the process: killing every
  // other service of this client over one field is the failure a reload is
  // meant to prevent.
  match crate::proxy::http::tls_floor(spec.min_tls_version.as_deref()) {
    Ok(Some(floor)) => builder = builder.min_tls_version(floor),
    Ok(None) => {}
    Err(e) => error!("{e}; continuing without a TLS floor for this backend"),
  }
  let client = builder
    // Same reasoning as the tunnel socket: these are request and response
    // messages on a loopback or LAN hop, and holding one back for Nagle is
    // latency on a request a visitor is waiting for.
    .tcp_nodelay(true)
    .build()
    .unwrap_or_else(|e| {
      error!("Failed to build the forwarding HTTP client: {e}; using a client without a timeout");
      crate::proxy::http::backend_client_fallback()
    });
  if crate::proxy::h2::is_h2_target(&spec.target) && spec.pass_hostname {
    warn!(
      "[{}] pass_hostname is ignored for HTTP/2 targets ({}): the backend sees the target authority",
      spec.label(),
      spec.target
    );
  }
  ForwardContext {
    client,
    stream_pauses: stream_pauses.clone(),
    h2_client: crate::proxy::h2::build_h2_client(&spec.target, spec.min_tls_version.as_deref())
      .map(Arc::new),
    unix_socket: crate::proxy::unix::unix_socket_path(&spec.target),
    timeout_secs: spec.timeout_secs,
    // One breaker per service per connection, shared by every request it
    // serves: a breaker that could not see the other requests' failures would
    // never trip, and one shared across services would trip a healthy backend
    // over a broken neighbour's failures.
    resilience: crate::proxy::http::BackendResilience::new(
      spec.retry_attempts,
      spec.retry_backoff_ms,
      spec.retry_all_methods,
      spec.breaker_failures,
      spec.breaker_open_for_secs,
    ),
    target: spec.target.clone(),
    // Parsed once here rather than per request. `None` keeps the answer the
    // request path used to give for a target that is not a URL: 502, a
    // configuration error, not the visitor's fault.
    target_url: url::Url::parse(&spec.target).ok(),
    pass_hostname: spec.pass_hostname,
    path_bind: spec.path.clone(),
    trim_bind: spec.trim_bind,
    max_response_body_size: spec.max_response_body,
    tunnel_tx: tunnel_tx.clone(),
    request_headers: HeaderTransform::compile(
      spec.headers.as_ref().and_then(|h| h.request.as_ref()),
    ),
    response_headers: HeaderTransform::compile(
      spec.headers.as_ref().and_then(|h| h.response.as_ref()),
    ),
  }
}

/// Starts the backend health probe for one service, when it configured one.
///
/// A function rather than a block inside `run_service` because the probe is a
/// property of the *service* and of nothing else: it reads the spec, writes the
/// service's shared health state, and never touches the socket. That is what
/// lets a connection carrying several services start one of these per service.
/// The ownership rule is unchanged, only the connection that owns a service's
/// probes runs them, and the rest of that service's parallel connections report
/// what these write.
pub(crate) fn spawn_health_probe(
  spec: &ServiceSpec,
  health: &BackendHealth,
) -> Option<tokio::task::JoinHandle<()>> {
  let health_path = spec.target_health.as_ref()?;
  let label = spec.label();
  let health_changed = health.changed.clone();
  let probed = health.probed.clone();
  let flag = health.healthy.clone();
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
  // Health checks never follow redirects: a 3xx to some other page must
  // not let a broken backend look healthy via the redirect target.
  let probe_client = crate::proxy::http::backend_client_builder()
    .tcp_nodelay(true)
    .timeout(Duration::from_secs(spec.health_timeout))
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .unwrap_or_else(|e| {
      error!("Failed to build the health-probe HTTP client: {e}; using a client without a timeout");
      crate::proxy::http::backend_client_fallback()
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
  Some(tokio::spawn(async move {
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
            info!(
              "[{}] Backend healthy: {}, now routable",
              label, health_url_log
            );
          } else {
            info!("[{}] Backend health restored: {}", label, health_url_log);
          }
        }
      } else {
        consecutive_failures = consecutive_failures.saturating_add(1);
        if consecutive_failures >= threshold && flag.swap(false, Ordering::SeqCst) {
          health_changed.notify_waiters();
          warn!(
            "[{}] Backend health check failed {} consecutive time(s): {}, reporting unhealthy to the server (tunnel stays connected)",
            label, consecutive_failures, health_url_log
          );
        } else if first_result {
          // Started unhealthy and the first probe also failed: make it clear
          // why the backend is not yet routable (the threshold warning above
          // only fires on a healthy→unhealthy transition).
          info!(
            "[{}] Backend not healthy yet: {}, staying out of routing until a probe passes",
            label, health_url_log
          );
        }
      }
      if first_result {
        health_changed.notify_waiters();
      }
      first_result = false;
      tokio::time::sleep(Duration::from_secs(interval)).await;
    }
  }))
}

/// Starts one service's wait-for-backend startup gate (`wait_for_backend:
/// true`), when it asked for one and has no health check doing the job already.
///
/// Without a configured health check the service normally claims a healthy
/// backend immediately, which yields connection-refused errors while a slow dev
/// server is still booting. The gate starts the service out of routing and a
/// lightweight connect-probe loop marks it routable the first time the backend
/// accepts a connection; after that the gate never re-engages (`target_health`
/// is the tool for continuous health tracking, and it supersedes this gate
/// entirely when configured).
pub(crate) fn spawn_backend_wait(
  spec: &ServiceSpec,
  health: &BackendHealth,
) -> Option<tokio::task::JoinHandle<()>> {
  let label = spec.label();
  if !spec.wait_for_backend || spec.target.is_empty() {
    return None;
  }
  if spec.target_health.is_some() {
    info!(
      "[{}] wait_for_backend is implied by target_health; the health check already gates startup",
      label
    );
    return None;
  }
  health.healthy.store(false, Ordering::SeqCst);
  health.probed.store(false, Ordering::SeqCst);
  let flag = health.healthy.clone();
  let probed = health.probed.clone();
  let health_changed = health.changed.clone();
  let target = spec.target.clone();
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
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
