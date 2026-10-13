//! Which peers a public listener's `allowed_ips` covers: a range or one
//! address in either family, and an IPv4 peer that a `::` listener reports
//! in its IPv4-mapped IPv6 form, which is the same peer.

use super::*;

fn ip(s: &str) -> IpAddr {
  s.parse().unwrap()
}

#[test]
fn a_network_covers_its_addresses_and_no_others() {
  assert!(network_contains("10.0.0.0/8", ip("10.1.2.3")));
  assert!(!network_contains("10.0.0.0/8", ip("11.0.0.1")));
  assert!(network_contains("192.168.1.5", ip("192.168.1.5")));
  assert!(!network_contains("192.168.1.5", ip("192.168.1.6")));
  assert!(network_contains("2001:db8::/32", ip("2001:db8::1")));
  assert!(!network_contains("2001:db8::/32", ip("2001:dead::1")));
  assert!(!network_contains("not a network", ip("10.0.0.1")));
}

#[test]
fn an_ipv4_mapped_peer_is_the_ipv4_peer() {
  // A listener on `::` reports an IPv4 peer as `::ffff:a.b.c.d`; an IPv4
  // range has to cover it, or a fenced listener refuses everyone it was
  // written to admit.
  assert!(network_contains("10.0.0.0/8", ip("::ffff:10.1.2.3")));
  assert!(!network_contains("10.0.0.0/8", ip("::ffff:11.0.0.1")));
  assert!(network_contains("192.168.1.5", ip("::ffff:192.168.1.5")));
  // A range written in the mapped form covers the plain peer.
  assert!(network_contains("::ffff:10.0.0.0/104", ip("10.1.2.3")));
}
