//! Autoscaling runtime: the control loop that turns "this bind needs capacity"
//! into one call to an operator-controlled endpoint.
//!
//! Aperio is the sensor and the policy, never the orchestrator. It knows the
//! demand (requests arriving, requests waiting on a client's concurrency
//! limiter) and the supply (connected clients and the limits they announced),
//! which no other component sees at once. What it does with that is emit a
//! *desired capacity* to a URL the operator owns; starting and stopping
//! anything is the provider's business.
//!
//! Two triggers share one state machine, because they are the same problem at
//! different points on the curve:
//!
//! * **cold start (0 to 1)**, a request arrives for a bind no client serves.
//! * **scale out (N to N+1)**, the connected pool is saturated.
//! * **scale in (N to N-1)**, it has been idle for far longer than it took to
//!   be called busy.
//!
//! Scale in emits a *lower* desired capacity to the same endpoint; it still
//! kills nothing, which keeps the rule that Aperio is the sensor and the
//! policy and never the orchestrator. It exists because the alternative,
//! leaving it entirely to a client noticing it is idle, only ever solves 1 to
//! 0: `idle_timeout` retires a whole client process, so a pool that scaled out
//! to six stays at six until traffic stops completely. The last instance is
//! still not this loop's business, going to zero is the client's own decision,
//! because it knows about in-flight requests the server cannot see.
//!
//! The machine per bind is `Idle -> Waking -> Idle`, with a cooldown and an
//! exponential backoff on failure, and a breaker that disarms a record that
//! keeps failing. Single flight is the point: a burst of a hundred requests
//! against a sleeping service produces exactly one call.

use std::collections::HashMap;
use std::net::IpAddr;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::state::AppState;
use crate::store::scaling::ScalingRecord;

/// Consecutive failures after which a record stops being called at all.
const BREAKER_THRESHOLD: u32 = 5;
/// Ceiling for the exponential backoff between failed attempts.
const MAX_BACKOFF_SECS: u64 = 15 * 60;
/// Calls in flight across every bind at once. A server restart can otherwise
/// fire one call per armed bind the moment traffic resumes.
const MAX_CONCURRENT_CALLS: usize = 8;

/// Why the server is asking for capacity. Rides along in the payload so the
/// receiving endpoint can tell a cold start from a scale-out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reason {
  ColdStart,
  ScaleOut,
  ScaleIn,
}

impl Reason {
  pub(crate) fn as_str(self) -> &'static str {
    match self {
      Reason::ColdStart => "cold_start",
      Reason::ScaleOut => "scale_out",
      Reason::ScaleIn => "scale_in",
    }
  }
}

/// Per-bind runtime state. Deliberately in memory: a restart should forget
/// that a wake was in flight rather than inherit a stale one, and the record
/// itself (which is persisted) is the only thing worth keeping.
#[derive(Default)]
pub(crate) struct BindState {
  /// Set while a call is in flight, so the burst behind it waits instead of
  /// firing again.
  waking: bool,
  /// Earliest instant at which another call may be made.
  cooldown_until: Option<Instant>,
  /// Consecutive failures, driving the backoff and the breaker.
  failures: u32,
  /// Set once the breaker tripped; cleared by a successful call or by the
  /// record being re-armed.
  disarmed: bool,
  /// First instant utilization was seen above the target, so `window` can be
  /// enforced without a background sampler holding history.
  saturated_since: Option<Instant>,
  /// The same, for the far side of the curve: first instant utilization was
  /// seen well below the target.
  idle_since: Option<Instant>,
}

/// Fraction of the target below which a bind counts as over-provisioned.
///
/// Not the target itself: a pool hovering at exactly the target would then be
/// both saturated and idle, and would scale out and in on alternating
/// samples. The gap between this and the target is the hysteresis.
const SCALE_IN_FRACTION: f64 = 0.5;

/// How many `window`s of idleness are needed to give an instance back.
///
/// Deliberately asymmetric with scale-out, which needs one. Being an instance
/// short costs latency on live traffic while being one over costs money, and
/// the cost of guessing wrong on the way down is paid by the visitor who
/// arrives during the next cold start.
const SCALE_IN_WINDOWS: u32 = 4;

/// The whole runtime, keyed by record id.
#[derive(Default)]
pub(crate) struct ScalingRuntime {
  binds: HashMap<String, BindState>,
}

/// What [`ScalingRuntime::begin`] decided.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum Begin {
  /// This caller owns the call and must perform it.
  Fire,
  /// A call is already in flight; do not make a second one.
  AlreadyWaking,
  /// Cooling down, disarmed, or otherwise not callable right now.
  Skip,
}

