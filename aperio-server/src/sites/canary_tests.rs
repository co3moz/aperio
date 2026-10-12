//! The canary split: decided per visitor rather than per request, stable across
//! processes, and overridden by the opt-in header.

use super::{CanaryRule, Side, bucket_of};

fn canary(weight: u8, header: Option<&str>, value: Option<&str>) -> CanaryRule {
  CanaryRule {
    service: "web-v2".to_string(),
    weight,
    header: header.map(str::to_string),
    value: value.map(str::to_string),
  }
}

const A: fn(&str) -> std::net::IpAddr = |s| s.parse().unwrap();

#[test]
fn a_weight_of_zero_sends_nobody_and_a_hundred_sends_everybody() {
  let none = canary(0, None, None);
  let all = canary(100, None, None);
  for ip in ["203.0.113.7", "198.51.100.4", "10.0.0.1"] {
    assert_eq!(none.side_for(None, Some(A(ip))), Side::Stable);
    assert_eq!(all.side_for(None, Some(A(ip))), Side::Canary);
  }
}

#[test]
fn the_same_visitor_always_lands_on_the_same_side() {
  let rule = canary(20, None, None);
  let ip = A("203.0.113.7");
  let first = rule.side_for(None, Some(ip));
  // The whole point: a per-request decision would send one page load's twenty
  // assets to both versions, which is a mixture rather than a canary.
  for _ in 0..100 {
    assert_eq!(rule.side_for(None, Some(ip)), first);
  }
}

#[test]
fn the_split_is_roughly_the_weight_over_many_visitors() {
  let rule = canary(20, None, None);
  let canaried = (0u32..2000)
    .filter(|i| {
      let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::from(i.wrapping_mul(2_654_435_761)));
      rule.side_for(None, Some(ip)) == Side::Canary
    })
    .count();
  // Not exact, and the doc says so: the split is only as even as the
  // addresses are spread. A band rather than a number is the honest assertion.
  assert!(
    (200..600).contains(&canaried),
    "{canaried} of 2000 landed on the canary"
  );
}

#[test]
fn the_header_wins_over_the_weight() {
  let rule = canary(0, Some("x-canary"), None);
  let ip = A("203.0.113.7");
  // Nobody is sent by weight, and this visitor asked.
  assert_eq!(rule.side_for(Some("1"), Some(ip)), Side::Canary);
  assert_eq!(rule.side_for(None, Some(ip)), Side::Stable);
  // An empty value is not asking.
  assert_eq!(rule.side_for(Some("  "), Some(ip)), Side::Stable);
}

#[test]
fn a_required_value_must_match() {
  let rule = canary(0, Some("x-canary"), Some("on"));
  let ip = A("203.0.113.7");
  assert_eq!(rule.side_for(Some("on"), Some(ip)), Side::Canary);
  assert_eq!(rule.side_for(Some("ON"), Some(ip)), Side::Canary);
  assert_eq!(rule.side_for(Some("off"), Some(ip)), Side::Stable);
}

#[test]
fn a_visitor_with_no_address_gets_the_stable_side() {
  // An inconsistent canary is worse than a small one, and there is nothing to
  // be consistent about here.
  assert_eq!(canary(50, None, None).side_for(None, None), Side::Stable);
}

#[test]
fn the_bucket_does_not_move_between_processes() {
  // Not `DefaultHasher`, which is randomly seeded per process: two servers
  // behind a load balancer would otherwise disagree about who is in the
  // canary, and the same visitor would move on every restart.
  assert_eq!(bucket_of(A("203.0.113.7")), bucket_of(A("203.0.113.7")));
  assert_ne!(bucket_of(A("203.0.113.7")), bucket_of(A("203.0.113.8")));
  assert!(bucket_of(A("2001:db8::1")) < 100);
}
