//! What these pin down: that the announced number falls fast and climbs
//! slowly, that it never leaves the band the operator set, and that silence
//! is not taken as recovery.

use super::*;

fn adaptive(configured: u32) -> Adaptive {
  Adaptive::new(configured, Arc::new(Semaphore::new(configured as usize)))
}

fn record(a: &Adaptive, wait: Duration, times: usize) {
  for _ in 0..times {
    a.record_wait(wait);
  }
}

#[test]
fn queueing_halves_the_announced_concurrency() {
  let a = adaptive(16);
  record(&a, Duration::from_millis(400), 5);
  assert_eq!(a.tick(), Some(8));
  record(&a, Duration::from_millis(400), 5);
  assert_eq!(a.tick(), Some(4));
  // The limiter is resized with the announcement, so a server that ignores
  // the number cannot push past it either.
  assert_eq!(a.limiter.available_permits(), 4);
}

#[test]
fn recovery_climbs_one_at_a_time() {
  let a = adaptive(16);
  record(&a, Duration::from_millis(400), 5);
  assert_eq!(a.tick(), Some(8));
  // Additive increase: being too high costs every visitor in the queue, being
  // too low costs some throughput, and those are not symmetric.
  record(&a, Duration::from_millis(1), 5);
  assert_eq!(a.tick(), Some(9));
  record(&a, Duration::from_millis(1), 5);
  assert_eq!(a.tick(), Some(10));
}

#[test]
fn it_never_leaves_the_band_the_operator_set() {
  let a = adaptive(2);
  // The floor: one request at a time is still a working service, zero is an
  // outage the client inflicted on itself.
  for _ in 0..10 {
    record(&a, Duration::from_secs(5), 3);
    a.tick();
  }
  assert_eq!(a.announced(), FLOOR);

  // The ceiling: this lowers a limit under pressure, it does not raise one.
  for _ in 0..10 {
    record(&a, Duration::from_millis(1), 3);
    a.tick();
  }
  assert_eq!(a.announced(), 2);
  assert_eq!(a.limiter.available_permits(), 2);
}

#[test]
fn a_wait_between_the_two_thresholds_changes_nothing() {
  let a = adaptive(16);
  // The gap is the hysteresis: a service hovering here would otherwise halve
  // and climb forever.
  record(&a, Duration::from_millis(120), 5);
  assert_eq!(a.tick(), None);
  assert_eq!(a.announced(), 16);
}

#[test]
fn an_idle_window_is_not_recovery() {
  let a = adaptive(16);
  record(&a, Duration::from_millis(400), 5);
  assert_eq!(a.tick(), Some(8));
  // No requests at all: an idle service has produced no evidence that its
  // backend recovered, and climbing on silence would restore the ceiling
  // every quiet minute and rediscover the problem with live traffic.
  assert_eq!(a.tick(), None);
  assert_eq!(a.announced(), 8);
}

#[test]
fn each_window_is_judged_on_its_own_evidence() {
  let a = adaptive(16);
  // A bad window followed by a good one must not be averaged together: the
  // counters are cleared when they are read.
  record(&a, Duration::from_millis(400), 1);
  assert_eq!(a.tick(), Some(8));
  record(&a, Duration::from_millis(1), 1);
  assert_eq!(a.tick(), Some(9));
}
