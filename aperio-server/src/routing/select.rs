//! Choosing which service serves a request.
//!
//! The pool is narrowed in stages, hostname then path then health, and each
//! stage keeps the *service* rather than the connection: a connection may carry
//! several, and one of them being ejected or unhealthy says nothing about its
//! neighbours. `ServiceRef` is what comes out, and it is an index pair rather
//! than a borrow because the caller releases the lock before dispatching.

use std::net::IpAddr;
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Selects the pool of candidate client IDs for a request, honoring hostname
/// binds first, then path binds within the hostname group. Returns the pool
/// together with the round-robin group key.
///
/// Hostname stage:
/// - Clients whose effective hostname bind equals the request host win.
/// - Otherwise, when `require_hostname_bind` is off, clients without any
///   hostname bind act as the fallback pool. When the flag is on, clients
///   without a hostname bind never receive traffic.
///
/// Path stage (within the hostname pool): longest matching path bind wins;
/// clients without a path bind are the fallback.
pub(crate) fn select_client_pool(
  clients: &HashMap<String, ClientHandle>,
  uri_path: &str,
  request_host: Option<&str>,
  require_hostname_bind: bool,
  down_threshold: Duration,
) -> Option<(Vec<ServiceRef>, RouteGroupKey)> {
  // --- Eligibility stage ---
  //
  // The set routing chooses from is every *service* of every connection, not
  // every connection. Health and draining are asked of the connection, which
  // is what a heartbeat and a shutdown belong to; the backend probe and the
  // dashboard's kill switch are asked of the service, which is what they
  // describe. A connection carrying one service, which is all of them today,
  // gives exactly the set the previous version built.
  let eligible: Vec<(ServiceRef, &ServiceState)> = clients
    .iter()
    .filter(|(_, c)| c.is_healthy(down_threshold) && !c.draining)
    .flat_map(|(id, c)| {
      c.services.iter().enumerate().map(move |(index, s)| {
        (
          ServiceRef {
            client: id.clone(),
            index,
          },
          s,
        )
      })
    })
    .filter(|(_, s)| s.backend_healthy && s.admin_enabled)
    .collect();

  // --- Hostname stage ---
  let host_matched: Vec<(ServiceRef, &ServiceState)> = match request_host {
    Some(host) => eligible
      .iter()
      .filter(|(_, s)| s.matches_host(host))
      .map(|(r, s)| (r.clone(), *s))
      .collect(),
    None => Vec::new(),
  };

  let (host_pool, host_key): (Vec<(ServiceRef, &ServiceState)>, Option<String>) =
    if !host_matched.is_empty() {
      (host_matched, request_host.map(|h| h.to_string()))
    } else if require_hostname_bind {
      // Strict mode: unbound services are never eligible.
      return None;
    } else {
      let unbound: Vec<(ServiceRef, &ServiceState)> = eligible
        .iter()
        .filter(|(_, s)| !s.has_hostname_bind())
        .map(|(r, s)| (r.clone(), *s))
        .collect();
      (unbound, None)
    };

  if host_pool.is_empty() {
    return None;
  }

  // --- Path stage ---
  let path_matched: Vec<(&ServiceRef, &String)> = host_pool
    .iter()
    .filter_map(|(r, s)| {
      s.effective_path_bind()
        .filter(|bind| path_matches_bind(uri_path, bind))
        .map(|bind| (r, bind))
    })
    .collect();

  let (pool, path_key): (Vec<ServiceRef>, Option<String>) = if !path_matched.is_empty() {
    // Longest matching bind wins; only services with that exact bind pool
    // together.
    let longest = path_matched
      .iter()
      .map(|(_, b)| (*b).clone())
      .max_by_key(|b| b.len())
      .unwrap();
    let refs = path_matched
      .iter()
      .filter(|(_, b)| **b == longest)
      .map(|(r, _)| (*r).clone())
      .collect();
    (refs, Some(longest))
  } else {
    let refs: Vec<ServiceRef> = host_pool
      .iter()
      .filter(|(_, s)| s.effective_path_bind().is_none())
      .map(|(r, _)| r.clone())
      .collect();
    (refs, None)
  };

  // Passive outlier ejection: drop services currently ejected after repeated
  // dispatch failures, but only if a non-ejected candidate remains for this
  // route, so a route whose whole pool is struggling still fails open rather
  // than returning no route at all. When the feature is disabled nothing is
  // ever ejected, so this is a no-op. Independent of the `/health` probe above.
  let now = std::time::Instant::now();
  let live: Vec<ServiceRef> = pool
    .iter()
    .filter(|r| r.get(clients).is_none_or(|s| !s.is_ejected(now)))
    .cloned()
    .collect();
  let pool = if live.is_empty() { pool } else { live };

  if pool.is_empty() {
    None
  } else {
    Some((pool, (host_key, path_key)))
  }
}

