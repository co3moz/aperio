//! A site's `maintenance`: recurring windows during which its hostname
//! answers the maintenance page by itself.
//!
//! Maintenance mode has always been a switch somebody flips, with an optional
//! TTL. That covers the unplanned outage and nothing else: a window at 02:00
//! every Sunday meant an operator setting an alarm, and the flag that causes
//! the outage is the one switched on for twenty minutes of work and
//! forgotten. A recurring window is standing configuration rather than a
//! runtime action, so it is a setting, and survives a restart that the
//! runtime flags do not.
//!
//! Times are local to an explicit IANA zone, defaulting to UTC. That is the
//! whole reason `chrono-tz` is a dependency: an operator writing 02:00 means
//! 02:00 where they live, in March as well as in July, which a fixed offset
//! cannot express.

use chrono::{Datelike, NaiveTime, TimeZone, Weekday};

/// One compiled window.
#[derive(Clone, Debug)]
pub(crate) struct Window {
  start: NaiveTime,
  end: NaiveTime,
  /// Weekdays the window may *start* on. Empty = every day.
  days: Vec<Weekday>,
  tz: chrono_tz::Tz,
  pub(crate) reason: Option<String>,
}

impl Window {
  /// True when `now` falls inside this window, and the unix second it ends.
  ///
  /// A window whose `to` is earlier than its `from` wraps midnight, and then
  /// `days` names the day it *starts*: `22:00 -> 02:00` on Saturday runs into
  /// Sunday morning, and asking whether Sunday is listed would end it at
  /// midnight, which is not what anybody writing that meant.
  pub(crate) fn active_until(&self, now_secs: u64) -> Option<u64> {
    let now = chrono::DateTime::from_timestamp(now_secs as i64, 0)?.with_timezone(&self.tz);
    let today = now.weekday();
    let time = now.time();
    let listed = |d: Weekday| self.days.is_empty() || self.days.contains(&d);

    if self.start < self.end {
      if listed(today) && time >= self.start && time < self.end {
        return self.instant_of(now.date_naive(), self.end);
      }
      return None;
    }
    // Wrapping window.
    if listed(today) && time >= self.start {
      // Ends tomorrow morning.
      return self.instant_of(now.date_naive().succ_opt()?, self.end);
    }
    let yesterday = now.date_naive().pred_opt()?;
    if listed(yesterday.weekday()) && time < self.end {
      return self.instant_of(now.date_naive(), self.end);
    }
    None
  }

  /// The unix second at which `time` occurs on `date` in this window's zone.
  ///
  /// A daylight-saving jump can make that local time not exist, or exist
  /// twice. Neither is worth failing over for an end-of-window instant, so
  /// the earliest interpretation is taken and, when the time is skipped
  /// entirely, the window simply reports no end rather than a wrong one.
  fn instant_of(&self, date: chrono::NaiveDate, time: NaiveTime) -> Option<u64> {
    let naive = date.and_time(time);
    self
      .tz
      .from_local_datetime(&naive)
      .earliest()
      .map(|dt| dt.timestamp().max(0) as u64)
  }
}

impl Window {
  /// Compiles one schedule; `at` names it in an error (`maintenance[0]`),
  /// and the site puts its own name in front.
  /// Every rejection is a message the operator can act on, because a window
  /// that silently does not fire is a maintenance page nobody sees and a
  /// deploy nobody stopped.
  pub(crate) fn compile(
    w: &aperio_config::MaintenanceSchedule,
    at: &str,
  ) -> Result<Window, String> {
    let at = |what: &str| format!("{at}: {what}");
    let parse_time = |v: &str| {
      NaiveTime::parse_from_str(v.trim(), "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(v.trim(), "%H:%M:%S"))
    };
    let start = parse_time(&w.from).map_err(|_| at(&format!("`from: {}` is not HH:MM", w.from)))?;
    let end = parse_time(&w.to).map_err(|_| at(&format!("`to: {}` is not HH:MM", w.to)))?;
    if start == end {
      return Err(at(
        "`from` and `to` are the same time, which is either an empty window or a whole day; \
         write 00:00 to 23:59 if you meant all day",
      ));
    }
    let tz: chrono_tz::Tz = match w.tz.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
      None => chrono_tz::UTC,
      Some(name) => name.parse().map_err(|_| {
        at(&format!(
          "`tz: {name}` is not an IANA time zone (e.g. Europe/Istanbul)"
        ))
      })?,
    };
    let mut days = Vec::new();
    for d in w.days.clone().unwrap_or_default() {
      let parsed = match d.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        other => return Err(at(&format!("`{other}` is not a weekday"))),
      };
      if !days.contains(&parsed) {
        days.push(parsed);
      }
    }
    Ok(Window {
      start,
      end,
      days,
      tz,
      reason: w.reason.clone().filter(|r| !r.trim().is_empty()),
    })
  }
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
