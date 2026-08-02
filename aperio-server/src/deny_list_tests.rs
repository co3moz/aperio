//! What the source-IP deny list must get right: which addresses a range
//! covers, that an unconfigured list is inert, and that a malformed entry is
//! reported rather than silently applied in part, since a partial block list
//! leaves somebody reachable while the operator believes otherwise.

use super::*;

fn list(raw: &str) -> DenyList {
  DenyList::parse(raw).expect("valid entries")
}

#[test]
fn a_bare_address_blocks_exactly_itself() {
  let deny = list("203.0.113.7");
  assert!(deny.blocks("203.0.113.7".parse().unwrap()));
  assert!(!deny.blocks("203.0.113.8".parse().unwrap()));
  assert_eq!(deny.len(), 1);
}

#[test]
fn a_cidr_blocks_its_whole_range() {
  let deny = list("10.0.0.0/8, 192.168.1.0/24");
  assert!(deny.blocks("10.9.9.9".parse().unwrap()));
  assert!(deny.blocks("192.168.1.255".parse().unwrap()));
  assert!(!deny.blocks("192.168.2.1".parse().unwrap()));
  assert!(!deny.blocks("11.0.0.1".parse().unwrap()));
}

#[test]
fn ipv6_ranges_work_and_do_not_cross_families() {
  let deny = list("2001:db8::/32");
  assert!(deny.blocks("2001:db8::1".parse().unwrap()));
  assert!(!deny.blocks("2001:db9::1".parse().unwrap()));
  // A v4 address is not inside a v6 range, and vice versa.
  assert!(!deny.blocks("10.0.0.1".parse().unwrap()));
}

#[test]
fn an_empty_list_blocks_nothing_and_reports_itself_empty() {
  let deny = DenyList::default();
  assert!(deny.is_empty());
  assert!(!deny.blocks("203.0.113.7".parse().unwrap()));
}

#[test]
fn a_malformed_entry_is_an_error_rather_than_a_partial_list() {
  // The whole list is refused: applying the valid half would leave the
  // operator believing an address is blocked when it is not.
  assert!(DenyList::parse("203.0.113.7, not-an-ip").is_err());
  assert!(DenyList::parse("10.0.0.0/99").is_err());
}