/// Applies the configured load-balancing strategy to a routed pool.
/// `RoundRobin` keeps the whole pool (the caller's per-group counter rotates
/// through it); `PrimaryStandby` narrows it to the clients sharing the lowest
/// announced priority, so standbys only receive traffic once every
/// more-primary client has dropped out of the pool.
pub(crate) fn apply_lb_strategy(
  pool: Vec<ServiceRef>,
  clients: &HashMap<String, ClientHandle>,
  strategy: LbStrategy,
) -> Vec<ServiceRef> {
  match strategy {
    // Sticky affinity is resolved later in pick_proxy_client; the pool
    // itself is built exactly like round-robin.
    LbStrategy::RoundRobin | LbStrategy::Sticky => pool,
    LbStrategy::PrimaryStandby => {
      // Priority is the service's, not the connection's: a process may well
      // run a primary of one service beside a standby of another.
      let min_priority = pool
        .iter()
        .filter_map(|r| r.get(clients))
        .map(|s| s.priority)
        .min()
        .unwrap_or(0);
      pool
        .into_iter()
        .filter(|r| r.get(clients).is_some_and(|s| s.priority == min_priority))
        .collect()
    }
  }
}

/// Which service of which connection: the unit routing decides on.
///
/// A connection id alone was the answer for as long as a connection carried
/// one service. It is the wrong answer the moment it carries two, and the
/// wrongness is quiet: the second service's traffic would go to the first
/// one's backend, which is a live backend that answers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ServiceRef {
  /// Connection id, the key of `AppState::clients`.
  pub(crate) client: String,
  /// Index into that connection's `services`.
  pub(crate) index: usize,
}

impl ServiceRef {
  /// The service this points at, or `None` if the connection has gone since
  /// the pool was built. Routing holds the read lock across a pass, so within
  /// one pass this is always `Some`; the callers that resolve later are the
  /// ones that have released it.
  pub(crate) fn get<'a>(
    &self,
    clients: &'a HashMap<String, ClientHandle>,
  ) -> Option<&'a ServiceState> {
    clients.get(&self.client)?.services.get(self.index)
  }

  /// The connection carrying it, for the things that genuinely belong to the
  /// socket: the sender, the token, the announced instance.
  pub(crate) fn connection<'a>(
    &self,
    clients: &'a HashMap<String, ClientHandle>,
  ) -> Option<&'a ClientHandle> {
    clients.get(&self.client)
  }
}

/// A dispatch target chosen from the routed pool.
pub(crate) struct SelectedClient {
  pub(crate) id: String,
  pub(crate) tx: mpsc::Sender<Message>,
  pub(crate) request_count: Arc<AtomicU64>,
  pub(crate) inflight_limiter: Option<Arc<Semaphore>>,
  pub(crate) token_name: Option<String>,
  /// Record ID of the dynamic token (None = master); limits key on this.
  pub(crate) token_id: Option<String>,
  /// Organization the serving client belongs to (None = master); traffic,
  /// captures, and per-org stats are attributed to it.
  pub(crate) org_id: Option<String>,
  /// Client-process instance ID (from Ping); used by failover `wait` mode.
  pub(crate) instance_id: Option<String>,
  /// Tunnel protocol version the client announced (None until known).
  pub(crate) protocol: Option<u32>,
  /// The client opted into the server-side response cache (Ping `cache`).
  pub(crate) cache: bool,
  /// The client asked for serve-stale resilience (Ping `resilience`).
  pub(crate) resilience: bool,
  /// False when this service asked not to be recorded for the request
  /// inspector. Resolved here, under the lock the routing pass already holds,
  /// so the capture site does not take a second one per request.
  pub(crate) capture: bool,
  /// Client-declared request body cap in bytes (Ping `max_request_body`);
  /// tightens, never loosens, the global body size limit.
  pub(crate) max_request_body: Option<u64>,
  /// Client-declared per-service response timeout in seconds (Ping
  /// `response_timeout`); overrides the global gateway response timeout for
  /// this dispatch (None = use the global value).
  pub(crate) response_timeout: Option<u64>,
  /// The client asked to persist inbound POSTs into the webhook inbox.
  pub(crate) webhook_inbox: bool,
  /// Service name announced via Ping, and what identifies the chosen service
  /// after the dispatch.
  ///
  /// Carried instead of an index on purpose: the dispatch outlives the read
  /// lock it was chosen under, and a Ping carrying a list rebuilds `services`
  /// wholesale, so an index captured here can point at a different service,
  /// or at none, by the time anything acts on it. The name is what reconcile
  /// preserves, which is why outlier ejection charges by it.
  pub(crate) service_name: Option<String>,
  /// The `custom_name` an operator gave the service, which wins over
  /// `service_name` wherever a client is named for a person to read.
  pub(crate) service_custom_name: Option<String>,
}

