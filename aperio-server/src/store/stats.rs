use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{error, info};

/// Counters for a single calendar period (day/week/month/year).
#[derive(Serialize, Deserialize, Default, Clone, utoipa::ToSchema)]
pub struct PeriodStats {
  pub requests: u64,
  pub success: u64,
  pub failed: u64,
  pub bytes_sent: u64,
  pub bytes_received: u64,
  pub duration_ms: u64,
}

/// Counters that survive server restarts, plus per-period breakdowns.
#[derive(Serialize, Deserialize, Default, Clone, utoipa::ToSchema)]
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
  /// Lifetime traffic per token label (`master` for the master token).
  #[serde(default)]
  pub by_token: HashMap<String, PeriodStats>,
  /// Lifetime traffic per request hostname.
  #[serde(default)]
  pub by_hostname: HashMap<String, PeriodStats>,
}

/// Maximum number of distinct token/hostname labels tracked; extra labels
/// are folded into `__other` so a flood of random hostnames cannot grow the
/// stats file without bound.
const LABEL_CAP: usize = 200;

/// Bumps a label bucket, folding overflow labels into `__other`.
fn bump_label(
  map: &mut HashMap<String, PeriodStats>,
  label: &str,
  success: bool,
  bytes_in: u64,
  bytes_out: u64,
  duration_ms: u64,
) {
  let key = if map.contains_key(label) || map.len() < LABEL_CAP {
    label
  } else {
    "__other"
  };
  let p = map.entry(key.to_string()).or_default();
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

/// Disk-backed statistics store (the `stats` table of the shared SQLite
/// store, `<data_dir>/aperio.db`). Mutations mark the store dirty; a
/// background task flushes periodically.
pub struct StatsStore {
  conn: rusqlite::Connection,
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

/// Chronological bucket keys for the last `count` periods of `unit`
/// (`"day"`, `"week"`, `"month"`, or `"year"`), oldest first, including the
/// current period. Returns `None` for an unknown unit.
pub fn recent_period_keys(unit: &str, count: usize) -> Option<Vec<String>> {
  let now = chrono::Local::now();
  let today = now.date_naive();
  let mut keys = Vec::with_capacity(count);
  match unit {
    "day" => {
      for i in (0..count).rev() {
        let d = today - chrono::Duration::days(i as i64);
        keys.push(format!("d:{}", d.format("%Y-%m-%d")));
      }
    }
    "week" => {
      for i in (0..count).rev() {
        let d = today - chrono::Duration::weeks(i as i64);
        keys.push(format!("w:{}", d.format("%G-W%V")));
      }
    }
    "month" => {
      let (mut year, mut month) = (
        chrono::Datelike::year(&today),
        chrono::Datelike::month(&today) as i32,
      );
      let mut rev = Vec::with_capacity(count);
      for _ in 0..count {
        rev.push(format!("m:{:04}-{:02}", year, month));
        month -= 1;
        if month == 0 {
          month = 12;
          year -= 1;
        }
      }
      rev.reverse();
      keys = rev;
    }
    "year" => {
      let year = chrono::Datelike::year(&today);
      for i in (0..count as i32).rev() {
        keys.push(format!("y:{}", year - i));
      }
    }
    _ => return None,
  }
  Some(keys)
}

/// Chronological day-bucket keys covering `from..=to` (`YYYY-MM-DD`).
/// Returns `None` on unparsable dates or a reversed range; the span is
/// capped to the day-bucket retention window (last buckets win).
pub fn day_keys_between(from: &str, to: &str) -> Option<Vec<String>> {
  let from = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").ok()?;
  let to = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").ok()?;
  if from > to {
    return None;
  }
  let mut keys: Vec<String> = from
    .iter_days()
    .take_while(|d| *d <= to)
    .take(366)
    .map(|d| format!("d:{}", d.format("%Y-%m-%d")))
    .collect();
  let cap = RETENTION[0].1;
  if keys.len() > cap {
    keys = keys.split_off(keys.len() - cap);
  }
  Some(keys)
}

impl StatsStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let stats = conn
      .query_row("SELECT data FROM stats WHERE key = 'stats'", [], |row| {
        row.get::<_, String>(0)
      })
      .ok()
      .and_then(|raw| serde_json::from_str::<PersistentStats>(&raw).ok())
      .unwrap_or_default();
    if stats.total_requests > 0 {
      info!(
        "Loaded persistent stats from the store (total_requests={})",
        stats.total_requests
      );
    }
    StatsStore {
      conn,
      stats,
      dirty: false,
    }
  }

  /// Records a completed proxied request across all-time and period buckets.
  pub fn record_request(&mut self, success: bool, bytes_in: u64, bytes_out: u64, duration_ms: u64) {
    self.record_request_labeled(success, bytes_in, bytes_out, duration_ms, None, None)
  }

  /// Like [`record_request`], additionally attributing the traffic to a
  /// token label and/or request hostname for per-tenant traceability.
  pub fn record_request_labeled(
    &mut self,
    success: bool,
    bytes_in: u64,
    bytes_out: u64,
    duration_ms: u64,
    token: Option<&str>,
    hostname: Option<&str>,
  ) {
    if let Some(token) = token {
      bump_label(
        &mut self.stats.by_token,
        token,
        success,
        bytes_in,
        bytes_out,
        duration_ms,
      );
    }
    if let Some(hostname) = hostname {
      bump_label(
        &mut self.stats.by_hostname,
        hostname,
        success,
        bytes_in,
        bytes_out,
        duration_ms,
      );
    }
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

  /// Lifetime proxied-request count (cheap accessor for the first-run check).
  pub fn lifetime_requests(&self) -> u64 {
    self.stats.total_requests
  }

  /// Writes to the store when there are unsaved changes.
  pub fn save_if_dirty(&mut self) {
    if !self.dirty {
      return;
    }
    match serde_json::to_string(&self.stats) {
      Ok(json) => {
        let res = self.conn.execute(
          "INSERT INTO stats (key, data) VALUES ('stats', ?1)
           ON CONFLICT(key) DO UPDATE SET data = excluded.data",
          rusqlite::params![json],
        );
        match res {
          Ok(_) => self.dirty = false,
          Err(e) => error!("Failed to persist stats to the store: {}", e),
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
  fn test_recent_period_keys() {
    let days = recent_period_keys("day", 7).unwrap();
    assert_eq!(days.len(), 7);
    assert!(days.iter().all(|k| k.starts_with("d:")));
    // Chronological, current period last.
    let [d, _, m, y] = period_keys();
    assert_eq!(days.last().unwrap(), &d);
    let mut sorted = days.clone();
    sorted.sort();
    assert_eq!(sorted, days);

    let months = recent_period_keys("month", 24).unwrap();
    assert_eq!(months.len(), 24);
    assert_eq!(months.last().unwrap(), &m);
    let mut sorted = months.clone();
    sorted.sort();
    assert_eq!(sorted, months);

    let years = recent_period_keys("year", 3).unwrap();
    assert_eq!(years.last().unwrap(), &y);

    let weeks = recent_period_keys("week", 26).unwrap();
    assert_eq!(weeks.len(), 26);
    assert!(weeks.iter().all(|k| k.starts_with("w:")));

    assert!(recent_period_keys("fortnight", 5).is_none());
  }

  #[test]
  fn test_day_keys_between() {
    let keys = day_keys_between("2026-07-01", "2026-07-05").unwrap();
    assert_eq!(
      keys,
      vec![
        "d:2026-07-01",
        "d:2026-07-02",
        "d:2026-07-03",
        "d:2026-07-04",
        "d:2026-07-05"
      ]
    );
    // Single day.
    assert_eq!(
      day_keys_between("2026-07-01", "2026-07-01").unwrap().len(),
      1
    );
    // Capped to the day retention window, keeping the newest buckets.
    let long = day_keys_between("2025-01-01", "2026-01-01").unwrap();
    assert_eq!(long.len(), RETENTION[0].1);
    assert_eq!(long.last().unwrap(), "d:2026-01-01");
    // Invalid input.
    assert!(day_keys_between("2026-07-05", "2026-07-01").is_none());
    assert!(day_keys_between("notadate", "2026-07-01").is_none());
  }

  #[test]
  fn test_record_and_reload() {
    let dir = std::env::temp_dir().join(format!("aperio-stats-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let mut store = StatsStore::load(&dir_str);
    store.record_request_labeled(true, 100, 2000, 40, Some("master"), Some("a.example.com"));
    store.record_request_labeled(false, 50, 0, 60, Some("tenant-a"), Some("a.example.com"));
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

    // Label breakdowns are attributed per token and hostname.
    assert_eq!(snap.by_token.get("master").unwrap().requests, 1);
    assert_eq!(snap.by_token.get("tenant-a").unwrap().failed, 1);
    let host = snap.by_hostname.get("a.example.com").unwrap();
    assert_eq!(host.requests, 2);
    assert_eq!(host.bytes_sent, 2000);

    // Reload from disk → counters survive.
    let store2 = StatsStore::load(&dir_str);
    assert_eq!(store2.snapshot().total_requests, 2);
    assert_eq!(
      store2
        .snapshot()
        .by_hostname
        .get("a.example.com")
        .unwrap()
        .requests,
      2
    );

    let _ = std::fs::remove_dir_all(&dir);
  }
}
