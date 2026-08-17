//! What this client does when a backend misbehaves: the retry policy, the
//! circuit breaker in front of it, and telling a connection that was closed
//! under us from one that refused.
//!
//! The stale-connection check is the reason retries are safe to offer at all:
//! a pooled connection the backend closed while idle is not a failed request,
//! it is a request that never left, so replaying it is not a second delivery.

use super::*;

#[derive(Default)]
pub(crate) struct BreakerState {
  /// Consecutive failures since the last success.
  failures: u32,
  /// While set, the breaker is open until this instant.
  open_until: Option<std::time::Instant>,
}

/// What the breaker says about dialing right now.
pub(crate) enum BreakerVerdict {
  /// Dial: either closed, or open and this is the probe that tests recovery.
  Proceed,
  /// Do not dial; the backend is being left alone for this long.
  Open(std::time::Duration),
}

impl BackendResilience {
  pub(crate) fn new(
    attempts: u32,
    backoff_ms: u64,
    all_methods: bool,
    breaker_failures: u32,
    breaker_open_for_secs: u64,
  ) -> Self {
    BackendResilience {
      attempts: attempts.max(1),
      backoff: std::time::Duration::from_millis(backoff_ms),
      all_methods,
      breaker_failures,
      breaker_open_for: std::time::Duration::from_secs(breaker_open_for_secs.max(1)),
      state: Default::default(),
    }
  }

  /// True when this method may be retried under the configured policy.
  /// Idempotent by the HTTP definition, plus whatever `all_methods` adds.
  pub(crate) fn may_retry_method(&self, method: &str) -> bool {
    self.all_methods
      || matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "PUT" | "DELETE" | "OPTIONS" | "TRACE"
      )
  }

  /// Asks the breaker whether to dial. When the open window has elapsed this
  /// returns `Proceed` and closes the window, so exactly one request probes
  /// the backend; if it fails, `record_failure` opens the window again.
  pub(crate) fn check(&self) -> BreakerVerdict {
    if self.breaker_failures == 0 {
      return BreakerVerdict::Proceed;
    }
    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
    match st.open_until {
      Some(until) => {
        let now = std::time::Instant::now();
        if now >= until {
          // The probe: the window is cleared here rather than on success, so
          // a flood while the backend is still down produces one dial per
          // window instead of one per request.
          st.open_until = None;
          BreakerVerdict::Proceed
        } else {
          BreakerVerdict::Open(until - now)
        }
      }
      None => BreakerVerdict::Proceed,
    }
  }

  /// A request reached the backend and got a response head.
  pub(crate) fn record_success(&self) {
    if self.breaker_failures == 0 {
      return;
    }
    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
    st.failures = 0;
    st.open_until = None;
  }

  /// A request failed before any response. Returns true when this failure
  /// opened the breaker, so the caller can say so once rather than per
  /// request.
  pub(crate) fn record_failure(&self) -> bool {
    if self.breaker_failures == 0 {
      return false;
    }
    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
    st.failures = st.failures.saturating_add(1);
    if st.failures >= self.breaker_failures {
      let was_closed = st.open_until.is_none();
      st.open_until = Some(std::time::Instant::now() + self.breaker_open_for);
      return was_closed;
    }
    false
  }
}

/// Whether a failed request looks like a connection the backend had already
/// closed, rather than a backend that is actually unwell.
///
/// An HTTP client keeps connections alive and reuses them. The backend closes
/// idle ones on its own schedule, so there is an unavoidable window where the
/// client writes a request onto a socket the backend has just finished with.
/// hyper reports that as `IncompleteMessage`, or as a reset or a broken pipe:
/// no response head ever arrives, and nothing is wrong with the backend.
///
/// This is worth telling apart because the answer is different. A backend
/// that is failing should reach the visitor as a failure. A connection that
/// was already dead should be dialed again, once, and the visitor should
/// never learn it happened, which is what every mainstream HTTP client does
/// and why the same backend behind nginx does not produce these 502s.
///
/// A connect-time error is excluded on purpose: nothing was reused there, so
/// a refusal is the backend's real answer.
pub(crate) fn is_stale_connection_error(e: &reqwest::Error) -> bool {
  !e.is_connect() && chain_says_connection_closed(e)
}

/// The source-chain half of [`is_stale_connection_error`], for the callers
/// whose error type is not reqwest's: the h2 and unix-socket paths dial with
/// hyper directly and pool their connections just the same.
pub(crate) fn chain_says_connection_closed(e: &(dyn std::error::Error + 'static)) -> bool {
  let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
  while let Some(err) = source {
    if let Some(h) = err.downcast_ref::<hyper::Error>()
      && h.is_incomplete_message()
    {
      return true;
    }
    if let Some(io) = err.downcast_ref::<std::io::Error>()
      && matches!(
        io.kind(),
        std::io::ErrorKind::ConnectionReset
          | std::io::ErrorKind::BrokenPipe
          | std::io::ErrorKind::UnexpectedEof
      )
    {
      return true;
    }
    source = err.source();
  }
  false
}

#[cfg(test)]
#[path = "resilience_tests.rs"]
mod tests;
