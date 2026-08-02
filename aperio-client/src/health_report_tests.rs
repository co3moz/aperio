//! What the self-reported health figures have to get right: a percentage
//! needs two points in time, a round trip needs the send it belongs to, and a
//! reconnect must not let the old link's numbers describe the new one.

use super::*;
use std::time::Duration;

#[test]
fn the_first_cpu_reading_establishes_a_baseline_rather_than_guessing() {
  let report = HealthReport::default();
  // On a platform where CPU time cannot be read this is None either way; what
  // must never happen is a number invented from process start, which would
  // report a run-long average as if it were current.
  let first = report.cpu_percent();
  if cpu_seconds().is_none() {
    assert!(first.is_none(), "no reading is possible here");
  } else {
    assert!(first.is_none(), "the first call only sets the baseline");
  }
}

#[test]
fn cpu_percent_is_measured_against_the_previous_sample() {
  if cpu_seconds().is_none() {
    return; // Not readable on this platform; the None path is covered above.
  }
  // A baseline claiming half a second of CPU one second ago: whatever this
  // process has actually used since then is added to that difference, so the
  // reading is at least the 50% the baseline implies.
  let report =
    HealthReport::with_cpu_baseline(cpu_seconds().unwrap() - 0.5, Duration::from_secs(1));
  let pct = report
    .cpu_percent()
    .expect("a second sample yields a reading");
  assert!(pct >= 40.0, "expected around 50% of one core, got {pct}");
  assert!(pct < 500.0, "an implausible reading: {pct}");
}

#[test]
fn a_round_trip_is_only_timed_against_its_own_ping() {
  let report = HealthReport::default();
  // A Pong with nothing outstanding is ignored: after a reconnect the two can
  // cross, and timing against the wrong send is worse than not timing.
  report.pong_received();
  assert_eq!(report.link().0, None);

  report.ping_sent();
  report.pong_received();
  let (rtt, jitter, _) = report.link();
  assert!(rtt.is_some(), "the first timed round trip is reported");
  assert_eq!(jitter, None, "jitter needs two round trips to exist");

  report.ping_sent();
  report.pong_received();
  assert!(
    report.link().1.is_some(),
    "the second round trip yields jitter"
  );
}

#[test]
fn one_ping_is_only_answered_once() {
  let report = HealthReport::default();
  report.ping_sent();
  report.pong_received();
  let after_first = report.link().0;
  // A duplicate Pong has no outstanding send to be timed against, so it
  // cannot move the figure.
  report.pong_received();
  assert_eq!(report.link().0, after_first);
}

#[test]
fn a_reconnect_counts_and_clears_the_previous_links_numbers() {
  let report = HealthReport::default();
  report.ping_sent();
  report.pong_received();
  report.ping_sent();
  report.pong_received();
  assert!(report.link().0.is_some());

  report.reconnected();
  let (rtt, jitter, reconnects) = report.link();
  assert_eq!(reconnects, 1);
  assert_eq!(
    rtt, None,
    "the old connection's round trip does not describe the new one"
  );
  assert_eq!(jitter, None);

  // And an in-flight Ping from before the reconnect cannot be answered by a
  // Pong from after it.
  report.pong_received();
  assert_eq!(report.link().0, None);
}

#[test]
fn reconnects_accumulate() {
  let report = HealthReport::default();
  for _ in 0..3 {
    report.reconnected();
  }
  assert_eq!(report.link().2, 3);
}
