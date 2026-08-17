use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::VecDeque;
use tracing::error;

/// Bucket width of the activity ring, in seconds.
///
/// Five rather than one: the dashboard's live chart derives per-second rates
/// from its own polls and covers a minute, which is the right tool for "is it
/// moving right now". This series answers the other question, "what did the
/// last quarter of an hour look like", and a second's resolution there is
/// noise the eye cannot use, at five times the memory.
pub(crate) const ACTIVITY_BUCKET_SECS: u64 = 5;

/// How many buckets are kept: 180 × 5 s = 15 minutes.
pub(crate) const ACTIVITY_BUCKETS: usize = 180;

/// The two coarse rings, and why the width grows with the span: a series is
/// readable at roughly sixty cells whatever it covers, and it costs what it
/// stores. A day at five-second resolution would be seventeen thousand points
/// drawn into a few hundred pixels, which is a wall, not a chart, and seventeen
/// thousand points per organization to hold it.
///
/// Two hours in two-minute slices: 60 buckets.
pub(crate) const ACTIVITY_COARSE_SECS: u64 = 120;
pub(crate) const ACTIVITY_COARSE_BUCKETS: usize = 60;
/// A day in quarter-hour slices: 96 buckets.
pub(crate) const ACTIVITY_DAILY_SECS: u64 = 900;
pub(crate) const ACTIVITY_DAILY_BUCKETS: usize = 96;

/// Which resolution of the activity series a caller wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityRange {
  /// 15 minutes in 5-second slices, in memory only.
  Quarter,
  /// 2 hours in 2-minute slices.
  TwoHours,
  /// 24 hours in 15-minute slices.
  Day,
}

impl ActivityRange {
  /// Parses the `range` query value. Anything unrecognized, including an
  /// absent one, is the quarter hour: that is what this endpoint returned
  /// before the parameter existed, so an old caller keeps its answer.
  pub(crate) fn parse(raw: Option<&str>) -> ActivityRange {
    match raw {
      Some("2h") => ActivityRange::TwoHours,
      Some("1d") => ActivityRange::Day,
      _ => ActivityRange::Quarter,
    }
  }

  pub(crate) fn width_secs(self) -> u64 {
    match self {
      ActivityRange::Quarter => ACTIVITY_BUCKET_SECS,
      ActivityRange::TwoHours => ACTIVITY_COARSE_SECS,
      ActivityRange::Day => ACTIVITY_DAILY_SECS,
    }
  }

  pub(crate) fn buckets(self) -> usize {
    match self {
      ActivityRange::Quarter => ACTIVITY_BUCKETS,
      ActivityRange::TwoHours => ACTIVITY_COARSE_BUCKETS,
      ActivityRange::Day => ACTIVITY_DAILY_BUCKETS,
    }
  }
}

/// Organizations tracked before new ones stop being admitted, the same guard
/// the route trends carry: this is in-memory per-request state, and a bound is
/// what keeps a tenant from growing it.
const ACTIVITY_ORG_CAP: usize = 100;

/// One five-second slice of served traffic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivityBucket {
  /// Unix seconds of the bucket's start, so a gap reads as a gap rather than
  /// as a shift.
  pub(crate) at: u64,
  /// Requests served in this slice, and how many of them failed.
  pub(crate) total: u32,
  pub(crate) failed: u32,
}

/// Recent request volume in fixed slices, per organization.
///
/// The dashboard's minute-long chart is built in the browser from successive
/// polls, so it starts empty on every reload and cannot look back further than
/// the tab has been open. This is the same shape kept by the server: it
/// survives a reload, it is the same for two people looking at once, and it
/// costs one increment per request on a lock the request already takes.
/// One resolution of the series: a fixed bucket width, a bounded ring per
/// organization, and nothing else. Three of these make up `Activity`.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ActivityRing {
  width_secs: u64,
  capacity: usize,
  /// Keyed by org id; `None` (master) is the empty string, so one map holds
  /// both without an Option key.
  by_org: HashMap<String, VecDeque<ActivityBucket>>,
}

impl ActivityRing {
  fn new(width_secs: u64, capacity: usize) -> ActivityRing {
    ActivityRing {
      width_secs,
      capacity,
      by_org: HashMap::new(),
    }
  }

  fn record(&mut self, key: &str, failed: bool, now: u64) {
    if !self.by_org.contains_key(key) && self.by_org.len() >= ACTIVITY_ORG_CAP {
      return;
    }
    let width = self.width_secs;
    let capacity = self.capacity;
    let buckets = self.by_org.entry(key.to_string()).or_default();
    let at = now - now % width;
    if buckets.back().map(|b| b.at) != Some(at) {
      if buckets.len() >= capacity {
        buckets.pop_front();
      }
      buckets.push_back(ActivityBucket {
        at,
        ..Default::default()
      });
    }
    let bucket = buckets.back_mut().expect("bucket just ensured");
    bucket.total += 1;
    if failed {
      bucket.failed += 1;
    }
  }

