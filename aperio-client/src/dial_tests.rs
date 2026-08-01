//! Tests for dialling: address family selection and the connect strategy.

use super::*;

#[test]
fn parse_maps_known_families_and_defaults_to_auto() {
  assert_eq!(IpFamily::parse(Some("ipv4")), IpFamily::V4);
  assert_eq!(IpFamily::parse(Some(" V4 ")), IpFamily::V4);
  assert_eq!(IpFamily::parse(Some("4")), IpFamily::V4);
  assert_eq!(IpFamily::parse(Some("ipv6")), IpFamily::V6);
  assert_eq!(IpFamily::parse(Some("6")), IpFamily::V6);
  assert_eq!(IpFamily::parse(Some("auto")), IpFamily::Auto);
  assert_eq!(IpFamily::parse(Some("nonsense")), IpFamily::Auto);
  assert_eq!(IpFamily::parse(Some("")), IpFamily::Auto);
  assert_eq!(IpFamily::parse(None), IpFamily::Auto);
}

fn v4(n: u8) -> SocketAddr {
  format!("10.0.0.{n}:443").parse().unwrap()
}
fn v6(n: u8) -> SocketAddr {
  format!("[::{n}]:443").parse().unwrap()
}

#[test]
fn interleave_starts_with_ipv4_and_alternates() {
  let out = interleave(vec![v4(1), v4(2)], vec![v6(1), v6(2)]);
  assert_eq!(out, vec![v4(1), v6(1), v4(2), v6(2)]);
}

#[test]
fn interleave_appends_the_longer_families_remainder() {
  let out = interleave(vec![v4(1)], vec![v6(1), v6(2), v6(3)]);
  assert_eq!(out, vec![v4(1), v6(1), v6(2), v6(3)]);
  let out = interleave(vec![v4(1), v4(2), v4(3)], vec![v6(1)]);
  assert_eq!(out, vec![v4(1), v6(1), v4(2), v4(3)]);
}

#[tokio::test]
async fn resolve_ordered_filters_by_family() {
  // Literal addresses resolve without DNS; the target only needs to name
  // both families. lookup_host on an IP echoes it, so we exercise ordering
  // by resolving a hostname is avoided, use loopback-style literals.
  let only_v4 = resolve_ordered("127.0.0.1", 443, IpFamily::V4)
    .await
    .unwrap();
  assert!(only_v4.iter().all(|a| a.is_ipv4()));

  // Asking for a family the target cannot provide is an error, not a hang.
  let none_v6 = resolve_ordered("127.0.0.1", 443, IpFamily::V6).await;
  assert!(none_v6.is_err());
}
