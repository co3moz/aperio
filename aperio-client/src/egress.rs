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

use std::fmt;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest `CONNECT` response head accepted, headers included.
///
/// Bounded while reading rather than after: the answer is a status line and a
/// few headers, and anything able to answer as the proxy should not get to
/// decide how much memory this connection costs before it is even a tunnel.
const MAX_RESPONSE_HEAD: usize = 8 * 1024;

/// A proxy the operator told us to dial through.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EgressProxy {
  pub(crate) host: String,
  pub(crate) port: u16,
  /// User and password, when the URL carried them.
  ///
  /// Kept apart rather than pre-encoded because they are needed in two
  /// shapes: a `Proxy-Authorization` header for our own `CONNECT`, and
  /// reqwest's own basic-auth for the `check` command's health probe, which
  /// has to reach the server through the same proxy or it reports a failure
  /// that only exists because it dialed direct.
  credentials: Option<(String, String)>,
  /// Host and port only: what may be logged.
  redacted: String,
}

impl fmt::Debug for EgressProxy {
  /// Hand-written so a credential cannot reach a log through `{:?}`.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "EgressProxy({})", self.redacted)
  }
}

impl EgressProxy {
  /// Parses the `egress_proxy` value.
  ///
  /// Accepts `http://host:port`, a bare `host:port`, and either with
  /// `user:password@` in front of the host. An `https://` proxy, meaning TLS
  /// to the proxy itself, is refused rather than quietly dialed in the clear:
  /// treating it as plaintext would hand the proxy credential to the network
  /// the scheme was chosen to hide it from.
  pub(crate) fn parse(raw: &str) -> Result<EgressProxy, String> {
    let raw = raw.trim();
    if raw.is_empty() {
      return Err("egress_proxy is empty".to_string());
    }
    let (scheme, rest) = match raw.split_once("://") {
      Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
      None => ("http".to_string(), raw),
    };
    match scheme.as_str() {
      "http" => {}
      "https" => {
        return Err(
          "egress_proxy names an https:// proxy, which this client cannot dial yet \
           (TLS to the proxy itself). Use the http:// form; the tunnel inside it is \
           still TLS end to end."
            .to_string(),
        );
      }
      other => {
        return Err(format!(
          "egress_proxy scheme '{other}' is not supported (write http:// or host:port)"
        ));
      }
    }
    // A path is meaningless for a CONNECT proxy, and silently dropping one
    // would hide a typo such as a whole URL pasted in.
    let rest = rest.trim_end_matches('/');
    if rest.contains('/') {
      return Err(format!(
        "egress_proxy '{}' has a path; a proxy is a host and a port",
        redact_raw(raw)
      ));
    }
    // `@` last, so a password containing one is kept with the credential.
    let (credential, hostport) = match rest.rsplit_once('@') {
      Some((credential, hostport)) => (Some(credential), hostport),
      None => (None, rest),
    };
    if hostport.is_empty() {
      return Err("egress_proxy names no host".to_string());
    }
    let (host, port) = split_host_port(hostport)?;
    // The *first* colon separates them: a password may contain one, a user
    // name may not, which is what basic auth's own grammar says.
    let credentials = credential.map(|c| match c.split_once(':') {
      Some((user, password)) => (user.to_string(), password.to_string()),
      None => (c.to_string(), String::new()),
    });
    Ok(EgressProxy {
      redacted: format!("{host}:{port}"),
      host,
      port,
      credentials,
    })
  }

  /// Host and port, never the credential. Everything user-facing uses this.
  pub(crate) fn redacted(&self) -> &str {
    &self.redacted
  }

  /// Whether a credential was configured, without revealing it.
  pub(crate) fn has_credentials(&self) -> bool {
    self.credentials.is_some()
  }

  /// The same proxy as reqwest wants it, for the one place that makes an
  /// ordinary HTTP request through it rather than a `CONNECT`: the `check`
  /// command's server health probe. Without this, `check` on a proxy-only
  /// network reports the server unreachable while the client it is meant to
  /// be diagnosing connects perfectly well.
  pub(crate) fn as_reqwest(&self) -> Result<reqwest::Proxy, String> {
    let proxy = reqwest::Proxy::all(format!("http://{}:{}", self.host, self.port))
      .map_err(|e| format!("the proxy {} cannot be used for HTTP: {e}", self.redacted))?;
    Ok(match self.credentials {
      Some((ref user, ref password)) => proxy.basic_auth(user, password),
      None => proxy,
    })
  }

  /// The bytes of the `CONNECT` request for `host:port`.
  fn request(&self, host: &str, port: u16) -> Vec<u8> {
    // `Host` repeats the authority because some proxies log or route on it,
    // and HTTP/1.1 requires it regardless of the method.
    let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some((ref user, ref password)) = self.credentials {
      let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
      req.push_str("Proxy-Authorization: Basic ");
      req.push_str(&encoded);
      req.push_str("\r\n");
    }
    req.push_str("\r\n");
    req.into_bytes()
  }
}

/// Splits `host:port`, including the `[v6]:port` form.
fn split_host_port(raw: &str) -> Result<(String, u16), String> {
  // A bracketed IPv6 literal, where the colons inside are part of the address.
  if let Some(rest) = raw.strip_prefix('[') {
    let (addr, tail) = rest
      .split_once(']')
      .ok_or_else(|| format!("egress_proxy '{raw}' opens a bracket it never closes"))?;
    let port = tail
      .strip_prefix(':')
      .ok_or_else(|| format!("egress_proxy '{raw}' needs a port after the address"))?;
    return Ok((addr.to_string(), parse_port(port, raw)?));
  }
  match raw.rsplit_once(':') {
    Some((host, port)) if !host.is_empty() => Ok((host.to_string(), parse_port(port, raw)?)),
    // No port: the scheme's default. Named in the docs, because a proxy on 80
    // is unusual and a missing port is more often an omission than a choice.
    _ => Ok((raw.to_string(), 80)),
  }
}

fn parse_port(raw: &str, whole: &str) -> Result<u16, String> {
  raw
    .parse::<u16>()
    .map_err(|_| format!("egress_proxy '{whole}' has no usable port"))
}

/// Hides a credential in a value that failed to parse, since the failure
/// message is printed.
fn redact_raw(raw: &str) -> String {
  match raw.rsplit_once('@') {
    Some((_, tail)) => format!("***@{tail}"),
    None => raw.to_string(),
  }
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
    .write_all(&proxy.request(host, port))
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
