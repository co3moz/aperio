use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

/// Bucket tracking current tokens and refill state for rate limiting.
pub(crate) struct RateLimitState {
  /// Current token balance.
  pub(crate) tokens: f64,
  /// Last instant when tokens were updated.
  pub(crate) last_updated: Instant,
}

/// Size past which the per-token rate/quota maps are garbage-collected of
/// stale entries (idle or revoked tokens), so a churn of dynamic tokens
/// cannot grow them without bound. Mirrors the per-IP rate limiter's
/// failsafe threshold.
pub(crate) const TOKEN_MAP_GC_THRESHOLD: usize = 1000;

/// Token-rate map GC: once the map is large, drop buckets for tokens idle
/// past the refill window (revoked or unused) so churned dynamic tokens do
/// not accumulate forever. A fully-refilled idle bucket carries no state
/// worth keeping.
pub(crate) fn gc_token_rate(map: &mut HashMap<String, RateLimitState>, now: Instant) {
  if map.len() > TOKEN_MAP_GC_THRESHOLD {
    map.retain(|_, v| now.duration_since(v.last_updated) < Duration::from_secs(600));
  }
}

/// Token daily-byte map GC: once the map is large, drop entries from a past
/// day (only the current day's usage feeds any quota).
pub(crate) fn gc_token_daily_bytes(map: &mut HashMap<String, (String, u64)>, today: &str) {
  if map.len() > TOKEN_MAP_GC_THRESHOLD {
    map.retain(|_, (day, _)| day == today);
  }
}

/// Upper bounds (seconds) of the request duration histogram buckets exposed
/// on `/aperio/metrics`; a `+Inf` bucket is added implicitly.
const DURATION_BUCKETS: [f64; 12] = [
  0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Lock-free in-memory histogram of proxied request durations, rendered as a
/// Prometheus `histogram` (cumulative buckets + sum + count). In-memory only:
/// counters reset on restart, which Prometheus handles natively.
#[derive(Default)]
pub(crate) struct DurationHistogram {
  pub(crate) buckets: [AtomicU64; DURATION_BUCKETS.len()],
  pub(crate) sum_micros: AtomicU64,
  pub(crate) count: AtomicU64,
}

impl DurationHistogram {
  pub(crate) fn observe(&self, duration: Duration) {
    let secs = duration.as_secs_f64();
    for (i, bound) in DURATION_BUCKETS.iter().enumerate() {
      if secs <= *bound {
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
      }
    }
    self
      .sum_micros
      .fetch_add(duration.as_micros() as u64, Ordering::Relaxed);
    self.count.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn render(&self, out: &mut String) {
    out.push_str(
      "# HELP aperio_request_duration_seconds Proxied request duration (dispatch to response).\n",
    );
    out.push_str("# TYPE aperio_request_duration_seconds histogram\n");
    for (i, bound) in DURATION_BUCKETS.iter().enumerate() {
      out.push_str(&format!(
        "aperio_request_duration_seconds_bucket{{le=\"{}\"}} {}\n",
        bound,
        self.buckets[i].load(Ordering::Relaxed)
      ));
    }
    let count = self.count.load(Ordering::Relaxed);
    out.push_str(&format!(
      "aperio_request_duration_seconds_bucket{{le=\"+Inf\"}} {}\n",
      count
    ));
    out.push_str(&format!(
      "aperio_request_duration_seconds_sum {}\n",
      self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
    ));
    out.push_str(&format!(
      "aperio_request_duration_seconds_count {}\n",
      count
    ));
  }
}

pub(crate) use crate::store::sessions::SessionInfo;

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;

/// three are ordered and separated, not that `Expensive` is exactly five
/// requests' worth. Sizing the bucket is still `ip_limit_max`, and an operator
/// who wants a different shape moves that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RateCost {
  /// A read, a proxied request, a tunnel handshake. The ordinary price, and
  /// what the whole surface used to be charged.
  Cheap,
  /// Something that authenticates a credential, so a wrong answer is a guess
  /// somebody made: login, WebAuthn, TOTP. Priced up because the bucket is
  /// the only thing standing between a stolen password list and this server.
  Guessable,
  /// Something that writes, provisions or reads the whole store: creating a
  /// token, provisioning an ephemeral tunnel, an export. Priced up because it
  /// costs the server far more than a page view, not because it is suspicious.
  Expensive,
}

impl RateCost {
  /// The price, in tokens.
  ///
  /// Deliberately gentle multiples. The bucket was sized when every call cost
  /// one, so multiplying a class by five does not make that class cost five,
  /// it tightens the limit on it fivefold against a ceiling nobody re-chose.
  /// The e2e suite found this the hard way: one address making a couple of
  /// hundred calls, fourteen of them logins, went from comfortable to
  /// throttled. An office of fifty people behind one NAT signing in on a
  /// Monday morning is the same shape, and being ordered and separated is
  /// what this needs to be, not steep.
  pub(crate) fn tokens(self) -> f64 {
    match self {
      RateCost::Cheap => 1.0,
      RateCost::Guessable => 2.0,
      RateCost::Expensive => 5.0,
    }
  }
}

/// RAII slot in the global proxied-request concurrency limit; the slot is
/// released when dropped.
pub(crate) struct RequestSlot(pub(crate) Arc<AtomicUsize>);

impl Drop for RequestSlot {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}

/// RAII slot in the per-visitor streamed-response limit (planned_features
/// #20). Released when the streamed body it was moved into is dropped, which
/// is what makes it a *concurrency* limit rather than a rate limit: a visitor
/// that opens and closes streams as fast as it likes never trips it, and one
/// that holds them open does.
///
/// The counter is removed at zero rather than left at zero, so the map holds
/// only the addresses currently streaming. Without that it would grow one
/// entry per address ever seen, which is a slow leak driven by strangers.
pub(crate) struct StreamSlot {
  pub(crate) ip: std::net::IpAddr,
  pub(crate) counts: Arc<std::sync::Mutex<HashMap<std::net::IpAddr, u32>>>,
}

impl Drop for StreamSlot {
  fn drop(&mut self) {
    let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = counts.get_mut(&self.ip) {
      *n = n.saturating_sub(1);
      if *n == 0 {
        counts.remove(&self.ip);
      }
    }
  }
}

/// RAII slot in the live-WebSocket limit; released when the proxied WebSocket
/// (and this permit, moved into its relay) drops.
pub(crate) struct WsSlot(pub(crate) Arc<AtomicUsize>);

impl Drop for WsSlot {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}
