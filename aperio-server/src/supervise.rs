//! Keeping the background loops alive (planned_features #11).
//!
//! Under the default `unwind` panic strategy a panic unwinds only its own
//! task, so the process survives. A bare `tokio::spawn`ed loop that panics
//! therefore just *stops*, and its function is gone for the life of the
//! process. The panic hook makes the panic visible in the log; it does not
//! bring the loop back, and nothing else notices, because a ticker that has
//! stopped and a ticker with nothing to do look exactly alike from outside.
//!
//! That is the whole failure: alerting stops firing and reads as "no alerts",
//! retention stops pruning and reads as "nothing expired", the stats flush
//! stops and reads as "no traffic".
//!
//! ## Restarting is not the same as forgiving
//!
//! A panicking loop is a bug, and a supervisor that restarts it forever turns
//! a loud bug into a quiet one. So restarting here is bounded: each restart is
//! logged at error with the loop's name, the delay grows, and after
//! [`MAX_RESTARTS`] consecutive panics the loop is left down with a final
//! message saying so. A loop that panics every time it runs will not be fixed
//! by running it again, and pretending otherwise burns CPU while hiding the
//! cause. A loop that survives [`STABLE_AFTER`] gets its budget back, because
//! by then the panic was a transient, not a certainty.
//!
//! Restarting is safe from lock poisoning: the state these loops touch is
//! behind `tokio::sync` mutexes, which do not poison. It is *not* a guarantee
//! that a half-applied change was rolled back, and no supervisor can offer
//! that; the tick functions are written to be re-runnable, which is what makes
//! this sound.

use std::future::Future;
use std::time::{Duration, Instant};

/// Consecutive panics after which a loop is left down.
const MAX_RESTARTS: u32 = 5;
/// How long a loop must run without panicking to have its budget restored.
const STABLE_AFTER: Duration = Duration::from_secs(300);
/// Delay before the first restart; it doubles up to [`MAX_RESTART_DELAY`].
const FIRST_RESTART_DELAY: Duration = Duration::from_secs(1);
/// Ceiling on the restart delay.
const MAX_RESTART_DELAY: Duration = Duration::from_secs(60);

/// Spawns a background loop that comes back if it panics.
///
/// `body` is called to produce the future each time, so a restart begins with
/// fresh local state rather than resuming a future that has already unwound.
/// A body that returns normally is taken at its word and not restarted: that
/// is a loop deciding it is done, which several of these do when their feature
/// is switched off.
pub(crate) fn spawn_supervised<F, Fut>(name: &'static str, body: F)
where
  F: Fn() -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  tokio::spawn(async move {
    let mut consecutive = 0u32;
    loop {
      let started = Instant::now();
      let handle = tokio::spawn(body());
      match handle.await {
        Ok(()) => return,
        Err(e) if e.is_cancelled() => return,
        Err(_) => {}
      }
      // The panic itself is already in the log, from the hook. What is worth
      // adding is which loop it was, since the hook only knows a location.
      if started.elapsed() >= STABLE_AFTER {
        consecutive = 0;
      }
      consecutive += 1;
      if consecutive > MAX_RESTARTS {
        tracing::error!(
          "Background loop `{name}` panicked {} times in a row and is being left down. \
           Its function is lost until the server restarts; the panic above is the bug to fix.",
          consecutive
        );
        return;
      }
      let delay = restart_delay(consecutive);
      tracing::error!(
        "Background loop `{name}` panicked (attempt {} of {}); restarting in {}s",
        consecutive,
        MAX_RESTARTS,
        delay.as_secs()
      );
      tokio::time::sleep(delay).await;
    }
  });
}

/// Watches a task that cannot be restarted, and says so loudly if it dies.
///
/// Some long-lived tasks own something a restart cannot reproduce: a channel
/// receiver (the telemetry collector, the access-log writer) or a bound socket
/// (an `expose:` listener). Calling them again would need the thing back, and
/// the caller has already given it away.
///
/// So this escalates rather than restarts, which is the other half of the
/// problem: the panic hook prints a location, and nobody connects that
/// location to "the access log has stopped being written". This names the task
/// and says what stopped working.
///
/// These fail *visibly* where the tickers fail silently, which is why they get
/// the lesser treatment: a dead channel consumer makes every sender's `send`
/// fail, and both of ours already count and report that. A dead listener stops
/// accepting, which the next connection discovers.
pub(crate) fn spawn_critical<Fut>(name: &'static str, task: Fut)
where
  Fut: Future<Output = ()> + Send + 'static,
{
  tokio::spawn(async move {
    let handle = tokio::spawn(task);
    match handle.await {
      Ok(()) => {}
      Err(e) if e.is_cancelled() => {}
      Err(_) => tracing::error!(
        "Task `{name}` panicked and cannot be restarted: what it owns (a channel or a bound \
         socket) is gone with it. Its function is lost until the server restarts; the panic \
         above is the bug to fix."
      ),
    }
  });
}

/// Delay before restart number `attempt`, doubling and capped.
///
/// Not zero, even for the first: a loop whose panic is caused by something
/// transient (a disk that is momentarily full, a lock contention timeout)
/// wants a moment to pass, and one whose panic is immediate would otherwise
/// spin through its whole budget in microseconds and be declared dead before
/// anybody could read the log.
fn restart_delay(attempt: u32) -> Duration {
  let secs = FIRST_RESTART_DELAY
    .as_secs()
    .saturating_mul(1u64 << attempt.saturating_sub(1).min(6));
  Duration::from_secs(secs.min(MAX_RESTART_DELAY.as_secs()))
}

/// The common shape: sleep, do one tick, repeat, supervised.
///
/// Almost every background loop in the server is exactly this, so wrapping it
/// once keeps the call sites saying what they do rather than how they loop.
/// The sleep comes first, matching what these loops already did: a tick at
/// startup would run against a server that has not finished coming up.
pub(crate) fn spawn_ticker<F, Fut>(name: &'static str, interval: Duration, tick: F)
where
  F: Fn() -> Fut + Send + Sync + Clone + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  spawn_supervised(name, move || {
    let tick = tick.clone();
    async move {
      loop {
        tokio::time::sleep(interval).await;
        tick().await;
      }
    }
  });
}

#[cfg(test)]
#[path = "supervise_tests.rs"]
mod tests;
