//! Per-service availability history: a background task periodically observes
//! every known service entity (up / degraded / down) and accrues the elapsed
//! time into daily counters, so the dashboard can report uptime percentages
//! and a downtime timeline. Time while the server itself is not running is
//! not attributed to anyone (the counters simply don't advance).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::error;

/// Availability state of a service entity at observation time.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
  /// At least one tunnel connection is healthy and its backend probe passes.
  Up,
  /// Connected, but every connection reports an unhealthy backend (or is
  /// draining/disabled), reachable tunnel, unserved traffic.
  Degraded,
  /// No live tunnel connection.
  Down,
}

/// Seconds spent in each availability state during one calendar day.
#[derive(Serialize, Deserialize, Default, Clone, utoipa::ToSchema)]
pub struct DayAvailability {
  pub up_secs: u64,
  pub degraded_secs: u64,
  pub down_secs: u64,
}

impl DayAvailability {
  fn add(&mut self, status: Availability, secs: u64) {
    match status {
      Availability::Up => self.up_secs += secs,
      Availability::Degraded => self.degraded_secs += secs,
      Availability::Down => self.down_secs += secs,
    }
  }

  pub fn observed_secs(&self) -> u64 {
    self.up_secs + self.degraded_secs + self.down_secs
  }
}

/// Availability history of one service entity (keyed by service name or
/// stable client instance id).
#[derive(Serialize, Deserialize, Clone)]
pub struct EntityUptime {
  /// Current status as of the last tick.
  pub status: Availability,
  /// Unix seconds when the entity was last observed connected (up/degraded).
  pub last_seen: u64,
  /// Organization serving this entity (`None` = master), from the client's
  /// token. The dashboard filters the uptime view to the caller's org.
  #[serde(default)]
  pub org_id: Option<String>,
  /// Daily accrual buckets keyed `YYYY-MM-DD`.
  pub days: HashMap<String, DayAvailability>,
}

/// Days of per-day availability kept per entity.
const DAY_RETENTION: usize = 60;
/// Entities dropped after this long without a live connection.
const ENTITY_RETENTION_SECS: u64 = 30 * 86_400;
/// Maximum number of tracked entities (oldest last_seen evicted first).
const ENTITY_CAP: usize = 200;

/// Disk-backed availability store (the `stats` table row `uptime` of the
/// shared SQLite store). Mutated only by the tick task; flushed with the
/// periodic stats flush.
pub struct UptimeStore {
  conn: rusqlite::Connection,
  entities: HashMap<String, EntityUptime>,
  /// Unix seconds of the previous tick; elapsed time is attributed to the
  /// status each entity had *before* the tick.
  last_tick: Option<u64>,
  dirty: bool,
}

