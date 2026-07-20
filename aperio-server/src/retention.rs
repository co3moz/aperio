//! Per-data-type retention policies: a background pruner that enforces
//! independent TTLs on the persisted traffic records.
//!
//! Configuration (all in **days**, unset/0 = keep forever, the historical
//! behavior):
//!
//! - `APERIO_RETENTION_CAPTURES`   — inspector captures and webhook inbox entries
//! - `APERIO_RETENTION_ACCESS_LOG` — lines of the `APERIO_ACCESS_LOG` file
//! - `APERIO_RETENTION_AUDIT`     — audit events (rotated generations whose
//!   newest event expired are deleted whole; the active file only loses its
//!   leading prefix, keeping the hash chain verifiable)
//! - `APERIO_RETENTION_STATS`     — day-granularity statistics buckets
//!   (week/month/year buckets keep their built-in caps)
//!
//! The pruner runs once at startup and then hourly. Each cycle that removes
//! anything is logged; a `retention_pruned` audit event records the counts.

use std::sync::Arc;
use tracing::{info, warn};

use crate::state::AppState;

/// Seconds between pruner cycles.
const PRUNE_INTERVAL_SECS: u64 = 3600;

/// Reads one retention env var: days, `None` when unset, unparsable, or 0.
fn retention_days(name: &str) -> Option<u64> {
  std::env::var(name)
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .filter(|d| *d > 0)
}

/// Unix cutoff timestamp for a TTL of `days` days.
fn cutoff_ts(days: u64) -> u64 {
  crate::store::tokens::now_secs().saturating_sub(days.saturating_mul(24 * 3600))
}

/// What one pruner cycle removed, per surface.
#[derive(Default)]
pub(crate) struct RetentionReport {
  pub(crate) captures: usize,
  pub(crate) inbox: usize,
  pub(crate) access_log_lines: usize,
  pub(crate) audit_events: usize,
  pub(crate) stats_buckets: usize,
}

impl RetentionReport {
  fn total(&self) -> usize {
    self.captures + self.inbox + self.access_log_lines + self.audit_events + self.stats_buckets
  }
}

/// Rewrites the access log file in place, dropping lines whose `ts` field is
/// older than the cutoff. Holds the append lock across the rewrite (same
/// discipline as the right-to-erasure purge).
fn prune_access_log(state: &AppState, cutoff: u64) -> usize {
  let Some(path) = state.access_log_path.as_deref() else {
    return 0;
  };
  let Some(file_lock) = state.access_log.as_ref() else {
    return 0;
  };
  let Ok(mut guard) = file_lock.lock() else {
    return 0;
  };
  let Ok(raw) = std::fs::read_to_string(path) else {
    return 0;
  };
  let mut kept = String::with_capacity(raw.len());
  let mut removed = 0usize;
  for line in raw.lines() {
    let expired = serde_json::from_str::<serde_json::Value>(line)
      .ok()
      .and_then(|v| {
        v.get("ts")
          .and_then(|t| t.as_str())
          .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
      })
      .is_some_and(|dt| (dt.timestamp() as u64) < cutoff);
    if expired {
      removed += 1;
    } else {
      kept.push_str(line);
      kept.push('\n');
    }
  }
  if removed > 0 {
    if crate::store::atomic_write(std::path::Path::new(path), kept.as_bytes()).is_err() {
      warn!("Retention: failed to rewrite the access log {}", path);
      return 0;
    }
    // Reopen the append handle past the truncated content.
    match std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
    {
      Ok(f) => *guard = f,
      Err(e) => warn!("Retention: failed to reopen the access log {}: {}", path, e),
    }
  }
  removed
}

/// Runs one pruner cycle over every configured surface.
pub(crate) async fn run_cycle(state: &Arc<AppState>) -> RetentionReport {
  let mut report = RetentionReport::default();

  if let Some(days) = retention_days("APERIO_RETENTION_CAPTURES") {
    let cutoff = cutoff_ts(days);
    {
      let mut captures = state.captured_requests.lock().await;
      let before = captures.len();
      captures.retain(|c| {
        chrono::DateTime::parse_from_rfc3339(&c.timestamp)
          .map(|dt| dt.timestamp() as u64 >= cutoff)
          .unwrap_or(true)
      });
      report.captures = before - captures.len();
    }
    report.inbox = state.inbox_store.lock().await.prune_older_than(cutoff);
  }

  if let Some(days) = retention_days("APERIO_RETENTION_ACCESS_LOG") {
    report.access_log_lines = prune_access_log(state, cutoff_ts(days));
  }

  if let Some(days) = retention_days("APERIO_RETENTION_AUDIT") {
    report.audit_events = state.audit.lock().await.prune_older_than(cutoff_ts(days));
  }

  if let Some(days) = retention_days("APERIO_RETENTION_STATS") {
    report.stats_buckets = state
      .persistent_stats
      .lock()
      .await
      .prune_day_buckets_older_than(days);
  }

  report
}

// --- Disk-usage guard (#115) ------------------------------------------------