/// Returns the pool member matching an affinity value, either a client's
/// self-reported instance ID (survives reconnects) or its connection ID.
pub(crate) fn find_affinity_match(
  pool: &[ServiceRef],
  clients: &HashMap<String, ClientHandle>,
  affinity: &str,
) -> Option<ServiceRef> {
  pool
    .iter()
    .find(|r| {
      r.connection(clients).is_some_and(|c| {
        c.reported_instance_id.as_deref() == Some(affinity) || r.client == affinity
      })
    })
    .cloned()
}

/// Picks a client for a request with the full routing pipeline (eligibility →
/// hostname → path → strategy → round-robin). When `require_instance` is
/// given, only clients that reported that instance ID qualify (failover
/// `wait` mode waiting for a specific client process to return). With the
/// sticky strategy, a matching `affinity` cookie value pins the choice to
/// the client that served this visitor before.
pub(crate) async fn pick_proxy_client(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
  require_instance: Option<&str>,
  affinity: Option<&str>,
  visitor_ip: Option<IpAddr>,
  // Which side of a route's canary split this request belongs on, already
  // decided from the route policy, the request's headers and the visitor.
  canary: Option<(&str, crate::static_routes::Side)>,
) -> PickOutcome {
  let clients = state.clients.read().await;
  let Some((pool, group_key)) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  ) else {
    return PickOutcome::NoRoute;
  };
  // Per-candidate visitor-IP eligibility runs before the LB strategy so a
  // passing standby can serve a visitor its primary rejects.
  let pool = match filter_pool_by_ip(pool, &clients, visitor_ip) {
    IpFilterOutcome::Allowed(pool) => pool,
    IpFilterOutcome::Denied(redirect) => return PickOutcome::Denied(redirect),
  };
  let mut pool = apply_lb_strategy(pool, &clients, state.config().lb_strategy);
  // Canary split (planned_features #51): narrow the pool to one side, but
  // never to nothing. A canary that is down, or a stable side that has been
  // fully replaced, must not take the route with it: the experiment gives way
  // to serving the request.
  if let Some(side) = canary {
    let narrowed = narrow_to_side(&pool, &clients, side);
    if !narrowed.is_empty() {
      pool = narrowed;
    }
  }
  if let Some(instance) = require_instance {
    // An instance is a client *process*, so this is asked of the connection.
    pool.retain(|r| {
      r.connection(&clients)
        .is_some_and(|c| c.reported_instance_id.as_deref() == Some(instance))
    });
  }
  if pool.is_empty() {
    return PickOutcome::NoRoute;
  }

  // Sticky affinity: honor the visitor's cookie when that client is still in
  // the pool; otherwise fall back to rotation (and the response sets a fresh
  // cookie for the newly chosen client).
  let chosen = if state.config().lb_strategy == LbStrategy::Sticky
    && let Some(previous) = affinity.and_then(|a| find_affinity_match(&pool, &clients, a))
  {
    previous
  } else {
    let mut rr_map = state.path_rr.lock().await;
    let idx = rr_map.entry(group_key).or_insert(0);
    let chosen = pool[*idx % pool.len()].clone();
    *idx = (*idx + 1) % pool.len();
    chosen
  };

  // Everything per service is read from the service that was chosen, and
  // everything per connection from the connection carrying it. Before this
  // the whole struct came off the handle, which meant the same values for
  // every service on it.
  match (chosen.connection(&clients), chosen.get(&clients)) {
    (Some(c), Some(svc)) => PickOutcome::Selected(Box::new(SelectedClient {
      id: chosen.client.clone(),
      tx: c.tx.clone(),
      request_count: svc.request_count.clone(),
      inflight_limiter: svc.inflight_limiter.clone(),
      token_name: c.perms.token_name.clone(),
      token_id: c.perms.token_id.clone(),
      org_id: c.perms.org_id.clone(),
      instance_id: c.reported_instance_id.clone(),
      protocol: c.client_protocol,
      cache: svc.cache,
      resilience: svc.resilience,
      capture: svc.capture,
      max_request_body: svc.max_request_body,
      response_timeout: svc.response_timeout,
      webhook_inbox: svc.webhook_inbox,
      service_name: svc.service_name.clone(),
      service_custom_name: svc.service_custom_name.clone(),
    })),
    _ => PickOutcome::NoRoute,
  }
}

