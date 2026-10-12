//! What a scheduled window has to get right: the boundaries of the window
//! itself, a window that wraps midnight (where `days` names the day it
//! starts, not the day it ends), the time zone actually being applied, and
//! every malformed entry being reported rather than silently never firing.

use super::*;

/// Compiles one window from yaml, as a site's `maintenance` entry.
fn window(yaml: &str) -> Window {
  let raw: aperio_config::MaintenanceSchedule = serde_yaml::from_str(yaml).unwrap();
  Window::compile(&raw, "maintenance[0]").unwrap()
}

fn err(yaml: &str) -> String {
  let raw: aperio_config::MaintenanceSchedule = serde_yaml::from_str(yaml).unwrap();
  Window::compile(&raw, "maintenance[0]").expect_err("refused")
}

/// Unix seconds for a UTC wall-clock instant.
fn utc(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> u64 {
  chrono::NaiveDate::from_ymd_opt(y, m, d)
    .unwrap()
    .and_hms_opt(hh, mm, 0)
    .unwrap()
    .and_utc()
    .timestamp() as u64
}

#[test]
fn a_window_covers_its_own_hours_and_nothing_else() {
  let w = window("from: \"02:00\"\nto: \"04:00\"\n");
  // 2026-08-05 is a Wednesday; no `days`, so every day counts.
  assert!(
    w.active_until(utc(2026, 8, 5, 2, 0)).is_some(),
    "start is inclusive"
  );
  assert!(w.active_until(utc(2026, 8, 5, 3, 30)).is_some());
  assert!(w.active_until(utc(2026, 8, 5, 1, 59)).is_none());
  assert!(
    w.active_until(utc(2026, 8, 5, 4, 0)).is_none(),
    "the end is exclusive, so a 02:00-04:00 window is over at 04:00"
  );
}

#[test]
fn the_reported_end_is_when_the_window_closes() {
  let w = window("from: \"02:00\"\nto: \"04:00\"\n");
  assert_eq!(
    w.active_until(utc(2026, 8, 5, 3, 0)),
    Some(utc(2026, 8, 5, 4, 0)),
    "Retry-After points at the real end"
  );
}

#[test]
fn days_restrict_which_days_the_window_runs() {
  // 2026-08-08 is a Saturday, 2026-08-09 a Sunday, 2026-08-10 a Monday.
  let w = window("from: \"02:00\"\nto: \"04:00\"\ndays: [sat, Sunday]\n");
  assert!(w.active_until(utc(2026, 8, 8, 3, 0)).is_some(), "saturday");
  assert!(
    w.active_until(utc(2026, 8, 9, 3, 0)).is_some(),
    "sunday, long form"
  );
  assert!(
    w.active_until(utc(2026, 8, 10, 3, 0)).is_none(),
    "monday is not listed"
  );
}

#[test]
fn a_window_that_wraps_midnight_belongs_to_the_day_it_starts() {
  // Saturday 22:00 to Sunday 02:00. Naming Sunday would be the bug: the
  // window is written on the day it begins.
  let w = window("from: \"22:00\"\nto: \"02:00\"\ndays: [sat]\n");
  assert!(
    w.active_until(utc(2026, 8, 8, 23, 0)).is_some(),
    "saturday night"
  );
  assert_eq!(
    w.active_until(utc(2026, 8, 9, 1, 0)),
    Some(utc(2026, 8, 9, 2, 0)),
    "sunday morning is still the saturday window"
  );
  assert!(
    w.active_until(utc(2026, 8, 9, 23, 0)).is_none(),
    "sunday night is not a listed start"
  );
  assert!(w.active_until(utc(2026, 8, 8, 21, 59)).is_none());
}

#[test]
fn the_time_zone_is_applied_rather_than_decorative() {
  // 02:00 in Europe/Istanbul (UTC+3) is 23:00 UTC the previous day.
  let w = window("from: \"02:00\"\nto: \"04:00\"\ntz: Europe/Istanbul\n");
  assert!(
    w.active_until(utc(2026, 8, 4, 23, 30)).is_some(),
    "local 02:30"
  );
  assert!(
    w.active_until(utc(2026, 8, 5, 2, 30)).is_none(),
    "02:30 UTC is 05:30 local, past the window"
  );
}

#[test]
fn malformed_entries_are_refused_with_a_usable_message() {
  assert!(err("from: \"2 am\"\nto: \"04:00\"\n").contains("not HH:MM"));
  assert!(err("from: \"02:00\"\nto: \"02:00\"\n").contains("same time"));
  assert!(err("from: \"02:00\"\nto: \"04:00\"\ndays: [funday]\n").contains("not a weekday"));
  assert!(
    err("from: \"02:00\"\nto: \"04:00\"\ntz: Mars/Olympus\n").contains("not an IANA time zone")
  );
  assert!(
    err("from: \"2 am\"\nto: \"04:00\"\n").starts_with("maintenance[0]"),
    "the entry is named"
  );
}
