//! That a target and a visitor's path join into the URL the operator expects.
//!
//! The join is small and is the one place a path could change *which host* is
//! reached, which is what makes it worth pinning: `server_side_targets:` was
//! checked against the target, so anything the visitor supplies may only ever
//! be appended to it.

use super::*;

fn join(target: &str, pq: &str) -> String {
  join_target(target, pq).expect("joins").to_string()
}

#[test]
fn a_bare_host_port_target_is_assumed_to_be_http() {
  assert_eq!(join("127.0.0.1:8080", "/api"), "http://127.0.0.1:8080/api");
}

#[test]
fn a_scheme_the_operator_wrote_is_kept() {
  assert_eq!(
    join("https://internal.example.com", "/api"),
    "https://internal.example.com/api"
  );
}

#[test]
fn a_query_string_survives_the_join() {
  assert_eq!(
    join("127.0.0.1:8080", "/search?q=1&n=2"),
    "http://127.0.0.1:8080/search?q=1&n=2"
  );
}

/// A target with its own path prefix keeps it, and the visitor's path is
/// appended rather than replacing it.
#[test]
fn a_target_with_a_path_prefix_keeps_it() {
  assert_eq!(
    join("http://10.0.0.5:9000/service", "/v1/items"),
    "http://10.0.0.5:9000/service/v1/items"
  );
}

/// The visitor cannot move the request to another host.
///
/// This is the property the allowlist depends on: the check runs against the
/// target, so if a path could redirect the host, the check would be answering
/// about somewhere the request does not go. `url` normalises the traversal
/// rather than letting it climb out, and an absolute-looking path stays a
/// path.
#[test]
fn nothing_a_visitor_sends_can_change_the_host() {
  for pq in [
    "/../../etc/passwd",
    "//evil.example.com/x",
    "/..//..//x",
    "/%2e%2e/%2e%2e/x",
  ] {
    let joined = join("http://10.0.0.5:9000", pq);
    let host = reqwest::Url::parse(&joined)
      .unwrap()
      .host_str()
      .unwrap()
      .to_string();
    assert_eq!(
      host, "10.0.0.5",
      "path {pq:?} moved the request to {host}, which the allowlist never approved"
    );
  }
}