  fn series(&self, key: &str, count: usize, now: u64) -> Vec<ActivityBucket> {
    let latest = now - now % self.width_secs;
    let empty = VecDeque::new();
    let buckets = self.by_org.get(key).unwrap_or(&empty);
    (0..count)
      .rev()
      .map(|back| {
        let at = latest.saturating_sub(back as u64 * self.width_secs);
        buckets
          .iter()
          .find(|b| b.at == at)
          .copied()
          .unwrap_or(ActivityBucket {
            at,
            ..Default::default()
          })
      })
      .collect()
  }

  /// Drops what has aged out of the ring since it was written. Restoring a
  /// day-old file without this would draw yesterday's traffic as today's.
  fn forget_before(&mut self, cutoff: u64) {
    for buckets in self.by_org.values_mut() {
      buckets.retain(|b| b.at >= cutoff);
    }
    self.by_org.retain(|_, buckets| !buckets.is_empty());
  }

  /// Repairs a ring read back from disk: the file carries the widths it was
  /// written with, and a build that changed them must not fold the old
  /// buckets into the new geometry.
  fn matches(&self, width_secs: u64, capacity: usize) -> bool {
    self.width_secs == width_secs && self.capacity == capacity
  }
}

/// What is written to the store: the two coarse rings only.
///
/// The fine one is deliberately absent. Fifteen minutes of five-second
/// buckets is the view of "right now", and a restart is exactly the moment
/// when "right now" changed; restoring it would redraw the minutes before a
/// deploy as if the new process had served them.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct PersistedActivity {
  coarse: ActivityRing,
  daily: ActivityRing,
}

impl PersistedActivity {
  /// Drops every organization but master, for a dump that does not carry the
  /// organizations themselves: a ring keyed by an org the target does not
  /// have is an orphan, exactly as the other sections treat their rows.
  pub(crate) fn retain_master_only(&mut self) {
    for ring in [&mut self.coarse, &mut self.daily] {
      ring.by_org.retain(|org, _| org.is_empty());
    }
  }

  /// Organizations represented, for the dump's per-section count.
  pub(crate) fn len(&self) -> usize {
    self
      .coarse
      .by_org
      .keys()
      .chain(self.daily.by_org.keys())
      .collect::<std::collections::HashSet<_>>()
      .len()
  }
}

/// Recent request volume in fixed slices, per organization, at three
/// resolutions.
///
/// The dashboard's minute-long chart is built in the browser from successive
/// polls, so it starts empty on every reload and cannot look back further than
/// the tab has been open. This is the same shape kept by the server: it
/// survives a reload, it is the same for two people looking at once, and it
/// costs one increment per request on a lock the request already takes.
///
/// The two long ranges also survive a restart, which is not a nicety: a view
/// covering a day that empties on every deploy answers "what happened
/// overnight" with a shrug, and would be worse than not offering the range at
/// all. They are small enough to write whole (about 40 KB at the organization
/// cap) on the same flush the persistent stats already use.
pub(crate) struct Activity {
  fine: ActivityRing,
  coarse: ActivityRing,
  daily: ActivityRing,
  /// Absent in tests and wherever no data directory exists; then the long
  /// ranges simply behave like the fine one and start empty.
  conn: Option<rusqlite::Connection>,
  dirty: bool,
}

impl Default for Activity {
  fn default() -> Activity {
    Activity {
      fine: ActivityRing::new(ACTIVITY_BUCKET_SECS, ACTIVITY_BUCKETS),
      coarse: ActivityRing::new(ACTIVITY_COARSE_SECS, ACTIVITY_COARSE_BUCKETS),
      daily: ActivityRing::new(ACTIVITY_DAILY_SECS, ACTIVITY_DAILY_BUCKETS),
      conn: None,
      dirty: false,
    }
  }
}

impl Activity {
  fn key(org: Option<&str>) -> String {
    org.unwrap_or("").to_string()
  }

