use super::*;
use axum::http::HeaderMap;

fn ip(s: &str) -> IpAddr {
  s.parse().unwrap()
}

// --- token extraction -------------------------------------------------------

#[test]
fn extract_token_from_bearer_and_x_auth() {
  let mut bearer = HeaderMap::new();
  bearer.insert("authorization", "Bearer secret123".parse().unwrap());
  assert_eq!(extract_token(&bearer), Some("secret123".to_string()));

  let mut xauth = HeaderMap::new();
  xauth.insert("x-auth-token", "tok".parse().unwrap());
  assert_eq!(extract_token(&xauth), Some("tok".to_string()));

  // Non-Bearer authorization schemes are ignored (no x-auth-token fallback hit).
  let mut basic = HeaderMap::new();
  basic.insert("authorization", "Basic abc".parse().unwrap());
  assert_eq!(extract_token(&basic), None);

  assert_eq!(extract_token(&HeaderMap::new()), None);
}

#[test]
fn extract_and_verify_token_matches_constant_time() {
  let mut h = HeaderMap::new();
  h.insert("authorization", "Bearer right".parse().unwrap());
  assert!(extract_and_verify_token(&h, "right"));
  assert!(!extract_and_verify_token(&h, "wrong"));
  assert!(!extract_and_verify_token(&HeaderMap::new(), "right"));
}

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

// --- safe_redirect_path -----------------------------------------------------

#[test]
fn safe_redirect_path_blocks_open_redirects() {
  assert_eq!(safe_redirect_path("/dashboard"), "/dashboard");
  assert_eq!(safe_redirect_path("/a/b?c=d"), "/a/b?c=d");
  // Protocol-relative and backslash bypasses collapse to root.
  assert_eq!(safe_redirect_path("//evil.com"), "/");
  assert_eq!(safe_redirect_path("/\\evil.com"), "/");
  assert_eq!(safe_redirect_path("https://evil.com"), "/");
  assert_eq!(safe_redirect_path("relative"), "/");
}