impl ScalingRuntime {
  /// Claims the right to call the endpoint for `record`, or reports that
  /// somebody else already has it. This is the single-flight gate: exactly one
  /// caller gets `Fire`, everyone else gets something to wait on.
  pub(crate) fn begin(&mut self, record: &ScalingRecord, now: Instant) -> Begin {
    let state = self.binds.entry(record.id.clone()).or_default();
    if state.disarmed {
      return Begin::Skip;
    }
    if state.waking {
      return Begin::AlreadyWaking;
    }
    if state.cooldown_until.is_some_and(|until| now < until) {
      return Begin::Skip;
    }
    state.waking = true;
    Begin::Fire
  }

  /// Records the outcome of a call and reopens the gate. A success clears the
  /// failure count; a failure backs off exponentially and eventually disarms
  /// the record, so a permanently broken endpoint is not called forever.
  pub(crate) fn finish(&mut self, id: &str, success: bool, cooldown: Duration, now: Instant) {
    let state = self.binds.entry(id.to_string()).or_default();
    state.waking = false;
    if success {
      state.failures = 0;
      state.cooldown_until = Some(now + cooldown);
    } else {
      state.failures = state.failures.saturating_add(1);
      let backoff = cooldown
        .as_secs()
        .saturating_mul(1u64 << state.failures.min(6))
        .min(MAX_BACKOFF_SECS);
      state.cooldown_until = Some(now + Duration::from_secs(backoff.max(1)));
      if state.failures >= BREAKER_THRESHOLD {
        state.disarmed = true;
      }
    }
  }

  /// True when the breaker has tripped for this record.
  pub(crate) fn is_disarmed(&self, id: &str) -> bool {
    self.binds.get(id).is_some_and(|s| s.disarmed)
  }

  /// Re-arms a record after a config change or an operator action.
  pub(crate) fn rearm(&mut self, id: &str) {
    let state = self.binds.entry(id.to_string()).or_default();
    state.disarmed = false;
    state.failures = 0;
    state.cooldown_until = None;
  }

  /// Feeds one utilization sample in and reports whether the bind has now been
  /// saturated for its whole window. Keeping the "since" instant here means no
  /// history buffer and no separate sampler state.
  pub(crate) fn saturation_reached(
    &mut self,
    record: &ScalingRecord,
    utilization: f64,
    now: Instant,
  ) -> bool {
    let state = self.binds.entry(record.id.clone()).or_default();
    if utilization < record.target_utilization {
      state.saturated_since = None;
      return false;
    }
    state.idle_since = None;
    match state.saturated_since {
      Some(since) => now.duration_since(since) >= Duration::from_secs(record.window_secs),
      None => {
        state.saturated_since = Some(now);
        false
      }
    }
  }

  /// The mirror of `saturation_reached`: reports whether the bind has been
  /// well under its target for long enough to give an instance back.
  pub(crate) fn idle_reached(
    &mut self,
    record: &ScalingRecord,
    utilization: f64,
    now: Instant,
  ) -> bool {
    let state = self.binds.entry(record.id.clone()).or_default();
    if utilization >= record.target_utilization * SCALE_IN_FRACTION {
      state.idle_since = None;
      return false;
    }
    let window = Duration::from_secs(record.window_secs.saturating_mul(SCALE_IN_WINDOWS as u64));
    match state.idle_since {
      Some(since) => now.duration_since(since) >= window,
      None => {
        state.idle_since = Some(now);
        false
      }
    }
  }

  /// Forgets a bind entirely (record deleted).
  pub(crate) fn forget(&mut self, id: &str) {
    self.binds.remove(id);
  }
}

/// Live capacity of one bind's pool.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct Capacity {
  /// Clients currently eligible to serve the bind.
  pub instances: u32,
  /// Sum of their announced concurrency limits (0 when none announced one, in
  /// which case utilization is not computable and stays 0).
  pub capacity: u32,
  /// Requests those clients are handling right now.
  pub inflight: u32,
  /// `inflight / capacity`, 0 when capacity is unknown.
  pub utilization: f64,
}

