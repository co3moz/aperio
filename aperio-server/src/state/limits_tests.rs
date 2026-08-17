//! What a request is charged against: the latency histogram's bucket
//! boundaries, and that the differentiated rate budgets charge each kind of
//! work what it actually costs.

use super::*;
use crate::state::*;

// ----- DurationHistogram -----

#[test]
fn test_duration_histogram_observe_and_render() {
  let h = DurationHistogram::default();
  h.observe(Duration::from_millis(3)); // <= 0.005 → every bucket
  h.observe(Duration::from_millis(300)); // between 0.25 and 0.5
  h.observe(Duration::from_secs(60)); // beyond the last finite bound (+Inf only)

  let mut out = String::new();
  h.render(&mut out);
  assert!(out.contains("# TYPE aperio_request_duration_seconds histogram"));
  // The 3ms sample lands in the smallest (0.005) bucket.
  assert!(out.contains("le=\"0.005\"} 1"), "{out}");
  // All three samples fall under +Inf.
  assert!(out.contains("le=\"+Inf\"} 3"), "{out}");
  assert!(
    out.contains("aperio_request_duration_seconds_count 3"),
    "{out}"
  );
  // Sum reflects the observed micros (~60.303s).
  assert!(
    out.contains("aperio_request_duration_seconds_sum "),
    "{out}"
  );
}

// Differentiated rate budgets (planned_features #64)
// ---------------------------------------------------------------------------

/// A state whose bucket holds exactly `tokens` and never refills, so what a
/// test measures is the price and not the clock.
fn state_with_budget(tokens: f64) -> AppState {
  let mut config = crate::test_support::test_config();
  config.ip_limit_max = tokens;
  config.ip_limit_refill = 0.0;
  crate::test_support::test_state_with(config)
}

#[test]
fn the_three_prices_are_ordered_and_separated() {
  // This, not the specific numbers, is what the design claims. The magnitudes
  // are a judgement about how much pressure a shared bucket can take, and
  // they have moved once already.
  assert!(RateCost::Cheap.tokens() < RateCost::Guessable.tokens());
  assert!(RateCost::Guessable.tokens() < RateCost::Expensive.tokens());
}

#[tokio::test]
async fn a_credential_attempt_costs_more_than_a_read() {
  let budget = 20.0;
  let state = state_with_budget(budget);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();

  let reads = (budget / RateCost::Cheap.tokens()) as usize;
  let guesses = (budget / RateCost::Guessable.tokens()) as usize;
  assert!(guesses < reads, "a login has to be dearer than a page view");

  for i in 0..guesses {
    assert!(
      state.check_rate_limit_cost(ip, RateCost::Guessable).await,
      "attempt {i} fits"
    );
  }
  assert!(
    !state.check_rate_limit_cost(ip, RateCost::Guessable).await,
    "and the next does not"
  );
}

#[tokio::test]
async fn the_budget_is_shared_between_the_classes() {
  let state = state_with_budget(RateCost::Guessable.tokens() * 2.0);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
  // Two login attempts empty a bucket that holds exactly two of them, and
  // nothing is left for a read. One bucket at different prices, not a bucket
  // per class: separate buckets would let an attacker spend a full allowance
  // on each.
  assert!(state.check_rate_limit_cost(ip, RateCost::Guessable).await);
  assert!(state.check_rate_limit_cost(ip, RateCost::Guessable).await);
  assert!(!state.check_rate_limit(ip).await, "nothing is left");
}

#[tokio::test]
async fn a_refused_call_is_not_charged() {
  // Just under the price of the expensive call, so it is refused.
  let state = state_with_budget(RateCost::Expensive.tokens() - 0.5);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
  assert!(!state.check_rate_limit_cost(ip, RateCost::Expensive).await);
  // A call that was turned away has not been served, so it should not have
  // been paid for either: the cheap budget is untouched.
  assert!(state.check_rate_limit(ip).await);
}

// ---------------------------------------------------------------------------
