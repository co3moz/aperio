//! Every loop the server runs behind the request path: statistics flushing,
//! uptime sampling, config hot-reload, token expiry, alerting and the stores'
//! own housekeeping.
//!
//! Each tick is also a `*_tick_once` function, which is what lets a test drive
//! one iteration without waiting for a timer.

use crate::state::AppState;
use crate::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Spawns every background loop the server runs: the stats flush, the
/// autoscaling sampler and sweep, the uptime ticker, config hot-reload,
/// alerting, token-expiry warnings, the public expose listeners, retention,
/// backups, and the QoS 1 ack sweeper. Side effects only; the caller keeps
/// the state.
pub(crate) fn spawn_background(state: &Arc<AppState>) {
  let state = state.clone();
  // Flush persistent stats periodically and once more on shutdown.
  let stats_flush_state = state.clone();
  crate::supervise::spawn_ticker("stats-flush", Duration::from_secs(30), move || {
    let state = stats_flush_state.clone();
    async move { flush_stats_once(&state).await }
  });

  // Autoscaling loops: the scale-out sampler and the record TTL sweep. Both
  // check `scaling.enabled` every pass, so the feature can be switched on
  // and off without a restart, and a server that never uses it pays a sleep.
  let sampler_state = state.clone();
  crate::supervise::spawn_supervised("scaling-sampler", move || {
    crate::scaling::run_scale_out_loop(sampler_state.clone())
  });
  let prune_state = state.clone();
  crate::supervise::spawn_supervised("scaling-prune", move || {
    crate::scaling::run_prune_loop(prune_state.clone())
  });
  if state.config().scaling_enabled {
    info!("Autoscaling enabled: client `scaling:` declarations are honored");
  }

  // Availability ticker: observe every service entity and accrue elapsed
  // time into the uptime history (`uptime.tick`, default 10s, read each
  // beat).
  let uptime_state = state.clone();
  // The one loop that ticks before it sleeps, which is why it is written out
  // rather than using `spawn_ticker`: uptime accrues from the moment the
  // server is up, and skipping the first interval would lose it.
  crate::supervise::spawn_supervised("uptime", move || {
    let state = uptime_state.clone();
    async move {
      loop {
        uptime_tick_once(&state).await;
        let tick = crate::live::current().uptime.tick.as_secs().max(1);
        tokio::time::sleep(Duration::from_secs(tick)).await;
      }
    }
  });
  // Threshold alerting: the two built-in thresholds (`alerts`) and the
  // operator's own rules (`alerts.rules`), evaluated by one background ticker
  // that reads both every beat and emits webhook/audit events.
  for rule in state.config().alert_rules.rules() {
    if !rule.metric.readable_here() {
      warn!(
        "Alert rule '{}' watches {}, which cannot be read on this platform; it will never fire",
        rule.name,
        rule.metric.as_str()
      );
    }
  }
  alerts::spawn(state.clone());

  // Token expiry early-warning ticker: emits one `token_expiring`
  // webhook/audit event per token (per expiry window) once its remaining
  // lifetime drops under `security.token_expiry_warning` (default 1d, 0s
  // disables, read each beat). The warned set is in-memory: a restart
  // re-arms warnings, and a refresh (new expires_at) re-arms them too.
  let warn_state = state.clone();
  crate::supervise::spawn_supervised("token-expiry-warning", move || {
    let state = warn_state.clone();
    async move {
      // The warned set starts empty again after a restart, which re-arms
      // warnings that were already sent. That is the same thing a server
      // restart does, and a duplicate warning is a far better failure than
      // a silence caused by state nobody can inspect.
      let mut warned: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
      loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let window = crate::live::current()
          .security
          .token_expiry_warning
          .as_secs();
        if window == 0 {
          continue;
        }
        let now = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map(|d| d.as_secs())
          .unwrap_or(0);
        token_expiry_tick_once(&state, window, now, &mut warned).await;
      }
    }
  });

  // Per-data-type retention pruner (`retention`): inert when nothing is
  // configured.
  retention::spawn(state.clone());

  // Scheduled physical DB snapshots (`backup`): inert unless both an
  // interval and a directory are configured. Read once; a restart path.
  backup::spawn(state.clone());

  // Routine sweeps of the per-IP/per-route rate buckets and expired
  // sessions, off the request path: the sweep used to ride on whichever
  // request drew the five-minute tick, with the lock held.
  let gc_state = state.clone();
  crate::supervise::spawn_ticker("rate-and-session-gc", Duration::from_secs(300), move || {
    let state = gc_state.clone();
    async move { state.gc_tick_once(Instant::now()).await }
  });

  // Connections that stopped heart-beating are closed on the server's own
  // timer, so a ghost behind a proxy chain does not hold its slot for as
  // long as the operating system keeps the socket (#156).
  let reap_state = state.clone();
  crate::supervise::spawn_ticker("silent-client-reaper", Duration::from_secs(10), move || {
    let state = reap_state.clone();
    async move {
      state.reap_silent_clients().await;
    }
  });

  // Resends QoS 1 messages nobody acknowledged, and gives up on the ones
  // that waited out the window.
  crate::tunnel::pubsub::run_ack_sweeper(state);
}

/// One beat of the stats flush loop: writes whatever is dirty.
pub(crate) async fn flush_stats_once(state: &Arc<AppState>) {
  state.persistent_stats.lock().await.save_if_dirty();
  state.uptime.lock().await.save_if_dirty();
  state.activity.lock().await.save_if_dirty();
}

/// One beat of the availability ticker: observe every service entity and
/// accrue the elapsed time into the uptime history.
pub(crate) async fn uptime_tick_once(state: &Arc<AppState>) {
  let live = observe_service_availability(state).await;
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  state.uptime.lock().await.tick(now, live);
}

/// One beat of the token-expiry warning ticker: warns (once per token per
/// expiry) for every token whose remaining lifetime dropped under the window,
/// and forgets warnings whose expiry passed or moved.
pub(crate) async fn token_expiry_tick_once(
  state: &Arc<AppState>,
  expiry_warning_secs: u64,
  now: u64,
  warned: &mut std::collections::HashSet<(String, u64)>,
) {
  let expiring: Vec<(String, String, u64, Option<String>)> = {
    let store = state.token_store.lock().await;
    store
      .list()
      .iter()
      .filter_map(|t| {
        let exp = t.expires_at?;
        let expires_within = exp > now && exp - now <= expiry_warning_secs;
        (expires_within && !warned.contains(&(t.id.clone(), exp)))
          .then(|| (t.id.clone(), t.name.clone(), exp, t.org_id.clone()))
      })
      .collect()
  };
  for (id, name, exp, org) in expiring {
    warned.insert((id.clone(), exp));
    warn!(
      "Token '{}' expires in {} minutes (at unix {})",
      name,
      (exp - now) / 60,
      exp
    );
    state
      .audit_in(
        "token_expiring",
        "system",
        "system",
        org.clone(),
        &format!("name={} expires_at={}", name, exp),
      )
      .await;
    state
      .emit_event_in(
        "token_expiring",
        serde_json::json!({
          "id": id,
          "name": name,
          "expires_at": exp,
          "seconds_left": exp - now,
        }),
        org,
      )
      .await;
  }
  // Drop warned entries whose expiry has passed or moved (refresh).
  warned.retain(|(_, exp)| *exp > now);
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
