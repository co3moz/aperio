//! Announcing less capacity when the backend stops keeping up
//! (planned_features #65).
//!
//! ## What the backlog asked for, and why this is not quite that
//!
//! The entry asked for load *shedding*: when the host is saturated, fail some
//! requests fast instead of making every visitor wait. Two things are wrong
//! with that as stated, and both change the design.
//!
//! First, the signal. `#37` measures this *process's* CPU, which is not host
//! saturation and not backend saturation. The interesting case is precisely
//! the one it cannot see: a client sitting at 3% CPU in front of a backend
//! that has fallen over. What does measure it is already here, requests
//! waiting for the local `max_concurrent` permit. If they are queueing
//! locally, the backend is behind, whatever the CPU says.
//!
//! Second, the action. A client that refuses a request the server already
//! dispatched turns a slow success into a fast failure, which is worse for the
//! visitor unless the wait would have exceeded the timeout anyway. The client
//! is the wrong place to shed, because it is the place with the least context.
//!
//! ## What this does instead
//!
//! `max_concurrent` is announced in every Ping and the **server already
//! queues** rather than dispatching past it. So the gap is not that nothing
//! sheds, it is that the announced number is *static*: a client that has
//! become slow keeps advertising the capacity it had at startup.
//!
//! This makes that number move. When requests queue locally the client
//! announces less, and the server, which needs no new code to honour it, holds
//! the request, hands it to another client in the pool, or asks for capacity
//! through autoscaling. All three are better answers than a refusal, and all
//! three are decisions the server can make and the client cannot.
//!
//! ## AIMD, for the reason TCP uses it
//!
//! Additive increase, multiplicative decrease: halve on trouble, climb back
//! one at a time. Being too high costs every visitor in the queue; being too
//! low costs some throughput. Those are not symmetric, so the response is not
//! either.
//!
//! One interaction worth writing down: lowering the announced number *raises*
//! the utilization the server's autoscaler measures (`inflight / sum
//! max_concurrent`), so a client that shrinks can trigger a scale-out. That is
//! the right outcome, a struggling client should attract capacity, and both
//! loops move on windows of tens of seconds, so the pair settles rather than
//! oscillates.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Semaphore;

/// How often the verdict is taken.
const WINDOW: Duration = Duration::from_secs(10);
/// Mean permit wait above which the backend is judged to be behind.
///
/// Not zero: a busy client always waits a little, and a limit that reacted to
/// any wait at all would shrink a perfectly healthy service. A quarter of a
/// second of *queueing*, before the backend has even been asked, is time the
/// visitor is spending on nothing.
const SLOW_WAIT: Duration = Duration::from_millis(250);
/// Mean permit wait below which the client tries to climb back.
///
/// Well under `SLOW_WAIT` rather than just under it: the gap is the hysteresis
/// that stops a service hovering between the two from halving and climbing
/// forever.
const CLEAR_WAIT: Duration = Duration::from_millis(50);
/// Fewest permits the client will announce. One request at a time is still a
/// working service; zero is an outage the client inflicted on itself.
const FLOOR: u32 = 1;

/// A service's announced concurrency, and the evidence for changing it.
pub(crate) struct Adaptive {
  /// What the config asked for. The current value never exceeds it: this
  /// lowers a ceiling under pressure, it does not raise one the operator set.
  configured: u32,
  /// What is announced right now.
  current: AtomicU32,
  /// The local limiter, resized in step with the announcement so a server
  /// that ignores the number cannot push past it either.
  limiter: Arc<Semaphore>,
  /// Permits actually taken out of the limiter so far.
  ///
  /// Not derivable from `current`, and that is the whole reason it exists.
  /// `Semaphore::forget_permits` removes *at most* what it is asked for: with
  /// every permit in flight there is nothing available to take, so a shrink
  /// removes fewer than it meant to, or none. Giving back the difference later
  /// would then hand the limiter more permits than the operator configured,
  /// which is the opposite of what this feature promises. Only what was taken
  /// is ever given back.
  forgotten: AtomicU32,
  /// Permit waits observed in this window: total microseconds and count.
  wait_micros: AtomicU64,
  waits: AtomicU64,
}

