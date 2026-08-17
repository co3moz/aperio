use axum::extract::ws::Message;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, mpsc};

/// Where each field of a wire `ServiceDecl` lands on this server today.
///
/// The two scopes are now two types, `ClientHandle` for the connection and
/// `ServiceState` for what it carries, so the compiler enforces the split
/// that this table used to only describe. What the table still does is the
/// part a type cannot: say that the division is the *right* one. A field can
/// sit in the wrong struct and compile perfectly.
///
/// It is keyed by the wire contract rather than by either struct, because the
/// wire is what defines a service: a field of `ServiceDecl` is service-scoped
/// by construction, so anything it maps to belongs in `ServiceState`.
///
/// `None` means the field does not reach either struct. That is not an
/// omission: `scaling` arms a record per hostname in the autoscaling store,
/// and the only trace it leaves is a warn-once flag.
///
/// A test holds this to the wire, so a field added to `ServiceDecl` cannot
/// arrive without somebody saying where it goes.
#[cfg(test)]
pub(crate) const SERVICE_DECL_IN_SERVICE_STATE: &[(&str, Option<&str>)] = &[
  ("service", Some("service_name")),
  ("service_custom_name", Some("service_custom_name")),
  ("path_bind", Some("declared_path")),
  ("hostname_bind", Some("declared_hostname")),
  ("hostname_binds", Some("declared_hostnames")),
  ("max_concurrent", Some("max_concurrent")),
  ("bandwidth_bps", Some("bandwidth_bps")),
  ("priority", Some("priority")),
  ("tcp", Some("tcp_enabled")),
  ("public", Some("public")),
  ("visitor_auth", Some("visitor_auth")),
  ("visitor_auth_methods", Some("visitor_auth_policy")),
  ("allowed_ips", Some("allowed_ips")),
  ("tunnels", Some("tunnels")),
  ("cache", Some("cache")),
  ("resilience", Some("resilience")),
  // Inverted on arrival: the wire says "do not capture", the handle says
  // whether it captures.
  ("no_capture", Some("capture")),
  ("max_request_body", Some("max_request_body")),
  ("response_timeout", Some("response_timeout")),
  ("webhook_inbox", Some("webhook_inbox")),
  ("denied", Some("denied")),
  ("backend_healthy", Some("backend_healthy")),
  ("backend_probed", Some("backend_probed")),
  ("connections", Some("connections")),
  ("connections_min", Some("connections_min")),
  ("connections_max", Some("connections_max")),
  ("config_notes", Some("config_notes")),
  ("metrics_labels", Some("metrics_labels")),
  ("scaling", None),
];

/// Service-scoped fields the server derives rather than receives.
///
/// These belong to the service as surely as the declared ones, and they are
/// the half that is easy to miss when reading the wire alone: the binds a
/// token granted rather than the client asked for, the dashboard's temporary
/// overrides of them, the limiter built from the announced concurrency, the
/// failover bookkeeping, and the warn-once flags that exist so a
/// misconfiguration is reported to the operator once instead of every
/// heartbeat. Under #46 each of them becomes one per service; a warn-once
/// flag left on the connection would silence the second service's warning
/// because the first already warned.
#[cfg(test)]
pub(crate) const SERVICE_SCOPED_DERIVED: &[&str] = &[
  "assigned_path",
  "assigned_hostnames",
  "random_hostname",
  "override_path_bind",
  "override_hostname_binds",
  "inflight_limiter",
  // The band `adaptive_concurrency` moves the limiter inside, which is one
  // service's band: two services on a connection announce their own numbers
  // and their backends fall behind independently, so a ceiling shared between
  // them would let one service's recovery raise the other's limit.
  "max_concurrent_ceiling",
  "recent_failures",
  "ejected_until",
  "admin_enabled",
  "request_count",
  "public_denied_warned",
  "visitor_auth_denied_warned",
  "ungated_warned",
  "allowed_ips_invalid_warned",
  "cache_ignored_warned",
  "scaling_invalid_warned",
];

/// What genuinely belongs to the connection, and stays one per socket.
///
/// These are `ClientHandle`'s own fields, everything it has besides the
/// service it carries.
///
/// The socket and its liveness, the peer, the token that authenticated it,
/// the process and link telemetry, the identity the client announces for
/// itself. #37 is the entry to read before moving any of the telemetry:
/// multiplexed, RTT and reconnects become properties of the process rather
/// than of a service, which is arguably the more useful reading but is a
/// reporting change to make deliberately rather than by accident.
#[cfg(test)]
pub(crate) const CONNECTION_SCOPED: &[&str] = &[
  "tx",
  "disconnect",
  "connected_at",
  "client_ip",
  "last_ping_at",
  "perms",
  "draining",
  "drain_secs",
  "declared_client_id",
  "client_version",
  "client_protocol",
  "cpu_percent",
  "rss_bytes",
  "rtt_ms",
  "jitter_ms",
  "reconnects",
  "reported_instance_id",
  "instance_group",
  "subscriptions",
];

