//! Address allowlists: exact entries, CIDR ranges in both families, the
//! wildcard, and the entries refused as unparseable rather than matched
//! loosely.

use super::super::tests::*;
use super::*;
use crate::auth::{ip_allowed, valid_ip_entry};
use std::net::IpAddr;

// --- ip_allowed / cidr ------------------------------------------------------

#[test]
fn ip_allowed_empty_and_wildcards() {
  assert!(ip_allowed(ip("1.2.3.4"), &[]));
  for w in ["*", "0.0.0.0/0", "::/0", "0.0.0.0"] {
    assert!(ip_allowed(ip("9.9.9.9"), &[w.to_string()]));
  }
}

#[test]
fn ip_allowed_exact_and_cidr() {
  let list = vec!["10.0.0.0/8".to_string(), "192.168.1.5".to_string()];
  assert!(ip_allowed(ip("10.1.2.3"), &list)); // inside /8
  assert!(ip_allowed(ip("192.168.1.5"), &list)); // exact
  assert!(!ip_allowed(ip("192.168.1.6"), &list)); // no match
  assert!(!ip_allowed(ip("11.0.0.1"), &list)); // outside /8
}

#[test]
fn ip_allowed_ipv6_cidr_and_family_mismatch() {
  let list = vec!["2001:db8::/32".to_string()];
  assert!(ip_allowed(ip("2001:db8::1"), &list));
  assert!(!ip_allowed(ip("2001:dead::1"), &list));
  // A v4 address never matches a v6 CIDR.
  assert!(!ip_allowed(ip("10.0.0.1"), &list));
}

#[test]
fn ip_allowed_rejects_malformed_entries() {
  assert!(!ip_allowed(ip("1.2.3.4"), &["not-an-ip".to_string()]));
  assert!(!ip_allowed(ip("1.2.3.4"), &["1.2.3.4/notnum".to_string()]));
  // Prefix out of range → the entry never matches.
  assert!(!ip_allowed(ip("1.2.3.4"), &["1.2.3.4/40".to_string()]));
}

// --- valid_ip_entry ---------------------------------------------------------

#[test]
fn valid_ip_entry_accepts_and_rejects() {
  for good in ["*", "1.2.3.4", "10.0.0.0/8", "2001:db8::/32", "::1"] {
    assert!(valid_ip_entry(good), "{good} should be valid");
  }
  for bad in ["garbage", "1.2.3.4/33", "2001:db8::/129", "1.2.3.4/x", ""] {
    assert!(!valid_ip_entry(bad), "{bad} should be invalid");
  }
}

// --- constant_time_eq_str ---------------------------------------------------

#[test]
fn constant_time_eq_str_semantics() {
  assert!(constant_time_eq_str("hunter2", "hunter2"));
  assert!(!constant_time_eq_str("hunter2", "hunter3"));
  // Length differences are handled (both sides are hashed first).
  assert!(!constant_time_eq_str("short", "a-much-longer-secret"));
  assert!(constant_time_eq_str("", ""));
}

#[test]
pub(crate) fn test_ip_allowed() {
  let ip = |s: &str| s.parse::<IpAddr>().unwrap();

  // Empty list or wildcards allow everything
  assert!(ip_allowed(ip("1.2.3.4"), &[]));
  assert!(ip_allowed(ip("1.2.3.4"), &["*".to_string()]));
  assert!(ip_allowed(ip("1.2.3.4"), &["0.0.0.0/0".to_string()]));
  assert!(ip_allowed(ip("::1"), &["::/0".to_string()]));

  // Exact IP match
  assert!(ip_allowed(ip("1.2.3.4"), &["1.2.3.4".to_string()]));
  assert!(!ip_allowed(ip("1.2.3.5"), &["1.2.3.4".to_string()]));

  // CIDR ranges
  assert!(ip_allowed(ip("10.1.2.3"), &["10.0.0.0/8".to_string()]));
  assert!(!ip_allowed(ip("11.1.2.3"), &["10.0.0.0/8".to_string()]));
  assert!(ip_allowed(
    ip("192.168.1.77"),
    &["192.168.1.0/24".to_string()]
  ));
  assert!(!ip_allowed(
    ip("192.168.2.77"),
    &["192.168.1.0/24".to_string()]
  ));

  // Multiple entries: any match wins
  assert!(ip_allowed(
    ip("203.0.113.9"),
    &["10.0.0.0/8".to_string(), "203.0.113.0/24".to_string()]
  ));

  // IPv6 CIDR
  assert!(ip_allowed(ip("fd00::1"), &["fd00::/8".to_string()]));
  assert!(!ip_allowed(ip("2001:db8::1"), &["fd00::/8".to_string()]));
  // Family mismatch never matches
  assert!(!ip_allowed(ip("1.2.3.4"), &["fd00::/8".to_string()]));

  // Malformed entries are ignored (do not match)
  assert!(!ip_allowed(ip("1.2.3.4"), &["not-an-ip".to_string()]));

  // Validation helper
  assert!(valid_ip_entry("10.0.0.0/8"));
  assert!(valid_ip_entry("1.2.3.4"));
  assert!(valid_ip_entry("::1"));
  assert!(valid_ip_entry("*"));
  assert!(!valid_ip_entry("10.0.0.0/33"));
  assert!(!valid_ip_entry("banana"));
}
