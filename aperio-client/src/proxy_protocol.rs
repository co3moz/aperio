//! Writing a PROXY protocol v2 header to a tunnelled backend
//! (planned_features #44).
//!
//! A TCP tunnel delivers bytes to the backend over a fresh local connection,
//! so the backend sees a connection from the client process, `127.0.0.1`, and
//! the visitor's real address is lost at the last hop. Every postgres log line
//! says localhost. The server knows the address; nothing carried it the last
//! few metres.
//!
//! PROXY protocol is how that gap is normally closed: a short header, written
//! before any payload byte, that says "this connection is really from here".
//! nginx (`listen ... proxy_protocol`), HAProxy and MySQL read it; postgres
//! and redis do not, which is exactly why this is opt-in per tunnel rather
//! than something the client does on its own. A backend that is not expecting
//! the header will read it as protocol garbage and drop the connection, so
//! turning it on is a statement about the backend, not a preference.
//!
//! v2 (binary) rather than v1 (text): it is what current receivers implement,
//! its 12-byte signature cannot be confused with a payload that happens to
//! start with "PROXY", and it is fixed-width to encode.
//!
//! ## What the destination fields say
//!
//! v2 carries both ends. The source is the visitor, which is the whole point
//! and is what every receiver reads. The destination is written as the
//! backend's own address, because that is the one this side can state
//! truthfully; the address the visitor actually dialled is a public port on
//! the server, and inventing a value for it would be worse than naming
//! something real. Receivers that act on the destination (rare) should know
//! that.

use std::net::SocketAddr;

/// The v2 signature: twelve bytes chosen so no plausible payload starts with
/// them.
const SIGNATURE: [u8; 12] = [
  0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

/// Version 2, command PROXY (as opposed to LOCAL, which means "this is my own
/// health check, ignore the addresses").
const VERSION_AND_COMMAND: u8 = 0x21;

/// TCP over IPv4, and TCP over IPv6.
const TCP4: u8 = 0x11;
const TCP6: u8 = 0x21;

/// Encodes a header announcing a connection from `source` to `destination`.
///
/// Returns `None` when the two are of different families. Mixing them is not
/// representable in the wire format, and it happens for real: a visitor on
/// IPv6 reaching a backend listening on IPv4. Answering `None` lets the caller
/// relay the connection without a header rather than write a malformed one,
/// which a receiver would treat as a protocol error and close.
pub(crate) fn header_v2(source: SocketAddr, destination: SocketAddr) -> Option<Vec<u8>> {
  let mut out = Vec::with_capacity(52);
  out.extend_from_slice(&SIGNATURE);
  out.push(VERSION_AND_COMMAND);
  match (source, destination) {
    (SocketAddr::V4(src), SocketAddr::V4(dst)) => {
      out.push(TCP4);
      out.extend_from_slice(&12u16.to_be_bytes());
      out.extend_from_slice(&src.ip().octets());
      out.extend_from_slice(&dst.ip().octets());
      out.extend_from_slice(&src.port().to_be_bytes());
      out.extend_from_slice(&dst.port().to_be_bytes());
    }
    (SocketAddr::V6(src), SocketAddr::V6(dst)) => {
      out.push(TCP6);
      out.extend_from_slice(&36u16.to_be_bytes());
      out.extend_from_slice(&src.ip().octets());
      out.extend_from_slice(&dst.ip().octets());
      out.extend_from_slice(&src.port().to_be_bytes());
      out.extend_from_slice(&dst.port().to_be_bytes());
    }
    _ => return None,
  }
  Some(out)
}

/// The header for a visitor address as the server reported it, against the
/// local address of the connection to the backend.
///
/// An address the server could not report, or one that does not parse, means
/// no header: the backend is expecting one only because an operator said the
/// tunnel carries them, and a header full of zeroes would be a lie the
/// receiver acts on.
pub(crate) fn header_for(visitor: Option<&str>, local: SocketAddr) -> Option<Vec<u8>> {
  let source: SocketAddr = visitor?.parse().ok()?;
  header_v2(source, local)
}

#[cfg(test)]
#[path = "proxy_protocol_tests.rs"]
mod tests;