/// Which existing service each declaration in a Ping updates.
///
/// A Ping carrying a list has to say not only *what* the services are but
/// *which* of the ones already here each entry is. Position alone cannot: a
/// client that reorders its `services:` block would hand service A's
/// ejection state, warn-once flags, request counter and concurrency limiter
/// to service B, and none of that is on the wire to correct itself. It would
/// look like two healthy services with each other's history.
///
/// So a named declaration matches the service of that name; anything left
/// over, named or not, adopts an unadopted (nameless) service in order.
/// Names come from the client's
/// own `services:` entries, which is what an operator already thinks of as
/// the service's identity and what the dashboard shows.
///
/// Returns one entry per declaration: `Some(i)` to update the service at `i`,
/// `None` for one this connection does not carry yet. `Err` when two
/// declarations claim the same name, which is a client-side mistake that
/// must be refused rather than resolved: either answer silently merges two
/// services into one.
pub(crate) fn match_declarations(
  existing: &[ServiceState],
  declared_names: &[Option<String>],
) -> Result<Vec<Option<usize>>, String> {
  let mut seen: Vec<&str> = Vec::new();
  for name in declared_names.iter().flatten() {
    if seen.contains(&name.as_str()) {
      return Err(name.clone());
    }
    seen.push(name);
  }

  // Consumed as they are claimed, so two nameless declarations never land on
  // the same existing service.
  let mut taken = vec![false; existing.len()];
  let mut out = Vec::with_capacity(declared_names.len());

  // Named first, and across the whole list, so a rename or a reorder cannot
  // make a named declaration lose to a nameless one that happened to sit at
  // its index.
  for name in declared_names {
    let found = name.as_ref().and_then(|n| {
      (0..existing.len())
        .find(|&i| !taken[i] && existing[i].service_name.as_deref() == Some(n.as_str()))
    });
    // Marked, though nothing can currently reach the case: the duplicate
    // check above means two declarations never carry the same name, and a
    // named service is not adoptable, so no second claimant exists. Deleting
    // it changes no test, which is exactly why the reason is written here
    // rather than left to be rediscovered. It is what keeps this pass correct
    // if the duplicate refusal above is ever relaxed.
    if let Some(i) = found {
      taken[i] = true;
    }
    out.push(found);
  }

  // Then everything still unmatched, named or not, against the *unadopted*
  // services: the nameless ones still unclaimed, in order.
  //
  // A nameless service is one nothing has claimed by name yet, and that
  // includes the placeholder a connection is created with, before its first
  // Ping has said anything. Without this a client that names its service
  // would be told, on that first Ping, that it is describing a service this
  // connection does not carry, and a second one would be appended beside the
  // empty one it meant to fill.
  //
  // It also does the kinder thing for a client that adds a `name:` to a
  // service it had been running without one: same service, new label, and no
  // reason to lose its counters, its ejection state and its warn-once flags
  // over it. The named-first pass above means adoption only happens when no
  // service of that name exists, so it never steals from one.
  for slot in out.iter_mut() {
    if slot.is_some() {
      continue;
    }
    if let Some(i) = (0..existing.len()).find(|i| !taken[*i] && existing[*i].service_name.is_none())
    {
      taken[i] = true;
      *slot = Some(i);
    }
  }

  Ok(out)
}