  /// Reads the coarse rings back from the store and keeps the connection for
  /// later flushes.
  pub(crate) fn load(data_dir: &str, now: u64) -> Activity {
    let conn = crate::store::open_db(data_dir);
    let persisted = conn
      .query_row("SELECT data FROM stats WHERE key = 'activity'", [], |row| {
        row.get::<_, String>(0)
      })
      .ok()
      .and_then(|raw| serde_json::from_str::<PersistedActivity>(&raw).ok())
      .unwrap_or_default();
    let mut activity = Activity {
      conn: Some(conn),
      ..Default::default()
    };
    if persisted
      .coarse
      .matches(ACTIVITY_COARSE_SECS, ACTIVITY_COARSE_BUCKETS)
    {
      activity.coarse = persisted.coarse;
      activity
        .coarse
        .forget_before(now.saturating_sub(ACTIVITY_COARSE_SECS * ACTIVITY_COARSE_BUCKETS as u64));
    }
    if persisted
      .daily
      .matches(ACTIVITY_DAILY_SECS, ACTIVITY_DAILY_BUCKETS)
    {
      activity.daily = persisted.daily;
      activity
        .daily
        .forget_before(now.saturating_sub(ACTIVITY_DAILY_SECS * ACTIVITY_DAILY_BUCKETS as u64));
    }
    activity
  }

  /// Records one served request into the current slice of every resolution.
  pub(crate) fn record(&mut self, org: Option<&str>, failed: bool, now: u64) {
    let key = Self::key(org);
    self.fine.record(&key, failed, now);
    self.coarse.record(&key, failed, now);
    self.daily.record(&key, failed, now);
    self.dirty = true;
  }

  /// The last `count` slices of one resolution for one organization, oldest
  /// first, with the silent ones filled in. A quiet minute is a real answer,
  /// and a series that simply omits it draws the traffic on either side as if
  /// it were adjacent.
  pub(crate) fn series(
    &self,
    org: Option<&str>,
    range: ActivityRange,
    now: u64,
  ) -> Vec<ActivityBucket> {
    let key = Self::key(org);
    let ring = match range {
      ActivityRange::Quarter => &self.fine,
      ActivityRange::TwoHours => &self.coarse,
      ActivityRange::Day => &self.daily,
    };
    ring.series(&key, range.buckets(), now)
  }

  /// The coarse rings, for a dump. The fine one is not included for the same
  /// reason it is not persisted: fifteen minutes of five-second slices is the
  /// view of *right now*, and "now" is not a thing a dump can carry.
  pub(crate) fn export(&self) -> PersistedActivity {
    PersistedActivity {
      coarse: ActivityRing {
        width_secs: self.coarse.width_secs,
        capacity: self.coarse.capacity,
        by_org: self.coarse.by_org.clone(),
      },
      daily: ActivityRing {
        width_secs: self.daily.width_secs,
        capacity: self.daily.capacity,
        by_org: self.daily.by_org.clone(),
      },
    }
  }

  /// Replaces the coarse rings from a dump, dropping what has aged out and
  /// refusing a ring whose geometry this build no longer uses, exactly as
  /// reading them back from the store does. Returns the organizations taken.
  pub(crate) fn import(&mut self, dump: PersistedActivity, now: u64) -> usize {
    let mut taken = 0;
    if dump
      .coarse
      .matches(ACTIVITY_COARSE_SECS, ACTIVITY_COARSE_BUCKETS)
    {
      self.coarse = dump.coarse;
      self
        .coarse
        .forget_before(now.saturating_sub(ACTIVITY_COARSE_SECS * ACTIVITY_COARSE_BUCKETS as u64));
      taken += self.coarse.by_org.len();
    }
    if dump
      .daily
      .matches(ACTIVITY_DAILY_SECS, ACTIVITY_DAILY_BUCKETS)
    {
      self.daily = dump.daily;
      self
        .daily
        .forget_before(now.saturating_sub(ACTIVITY_DAILY_SECS * ACTIVITY_DAILY_BUCKETS as u64));
      taken = taken.max(self.daily.by_org.len());
    }
    self.dirty = true;
    taken
  }

  /// Writes the coarse rings to the store, if anything was recorded since the
  /// last write. Called on the same periodic flush and shutdown path as the
  /// persistent stats.
  pub(crate) fn save_if_dirty(&mut self) {
    if !self.dirty {
      return;
    }
    let Some(conn) = self.conn.as_ref() else {
      self.dirty = false;
      return;
    };
    let persisted = PersistedActivity {
      coarse: std::mem::take(&mut self.coarse),
      daily: std::mem::take(&mut self.daily),
    };
    match serde_json::to_string(&persisted) {
      Ok(json) => {
        match conn.execute(
          "INSERT INTO stats (key, data) VALUES ('activity', ?1)
           ON CONFLICT(key) DO UPDATE SET data = excluded.data",
          rusqlite::params![json],
        ) {
          Ok(_) => self.dirty = false,
          Err(e) => error!("Failed to persist the activity rings to the store: {}", e),
        }
      }
      Err(e) => error!("Failed to serialize the activity rings: {}", e),
    }
    self.coarse = persisted.coarse;
    self.daily = persisted.daily;
  }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