/// Fraction of the cap at which the early-warning event fires.
const DISK_WARN_RATIO: f64 = 0.9;
/// Fraction of the cap the warning episode resets below (hysteresis).
const DISK_WARN_RESET_RATIO: f64 = 0.8;
/// One warning per episode: set while a `disk_usage_warning` is outstanding.
static DISK_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The configured cap on `aperio.db` (plus its WAL/SHM sidecars), in bytes.
fn db_max_bytes() -> Option<u64> {
  std::env::var("APERIO_DB_MAX_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .filter(|b| *b > 0)
}

/// Current on-disk footprint of the SQLite store: the database file plus its
/// `-wal` and `-shm` sidecars.
fn db_size_bytes(data_dir: &std::path::Path) -> u64 {
  ["aperio.db", "aperio.db-wal", "aperio.db-shm"]
    .iter()
    .filter_map(|name| std::fs::metadata(data_dir.join(name)).ok())
    .map(|m| m.len())
    .sum()
}

/// One disk-guard cycle: warns as the cap nears (once per episode, with
/// hysteresis) and, past the cap, prunes the lowest-priority persisted data —
/// oldest webhook inbox entries, oldest webhook deliveries, oldest day-stat
/// buckets — then vacuums so the file actually shrinks.
async fn disk_guard_cycle(state: &Arc<AppState>, cap: u64, data_dir: &std::path::Path) {
  use std::sync::atomic::Ordering;
  let size = db_size_bytes(data_dir);

  if (size as f64) < cap as f64 * DISK_WARN_RESET_RATIO {
    DISK_WARNED.store(false, Ordering::SeqCst);
  } else if (size as f64) >= cap as f64 * DISK_WARN_RATIO
    && !DISK_WARNED.swap(true, Ordering::SeqCst)
  {
    warn!(
      "Disk guard: aperio.db is at {} of the {} byte cap",
      size, cap
    );
    state
      .audit(
        "disk_usage_warning",
        "system",
        "-",
        &format!("size={} cap={}", size, cap),
      )
      .await;
    state
      .emit_event(
        "disk_usage_warning",
        serde_json::json!({"size_bytes": size, "cap_bytes": cap}),
      )
      .await;
  }

  if size <= cap {
    return;
  }

  // Over the cap: shed the lowest-priority stores, oldest first. Halving the
  // caps each cycle converges without wiping everything in one swing.
  let inbox_removed = state.inbox_store.lock().await.truncate_oldest(100);
  let deliveries_removed = state.webhook_deliveries.lock().await.truncate_oldest(100);
  let stat_buckets_removed = state
    .persistent_stats
    .lock()
    .await
    .drop_oldest_day_buckets(15);
  state.persistent_stats.lock().await.vacuum();
  let after = db_size_bytes(data_dir);
  warn!(
    "Disk guard: pruned inbox={} deliveries={} day_buckets={} and vacuumed: {} → {} bytes (cap {})",
    inbox_removed, deliveries_removed, stat_buckets_removed, size, after, cap
  );
  state
    .audit(
      "disk_pruned",
      "system",
      "-",
      &format!(
        "inbox={} deliveries={} day_buckets={} size_before={} size_after={} cap={}",
        inbox_removed, deliveries_removed, stat_buckets_removed, size, after, cap
      ),
    )
    .await;
  state
    .emit_event(
      "disk_pruned",
      serde_json::json!({
        "inbox_removed": inbox_removed,
        "deliveries_removed": deliveries_removed,
        "day_buckets_removed": stat_buckets_removed,
        "size_before_bytes": size,
        "size_after_bytes": after,
        "cap_bytes": cap,
      }),
    )
    .await;
}

/// Spawns the background pruner: one cycle at startup, then hourly. Inert
/// when neither a retention variable nor the disk cap is configured.
pub(crate) fn spawn(state: Arc<AppState>) {
  let configured: Vec<&str> = [
    "APERIO_RETENTION_CAPTURES",
    "APERIO_RETENTION_ACCESS_LOG",
    "APERIO_RETENTION_AUDIT",
    "APERIO_RETENTION_STATS",
  ]
  .into_iter()
  .filter(|name| retention_days(name).is_some())
  .collect();
  let disk_cap = db_max_bytes();
  if configured.is_empty() && disk_cap.is_none() {
    return;
  }
  // The data dir is where the settings overrides live.
  let data_dir = state
    .settings_path
    .parent()
    .map(|p| p.to_path_buf())
    .unwrap_or_else(|| std::path::PathBuf::from("."));
  info!(
    "Retention pruner enabled ({}{}); running at startup and hourly",
    configured.join(", "),
    if disk_cap.is_some() {
      if configured.is_empty() {
        "APERIO_DB_MAX_BYTES"
      } else {
        ", APERIO_DB_MAX_BYTES"
      }
    } else {
      ""
    }
  );
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(PRUNE_INTERVAL_SECS));
    loop {
      interval.tick().await;
      if let Some(cap) = disk_cap {
        disk_guard_cycle(&state, cap, &data_dir).await;
      }
      let report = run_cycle(&state).await;
      if report.total() > 0 {
        info!(
          "Retention pruned {} record(s): captures={} inbox={} access_log_lines={} audit_events={} stats_buckets={}",
          report.total(),
          report.captures,
          report.inbox,
          report.access_log_lines,
          report.audit_events,
          report.stats_buckets
        );
        state
          .audit(
            "retention_pruned",
            "system",
            "-",
            &format!(
              "captures={} inbox={} access_log_lines={} audit_events={} stats_buckets={}",
              report.captures,
              report.inbox,
              report.access_log_lines,
              report.audit_events,
              report.stats_buckets
            ),
          )
          .await;
      }
    }
  });
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