impl ServiceState {
  /// A service this connection has just been told it carries.
  ///
  /// Not `Default`, because two of these are not the zero value and getting
  /// them wrong is silent: a fresh service is `admin_enabled` (nothing has
  /// switched it off) and `backend_healthy` (nothing has probed it yet, and
  /// starting unhealthy would keep it out of routing until the first probe
  /// happened to succeed). Everything else genuinely starts empty.
  ///
  /// The binds a token grants are not filled in here. They belong to the
  /// connection's first service, which the handshake sets up; a service the
  /// client adds later declares its own and is checked against the same
  /// permissions.
  /// `pacer` is the connection's own bandwidth cell, the one the writer task
  /// reads. It is shared rather than freshly minted because the shaper is per
  /// *socket*: a connection has one writer, so a service that announced a cap
  /// into a cell nothing reads would be reported as throttled by the API
  /// while the wire ran unthrottled. Sharing keeps the two honest. What it
  /// cannot do is enforce two different caps on one socket, which is the same
  /// per-service-on-a-shared-writer question `planned_features.md` #46 raises
  /// for the pacer and for flow control.
  pub(crate) fn newly_declared(pacer: Arc<AtomicU64>) -> Self {
    Self {
      request_count: Arc::new(AtomicU64::new(0)),
      declared_path: None,
      assigned_path: None,
      declared_hostname: None,
      declared_hostnames: Vec::new(),
      assigned_hostnames: Vec::new(),
      random_hostname: None,
      override_path_bind: None,
      override_hostname_binds: Vec::new(),
      capture: true,
      connections: None,
      connections_min: None,
      connections_max: None,
      config_notes: Vec::new(),
      metrics_labels: Vec::new(),
      max_concurrent: None,
      max_concurrent_ceiling: None,
      inflight_limiter: None,
      admin_enabled: true,
      tcp_enabled: false,
      backend_healthy: true,
      backend_probed: true,
      priority: 0,
      bandwidth_bps: pacer,
      service_name: None,
      service_custom_name: None,
      public: false,
      public_denied_warned: false,
      visitor_auth: None,
      visitor_auth_policy: None,
      visitor_auth_denied_warned: false,
      ungated_warned: false,
      allowed_ips: Vec::new(),
      allowed_ips_invalid_warned: false,
      scaling_invalid_warned: false,
      tunnels: Vec::new(),
      cache: false,
      cache_ignored_warned: false,
      resilience: false,
      max_request_body: None,
      response_timeout: None,
      webhook_inbox: false,
      denied: None,
      recent_failures: VecDeque::new(),
      ejected_until: None,
    }
  }
}

/// What routing asks of a service.
///
/// These hung off `ClientHandle`, which was true only because a connection
/// carried one service: every one of them reads nothing but this struct's own
/// fields. On the connection they would have had to guess which service they
/// meant the moment there were two.
impl ServiceState {
  pub(crate) fn is_ejected(&self, now: Instant) -> bool {
    self.ejected_until.is_some_and(|t| now < t)
  }

  /// Records one dispatch failure (5xx / timeout / connection loss). Prunes
  /// the failure window, then ejects the client for `eject_for` once
  /// `threshold` failures land inside `window`. Returns true when this call
  /// caused the ejection.
  pub(crate) fn record_failure(
    &mut self,
    now: Instant,
    window: Duration,
    threshold: u32,
    eject_for: Duration,
  ) -> bool {
    while self
      .recent_failures
      .front()
      .is_some_and(|t| now.duration_since(*t) > window)
    {
      self.recent_failures.pop_front();
    }
    self.recent_failures.push_back(now);
    if !self.is_ejected(now) && self.recent_failures.len() as u32 >= threshold {
      self.ejected_until = Some(now + eject_for);
      self.recent_failures.clear();
      return true;
    }
    false
  }

  /// Path bind used for routing: dashboard override wins over the declared
  /// value, which wins over the token-granted value.
  pub(crate) fn effective_path_bind(&self) -> Option<&String> {
    self
      .override_path_bind
      .as_ref()
      .or(self.declared_path.as_ref())
      .or(self.assigned_path.as_ref())
  }

  /// Hostnames used for routing. A dashboard override replaces the whole
  /// set; otherwise the union of assigned and declared hostnames applies.
  pub(crate) fn effective_hostnames(&self) -> Vec<&String> {
    if !self.override_hostname_binds.is_empty() {
      return self.override_hostname_binds.iter().collect();
    }
    let mut set: Vec<&String> = self.assigned_hostnames.iter().collect();
    if let Some(d) = &self.declared_hostname
      && !set.contains(&d)
    {
      set.push(d);
    }
    for d in &self.declared_hostnames {
      if !set.contains(&d) {
        set.push(d);
      }
    }
    set
  }

  /// What this client calls itself, if it says anything.
  ///
  /// The order the clients table uses, and every other place a client is
  /// shown to a person: the `custom_name` an operator gave the service, else
  /// the `name` of its `services:` entry. `None` leaves only the id.
  pub(crate) fn display_name(&self) -> Option<String> {
    self
      .service_custom_name
      .clone()
      .or_else(|| self.service_name.clone())
  }

  pub(crate) fn matches_host(&self, host: &str) -> bool {
    self
      .effective_hostnames()
      .iter()
      .any(|h| h.as_str() == host)
  }

  pub(crate) fn has_hostname_bind(&self) -> bool {
    !self.effective_hostnames().is_empty()
  }
}

