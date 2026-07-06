use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{error, info};

/// Counters for a single calendar period (day/week/month/year).
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PeriodStats {
  pub requests: u64,
  pub success: u64,
  pub failed: u64,
  pub bytes_sent: u64,
  pub bytes_received: u64,
  pub duration_ms: u64,
}

/// Counters that survive server restarts, plus per-period breakdowns.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PersistentStats {
  /// All-time totals — never reset.
  pub total_requests: u64,
  pub total_success: u64,
  pub total_failed: u64,
  /// Bytes sent to public visitors (response bodies).
  pub total_bytes_sent: u64,
  /// Bytes received from public visitors (request bodies).
  pub total_bytes_received: u64,
  /// Sum of request durations; divide by total_requests for the average.
  pub total_request_duration_ms: u64,
  /// Period buckets keyed as `d:2026-07-06`, `w:2026-W27`, `m:2026-07`, `y:2026`.
  pub periods: HashMap<String, PeriodStats>,
}

impl PersistentStats {
  /// Average response time in milliseconds across all recorded requests.
  pub fn avg_response_ms(&self) -> f64 {
    if self.total_requests == 0 {
      0.0
    } else {
      self.total_request_duration_ms as f64 / self.total_requests as f64
    }
  }
}

/// Retention per period kind: (prefix, max buckets kept).
const RETENTION: [(&str, usize); 4] = [("d:", 60), ("w:", 26), ("m:", 24), ("y:", 10)];

/// Disk-backed statistics store (`<data_dir>/stats.json`). Mutations mark the
/// store dirty; a background task flushes periodically.
pub struct StatsStore {
  path: PathBuf,
  stats: PersistentStats,
  dirty: bool,
}

/// Current period keys derived from the local clock.
pub fn period_keys() -> [String; 4] {
  let now = chrono::Local::now();
  [
    format!("d:{}", now.format("%Y-%m-%d")),
    format!("w:{}", now.format("%G-W%V")),
    format!("m:{}", now.format("%Y-%m")),
    format!("y:{}", now.format("%Y")),
  ]
}

impl StatsStore {
  pub fn load(data_dir: &str) -> Self {
    let path = PathBuf::from(data_dir).join("stats.json");
    let stats = std::fs::read_to_string(&path)
      .ok()
      .and_then(|raw| serde_json::from_str::<PersistentStats>(&raw).ok())
      .unwrap_or_default();
    if stats.total_requests > 0 {
      info!(
        "Loaded persistent stats from {:?} (total_requests={})",
        path, stats.total_requests
      );
    }
    StatsStore {
      path,
      stats,
      dirty: false,
    }
  }

  /// Records a completed proxied request across all-time and period buckets.
  pub fn record_request(&mut self, success: bool, bytes_in: u64, bytes_out: u64, duration_ms: u64) {
    self.stats.total_requests += 1;
    if success {
      self.stats.total_success += 1;
    } else {
      self.stats.total_failed += 1;
    }
    self.stats.total_bytes_received += bytes_in;
    self.stats.total_bytes_sent += bytes_out;
    self.stats.total_request_duration_ms += duration_ms;

    for key in period_keys() {
      let p = self.stats.periods.entry(key).or_default();
      p.requests += 1;
      if success {
        p.success += 1;
      } else {
        p.failed += 1;
      }
      p.bytes_received += bytes_in;
      p.bytes_sent += bytes_out;
      p.duration_ms += duration_ms;
    }
    self.prune();
    self.dirty = true;
  }

  /// Adds streamed response bytes that were not known at request-record time.
  pub fn record_bytes_sent(&mut self, bytes: u64) {
    self.stats.total_bytes_sent += bytes;
    for key in period_keys() {
      self.stats.periods.entry(key).or_default().bytes_sent += bytes;
    }
    self.dirty = true;
  }

  /// Drops the oldest buckets beyond the retention window for each kind.
  fn prune(&mut self) {
    for (prefix, keep) in RETENTION {
      let mut keys: Vec<String> = self
        .stats
        .periods
        .keys()
        .filter(|k| k.starts_with(prefix))
        .cloned()
        .collect();
      if keys.len() > keep {
        // Period keys sort chronologically within a kind (zero-padded).
        keys.sort();
        for key in keys.iter().take(keys.len() - keep) {
          self.stats.periods.remove(key);
        }
      }
    }
  }

  /// Writes to disk when there are unsaved changes.
  pub fn save_if_dirty(&mut self) {
    if !self.dirty {
      return;
    }
    match serde_json::to_string(&self.stats) {
      Ok(json) => {
        if let Err(e) = std::fs::write(&self.path, json) {
          error!("Failed to persist stats to {:?}: {}", self.path, e);
        } else {
          self.dirty = false;
        }
      }
      Err(e) => error!("Failed to serialize persistent stats: {}", e),
    }
  }

  pub fn snapshot(&self) -> PersistentStats {
    self.stats.clone()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_record_and_reload() {
    let dir = std::env::temp_dir().join(format!("aperio-stats-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let mut store = StatsStore::load(&dir_str);
    store.record_request(true, 100, 2000, 40);
    store.record_request(false, 50, 0, 60);
    store.record_bytes_sent(500);
    store.save_if_dirty();

    let snap = store.snapshot();
    assert_eq!(snap.total_requests, 2);
    assert_eq!(snap.total_success, 1);
    assert_eq!(snap.total_failed, 1);
    assert_eq!(snap.total_bytes_received, 150);
    assert_eq!(snap.total_bytes_sent, 2500);
    assert_eq!(snap.total_request_duration_ms, 100);
    assert!((snap.avg_response_ms() - 50.0).abs() < f64::EPSILON);

    // Period buckets exist for the current day/week/month/year.
    let [d, w, m, y] = period_keys();
    for key in [d, w, m, y] {
      let p = snap.periods.get(&key).expect("period bucket");
      assert_eq!(p.requests, 2);
      assert_eq!(p.bytes_sent, 2500);
    }

    // Reload from disk → counters survive.
    let store2 = StatsStore::load(&dir_str);
    assert_eq!(store2.snapshot().total_requests, 2);

    let _ = std::fs::remove_dir_all(&dir);
  }
}