/// What a window's evidence says to do.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum Verdict {
  Shrink(u32),
  Grow(u32),
  Hold,
}

impl Adaptive {
  pub(crate) fn new(configured: u32, limiter: Arc<Semaphore>) -> Self {
    Adaptive {
      configured,
      current: AtomicU32::new(configured),
      limiter,
      forgotten: AtomicU32::new(0),
      wait_micros: AtomicU64::new(0),
      waits: AtomicU64::new(0),
    }
  }

  /// The number to announce in the next Ping.
  pub(crate) fn announced(&self) -> u32 {
    self.current.load(Ordering::Relaxed)
  }

  /// Records how long one request waited for its permit.
  pub(crate) fn record_wait(&self, waited: Duration) {
    self
      .wait_micros
      .fetch_add(waited.as_micros() as u64, Ordering::Relaxed);
    self.waits.fetch_add(1, Ordering::Relaxed);
  }

  /// Reads the window and clears it.
  ///
  /// A window with no requests in it is `Hold`, not `Grow`: an idle service
  /// has produced no evidence that its backend recovered, and climbing on
  /// silence would restore the full ceiling every quiet minute and rediscover
  /// the problem with live traffic.
  fn verdict(&self) -> Verdict {
    let count = self.waits.swap(0, Ordering::Relaxed);
    let micros = self.wait_micros.swap(0, Ordering::Relaxed);
    if count == 0 {
      return Verdict::Hold;
    }
    let mean = Duration::from_micros(micros / count);
    let current = self.current.load(Ordering::Relaxed);
    if mean >= SLOW_WAIT && current > FLOOR {
      // Multiplicative decrease.
      return Verdict::Shrink((current / 2).max(FLOOR));
    }
    if mean <= CLEAR_WAIT && current < self.configured {
      // Additive increase.
      return Verdict::Grow(current + 1);
    }
    Verdict::Hold
  }

  /// Applies one window's verdict, resizing the local limiter to match.
  ///
  /// Shrinking *forgets* permits rather than holding them: a forgotten permit
  /// is one the semaphore will not hand out again, and returning them later is
  /// what growing does. Requests already in flight are never interrupted, they
  /// finish and simply find fewer permits behind them.
  ///
  /// Both directions move by what the limiter *actually* did rather than by
  /// what was asked, so the announced number never claims a ceiling the
  /// limiter is not enforcing, and the limiter never ends up above the
  /// configured one. A shrink that could only take some of what it wanted
  /// simply takes the rest next window, when the in-flight requests have
  /// finished and there are permits to take.
  fn apply(&self, verdict: Verdict) -> Option<u32> {
    let target = match verdict {
      Verdict::Hold => return None,
      Verdict::Shrink(n) | Verdict::Grow(n) => n,
    };
    let previous = self.current.load(Ordering::Relaxed);
    let moved = if target < previous {
      let taken = self.limiter.forget_permits((previous - target) as usize) as u32;
      self.forgotten.fetch_add(taken, Ordering::Relaxed);
      previous - taken
    } else {
      // Only ever give back what was taken. Anything more would be permits
      // the operator never configured.
      let wanted = target - previous;
      let available = self.forgotten.load(Ordering::Relaxed);
      let give = wanted.min(available);
      if give > 0 {
        self.limiter.add_permits(give as usize);
        self.forgotten.fetch_sub(give, Ordering::Relaxed);
      }
      previous + give
    };
    self.current.store(moved, Ordering::Relaxed);
    (moved != previous).then_some(moved)
  }

  /// One window: read the evidence, act on it, and say what changed.
  pub(crate) fn tick(&self) -> Option<u32> {
    self.apply(self.verdict())
  }
}

/// Runs the window loop for one service.
pub(crate) fn spawn(adaptive: Arc<Adaptive>, label: String) {
  tokio::spawn(async move {
    loop {
      tokio::time::sleep(WINDOW).await;
      if let Some(now) = adaptive.tick() {
        tracing::info!(
          "[{label}] adaptive concurrency: announcing {} of {} (backend queueing)",
          now,
          adaptive.configured
        );
      }
    }
  });
}

#[cfg(test)]
#[path = "adaptive_tests.rs"]
mod tests;