/// Handle tracking active WebSocket sender channel and metadata.
///
/// Two scopes live here, and the three lists above partition them: what the
/// wire declares per service (`SERVICE_DECL_IN_SERVICE_STATE`), what the server
/// derives per service (`SERVICE_SCOPED_DERIVED`), and what belongs to the
/// connection (`CONNECTION_SCOPED`). The first two become many under #46.
/// A test holds the partition exact, so a field cannot be added here without
/// being placed on one side of the seam.
/// One service carried by a connection.
///
/// Everything here is per service, which is the same as per connection only
/// while a connection carries one. Splitting it out is what lets #46 make it
/// many without hunting for which of the handle's fields should follow: the
/// three lists above are the record of that decision, and this struct is the
/// decision applied.
pub(crate) struct ServiceState {
  /// Total request count processed by this specific client connection.
  pub(crate) request_count: Arc<AtomicU64>,
  /// Path prefix the client declared via Ping (from APERIO_PATH),
  /// validated against the token permissions.
  pub(crate) declared_path: Option<String>,
  /// Path bind granted by the token permissions when the client declared none.
  pub(crate) assigned_path: Option<String>,
  /// Hostname the client declared via Ping (from APERIO_HOSTNAME),
  /// validated against the token permissions.
  pub(crate) declared_hostname: Option<String>,
  /// Additional hostnames the client declared beyond `declared_hostname`
  /// (multi-hostname services), each already validated against the token.
  pub(crate) declared_hostnames: Vec<String>,
  /// Hostnames granted automatically: token-bound hostnames and/or the
  /// randomly assigned subdomain.
  pub(crate) assigned_hostnames: Vec<String>,
  /// The randomly assigned hostname within `assigned_hostnames`, tracked
  /// separately so a runtime pattern change can swap it in place.
  pub(crate) random_hostname: Option<String>,
  /// Temporary path bind override set from the dashboard. Not persisted:
  /// lost when the client reconnects or the server restarts.
  pub(crate) override_path_bind: Option<String>,
  /// Temporary hostname binds set from the dashboard, replacing every declared
  /// and assigned name while set. A list rather than a single name so an
  /// operator can retarget the hostname the client declared without dropping
  /// the random subdomain the server handed it (or the other way round). Not
  /// persisted: lost when the client reconnects or the server restarts.
  pub(crate) override_hostname_binds: Vec<String>,
  /// The concurrency limit currently *enforced* for this service, which is
  /// also what is displayed.
  ///
  /// Not simply the last number the client announced. A shrink can only take
  /// the permits that are free, so under load it takes fewer than it asked
  /// for; this holds what the limiter actually ended up with, so the figure on
  /// screen and the figure on the semaphore can never disagree.
  pub(crate) max_concurrent: Option<u32>,
  /// The highest this service's limit may be moved back up to: the first
  /// number it announced on this connection.
  ///
  /// `adaptive_concurrency` lowers a ceiling under pressure and climbs back
  /// towards it; it never raises one the operator set. The client enforces
  /// that on its own limiter and the server keeps the same band, so a client
  /// announcing an ever-growing number cannot talk its way into more
  /// concurrency than its config asked for. A config reload that genuinely
  /// raises the number respawns the connection, which is where a new ceiling
  /// comes from.
  pub(crate) max_concurrent_ceiling: Option<u32>,
  /// Semaphore enforcing the client's announced concurrency limit. Requests
  /// beyond the limit wait here (bounded by the gateway timeout) instead of
  /// being dispatched, so the server never floods the client's backend.
  pub(crate) inflight_limiter: Option<Arc<Semaphore>>,
  /// Dashboard kill switch: false = temporarily excluded from routing even
  /// though the connection and heartbeats remain healthy.
  pub(crate) admin_enabled: bool,
  /// False when the service asked not to be recorded for the request
  /// inspector (`capture: false` in its aperio.yaml), announced via Ping.
  pub(crate) capture: bool,
  /// Parallel tunnel connections the client runs for this service
  /// (`connections:`), announced via Ping. Display-only: the server treats
  /// each connection as its own client regardless.
  pub(crate) connections: Option<u32>,
  /// The pool's floor and ceiling when the client runs an elastic one
  /// (`connections: {min, max}`); both absent for a fixed `connections: N`.
  /// Without them a pool sitting at its floor is indistinguishable from a
  /// fixed pool of the same size, so the dashboard cannot say whether the
  /// count beside it is expected to move.
  pub(crate) connections_min: Option<u32>,
  pub(crate) connections_max: Option<u32>,
  /// Settings the client resolved to something other than its config asked
  /// for (a bandwidth budget divided across connections, a clamped connection
  /// count, …), announced via Ping. Display-only, surfaced in the dashboard's
  /// per-connection config view.
  pub(crate) config_notes: Vec<crate::protocol::ConfigNote>,
  /// Static Prometheus labels this client announced, already validated and
  /// capped (planned_features #53). Attached to its own metric series only.
  pub(crate) metrics_labels: Vec<(String, String)>,
  /// True when the client announced a TCP target (experimental TCP tunneling).
  pub(crate) tcp_enabled: bool,
  /// Latest backend health verdict reported by the client's own probe
  /// (APERIO_TARGET_HEALTH). False = excluded from routing while the
  /// tunnel connection itself stays up.
  pub(crate) backend_healthy: bool,
  /// False only while a configured health check has not completed its first
  /// probe (dashboard shows "checking" instead of "backend down").
  pub(crate) backend_probed: bool,
  /// Announced load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: u32,
  /// Announced downstream link capacity in bytes/second (0 = unlimited).
  /// Shared with the connection's writer task, which paces outgoing frames.
  pub(crate) bandwidth_bps: Arc<AtomicU64>,
  /// Display name of the service this connection exposes (announced via
  /// Ping by multi-service clients).
  pub(crate) service_name: Option<String>,
  /// What that service is called on screen, when the client named one.
  pub(crate) service_custom_name: Option<String>,
  /// True when the client declared its service public AND its token permits
  /// publishing public services: the visitor auth gate is skipped for
  /// routes served exclusively by public clients.
  pub(crate) public: bool,
  /// Ensures the "public requested but not permitted" warning logs once.
  pub(crate) public_denied_warned: bool,
  /// Client-declared visitor login (`user:password`) for this service, honored
  /// only when the token may control the visitor gate. `None` = no override.
  pub(crate) visitor_auth: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach this client's service, declared via
  /// Ping (empty = everyone). Enforced against every proxied request routed
  /// here; invalid entries are dropped when the heartbeat is applied.
  pub(crate) allowed_ips: Vec<String>,
  /// Ensures the "visitor_auth requested but not permitted/invalid" warning
  /// logs once per connection.
  /// The client's full visitor-auth policy, when it declared one that the
  /// single `user:password` above cannot carry (`planned_features.md` #111).
  /// `None` means the scalar is the whole of what it said.
  pub(crate) visitor_auth_policy: Option<crate::visitor_auth::Policy>,
  pub(crate) visitor_auth_denied_warned: bool,
  /// Ensures the "nothing gates this service" warning fires once per client
  /// connection rather than on every heartbeat. It is the nudge before the
  /// default flips (`planned_features.md` #108), so it names the thing to
  /// write rather than only the state it found.
  pub(crate) ungated_warned: bool,
  /// Ensures the "allowed_ips entry invalid" warning fires once per client
  /// connection, not on every heartbeat.
  pub(crate) allowed_ips_invalid_warned: bool,
  /// True once this connection was warned about a malformed `scaling:` block,
  /// so a heartbeat every few seconds cannot flood the log.
  pub(crate) scaling_invalid_warned: bool,
  /// Tunnels declared by the client via Ping (`tunnels:` list): normally
  /// unexposed local services a peer client may bind with `--bind-tunnels`
  /// (same token, explicit client id required).
  pub(crate) tunnels: Vec<crate::protocol::TunnelDecl>,
  /// The client opted its service into the server-side response cache
  /// (`cache: true` via Ping). Effective only with APERIO_CACHE on.
  pub(crate) cache: bool,
  /// Ensures the "cache requested but the server cache is disabled" warning
  /// logs once per connection, not on every heartbeat.
  pub(crate) cache_ignored_warned: bool,
  /// The client asked for serve-stale resilience: cached responses for its
  /// routes stay servable (marked) while no healthy client is connected.
  pub(crate) resilience: bool,
  /// Client-declared request body cap for this service, in bytes (via Ping).
  /// Enforced before dispatch with an early 413; never loosens the global
  /// APERIO_MAX_BODY_SIZE limit.
  pub(crate) max_request_body: Option<u64>,
  /// Client-declared per-service response timeout, in seconds (via Ping).
  /// Overrides the global gateway response timeout for this service's
  /// dispatches (None = use the global value).
  pub(crate) response_timeout: Option<u64>,
  /// The client asked to persist inbound POSTs to this service into the
  /// webhook inbox (`webhook_inbox: true` via Ping).
  pub(crate) webhook_inbox: bool,
  /// Redirect URL for visitors this candidate's `allowed_ips` rejects
  /// (`denied:` via Ping). Used only when every candidate of a route rejects
  /// the visitor; without one anywhere, the answer is stealth (identical to
  /// an unclaimed route).
  pub(crate) denied: Option<String>,
  /// Passive outlier ejection: timestamps of recent dispatch failures
  /// (5xx / response timeout / connection loss) still inside the outlier
  /// window. Independent of the active `/health` probe (`backend_healthy`).
  pub(crate) recent_failures: VecDeque<Instant>,
  /// Instant until which this client is ejected from routing after crossing
  /// the failure threshold (None = not ejected). Re-admitted automatically.
  pub(crate) ejected_until: Option<Instant>,
}

