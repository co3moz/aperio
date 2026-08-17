//! Streamed responses under pressure: the per-visitor ceiling that stops one
//! consumer taking the whole budget, and the minimum-throughput guard that
//! ends a stream nobody is draining.

use super::*;

// ---------------------------------------------------------------------------
// Per-visitor streamed-response ceiling (planned_features #20)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_stream_ceiling_is_off_by_default() {
  let state = crate::test_support::test_state();
  // A NAT or a CGNAT puts many real people behind one address, so a default
  // here would be a guess with a queue of users behind it.
  assert_eq!(state.config().max_streams_per_ip, 0);
  assert!(
    state
      .try_acquire_stream_slot("203.0.113.7".parse().unwrap())
      .is_none(),
    "off means the caller takes the ungated path, not that a slot is handed out"
  );
}

#[tokio::test]
async fn a_visitor_holds_at_most_its_share_and_gets_it_back() {
  let mut config = crate::test_support::test_config();
  config.max_streams_per_ip = 2;
  let state = crate::test_support::test_state_with(config);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();

  let first = state.try_acquire_stream_slot(ip).expect("one");
  let second = state.try_acquire_stream_slot(ip).expect("two");
  assert!(
    state.try_acquire_stream_slot(ip).is_none(),
    "the third is refused"
  );

  // A concurrency limit, not a rate limit: closing a stream frees its slot at
  // once, so a visitor that opens and closes as fast as it likes never trips
  // it and one that holds them open does.
  drop(second);
  let third = state.try_acquire_stream_slot(ip).expect("a freed slot");
  drop(first);
  drop(third);

  // Nothing left behind: the map holds only the addresses currently
  // streaming, or it grows one entry per stranger for the life of the process.
  assert!(
    state.stream_counts.lock().unwrap().get(&ip).is_none(),
    "the entry is removed at zero, not left at zero"
  );
}

#[tokio::test]
async fn one_visitor_at_its_ceiling_does_not_block_another() {
  let mut config = crate::test_support::test_config();
  config.max_streams_per_ip = 1;
  let state = crate::test_support::test_state_with(config);
  let noisy: std::net::IpAddr = "203.0.113.7".parse().unwrap();
  let quiet: std::net::IpAddr = "198.51.100.4".parse().unwrap();

  let _held = state.try_acquire_stream_slot(noisy).expect("one");
  assert!(state.try_acquire_stream_slot(noisy).is_none());
  // The whole point: saturating the service now takes a botnet rather than
  // one host.
  assert!(state.try_acquire_stream_slot(quiet).is_some());
}

// ---------------------------------------------------------------------------
// Minimum-throughput guard for streamed responses (planned_features #17)
// ---------------------------------------------------------------------------

#[test]
fn the_throughput_floor_is_off_unless_asked_for() {
  let mut guard = super::ThroughputGuard::new(0);
  let start = Instant::now();
  // A stream that takes nothing at all, for far longer than the window.
  assert!(!guard.record(
    0,
    Duration::from_secs(600),
    start + Duration::from_secs(600)
  ));
}

#[test]
fn a_reader_that_keeps_data_waiting_and_takes_too_little_is_ended() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  // Thirty seconds of the consumer holding data up, and 100 bytes taken.
  // This is the hole the per-item stall timeout leaves: it accepted chunks,
  // so it never timed out, it just accepted them impossibly slowly.
  assert!(guard.record(
    100,
    Duration::from_secs(30),
    start + super::MIN_THROUGHPUT_WINDOW
  ));
}

#[test]
fn a_reader_that_keeps_up_is_left_alone() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  assert!(!guard.record(
    1024 * 60,
    Duration::from_secs(30),
    start + super::MIN_THROUGHPUT_WINDOW
  ));
}

#[test]
fn a_quiet_backend_never_costs_the_consumer_its_stream() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  // An hour of wall clock, one byte delivered, but the consumer only ever
  // kept data waiting for a millisecond: server-sent events and long polling
  // look exactly like this, and measuring wall-clock throughput would end
  // them. What is measured is "data was ready and you did not take it".
  assert!(!guard.record(
    1,
    Duration::from_millis(1),
    start + Duration::from_secs(3600)
  ));
}

#[test]
fn the_verdict_is_taken_once_per_window_and_the_counters_reset() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  // Inside the window nothing is decided, however bad it looks.
  assert!(!guard.record(0, Duration::from_secs(29), start + Duration::from_secs(29)));
  // The window closes and this one fails.
  let closed = start + super::MIN_THROUGHPUT_WINDOW + Duration::from_secs(1);
  assert!(guard.record(0, Duration::from_secs(2), closed));
  // A fresh window starts clean: the previous window's starvation must not
  // be held against a consumer that has started keeping up.
  assert!(!guard.record(
    1024 * 60,
    Duration::from_secs(30),
    closed + super::MIN_THROUGHPUT_WINDOW
  ));
}

// ---------------------------------------------------------------------------
