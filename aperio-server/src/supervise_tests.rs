//! What these pin down: that a panicking loop comes back, that a loop which
//! keeps panicking is eventually left down rather than spun forever, and that
//! a loop which finishes on purpose is not resurrected.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Runs `spawn_supervised` on a paused clock and lets the restart delays
/// elapse, so a test does not sit through them in real time.
async fn settle() {
  for _ in 0..64 {
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(120)).await;
  }
  tokio::task::yield_now().await;
}

#[tokio::test(start_paused = true)]
async fn a_panicking_loop_comes_back() {
  let runs = Arc::new(AtomicU32::new(0));
  let counter = runs.clone();
  spawn_supervised("test-restart", move || {
    let counter = counter.clone();
    async move {
      let n = counter.fetch_add(1, Ordering::SeqCst);
      if n < 2 {
        panic!("first two runs panic");
      }
      // The third settles into an ordinary loop.
      loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
      }
    }
  });
  settle().await;
  // Three runs: two that panicked and the one still going. Without the
  // supervisor the count would stop at one and the loop's function would be
  // gone for the life of the process.
  assert_eq!(runs.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn a_loop_that_always_panics_is_left_down() {
  let runs = Arc::new(AtomicU32::new(0));
  let counter = runs.clone();
  spawn_supervised("test-hopeless", move || {
    let counter = counter.clone();
    async move {
      counter.fetch_add(1, Ordering::SeqCst);
      panic!("every run panics");
    }
  });
  settle().await;
  // The first run plus MAX_RESTARTS attempts, and then it stops: running a
  // deterministic panic again does not fix it, and an endless restart turns a
  // loud bug into a quiet one that also burns CPU.
  assert_eq!(runs.load(Ordering::SeqCst), MAX_RESTARTS + 1);
}

#[tokio::test(start_paused = true)]
async fn a_loop_that_returns_is_taken_at_its_word() {
  let runs = Arc::new(AtomicU32::new(0));
  let counter = runs.clone();
  spawn_supervised("test-finished", move || {
    let counter = counter.clone();
    async move {
      counter.fetch_add(1, Ordering::SeqCst);
    }
  });
  settle().await;
  // Several of these loops end deliberately when their feature is off.
  // Restarting one would be a busy loop over a decision already made.
  assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_ticker_ticks_and_survives_a_panicking_tick() {
  let ticks = Arc::new(AtomicU32::new(0));
  let counter = ticks.clone();
  spawn_ticker("test-ticker", Duration::from_secs(10), move || {
    let counter = counter.clone();
    async move {
      let n = counter.fetch_add(1, Ordering::SeqCst);
      if n == 1 {
        panic!("one bad tick");
      }
    }
  });
  settle().await;
  let seen = ticks.load(Ordering::SeqCst);
  // More than the two it took to reach the panic: the ticker kept ticking
  // afterwards, which is the whole point.
  assert!(seen > 2, "only {seen} tick(s)");
}

#[test]
fn the_restart_delay_grows_and_is_capped() {
  // Never zero, even the first time: an immediate panic would otherwise spin
  // through the whole budget in microseconds and be declared dead before
  // anybody could read the log.
  assert_eq!(restart_delay(1), FIRST_RESTART_DELAY);
  assert!(restart_delay(2) > restart_delay(1));
  assert!(restart_delay(3) > restart_delay(2));
  assert_eq!(restart_delay(50), MAX_RESTART_DELAY);
}