pub(crate) struct ClientHandle {
  /// Sender channel to push messages to the client.
  pub(crate) tx: mpsc::Sender<Message>,
  /// Notified to force this connection's read loop to end (e.g. when the token
  /// it connected with is revoked), so the client leaves the routing pool at
  /// once instead of serving until it next reconnects.
  pub(crate) disconnect: Arc<Notify>,
  /// Instant when client connection was established.
  pub(crate) connected_at: Instant,
  /// Client remote IP address.
  pub(crate) client_ip: String,
  /// Instant of the last heartbeat Ping received from this client.
  pub(crate) last_ping_at: Option<Instant>,
  /// Permissions attached to the token this client authenticated with.
  pub(crate) perms: ClientPerms,
  /// True after the client announced a graceful shutdown: no new requests
  /// are routed to it while in-flight ones finish.
  pub(crate) draining: bool,
  /// The id the client calls this connection, `<base>-<service>` for the first
  /// of a service and `<base>-<service>-c<N>` for the rest. Not trusted for
  /// state changes (the server's own connection id is), but it is what names
  /// the *service* a connection belongs to, which is the unit the
  /// per-service connection ceiling is about.
  pub(crate) declared_client_id: Option<String>,
  /// Seconds this client says it gives its own in-flight requests when asked
  /// to stand down. Advisory: it sizes `shutdown_drain: auto`, under the
  /// operator's cap, and is never trusted on its own.
  pub(crate) drain_secs: Option<u64>,
  /// Client build version announced via Ping (None until the first Ping,
  /// or for clients predating version reporting).
  pub(crate) client_version: Option<String>,
  /// Tunnel protocol version announced via Ping.
  pub(crate) client_protocol: Option<u32>,
  /// What the client reports about itself (planned_features #37): CPU as a
  /// percentage of one core and resident memory of the client process, then
  /// round-trip time, jitter and reconnects of this tunnel connection. All
  /// `None` from a client that does not report them, and the process figures
  /// are `None` where they cannot be read without guessing.
  pub(crate) cpu_percent: Option<f64>,
  pub(crate) rss_bytes: Option<u64>,
  pub(crate) rtt_ms: Option<u64>,
  pub(crate) jitter_ms: Option<u64>,
  pub(crate) reconnects: Option<u32>,
  /// Client-process instance ID self-reported via Ping. Unlike the
  /// server-assigned connection ID it survives reconnects of the same
  /// process, letting the failover `wait` mode recognize a returning client.
  pub(crate) reported_instance_id: Option<String>,
  /// Process-wide instance group id from the `x-aperio-instance` handshake
  /// header (the client's raw `client_id` base). Shared by every service and
  /// parallel connection of one client process; used to group connections in
  /// the dashboard and to share one random hostname across them. `None` for
  /// clients that do not send the header.
  pub(crate) instance_group: Option<String>,
  /// Topic filters this connection has subscribed to. Held per connection
  /// because that is what the client re-sends after a reconnect, and reduced
  /// to one delivery per client *process* at publish time.
  pub(crate) subscriptions: Vec<String>,
  /// The services this connection carries.
  ///
  /// **Never empty.** A connection exists because something is served over
  /// it, and every construction puts one here; `sole` relies on that and is
  /// the only reason it can hand back a reference rather than an `Option`
  /// that four hundred call sites would have to answer for.
  ///
  /// A `Vec` while the length is always one, because the alternative was to
  /// keep the singular field and change it later, and "later" is where the
  /// four hundred sites come back. The representation is plural now; what is
  /// left is teaching each caller *which* service it means, and `sole` is
  /// the list of places that still have not been asked.
  pub(crate) services: Vec<ServiceState>,
}