/// True when some client currently serves this host/path, without choosing
/// one of them.
///
/// [`pick_proxy_client`] advances the route group's round-robin cursor as a
/// side effect, so asking it merely whether a route exists costs a rotation
/// step. The cold-start probe did exactly that on every request, spending two
/// steps per request instead of one: a pool of two clients then always landed
/// on the same member and the other never saw traffic, and every pool with an
/// even number of members skewed the same way.
pub(crate) async fn route_exists(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
  visitor_ip: Option<IpAddr>,
) -> bool {
  let clients = state.clients.read().await;
  let Some((pool, _)) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  ) else {
    return false;
  };
  let pool = match filter_pool_by_ip(pool, &clients, visitor_ip) {
    IpFilterOutcome::Allowed(pool) => pool,
    // A route this visitor is not allowed on still exists; starting capacity
    // for them would not change the answer they get.
    IpFilterOutcome::Denied(_) => return true,
  };
  !apply_lb_strategy(pool, &clients, state.config().lb_strategy).is_empty()
}

/// The pool members on one side of a canary split.
///
/// Membership is by the client's announced service name, which is what an
/// operator writes in `services:` and therefore the only name they can put in
/// the route. A client that announces no service name is on the stable side:
/// it predates the split, so treating it as the new version would send traffic
/// to the one candidate nobody nominated.
fn narrow_to_side(
  pool: &[ServiceRef],
  clients: &HashMap<String, ClientHandle>,
  (service, side): (&str, crate::static_routes::Side),
) -> Vec<ServiceRef> {
  pool
    .iter()
    .filter(|r| {
      let is_canary = r
        .get(clients)
        .and_then(|s| s.service_name.as_deref())
        .is_some_and(|name| name == service);
      match side {
        crate::static_routes::Side::Canary => is_canary,
        crate::static_routes::Side::Stable => !is_canary,
      }
    })
    .cloned()
    .collect()
}

/// True when the route for this host/path is served exclusively by clients
/// that declared themselves public (with a token permitting it): the visitor
/// auth gate is skipped. An empty or mixed pool keeps the gate, a request
/// must never leak past auth because one pool member happens to be public.
pub(crate) async fn route_is_public(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
) -> bool {
  // A traversal segment can widen the matched scope (`/public/../admin`
  // matches a `/public` path bind) and a backend that resolves `..` would then
  // serve the sibling path without the gate, never treat such a path as
  // public; it falls back to the normal login gate.
  if request_path_has_traversal(uri_path) {
    return false;
  }
  let clients = state.clients.read().await;
  let Some((pool, _)) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  ) else {
    return false;
  };
  !pool.is_empty()
    && pool
      .iter()
      .all(|r| r.get(&clients).is_some_and(|s| s.public))
}

/// Per-candidate visitor-IP eligibility of a routed pool: each candidate is
/// filtered by its *own* `allowed_ips` (a candidate without a list admits
/// everyone), and the request dispatches to any passing candidate, union
/// semantics. Note this is fail-open by design: one unrestricted client
/// joining a route opens it; route-wide lockdown belongs to the token-level
/// IP allowlist.
pub(crate) fn filter_pool_by_ip(
  pool: Vec<ServiceRef>,
  clients: &HashMap<String, ClientHandle>,
  ip: Option<IpAddr>,
) -> IpFilterOutcome {
  let Some(ip) = ip else {
    return IpFilterOutcome::Allowed(pool);
  };
  let (passing, rejecting): (Vec<ServiceRef>, Vec<ServiceRef>) = pool.into_iter().partition(|r| {
    r.get(clients)
      .is_none_or(|s| s.allowed_ips.is_empty() || crate::auth::ip_allowed(ip, &s.allowed_ips))
  });
  if !passing.is_empty() {
    return IpFilterOutcome::Allowed(passing);
  }
  // Every candidate rejected the visitor: answer with the `denied:` redirect
  // of the most-primary (lowest priority tier) rejecting entry that declares
  // one; with none declared anywhere, the caller answers stealth.
  let denied = rejecting
    .iter()
    .filter_map(|r| r.get(clients))
    .filter_map(|s| s.denied.clone().map(|url| (s.priority, url)))
    .min_by_key(|(priority, _)| *priority)
    .map(|(_, url)| url);
  IpFilterOutcome::Denied(denied)
}

/// Outcome of the per-candidate visitor-IP filter.
pub(crate) enum IpFilterOutcome {
  /// Candidates admitting the visitor (their own lists pass, or impose nothing).
  Allowed(Vec<ServiceRef>),
  /// Every candidate rejected the visitor; carries the winning `denied:`
  /// redirect, if any candidate declared one.
  Denied(Option<String>),
}

/// Outcome of picking a dispatch target for a visitor request.
pub(crate) enum PickOutcome {
  /// A client was selected; dispatch to it.
  Selected(Box<SelectedClient>),
  /// No client serves this route (unclaimed, or every candidate is down).
  NoRoute,
  /// Clients serve the route but every candidate's `allowed_ips` rejects the
  /// visitor. Carries the winning `denied:` redirect (None = stealth: the
  /// caller must answer exactly like [`PickOutcome::NoRoute`]).
  Denied(Option<String>),
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
