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
pub(crate) fn spawn_background(state: &Arc<AppState>, host: &str) {
  let state = state.clone();
  // Flush persistent stats periodically and once more on shutdown.
  let stats_flush_state = state.clone();
  crate::supervise::spawn_ticker("stats-flush", Duration::from_secs(30), move || {
    let state = stats_flush_state.clone();
    async move { flush_stats_once(&state).await }
  });

  // Autoscaling loops: the scale-out sampler and the record TTL sweep. Both
  // are no-ops unless the feature is enabled, so a server that never uses it
  // pays nothing.
  if state.config().scaling_enabled {
    let sampler_state = state.clone();
    crate::supervise::spawn_supervised("scaling-sampler", move || {
      crate::scaling::run_scale_out_loop(sampler_state.clone())
    });
    let prune_state = state.clone();
    let prune_ttl = state.config().scaling_record_ttl.as_secs();
    crate::supervise::spawn_supervised("scaling-prune", move || {
      crate::scaling::run_prune_loop(prune_state.clone(), prune_ttl)
    });
    info!("Autoscaling enabled: client `scaling:` declarations are honored");
  }

  // Availability ticker: observe every service entity and accrue elapsed
  // time into the uptime history (APERIO_UPTIME_TICK_SECS, default 10).
  let uptime_state = state.clone();
  let uptime_tick_secs = std::env::var("APERIO_UPTIME_TICK_SECS")
    .ok()
    .and_then(|v| v.parse::<u64>().ok())
    .filter(|v| *v >= 1)
    .unwrap_or(10);
  // The one loop that ticks before it sleeps, which is why it is written out
  // rather than using `spawn_ticker`: uptime accrues from the moment the
  // server is up, and skipping the first interval would lose it.
  crate::supervise::spawn_supervised("uptime", move || {
    let state = uptime_state.clone();
    async move {
      loop {
        uptime_tick_once(&state).await;
        tokio::time::sleep(Duration::from_secs(uptime_tick_secs)).await;
      }
    }
  });
  // Config hot-reload: watch aperio-server.yaml for changes and re-apply the
  // live-editable settings and structured headers/routes without a restart
  // (no `set_var`, so it is safe on the running server). Structural keys
  // (host/port/data_dir, proxy trust, OIDC, `expose` ports) still need a
  // restart. Off when no config file is in use, or when disabled explicitly.
  let hot_reload = std::env::var("APERIO_CONFIG_HOT_RELOAD")
    .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
    .unwrap_or(true);
  if hot_reload && let Some(watch_path) = config_file::watched_path() {
    let reload_state = state.clone();
    info!(
      "Watching {} for configuration changes",
      watch_path.display()
    );
    crate::supervise::spawn_supervised("config-hot-reload", move || {
      let state = reload_state.clone();
      let path = watch_path.clone();
      async move {
        // Re-read on restart rather than carrying a stale mtime across the
        // panic: a change made while the loop was down should be picked up,
        // not skipped because the remembered timestamp is newer.
        let mut last_mtime = std::fs::metadata(&path)
          .ok()
          .and_then(|m| m.modified().ok());
        loop {
          tokio::time::sleep(Duration::from_secs(5)).await;
          last_mtime = hot_reload_tick_once(&state, &path, last_mtime).await;
        }
      }
    });
  }

  // Threshold alerting: the two built-in rules (APERIO_ALERT_*) and any
  // operator-defined `alert_rules:`, evaluated by one background ticker and
  // emitted as webhook/audit events. The ticker runs when either exists,
  // since a file with only `alert_rules:` and no APERIO_ALERT_* would
  // otherwise have armed rules and nothing evaluating them.
  let rules = state.config().alert_rules.clone();
  for rule in rules.rules() {
    if !rule.metric.readable_here() {
      warn!(
        "Alert rule '{}' watches {}, which cannot be read on this platform; it will never fire",
        rule.name,
        rule.metric.as_str()
      );
    }
  }
  let alert_cfg = alerts::AlertConfig::from_env();
  if alert_cfg.is_some() || !rules.is_empty() {
    if !rules.is_empty() {
      info!("Alert rules armed: {}", rules.rules().len());
    }
    alerts::spawn(state.clone(), alert_cfg.unwrap_or_default());
  }

  // Token expiry early-warning ticker: emits one `token_expiring`
  // webhook/audit event per token (per expiry window) once its remaining
  // lifetime drops under APERIO_TOKEN_EXPIRY_WARNING seconds (default 24 h,
  // 0 disables). The warned set is in-memory: a restart re-arms warnings,
  // and a refresh (new expires_at) re-arms them too.
  let expiry_warning_secs = std::env::var("APERIO_TOKEN_EXPIRY_WARNING")
    .ok()
    .and_then(|v| v.parse::<u64>().ok())
    .unwrap_or(24 * 3600);
  if expiry_warning_secs > 0 {
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
          let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
          token_expiry_tick_once(&state, expiry_warning_secs, now, &mut warned).await;
        }
      }
    });
  }

  // Experimental public TCP expose ports (aperio-server.yaml `expose:`).
  expose::spawn_listeners(state.clone(), host, expose::from_config_file());

  // Per-data-type retention pruner (APERIO_RETENTION_*): inert when nothing
  // is configured.
  retention::spawn(state.clone());

  // Scheduled physical DB snapshots (APERIO_BACKUP_*): inert unless both an
  // interval and a directory are configured.
  backup::spawn(state.clone());

  // Routine sweeps of the per-IP/per-route rate buckets and expired
  // sessions, off the request path: the sweep used to ride on whichever
  // request drew the five-minute tick, with the lock held.
  let gc_state = state.clone();
  crate::supervise::spawn_ticker("rate-and-session-gc", Duration::from_secs(300), move || {
    let state = gc_state.clone();
    async move { state.gc_tick_once(Instant::now()).await }
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

/// One beat of the hot-reload watcher: compares the file's mtime against the
/// last one seen and re-applies the live-editable settings when it moved.
/// Returns the mtime to compare against next time.
pub(crate) async fn hot_reload_tick_once(
  state: &Arc<AppState>,
  watch_path: &std::path::Path,
  last_mtime: Option<std::time::SystemTime>,
) -> Option<std::time::SystemTime> {
  let mtime = std::fs::metadata(watch_path)
    .ok()
    .and_then(|m| m.modified().ok());
  if mtime == last_mtime {
    return last_mtime;
  }
  match config_file::reload() {
    Ok(_) => {
      let diff = state.reload_from_file().await;
      info!(
        "Reloaded {}: live settings and headers/routes re-applied (structural keys need a restart)",
        watch_path.display()
      );
      let detail = if diff.is_empty() {
        format!("{} (no live-setting changes)", watch_path.display())
      } else {
        format!("{} | {}", watch_path.display(), diff.join(", "))
      };
      state
        .audit("config_reloaded", "system", "system", &detail)
        .await;
    }
    Err(e) => warn!("Config reload of {} failed: {}", watch_path.display(), e),
  }
  mtime
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
