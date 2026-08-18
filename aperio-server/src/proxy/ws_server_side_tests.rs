//! That the socket URL is built from the target and never from the visitor.
//!
//! The same property the HTTP half is pinned on, and for the same reason:
//! `server_side_targets:` was checked against the target, so if anything a
//! visitor sends could move the host, the check would have been answering
//! about somewhere the socket does not go.

use super::*;

fn u(target: &str, pq: &str) -> String {
  socket_url(target, pq).expect("builds").to_string()
}

#[test]
fn a_plain_target_gets_a_plain_socket() {
  assert_eq!(u("127.0.0.1:8080", "/live"), "ws://127.0.0.1:8080/live");
  assert_eq!(
    u("http://10.0.0.5:9000", "/live"),
    "ws://10.0.0.5:9000/live"
  );
}

/// The scheme follows the target's, so a target the operator wrote as https
/// is not silently downgraded to a plaintext socket.
#[test]
fn an_https_target_gets_a_secure_socket() {
  assert_eq!(
    u("https://internal.example.com", "/live"),
    "wss://internal.example.com/live"
  );
}

#[test]
fn the_query_and_a_target_path_prefix_both_survive() {
  assert_eq!(
    u("127.0.0.1:8080", "/live?room=1"),
    "ws://127.0.0.1:8080/live?room=1"
  );
  assert_eq!(
    u("http://10.0.0.5:9000/svc", "/live"),
    "ws://10.0.0.5:9000/svc/live"
  );
}

/// Nothing a visitor puts in the path can move the socket to another host.
#[test]
fn nothing_a_visitor_sends_can_change_the_host() {
  for pq in ["/../../x", "//evil.example.com/x", "/%2e%2e/%2e%2e/x"] {
    let joined = u("http://10.0.0.5:9000", pq);
    let host = url::Url::parse(&joined)
      .unwrap()
      .host_str()
      .unwrap()
      .to_string();
    assert_eq!(
      host, "10.0.0.5",
      "path {pq:?} moved the socket to {host}, which the allowlist never approved"
    );
  }
}
