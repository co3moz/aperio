//! One firewall rule: what it matches on (path, method, header, body over a
//! limit), that a non-UTF-8 header value cannot match a regex, and that a rule
//! which cannot work is refused by name rather than dropped.

use super::*;

fn rule(yaml: &str) -> WafRule {
  let raw: aperio_config::WafRule = serde_yaml::from_str(yaml).unwrap();
  WafRule::compile(&raw, "waf[0]").unwrap()
}

fn refused(yaml: &str) -> String {
  let raw: aperio_config::WafRule = serde_yaml::from_str(yaml).unwrap();
  WafRule::compile(&raw, "waf[0]").expect_err("refused")
}

#[test]
fn deny_matches_path_and_method() {
  let r = rule("path: \"^/admin\"\nmethods: [POST]\n");
  let h = HeaderMap::new();
  assert!(r.denies("POST", "/admin/x", &h));
  assert!(!r.denies("GET", "/admin/x", &h), "wrong method");
  assert!(!r.denies("POST", "/public", &h), "wrong path");
}

#[test]
fn deny_matches_a_header_pattern() {
  let r = rule("header:\n  name: User-Agent\n  regex: \"(?i)sqlmap\"\n");
  let mut h = HeaderMap::new();
  assert!(!r.denies("GET", "/", &h), "no header, no match");
  h.insert("user-agent", "curl/8".parse().unwrap());
  assert!(!r.denies("GET", "/", &h));
  h.insert("user-agent", "sqlMAP/1.0".parse().unwrap());
  assert!(r.denies("GET", "/", &h));
  assert!(r.desc.contains("header=user-agent"), "{}", r.desc);
}

#[test]
fn non_utf8_header_value_does_not_match() {
  let r = rule("header:\n  name: x-test\n  regex: \".\"\n");
  let mut h = HeaderMap::new();
  h.insert(
    "x-test",
    axum::http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
  );
  assert!(!r.denies("GET", "/", &h));
}

#[test]
fn a_body_rule_only_trips_over_its_limit() {
  let r = rule("path: \"^/upload\"\nmax_body: 100\n");
  let h = HeaderMap::new();
  assert!(
    !r.denies("POST", "/upload", &h),
    "a size rule is not a deny rule"
  );
  assert!(!r.refuses_body("POST", "/upload", &h, 50));
  assert!(r.refuses_body("POST", "/upload", &h, 500));
  assert!(!r.refuses_body("POST", "/other", &h, 500));
  assert!(r.desc.contains("max_body=100"), "{}", r.desc);
}

#[test]
fn a_size_is_written_with_units() {
  let r = rule("max_body: 1KB\n");
  assert_eq!(r.max_body, Some(1024));
}

#[test]
fn methods_are_uppercased() {
  let r = rule("methods: [post, delete]\n");
  let h = HeaderMap::new();
  assert!(r.denies("POST", "/x", &h));
  assert!(r.denies("DELETE", "/x", &h));
  assert!(!r.denies("GET", "/x", &h));
}

#[test]
fn a_rule_that_cannot_work_is_refused_by_name() {
  // Dropping it would leave the operator believing a deny rule is up.
  assert!(refused("path: \"(unclosed\"\n").contains("waf[0].path"));
  assert!(refused("header:\n  name: x\n  regex: \"(bad\"\n").contains("header.regex"));
  assert!(refused("{}\n").contains("sets no condition"));
  assert!(refused("methods: []\n").contains("would match nothing"));
}