impl ClientHandle {
  /// The one service this connection carries, on the assumption that there
  /// is exactly one.
  ///
  /// **Test-only, and that is the whole point.** `grep sole` was the remaining
  /// work of #46: every call was a place that had not been taught to pick a
  /// service, and while no client could open a connection with two the
  /// assumption cost nothing. `#120` shipped a client that can, which turned
  /// all thirty-five of them live at once (#122), so they were converted and
  /// the accessor was left where only a test can reach it. A test that builds
  /// a handle with one service and pokes at it is not assuming anything; it
  /// said so, and now the compiler holds it to that.
  ///
  /// Production code asks a question instead. Something about *this* service
  /// takes it from routing or from `match_declarations`, which decide rather
  /// than guess, through `service_at`. Something about the connection, or
  /// about the process behind it, iterates `services`, and there are named
  /// helpers for the answers that recur: `effective_hostnames`, `tunnels`,
  /// `serves_process_scoped`, `process_name`.
  ///
  /// Panics on an empty list, which is the honest reading of an invariant a
  /// `Vec` cannot express: every construction puts a service here, so an
  /// empty one is a bug in this file rather than anything a peer can cause.
  #[cfg(test)]
  pub(crate) fn sole(&self) -> &ServiceState {
    self
      .services
      .first()
      .expect("a connection always carries at least one service")
  }