/// Measures a bind's pool: how many clients serve it, how much concurrency
/// they announced, and how much of it is in use.
///
/// The concurrency limiter each client carries *is* the demand signal: the
/// server queues on it rather than flooding a backend, so permits taken is
/// exactly "work the pool is currently doing", and requests waiting on it are
/// the ones a bigger pool would have absorbed. Raw request counts are far too
/// noisy to scale on.
pub(crate) async fn measure(state: &AppState, hostname: &str, path: Option<&str>) -> Capacity {
  let clients = state.clients.read().await;
  let down_threshold = state.config().client_down_threshold;
  let mut out = Capacity::default();
  for client in clients.values() {
    // Only clients that could actually take a request count as capacity.
    let eligible = client.is_healthy(down_threshold)
      && client.service.backend_healthy
      && !client.draining
      && client.service.admin_enabled
      // Standby tiers exist to be idle; counting them would hide saturation
      // of the primaries under `primary-standby`.
      && client.service.priority == 0
      && client.effective_hostnames().iter().any(|h| **h == hostname)
      && match path {
        Some(p) => client.effective_path_bind().map(String::as_str) == Some(p),
        None => true,
      };
    if !eligible {
      continue;
    }
    out.instances += 1;
    if let (Some(limit), Some(limiter)) = (
      client.service.max_concurrent,
      client.service.inflight_limiter.as_ref(),
    ) {
      out.capacity += limit;
      out.inflight += limit.saturating_sub(limiter.available_permits() as u32);
    }
  }
  if out.capacity > 0 {
    out.utilization = f64::from(out.inflight) / f64::from(out.capacity);
  }
  out
}

/// Rejects a destination the server must not be talked into calling.
///
/// The rule itself now lives in `outbound`, because a client-declared
/// `jwt.jwks_url` needs the identical one and two copies of an SSRF boundary
/// is one too many. The flags stay here: they are this feature's.
async fn destination_allowed(
  url: &url::Url,
  allow_insecure: bool,
  allow_private: bool,
) -> Result<(), String> {
  crate::outbound::client_declared_destination_allowed(
    url,
    allow_insecure,
    allow_private,
    "APERIO_SCALING",
  )
  .await
}

/// Performs one call to the record's endpoint. Returns Ok for a 2xx.
///
/// Nothing about the response is used beyond its status: the body is never
/// read into the server, redirects are never followed (a redirect is a way to
/// reach an address the pre-flight check just refused), and the secret never
/// reaches the log.
pub(crate) async fn call_endpoint(
  state: &AppState,
  record: &ScalingRecord,
  reason: Reason,
  current: u32,
  desired: u32,
) -> Result<(), String> {
  let url = url::Url::parse(&record.url).map_err(|e| format!("invalid url: {e}"))?;
  destination_allowed(
    &url,
    state.config().scaling_allow_http,
    state.config().scaling_allow_private,
  )
  .await?;
  // The operator's outbound policy (allowlist / block_private) applies on
  // top of the scaling-specific rules; empty policy = no extra restriction.
  state
    .config()
    .outbound_policy
    .check(record.url.as_str())
    .await?;

  let payload = serde_json::json!({
    "reason": reason.as_str(),
    "hostname": record.hostname,
    "path": record.path,
    "org_id": record.org_id,
    "current": current,
    "desired": desired,
    "min": record.min,
    "max": record.max,
  });

  let client = crate::outbound::client_builder()
    .timeout(Duration::from_secs(10))
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|e| format!("cannot build the http client: {e}"))?;
  let mut req = client.post(url.clone()).json(&payload);
  if let Some(ref secret) = record.secret {
    req = req.bearer_auth(secret);
  }
  let response = req
    .send()
    .await
    .map_err(|e| format!("request failed: {e}"))?;
  let status = response.status();
  if status.is_success() {
    Ok(())
  } else {
    Err(format!("endpoint answered {status}"))
  }
}

/// Whether the caller should hold a visitor request after asking for capacity.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum Ask {
  /// Capacity was asked for (or is already being asked for): an instance is
  /// plausibly on its way, so holding the request is worthwhile.
  Hold,
  /// Nothing is coming: the call failed, the record is disarmed, or the
  /// global call budget is exhausted. Holding would only add latency to a
  /// failure the visitor is going to see anyway.
  DoNotHold,
}

