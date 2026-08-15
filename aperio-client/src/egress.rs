//! Reaching the tunnel server through an HTTP proxy (`planned_features.md`
//! #117).
//!
//! Plenty of companies allow no direct outbound connection: everything leaves
//! through a proxy, and a client that cannot be pointed at one does not work
//! there at all. This module is the whole of that support, and it is small on
//! purpose.
//!
//! **Why `egress` and not `proxy`.** In this crate `proxy` already means the
//! *reverse* proxy to the local backend, the whole `crate::proxy::` tree. A
//! second meaning of the same word in the same crate is how the wrong one gets
//! edited.
//!
//! **What crosses the proxy.** `CONNECT host:443`, and after the proxy answers
//! `200` the socket is a plain byte tunnel to the server. The TLS and
//! WebSocket handshakes then run over it unchanged, so TLS stays end to end
//! and the proxy sees the hostname it was asked for and nothing else. That is
//! also what makes this safe to offer: a proxy that could read the traffic
//! would be a different feature with a different decision behind it (see #117
//! on TLS interception, deliberately out of scope).
//!
//! **The credential never reaches a log.** A proxy URL may carry
//! `user:password@`, so everything this module prints, including its errors,
//! goes through [`EgressProxy::redacted`]. The struct's `Debug` is written by
//! hand for the same reason: a derived one would put the password in any
//! future `{:?}`.

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest `CONNECT` response head accepted, headers included.
///
/// Bounded while reading rather than after: the answer is a status line and a
/// few headers, and anything able to answer as the proxy should not get to
/// decide how much memory this connection costs before it is even a tunnel.
const MAX_RESPONSE_HEAD: usize = 8 * 1024;

/// The shared parsed proxy, re-exported so the rest of the client names one
/// type. The parsing, the redaction and the credential handling live in
/// `aperio-config` because the server needs the identical value for #118, and
/// two copies of a redaction rule is one copy too many.
pub(crate) use aperio_config::egress::EgressProxy;

/// The same proxy as reqwest wants it, for the one place in the client that
/// makes an ordinary HTTP request through it rather than a `CONNECT`: the
/// `check` command's server health probe. Without this, `check` on a
/// proxy-only network reports the server unreachable while the client it is
/// meant to be diagnosing connects perfectly well.
pub(crate) fn as_reqwest(proxy: &EgressProxy) -> Result<reqwest::Proxy, String> {
  let built =
    reqwest::Proxy::all(format!("http://{}:{}", proxy.host(), proxy.port())).map_err(|e| {
      format!(
        "the proxy {} cannot be used for HTTP: {e}",
        proxy.redacted()
      )
    })?;
  Ok(match proxy.credentials() {
    Some((user, password)) => built.basic_auth(user, password),
    None => built,
  })
}

/// The `CONNECT` request for `host:port`, with the proxy credential when one
/// was configured.
///
/// Here rather than beside the parser because this is the only shape the
/// client needs and the only one that needs base64; the server turns the same
/// value into a reqwest proxy instead.
fn connect_request(proxy: &EgressProxy, host: &str, port: u16) -> Vec<u8> {
  // `Host` repeats the authority because some proxies log or route on it, and
  // HTTP/1.1 requires it regardless of the method.
  let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
  if let Some((user, password)) = proxy.credentials() {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    req.push_str("Proxy-Authorization: Basic ");
    req.push_str(&encoded);
    req.push_str("\r\n");
  }
  req.push_str("\r\n");
  req.into_bytes()
}

/// Turns an open socket to the proxy into a tunnel to `host:port`.
///
/// On success the stream is a transparent byte pipe and the caller runs the
/// ordinary TLS and WebSocket handshakes over it, which is why this returns
/// the same `TcpStream` it was given.
pub(crate) async fn connect_through(
  mut stream: TcpStream,
  proxy: &EgressProxy,
  host: &str,
  port: u16,
) -> Result<TcpStream, String> {
  stream
    .write_all(&connect_request(proxy, host, port))
    .await
    .map_err(|e| {
      format!(
        "could not send CONNECT to the proxy {}: {e}",
        proxy.redacted()
      )
    })?;

  let mut head = Vec::new();
  let mut byte = [0u8; 1];
  loop {
    match stream.read(&mut byte).await {
      Ok(0) => {
        return Err(format!(
          "the proxy {} closed the connection without answering CONNECT",
          proxy.redacted()
        ));
      }
      Ok(_) => head.push(byte[0]),
      Err(e) => {
        return Err(format!(
          "could not read the proxy {}'s answer to CONNECT: {e}",
          proxy.redacted()
        ));
      }
    }
    if head.ends_with(b"\r\n\r\n") {
      break;
    }
    if head.len() > MAX_RESPONSE_HEAD {
      return Err(format!(
        "the proxy {} answered CONNECT with more than {MAX_RESPONSE_HEAD} bytes of headers",
        proxy.redacted()
      ));
    }
  }

  let status = connect_status(&head).ok_or_else(|| {
    format!(
      "the proxy {} answered CONNECT with something that is not an HTTP status line",
      proxy.redacted()
    )
  })?;
  if !(200..300).contains(&status) {
    // Named, both the proxy and what it said. The failure an operator must
    // never get is a dial that fails three layers away from the cause: a 407
    // means the credential, a 403 means the destination, and a generic
    // "connection failed" would have said neither.
    let hint = match status {
      407 => {
        if proxy.has_credentials() {
          " (the proxy rejected the credential in egress_proxy)"
        } else {
          " (the proxy wants a credential; put it in egress_proxy as user:password@host:port)"
        }
      }
      403 => " (the proxy refused this destination)",
      _ => "",
    };
    return Err(format!(
      "the proxy {} refused CONNECT to {host}:{port} with {status}{hint}",
      proxy.redacted()
    ));
  }
  Ok(stream)
}

/// The status code from a response head.
fn connect_status(head: &[u8]) -> Option<u16> {
  let line = head.split(|b| *b == b'\r').next()?;
  let line = std::str::from_utf8(line).ok()?;
  let mut parts = line.split_whitespace();
  let version = parts.next()?;
  if !version.starts_with("HTTP/") {
    return None;
  }
  parts.next()?.parse().ok()
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
