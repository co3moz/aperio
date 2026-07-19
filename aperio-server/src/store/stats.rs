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

/// Records one request into a [`PersistentStats`] (totals, period buckets, and
/// optional token/hostname labels). Shared by the global aggregate and each
/// organization's own slice.
fn bump_stats(
  stats: &mut PersistentStats,
  success: bool,
  bytes_in: u64,
  bytes_out: u64,
  duration_ms: u64,
  token: Option<&str>,
  hostname: Option<&str>,
) {
  if let Some(token) = token {
    bump_label(
      &mut stats.by_token,
      token,
      success,
      bytes_in,
      bytes_out,
      duration_ms,
    );
  }
  if let Some(hostname) = hostname {
    bump_label(
      &mut stats.by_hostname,
      hostname,
      success,
      bytes_in,
      bytes_out,
      duration_ms,
    );
  }
  stats.total_requests += 1;
  if success {
    stats.total_success += 1;
  } else {
    stats.total_failed += 1;
  }
  stats.total_bytes_received += bytes_in;
  stats.total_bytes_sent += bytes_out;
  stats.total_request_duration_ms += duration_ms;
  for key in period_keys() {
    let p = stats.periods.entry(key).or_default();
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
}

/// Adds streamed response bytes to a [`PersistentStats`]'s totals and buckets.
fn add_bytes_sent(stats: &mut PersistentStats, bytes: u64) {
  stats.total_bytes_sent += bytes;
  for key in period_keys() {
    stats.periods.entry(key).or_default().bytes_sent += bytes;
  }
}

/// Drops the oldest period buckets beyond the retention window for each kind.
fn prune_periods(stats: &mut PersistentStats) {
  for (prefix, keep) in RETENTION {
    let mut keys: Vec<String> = stats
      .periods
      .keys()
      .filter(|k| k.starts_with(prefix))
      .cloned()
      .collect();
    if keys.len() > keep {
      // Period keys sort chronologically within a kind (zero-padded).
      keys.sort();
      for key in keys.iter().take(keys.len() - keep) {
        stats.periods.remove(key);
      }
    }
  }
}

/// Retention per period kind: (prefix, max buckets kept).
const RETENTION: [(&str, usize); 4] = [("d:", 60), ("w:", 26), ("m:", 24), ("y:", 10)];

/// Disk-backed statistics store (the `stats` table of the shared SQLite
/// store, `<data_dir>/aperio.db`). Mutations mark the store dirty; a
/// background task flushes periodically.
/// On-disk shape: the global aggregate (flattened, so older files that were a
/// bare `PersistentStats` still load) plus a per-organization breakdown.
#[derive(Serialize, Deserialize, Default)]
struct PersistedStats {
  #[serde(flatten)]
  global: PersistentStats,
  /// Per-organization stats, keyed by org id (`master` for the implicit master
  /// org). Each entry is a full [`PersistentStats`] scoped to that org.
  #[serde(default)]
  by_org: HashMap<String, PersistentStats>,
}

/// The org-id key used for the implicit master organization (org `None`).
pub const MASTER_ORG_KEY: &str = "master";

pub struct StatsStore {
  conn: rusqlite::Connection,
  stats: PersistentStats,
  /// Per-organization aggregates, keyed by org id (`master` for org `None`).
  by_org: HashMap<String, PersistentStats>,
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
    let persisted = conn
      .query_row("SELECT data FROM stats WHERE key = 'stats'", [], |row| {
        row.get::<_, String>(0)
      })
      .ok()
      .and_then(|raw| serde_json::from_str::<PersistedStats>(&raw).ok())
      .unwrap_or_default();
    if persisted.global.total_requests > 0 {
      info!(
        "Loaded persistent stats from the store (total_requests={})",
        persisted.global.total_requests
      );
    }
    StatsStore {
      conn,
      stats: persisted.global,
      by_org: persisted.by_org,
      dirty: false,
    }
  }

  /// Records a completed proxied request across all-time and period buckets,
  /// attributed to the serving organization (`None` = master).
  pub fn record_request(
    &mut self,
    success: bool,
    bytes_in: u64,
    bytes_out: u64,
    duration_ms: u64,
    org: Option<&str>,
  ) {
    self.record_request_labeled(success, bytes_in, bytes_out, duration_ms, None, None, org)
  }

  /// Like [`record_request`], additionally attributing the traffic to a
  /// token label and/or request hostname for per-tenant traceability. The
  /// request is counted both in the global aggregate and in the serving
  /// organization's own slice (`org` = the org id, `None` = master).
  #[allow(clippy::too_many_arguments)]
  pub fn record_request_labeled(
    &mut self,
    success: bool,
    bytes_in: u64,
    bytes_out: u64,
    duration_ms: u64,
    token: Option<&str>,
    hostname: Option<&str>,
    org: Option<&str>,
  ) {
    bump_stats(
      &mut self.stats,
      success,
      bytes_in,
      bytes_out,
      duration_ms,
      token,
      hostname,
    );
    let org_stats = self
      .by_org
      .entry(org.unwrap_or(MASTER_ORG_KEY).to_string())
      .or_default();
    bump_stats(
      org_stats,
      success,
      bytes_in,
      bytes_out,
      duration_ms,
      token,
      hostname,
    );
    prune_periods(&mut self.stats);
    if let Some(s) = self.by_org.get_mut(org.unwrap_or(MASTER_ORG_KEY)) {
      prune_periods(s);
    }
    self.dirty = true;
  }

  /// Adds streamed response bytes that were not known at request-record time,
  /// attributed to the serving organization (`None` = master).
  pub fn record_bytes_sent(&mut self, bytes: u64, org: Option<&str>) {
    add_bytes_sent(&mut self.stats, bytes);
    add_bytes_sent(
      self
        .by_org
        .entry(org.unwrap_or(MASTER_ORG_KEY).to_string())
        .or_default(),
      bytes,
    );
    self.dirty = true;
  }

  /// Lifetime proxied-request count (cheap accessor for the first-run check).
  pub fn lifetime_requests(&self) -> u64 {
    self.stats.total_requests
  }

  /// Writes to the store when there are unsaved changes.
  /// Right-to-erasure: drops the per-hostname aggregate rows for one
  /// hostname (global and every org breakdown). Returns removed row count.
  pub fn purge_hostname(&mut self, hostname: &str) -> usize {
    let mut removed = 0;
    if self.stats.by_hostname.remove(hostname).is_some() {
      removed += 1;
    }
    for org in self.by_org.values_mut() {
      if org.by_hostname.remove(hostname).is_some() {
        removed += 1;
      }
    }
    if removed > 0 {
      self.dirty = true;
      self.save_if_dirty();
    }
    removed
  }

  /// Right-to-erasure: drops the per-token aggregate rows for one token
  /// label. Returns removed row count.
  pub fn purge_token(&mut self, token: &str) -> usize {
    let mut removed = 0;
    if self.stats.by_token.remove(token).is_some() {
      removed += 1;
    }
    for org in self.by_org.values_mut() {
      if org.by_token.remove(token).is_some() {
        removed += 1;
      }
    }
    if removed > 0 {
      self.dirty = true;
      self.save_if_dirty();
    }
    removed
  }

  pub fn save_if_dirty(&mut self) {
    if !self.dirty {
      return;
    }
    let persisted = PersistedStats {
      global: self.stats.clone(),
      by_org: self.by_org.clone(),
    };
    match serde_json::to_string(&persisted) {
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

  /// The global aggregate across all organizations (used by Prometheus and
  /// server-level operators).
  pub fn snapshot(&self) -> PersistentStats {
    self.stats.clone()
  }

  /// The stats slice for one organization (`None` = master). Empty when the org
  /// has served no traffic yet.
  pub fn snapshot_for_org(&self, org: Option<&str>) -> PersistentStats {
    self
      .by_org
      .get(org.unwrap_or(MASTER_ORG_KEY))
      .cloned()
      .unwrap_or_default()
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
    // First request served by the master org, second by child org "acme".
    store.record_request_labeled(
      true,
      100,
      2000,
      40,
      Some("master"),
      Some("a.example.com"),
      None,
    );
    store.record_request_labeled(
      false,
      50,
      0,
      60,
      Some("tenant-a"),
      Some("a.example.com"),
      Some("acme"),
    );
    store.record_bytes_sent(500, None);
    store.save_if_dirty();

    let snap = store.snapshot();
    assert_eq!(snap.total_requests, 2);
    assert_eq!(snap.total_success, 1);
    assert_eq!(snap.total_failed, 1);
    assert_eq!(snap.total_bytes_received, 150);
    assert_eq!(snap.total_bytes_sent, 2500);
    assert_eq!(snap.total_request_duration_ms, 100);
    assert!((snap.avg_response_ms() - 50.0).abs() < f64::EPSILON);

    // Per-org slices: the master org saw only its own request (+ the streamed
    // bytes), the "acme" org only its own; neither sees the other's traffic.
    let master = store.snapshot_for_org(None);
    assert_eq!(master.total_requests, 1);
    assert_eq!(master.total_success, 1);
    assert_eq!(master.total_bytes_sent, 2500);
    let acme = store.snapshot_for_org(Some("acme"));
    assert_eq!(acme.total_requests, 1);
    assert_eq!(acme.total_failed, 1);
    assert_eq!(acme.total_bytes_sent, 0);
    assert!(store.snapshot_for_org(Some("unknown")).total_requests == 0);

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

  #[test]
  fn test_purge_hostname_and_token() {
    let dir = std::env::temp_dir().join(format!("aperio-stats-purge-{}", uuid::Uuid::new_v4()));
    let dir_str = dir.to_string_lossy().to_string();
    let mut store = StatsStore::load(&dir_str);
    store.record_request_labeled(
      true,
      100,
      1000,
      50,
      Some("tenant-a"),
      Some("a.example.com"),
      Some("org-1"),
    );
    store.record_request_labeled(
      true,
      100,
      1000,
      50,
      Some("master"),
      Some("b.example.com"),
      None,
    );
    store.save_if_dirty();

    // Hostname purge removes the global row and the org breakdown row.
    assert!(store.purge_hostname("a.example.com") >= 1);
    assert!(!store.snapshot().by_hostname.contains_key("a.example.com"));
    // Other hostnames and totals are untouched.
    assert!(store.snapshot().by_hostname.contains_key("b.example.com"));
    assert_eq!(store.snapshot().total_requests, 2);

    // Token purge removes the label rows.
    assert!(store.purge_token("tenant-a") >= 1);
    assert!(!store.snapshot().by_token.contains_key("tenant-a"));

    // Purges persist across a reload.
    let store2 = StatsStore::load(&dir_str);
    assert!(!store2.snapshot().by_hostname.contains_key("a.example.com"));
    assert!(!store2.snapshot().by_token.contains_key("tenant-a"));

    // Unknown selectors remove nothing.
    assert_eq!(store.purge_hostname("nope.example.com"), 0);
    assert_eq!(store.purge_token("nope"), 0);

    let _ = std::fs::remove_dir_all(&dir);
  }
}