/// Asks for capacity for one bind, once.
///
/// Every caller funnels through here, so the single-flight gate, the global
/// concurrency cap, the audit trail and the breaker all apply uniformly to
/// cold starts and scale-outs alike.
pub(crate) async fn request_capacity(
  state: &Arc<AppState>,
  record: &ScalingRecord,
  reason: Reason,
  current: u32,
) -> Ask {
  let now = Instant::now();
  let gate = {
    let mut runtime = state.scaling_runtime.lock().await;
    runtime.begin(record, now)
  };
  match gate {
    Begin::Fire => {}
    // Somebody else is already calling: wait on their result rather than
    // making a second call.
    Begin::AlreadyWaking => return Ask::Hold,
    // Cooling down after a recent call (an instance may still be arriving) is
    // worth holding for; a disarmed record is not.
    Begin::Skip => {
      return if state.scaling_runtime.lock().await.is_disarmed(&record.id) {
        Ask::DoNotHold
      } else {
        Ask::Hold
      };
    }
  }

  // Bound the blast radius: after a restart, traffic resuming across many
  // binds must not turn into a burst of outbound calls.
  let permit = match state.scaling_calls.clone().try_acquire_owned() {
    Ok(permit) => permit,
    Err(_) => {
      let mut runtime = state.scaling_runtime.lock().await;
      runtime.finish(
        &record.id,
        false,
        Duration::from_secs(record.cooldown_secs),
        now,
      );
      warn!(
        "Scaling call for {} skipped: {} calls already in flight",
        record.hostname, MAX_CONCURRENT_CALLS
      );
      return Ask::DoNotHold;
    }
  };

  let desired = match reason {
    Reason::ColdStart => 1,
    Reason::ScaleOut => current.saturating_add(1),
    // Never below one from here: going to zero is the client's own decision
    // through `idle_timeout`, which knows about requests in flight that the
    // server cannot see.
    Reason::ScaleIn => current.saturating_sub(1).max(record.min).max(1),
  };
  let outcome = call_endpoint(state, record, reason, current, desired).await;
  drop(permit);

  let success = outcome.is_ok();
  match &outcome {
    Ok(()) => info!(
      "Scaling: asked for {} instance(s) of {} ({})",
      desired,
      record.hostname,
      reason.as_str()
    ),
    Err(e) => warn!(
      "Scaling: call for {} failed ({}): {}",
      record.hostname,
      reason.as_str(),
      e
    ),
  }
  {
    let mut runtime = state.scaling_runtime.lock().await;
    runtime.finish(
      &record.id,
      success,
      Duration::from_secs(record.cooldown_secs),
      now,
    );
    if runtime.is_disarmed(&record.id) {
      warn!(
        "Scaling: {} disarmed after {} consecutive failures; re-announce or edit the record to re-arm",
        record.hostname, BREAKER_THRESHOLD
      );
    }
  }
  state
    .audit_in(
      if success {
        "scaling_requested"
      } else {
        "scaling_failed"
      },
      "-",
      "-",
      record.org_id.clone(),
      &format!(
        "hostname={} reason={} current={} desired={}{}",
        record.hostname,
        reason.as_str(),
        current,
        desired,
        match &outcome {
          Ok(()) => String::new(),
          Err(e) => format!(" error={e}"),
        }
      ),
    )
    .await;
  state
    .emit_event_in(
      "scaling_requested",
      serde_json::json!({
        "hostname": record.hostname,
        "path": record.path,
        "reason": reason.as_str(),
        "current": current,
        "desired": desired,
        "ok": success,
      }),
      record.org_id.clone(),
    )
    .await;
  // A refused or failed call means nothing is starting, so the visitor should
  // not be held for the full budget waiting for it.
  if success { Ask::Hold } else { Ask::DoNotHold }
}

/// Background loop: samples every armed record's pool and asks for one more
/// instance when it has been saturated for its whole window, or one fewer when
/// it has been idle for several of them.
///
/// Sampling here rather than on the request path is deliberate: the hot path
/// must not pay for autoscaling, and a scale-out decision needs a *duration*
/// of saturation, which a single request cannot observe.
pub(crate) async fn run_scale_out_loop(state: Arc<AppState>) {
  loop {
    tokio::time::sleep(Duration::from_secs(5)).await;
    let records: Vec<ScalingRecord> = state.scaling_store.lock().await.list().to_vec();
    for record in records {
      // max = 0 means cold starts only; there is no scale-out target.
      if record.max == 0 {
        continue;
      }
      let capacity = measure(&state, &record.hostname, record.path.as_deref()).await;
      // Nothing running, or no announced limits to reason about: the cold
      // start path owns the first case, and the second is not measurable.
      if capacity.instances == 0 || capacity.capacity == 0 {
        continue;
      }
      // At the ceiling there is nothing to scale out to, but there may still
      // be something to scale in from, so this only skips the saturation half.
      let at_ceiling = capacity.instances >= record.max;
      let reached = !at_ceiling && {
        let mut runtime = state.scaling_runtime.lock().await;
        runtime.saturation_reached(&record, capacity.utilization, Instant::now())
      };
      if reached {
        request_capacity(&state, &record, Reason::ScaleOut, capacity.instances).await;
        continue;
      }
      // The far side of the curve. Guarded by the floor rather than by the
      // ceiling: a pool already at `min`, or at one instance, has nothing to
      // give back.
      if capacity.instances > record.min.max(1) {
        let idle = {
          let mut runtime = state.scaling_runtime.lock().await;
          runtime.idle_reached(&record, capacity.utilization, Instant::now())
        };
        if idle {
          request_capacity(&state, &record, Reason::ScaleIn, capacity.instances).await;
        }
      }
    }
  }
}