impl UptimeStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let entities = conn
      .query_row("SELECT data FROM stats WHERE key = 'uptime'", [], |row| {
        row.get::<_, String>(0)
      })
      .ok()
      .and_then(|raw| serde_json::from_str::<HashMap<String, EntityUptime>>(&raw).ok())
      .unwrap_or_default();
    UptimeStore {
      conn,
      entities,
      last_tick: None,
      dirty: false,
    }
  }

  /// Observes the current live entities. Elapsed time since the previous
  /// tick is accrued to each known entity under its *previous* status; then
  /// statuses are updated (entities absent from `live` count as down).
  pub fn tick(&mut self, now_secs: u64, live: HashMap<String, (Availability, Option<String>)>) {
    if let Some(prev) = self.last_tick {
      let elapsed = now_secs.saturating_sub(prev);
      if elapsed > 0 {
        for entity in self.entities.values_mut() {
          accrue_days(&mut entity.days, prev, now_secs, entity.status);
        }
        self.dirty = true;
      }
    }
    // Update statuses: live entities as reported, known-but-absent as down.
    for (key, (status, org_id)) in &live {
      let entity = self
        .entities
        .entry(key.clone())
        .or_insert_with(|| EntityUptime {
          status: *status,
          last_seen: now_secs,
          org_id: org_id.clone(),
          days: HashMap::new(),
        });
      entity.status = *status;
      entity.org_id = org_id.clone();
      if *status != Availability::Down {
        entity.last_seen = now_secs;
      }
      self.dirty = true;
    }
    for (key, entity) in self.entities.iter_mut() {
      if !live.contains_key(key) {
        entity.status = Availability::Down;
      }
    }
    self.prune(now_secs);
    self.last_tick = Some(now_secs);
  }

  /// Drops day buckets beyond retention, entities unseen for too long, and
  /// caps the number of tracked entities (oldest last_seen first).
  fn prune(&mut self, now_secs: u64) {
    for entity in self.entities.values_mut() {
      if entity.days.len() > DAY_RETENTION {
        let mut keys: Vec<String> = entity.days.keys().cloned().collect();
        keys.sort();
        for key in keys.iter().take(keys.len() - DAY_RETENTION) {
          entity.days.remove(key);
        }
      }
    }
    self
      .entities
      .retain(|_, e| now_secs.saturating_sub(e.last_seen) < ENTITY_RETENTION_SECS);
    if self.entities.len() > ENTITY_CAP {
      let mut by_age: Vec<(String, u64)> = self
        .entities
        .iter()
        .map(|(k, e)| (k.clone(), e.last_seen))
        .collect();
      by_age.sort_by_key(|(_, seen)| *seen);
      for (key, _) in by_age.iter().take(self.entities.len() - ENTITY_CAP) {
        self.entities.remove(key);
      }
    }
  }

  /// Replaces the recorded history with an imported one, and writes it out.
  /// Bookkeeping: the dump-restore path, whose caller reports on the whole
  /// import rather than on one row. See `store::replace_all`.
  pub fn import(&mut self, entities: HashMap<String, EntityUptime>) -> usize {
    self.entities = entities;
    // `last_tick` stays where it is: it is a clock reading of *this* process,
    // not part of what was imported, and clearing it would attribute the gap
    // since the last tick to the imported entities.
    self.dirty = true;
    self.save_if_dirty();
    self.entities.len()
  }

  pub fn snapshot(&self) -> HashMap<String, EntityUptime> {
    self.entities.clone()
  }

  /// Writes to the store when there are unsaved changes.
  pub fn save_if_dirty(&mut self) {
    if !self.dirty {
      return;
    }
    match serde_json::to_string(&self.entities) {
      Ok(json) => {
        let res = self.conn.execute(
          "INSERT INTO stats (key, data) VALUES ('uptime', ?1)
           ON CONFLICT(key) DO UPDATE SET data = excluded.data",
          rusqlite::params![json],
        );
        match res {
          Ok(_) => self.dirty = false,
          Err(e) => error!("Failed to persist uptime history to the store: {}", e),
        }
      }
      Err(e) => error!("Failed to serialize uptime history: {}", e),
    }
  }
}

/// Accrues the span `from..to` (unix seconds) into per-day buckets under
/// `status`, splitting across local-midnight boundaries.
fn accrue_days(
  days: &mut HashMap<String, DayAvailability>,
  from: u64,
  to: u64,
  status: Availability,
) {
  use chrono::TimeZone;
  let mut cursor = from;
  // Bounded: a tick gap never spans more than the retention window in
  // practice, but guard against a wildly wrong clock anyway.
  let mut guard = 0;
  while cursor < to && guard < 400 {
    guard += 1;
    let local = chrono::Local
      .timestamp_opt(cursor as i64, 0)
      .single()
      .unwrap_or_else(chrono::Local::now);
    let day_key = local.format("%Y-%m-%d").to_string();
    // Seconds until local midnight after `cursor`.
    let next_midnight = (local + chrono::Duration::days(1))
      .date_naive()
      .and_hms_opt(0, 0, 0)
      .and_then(|naive| naive.and_local_timezone(chrono::Local).single())
      .map(|dt| dt.timestamp() as u64)
      .unwrap_or(to);
    let span_end = to.min(next_midnight.max(cursor + 1));
    days
      .entry(day_key)
      .or_default()
      .add(status, span_end - cursor);
    cursor = span_end;
  }
}

#[cfg(test)]
#[path = "uptime_tests.rs"]
mod tests;