  /// The mutable half of `sole`, with the same meaning and the same fence.
  #[cfg(test)]
  pub(crate) fn sole_mut(&mut self) -> &mut ServiceState {
    self
      .services
      .first_mut()
      .expect("a connection always carries at least one service")
  }

  /// One service by index, for callers that have been told which they mean.
  ///
  /// Unlike `sole` these carry no assumption, so they are not on the #46
  /// list. The index comes from `match_declarations` or from routing, both
  /// of which decide it rather than guess it.
  pub(crate) fn service_at(&self, index: usize) -> &ServiceState {
    &self.services[index]
  }

  pub(crate) fn service_at_mut(&mut self, index: usize) -> &mut ServiceState {
    &mut self.services[index]
  }
}

/// Permissions resolved at connection time from the presented token.
#[derive(Clone)]
pub(crate) struct ClientPerms {
  /// True for the master `APERIO_SERVER_TOKEN`: no restrictions.
  pub(crate) master: bool,
  /// Allowed hostname binds. Empty or containing "*" = unrestricted.
  pub(crate) hostnames: Vec<String>,
  /// Allowed path binds. Empty or containing "*" = unrestricted.
  pub(crate) paths: Vec<String>,
  /// Name of the dynamic token used (None for the master token).
  pub(crate) token_name: Option<String>,
  /// Record ID of the dynamic token used (None for the master token);
  /// rate limits and quotas key on this.
  pub(crate) token_id: Option<String>,
  /// May this token publish services as public (visitor auth gate skipped)?
  pub(crate) allow_public: bool,
  /// May this token bind another client's tunnels within its organization?
  pub(crate) allow_bind: bool,
  /// May this token send OpenTelemetry exports through the OTel bridge?
  pub(crate) allow_otel: bool,
  /// Topic filters this token may publish to and subscribe to. Empty = no
  /// messaging at all, which is the default for a token that never asked for
  /// it: a capability that switches itself on for everything predating it is
  /// how a permission model stops meaning anything.
  pub(crate) topics: Vec<String>,
  /// Organization this token (and therefore this client) belongs to
  /// (None = master).
  pub(crate) org_id: Option<String>,
  /// Hostname allowlist of that organization, resolved once at connection
  /// time (empty = unrestricted). It fences every bind this client can claim,
  /// *outside* of what its token permits: a token minted before the org was
  /// fenced, or one carrying `*`, still cannot bind beyond the org's own
  /// hostnames.
  pub(crate) org_hostnames: Vec<String>,
  /// Parallel connections per service this token permits. `None` = whatever
  /// the server allows. It can only lower the server's ceiling: the effective
  /// number is the smaller of the two, resolved by `connection_ceiling`.
  pub(crate) max_connections: Option<u32>,
}

impl ClientPerms {
  pub(crate) fn master() -> Self {
    ClientPerms {
      master: true,
      hostnames: Vec::new(),
      paths: Vec::new(),
      token_name: None,
      token_id: None,
      allow_public: true,
      allow_bind: true,
      allow_otel: true,
      topics: vec!["#".to_string()],
      org_id: None,
      org_hostnames: Vec::new(),
      max_connections: None,
    }
  }

  /// The ceiling in force for this connection: the server's setting, lowered
  /// by the token's own if it asked for less. A token asking for more is not
  /// an error, it simply does not get it, which is what makes the server's
  /// number a policy rather than a suggestion.
  pub(crate) fn connection_ceiling(&self, server_max: u32) -> u32 {
    match self.max_connections {
      Some(token_max) => token_max.min(server_max).max(1),
      None => server_max.max(1),
    }
  }