/// Background loop: drops records nothing has re-announced for a long time, so
/// a service that was decommissioned does not stay wakeable forever.
pub(crate) async fn run_prune_loop(state: Arc<AppState>, ttl_secs: u64) {
  loop {
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let now = crate::store::tokens::now_secs();
    let removed = state.scaling_store.lock().await.prune(ttl_secs, now);
    if removed > 0 {
      info!("Pruned {} stale autoscaling record(s)", removed);
    }
  }
}

/// Concurrency cap shared by every outbound call.
pub(crate) fn call_semaphore() -> Arc<tokio::sync::Semaphore> {
  Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CALLS))
}

#[cfg(test)]
#[path = "scaling_tests.rs"]
mod tests;

/// Asks for a cold start for the bind a request just missed, then holds the
/// request until a client for it appears or the budget runs out.
///
/// Everything that must *not* trigger a paid cold start is filtered here:
/// a hostname in maintenance, a visitor the token's IP scope would have
/// rejected (with an empty pool there are no candidates left to evaluate that
/// against, so it has to happen up front), and requests that are not worth
/// waking for.
pub(crate) async fn cold_start_wait(
  state: &Arc<AppState>,
  hostname: Option<&str>,
  path: &str,
  visitor_ip: IpAddr,
) {
  let Some(hostname) = hostname else {
    return;
  };
  // Maintenance mode is explicit operator intent: the site is meant to be
  // down, so waking it would fight the operator.
  if state.maintenance_for(Some(hostname)).await.is_some() {
    return;
  }
  let record = {
    let store = state.scaling_store.lock().await;
    // Look up by bind, not by organization: a request carries a hostname and
    // a path, and only one organization can serve a hostname at a time. The
    // path-scoped record wins over the hostname-wide one when both exist.
    let mut best: Option<&ScalingRecord> = None;
    for candidate in store.list() {
      if candidate.hostname != hostname {
        continue;
      }
      let matches = match candidate.path.as_deref() {
        Some(p) => path == p || path.starts_with(&format!("{p}/")),
        None => true,
      };
      if !matches {
        continue;
      }
      if best.is_none_or(|current| current.path.is_none() && candidate.path.is_some()) {
        best = Some(candidate);
      }
    }
    best.cloned()
  };
  let Some(record) = record.filter(|r| r.cold_start_enabled()) else {
    return;
  };
  // With no client connected there is no candidate whose `allowed_ips` could
  // reject this visitor, so the check that would normally happen during
  // selection has to happen here. Without it, an address that would be denied
  // could still trigger a billable cold start and learn the route exists.
  if !visitor_allowed(state, &record, visitor_ip).await {
    return;
  }

  if request_capacity(state, &record, Reason::ColdStart, 0).await == Ask::DoNotHold {
    return;
  }

  // Wait for a *routable* candidate, not merely a connected client: an
  // instance that comes up with a failing backend probe is not yet able to
  // serve, and returning early would just produce the 504 we are avoiding.
  let deadline = Instant::now() + Duration::from_secs(record.cold_start_secs);
  let mut rx = state.client_connected.subscribe();
  loop {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
      return;
    }
    let capacity = measure(state, &record.hostname, record.path.as_deref()).await;
    if capacity.instances > 0 {
      return;
    }
    // Any connection change is a reason to re-measure; the timeout bounds the
    // wait when nothing ever arrives.
    if tokio::time::timeout(remaining.min(Duration::from_millis(500)), rx.changed())
      .await
      .is_err()
    {
      continue;
    }
  }
}

/// True when the record's owning tokens would admit this visitor. A record
/// whose owners place no IP restriction admits everyone, which is the common
/// case and costs one lock.
async fn visitor_allowed(state: &AppState, record: &ScalingRecord, visitor_ip: IpAddr) -> bool {
  if record.owners.is_empty() {
    return true;
  }
  let store = state.token_store.lock().await;
  let mut restricted = false;
  for owner in &record.owners {
    let Some(token) = store.list().iter().find(|t| &t.id == owner) else {
      continue;
    };
    if token.allowed_ips.is_empty() {
      return true;
    }
    restricted = true;
    if crate::auth::ip_allowed(visitor_ip, &token.allowed_ips) {
      return true;
    }
  }
  !restricted
}
