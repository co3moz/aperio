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

// ----- what a visitor may not hand to the target -----

/// The framing headers are stripped, exactly as the relayed path strips them.
///
/// This is not tidiness. `aperio-client`'s `proxy/http.rs` drops these with a
/// CRITICAL comment naming what they cost: a visitor-supplied
/// `transfer-encoding: chunked` collides with reqwest's own body framing and
/// opens an HTTP desync and request-smuggling surface. Both paths end at
/// reqwest, so a strip that exists on one and not the other means serving
/// from the server is a way around it.
#[test]
fn the_framing_headers_a_visitor_sends_do_not_reach_the_target() {
  for h in [
    "transfer-encoding",
    "Transfer-Encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "proxy-connection",
    "trailer",
    "accept-encoding",
    "sec-websocket-key",
    "Sec-WebSocket-Version",
  ] {
    assert!(is_hop_by_hop(h), "{h} must not be forwarded");
  }
}

/// The visitor does not get to choose which virtual host is asked for.
///
/// `server_side_targets:` was checked against the target, and reqwest lets an
/// explicit `Host` override the authority in the URL. Forwarding the
/// visitor's would mean the connection goes where the operator allowed while
/// the name it asks for is the visitor's, which is a different server on the
/// same address.
#[test]
fn a_visitors_host_header_cannot_repoint_the_request() {
  assert!(is_hop_by_hop("host"));
  assert!(is_hop_by_hop("Host"));
}

/// Everything else still travels: the strip is a named list, not a filter that
/// quietly eats ordinary headers.
#[test]
fn ordinary_headers_still_reach_the_target() {
  for h in [
    "authorization",
    "content-type",
    "content-length",
    "accept",
    "user-agent",
    "x-request-id",
    "cookie",
    "x-forwarded-for",
  ] {
    assert!(!is_hop_by_hop(h), "{h} should reach the target");
  }
}

/// This path strips everything every path strips.
///
/// The list lives in `aperio-config` and each crate asserts its own predicate
/// against it, which is rule 25 across a crate boundary: a header added to the
/// shared list has to fail here, in the suite a server change runs, rather
/// than in one nobody runs after editing this file. Shipping without this is
/// how the strip went missing in the first place.
#[test]
fn this_path_strips_everything_every_path_strips() {
  aperio_config::hop_by_hop::strips_the_core(is_hop_by_hop).expect("the shared strip");
  aperio_config::hop_by_hop::leaves_ordinary_headers(is_hop_by_hop).expect("ordinary headers");
}