  /// True when the organization's allowlist admits `host`. The master token
  /// and the master organization are never fenced.
  pub(crate) fn org_hostname_allowed(&self, host: &str) -> bool {
    self.master || crate::store::orgs::hostname_in_org_allowlist(host, &self.org_hostnames)
  }

  pub(crate) fn hostname_allowed(&self, host: &str) -> bool {
    // Both fences must admit the bind: the organization's allowlist first
    // (which a token can never widen), then the token's own permissions.
    self.org_hostname_allowed(host)
      && (self.master
        || self.hostnames.is_empty()
        || self.hostnames.iter().any(|h| h == "*" || h == host))
  }

  pub(crate) fn path_allowed(&self, path: &str) -> bool {
    self.master || self.paths.is_empty() || self.paths.iter().any(|p| p == "*" || p == path)
  }

  /// Specific (non-wildcard) hostnames granted by the token; these are
  /// auto-bound to the client on connect. Filtered by the organization's
  /// allowlist, so a token minted before the org was fenced cannot auto-bind
  /// a hostname the org may no longer claim.
  pub(crate) fn granted_hostnames(&self) -> Vec<String> {
    self
      .hostnames
      .iter()
      .filter(|h| *h != "*" && self.org_hostname_allowed(h))
      .cloned()
      .collect()
  }

  /// First specific path granted by the token, used as the automatic path
  /// bind when the client did not declare one.
  pub(crate) fn granted_path(&self) -> Option<String> {
    self.paths.iter().find(|p| *p != "*").cloned()
  }
}

impl ClientHandle {
  /// A client is healthy while its last heartbeat (or, before the first
  /// Ping, its connection time) is within the down threshold.
  ///
  /// Connection-scoped on purpose, unlike the predicates below it: a
  /// heartbeat is the socket's, not any one service's.
  pub(crate) fn is_healthy(&self, down_threshold: Duration) -> bool {
    let reference = self.last_ping_at.unwrap_or(self.connected_at);
    reference.elapsed() < down_threshold
  }

  /// The `tunnels:` this connection declares.
  ///
  /// Process-wide on the client, which resolves one list and copies it onto
  /// every service, so any service's copy is the answer and the first is as
  /// good as any. Read through here rather than through `sole()` at four call
  /// sites, so the reason is stated once and a future per-service `tunnels:`
  /// has one place to change instead of four to find.
  pub(crate) fn tunnels(&self) -> &[crate::protocol::TunnelDecl] {
    self
      .services
      .first()
      .map(|s| s.tunnels.as_slice())
      .unwrap_or(&[])
  }

  /// Whether this connection can be used for something that belongs to the
  /// *process* rather than to one of its services: a raw `tunnels:` open, an
  /// `expose` lookup, the topology view.
  ///
  /// Any enabled service is enough, and that is not a shortcut: what these ask
  /// about is not a service, so there is no service whose kill switch is the
  /// right one to consult. Reading the first one's meant that disabling `web`
  /// from the dashboard silently took away a tunnel declared by the process
  /// and served just as well through `api`.
  pub(crate) fn serves_process_scoped(&self, down_threshold: Duration) -> bool {
    self.is_healthy(down_threshold)
      && !self.draining
      && self.services.iter().any(|s| s.admin_enabled)
  }

  /// Every hostname this connection serves, across all of its services.
  ///
  /// The union, not the first service's, because every caller of this means
  /// "is this connection answering for that name": the organization fence
  /// that refuses to let one org act on a hostname another org is serving,
  /// the scaling probe, and the edge document. Reading only the first service
  /// made a hostname served by the second invisible to all three, and for the
  /// fence that is a tenant boundary with a hole in it.
  ///
  /// A caller that means one particular service asks that `ServiceState`
  /// directly; routing already does.
  pub(crate) fn effective_hostnames(&self) -> Vec<&String> {
    let mut out: Vec<&String> = Vec::new();
    for service in &self.services {
      for h in service.effective_hostnames() {
        if !out.contains(&h) {
          out.push(h);
        }
      }
    }
    out
  }

  /// What to call this client *process* on screen.
  ///
  /// Every service it carries, joined, because the callers are the ones that
  /// describe the process rather than a service: a raw tunnel listing, a
  /// subscriber view. A connection carrying one service reads exactly as it
  /// did, which is the common case and the whole of every deployment before
  /// multiplexing.
  pub(crate) fn process_name(&self) -> Option<String> {
    let names: Vec<String> = self
      .services
      .iter()
      .filter_map(|s| s.display_name())
      .collect();
    (!names.is_empty()).then(|| names.join(", "))
  }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
