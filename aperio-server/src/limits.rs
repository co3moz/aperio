//! Which limit refused a request, and where the operator changes it.
//!
//! Six different things answer `429`, and to the visitor they were
//! indistinguishable: a status code and a sentence. Under a load test that is
//! the wrong kind of mystery, the number that needs raising is one of six, and
//! finding out meant reading the server's log next to the run. Every refusal
//! now names itself in a header, and the name is the setting.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::atomic::{AtomicU64, Ordering};

/// The header every refusal carries. `curl -i` answers the question that used
/// to need a log: `x-aperio-limit: ip; setting=ip_limit_max`.
pub(crate) const LIMIT_HEADER: &str = "x-aperio-limit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
  /// Per-visitor token bucket, keyed on the caller's IP.
  Ip,
  /// The server's own in-flight ceiling, across every service.
  ServerConcurrency,
  /// A `rate_limits:` rule matching this hostname and path prefix.
  Route,
  /// The chosen client announced a `max_concurrent` and nothing freed up
  /// before the gateway timeout.
  ClientConcurrency,
  /// The dynamic token's own requests-per-second ceiling.
  TokenRate,
  /// The dynamic token's daily byte quota.
  TokenQuota,
  /// The organization's monthly byte quota.
  OrgQuota,
  /// Streamed responses this visitor IP already has open.
  StreamsPerIp,
}

impl Limit {
  /// Stable machine-readable kind. Scripts and dashboards key on this.
  pub(crate) fn kind(self) -> &'static str {
    match self {
      Limit::Ip => "ip",
      Limit::ServerConcurrency => "server-concurrency",
      Limit::Route => "route",
      Limit::ClientConcurrency => "client-concurrency",
      Limit::TokenRate => "token-rate",
      Limit::TokenQuota => "token-quota",
      Limit::OrgQuota => "org-quota",
      Limit::StreamsPerIp => "streams-per-ip",
    }
  }

  /// Where the number lives. A yaml key for the server's own settings, and
  /// for the rest the thing that carries it, since a token's rate limit is a
  /// property of that token rather than of the file.
  pub(crate) fn setting(self) -> &'static str {
    match self {
      Limit::Ip => "ip_limit_max",
      Limit::ServerConcurrency => "max_concurrent_requests",
      Limit::Route => "rate_limits",
      Limit::ClientConcurrency => "max_concurrent",
      Limit::TokenRate => "token.max_rps",
      Limit::TokenQuota => "token.daily_max_bytes",
      Limit::OrgQuota => "organization.monthly_max_bytes",
      Limit::StreamsPerIp => "max_streams_per_ip",
    }
  }

  /// One sentence for the person reading the body, naming the setting again
  /// because a body is what a browser shows and a header is not.
  pub(crate) fn explain(self) -> &'static str {
    match self {
      Limit::Ip => {
        "per-visitor IP rate limit (ip_limit_max / ip_limit_refill, env APERIO_IP_LIMIT_MAX / APERIO_IP_LIMIT_REFILL)"
      }
      Limit::ServerConcurrency => {
        "server-wide in-flight request limit (max_concurrent_requests, env APERIO_MAX_CONCURRENT_REQUESTS)"
      }
      Limit::Route => "per-route rate limit (a rate_limits: rule matched this hostname and path)",
      Limit::StreamsPerIp => {
        "the per-visitor limit on concurrently open streamed responses (max_streams_per_ip, env APERIO_MAX_STREAMS_PER_IP)"
      }
      Limit::ClientConcurrency => {
        "the serving client's own concurrency limit (max_concurrent in its aperio.yaml); no slot freed before the gateway timeout"
      }
      Limit::TokenRate => "the access token's requests-per-second limit (max_rps on the token)",
      Limit::TokenQuota => "the access token's daily byte quota (daily_max_bytes on the token)",
      Limit::OrgQuota => "the organization's monthly byte quota",
    }
  }

  /// Seconds to wait, for the limits that refill. A quota measured in days or
  /// months has no honest number to give, so it gets none: `Retry-After: 1` on
  /// a monthly quota would be a lie a client would act on.
  fn retry_after(self) -> Option<u32> {
    match self {
      Limit::Ip | Limit::ServerConcurrency | Limit::Route | Limit::ClientConcurrency => Some(1),
      Limit::TokenRate => Some(1),
      // A stream slot frees when a stream ends, which is not on a schedule,
      // so a second is the honest "try again shortly" rather than a promise.
      Limit::StreamsPerIp => Some(1),
      Limit::TokenQuota | Limit::OrgQuota => None,
    }
  }

  /// What the access log and the dashboard's traffic table show.
  pub(crate) fn log_detail(self) -> String {
    format!("429 {}: {}", self.kind(), self.explain())
  }
}

/// Every kind, in one place, so a counter can iterate them and a test can
/// assert none was forgotten.
pub(crate) const ALL_LIMITS: [Limit; 8] = [
  Limit::Ip,
  Limit::ServerConcurrency,
  Limit::Route,
  Limit::ClientConcurrency,
  Limit::TokenRate,
  Limit::TokenQuota,
  Limit::OrgQuota,
  Limit::StreamsPerIp,
];

/// Refusals since start, one counter per kind.
///
/// A header answers "why was *this* request refused". Under a load test the
/// question is "which limit is firing, and how often", and nobody reads
/// headers at ten thousand requests a second. Lock-free and unbounded in
/// time, since the set of kinds is fixed at compile time.
#[derive(Default, Debug)]
pub(crate) struct LimitCounters {
  counts: [AtomicU64; ALL_LIMITS.len()],
}

impl LimitCounters {
  fn index(limit: Limit) -> usize {
    ALL_LIMITS.iter().position(|l| *l == limit).unwrap_or(0)
  }

  pub(crate) fn record(&self, limit: Limit) {
    self.counts[Self::index(limit)].fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn get(&self, limit: Limit) -> u64 {
    self.counts[Self::index(limit)].load(Ordering::Relaxed)
  }
}

/// Refuses a request with the limit that caused it, counting it on the way
/// out. Everything that answers 429 goes through here: the counter behind
/// `aperio_rate_limited_total` is only trustworthy if no path can skip it,
/// and one did until a review of this change found it, the visitor-facing
/// WebSocket upgrade, which answered a bare 429 while its HTTP sibling
/// explained itself.
pub(crate) fn refuse(state: &crate::state::AppState, limit: Limit) -> Response {
  state.limit_counters.record(limit);
  too_many_requests(limit)
}

/// The refusal itself: the status, the header naming the limit, a body that
/// repeats it in prose, and `Retry-After` where there is a truthful one.
pub(crate) fn too_many_requests(limit: Limit) -> Response {
  let mut response = (
    StatusCode::TOO_MANY_REQUESTS,
    format!("429 Too Many Requests - {}\n", limit.explain()),
  )
    .into_response();

  let headers = response.headers_mut();
  if let Ok(value) =
    HeaderValue::from_str(&format!("{}; setting={}", limit.kind(), limit.setting()))
  {
    headers.insert(LIMIT_HEADER, value);
  }
  if let Some(seconds) = limit.retry_after() {
    headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
  }
  response
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
