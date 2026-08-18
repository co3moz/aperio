//! Headers a visitor may not hand to a backend, whichever way the request got
//! there.
//!
//! There is more than one path to a backend now. The client reaches one over
//! HTTP/1 (`proxy/http.rs`) or HTTP/2 (`proxy/h2.rs`), and since
//! `planned_features` `#139` the server reaches one itself
//! (`proxy/forward/server_side.rs`). All of them end at the same HTTP client,
//! and all of them are reachable by headers a visitor chose.
//!
//! **What that costs when the paths disagree is not hypothetical.** The
//! server-side path shipped without these strips and was a way around the
//! defence the relayed path performs: a visitor-supplied
//! `transfer-encoding: chunked` collides with the HTTP client's own body
//! framing, which is an HTTP desync and request-smuggling surface. It was
//! found by reading, days later.
//!
//! So the shared part lives here, and the parts that genuinely differ are
//! declared beside each path rather than folded in. A single flat list would
//! have had to erase the differences to exist, and they are deliberate:
//!
//! - `trailer` is stripped on HTTP/1 and by the server, and not on HTTP/2,
//!   where trailers are a framing concept of the protocol rather than a
//!   header a visitor writes.
//! - `host` is stripped by the server and on HTTP/2. On HTTP/1 it is taken
//!   out of the loop and put back exactly once, only when `pass_hostname` is
//!   set, because adding it in two places produced a duplicate.
//!
//! Each crate asserts its own list against this one, which is rule 25's
//! shape: a strip added on either side must fail on the side that added it,
//! not in a suite that side never runs.

/// Stripped by every path to a backend, no exceptions.
///
/// `sec-websocket-` is a prefix rather than a name and is checked separately;
/// it is in [`strips_the_core`] so a path cannot forget it.
pub const HOP_BY_HOP_CORE: &[&str] = &[
  "connection",
  "keep-alive",
  "upgrade",
  "proxy-connection",
  "accept-encoding",
  "transfer-encoding",
];

/// The prefix every path also strips: a visitor may not negotiate a WebSocket
/// with a backend through a path that is not carrying one.
pub const WEBSOCKET_PREFIX: &str = "sec-websocket-";

/// Does `f` strip everything shared, including the WebSocket family?
///
/// Takes the path's own predicate so each crate can test the code it actually
/// runs rather than a copy of the list. The names it is asked about are spelled
/// in both cases, because a path that lowercases and one that compares
/// case-insensitively both have to answer the same.
pub fn strips_the_core(f: impl Fn(&str) -> bool) -> Result<(), String> {
  for name in HOP_BY_HOP_CORE {
    for spelling in [name.to_string(), name.to_uppercase()] {
      if !f(&spelling) {
        return Err(format!(
          "`{spelling}` reaches the backend; it is in HOP_BY_HOP_CORE because every path has to \
           strip it (see aperio-config/src/hop_by_hop.rs)"
        ));
      }
    }
  }
  for suffix in ["key", "version", "extensions", "protocol", "accept"] {
    let name = format!("{WEBSOCKET_PREFIX}{suffix}");
    if !f(&name) {
      return Err(format!(
        "`{name}` reaches the backend; the whole family is stripped"
      ));
    }
  }
  Ok(())
}

/// Headers that must keep travelling, so a strip cannot quietly widen into a
/// filter that eats ordinary requests.
pub const MUST_TRAVEL: &[&str] = &[
  "authorization",
  "content-type",
  "content-length",
  "accept",
  "user-agent",
  "cookie",
  "x-forwarded-for",
  "x-request-id",
];

/// Does `f` leave the ordinary headers alone?
pub fn leaves_ordinary_headers(f: impl Fn(&str) -> bool) -> Result<(), String> {
  for name in MUST_TRAVEL {
    if f(name) {
      return Err(format!(
        "`{name}` is being stripped, but it is an ordinary header a backend needs"
      ));
    }
  }
  Ok(())
}
