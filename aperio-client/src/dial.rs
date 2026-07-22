//! Server dialing: resolves the tunnel server hostname and opens the TCP
//! connection ourselves before handing it to the WebSocket/TLS layer, so we
//! control IP-family selection and address fallback.
//!
//! tokio-tungstenite's own `connect_async` does a single
//! `TcpStream::connect("host:port")` and leaves address ordering to the OS
//! resolver. On some platforms (notably musl/Alpine) that is unreliable when a
//! hostname resolves to an unreachable family: a Cloudflare-fronted server may
//! publish AAAA records the host cannot route to, and the dial fails without
//! trying the reachable IPv4. Here we resolve every address, apply an operator
//! `ip_family` preference, and try each in turn (IPv4/IPv6 interleaved by
//! default) with a per-address connect timeout, so one dead address can never
//! strand the connection.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio_tungstenite::{
  MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
  tungstenite::{
    client::IntoClientRequest,
    error::{Error, UrlError},
    handshake::client::Response,
    protocol::WebSocketConfig,
  },
};

/// How long a single address's TCP connect may take before we move on to the
/// next resolved address. The reconnect loop owns the overall retry cadence.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which IP address family to dial the server over. `Auto` tries both
/// (IPv4 first, interleaved); `V4`/`V6` restrict to that family, letting an
/// operator dodge an unreachable family deterministically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum IpFamily {
  #[default]
  Auto,
  V4,
  V6,
}

impl IpFamily {
  /// Parses the `ip_family` config value. Unset/empty/unrecognized -> `Auto`.
  pub(crate) fn parse(value: Option<&str>) -> IpFamily {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
      Some("ipv4") | Some("v4") | Some("4") => IpFamily::V4,
      Some("ipv6") | Some("v6") | Some("6") => IpFamily::V6,
      _ => IpFamily::Auto,
    }
  }
}

/// Process-wide dialing family, resolved once from config at startup (mirrors
/// the device-key global). A change takes effect on the next process start,
/// not on config hot-reload.
static IP_FAMILY: OnceLock<IpFamily> = OnceLock::new();

/// Records the resolved dialing family. Idempotent; a second call is ignored.
pub(crate) fn set_ip_family(family: IpFamily) {
  let _ = IP_FAMILY.set(family);
}

/// The effective dialing family (`Auto` until `set_ip_family` runs).
fn ip_family() -> IpFamily {
  IP_FAMILY.get().copied().unwrap_or_default()
}

/// Interleaves two address lists, IPv4 first, so a run of one unreachable
/// family cannot exhaust the whole connect budget before the other is tried.
fn interleave(v4: Vec<SocketAddr>, v6: Vec<SocketAddr>) -> Vec<SocketAddr> {
  let mut out = Vec::with_capacity(v4.len() + v6.len());
  let mut a = v4.into_iter();
  let mut b = v6.into_iter();
  loop {
    let (x, y) = (a.next(), b.next());
    if x.is_none() && y.is_none() {
      break;
    }
    out.extend(x);
    out.extend(y);
  }
  out
}

/// Resolves `host:port` to the ordered list of addresses to attempt, honoring
/// the family preference.
async fn resolve_ordered(
  host: &str,
  port: u16,
  family: IpFamily,
) -> Result<Vec<SocketAddr>, Error> {
  let (mut v4, mut v6) = (Vec::new(), Vec::new());
  for addr in tokio::net::lookup_host((host, port))
    .await
    .map_err(Error::Io)?
  {
    if addr.is_ipv4() {
      v4.push(addr);
    } else {
      v6.push(addr);
    }
  }
  let ordered = match family {
    IpFamily::V4 => v4,
    IpFamily::V6 => v6,
    IpFamily::Auto => interleave(v4, v6),
  };
  if ordered.is_empty() {
    return Err(Error::Io(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      format!("no {family:?} address resolved for {host}:{port}"),
    )));
  }
  Ok(ordered)
}

/// Opens a WebSocket (optionally TLS) connection to the server named in
/// `request`, choosing the TCP address ourselves per the configured
/// `ip_family` and falling back across every resolved address. TLS handling is
/// unchanged from `connect_async` — the default webpki-roots connector is used.
pub(crate) async fn connect_ws<R>(
  request: R,
  config: Option<WebSocketConfig>,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), Error>
where
  R: IntoClientRequest + Unpin,
{
  let request = request.into_client_request()?;
  let uri = request.uri();
  let host = uri
    .host()
    .ok_or(Error::Url(UrlError::NoHostName))?
    .to_string();
  let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
    Some("ws") => 80,
    _ => 443,
  });

  let addrs = resolve_ordered(&host, port, ip_family()).await?;

  let mut last_err: Option<std::io::Error> = None;
  let mut stream = None;
  for addr in addrs {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
      Ok(Ok(sock)) => {
        stream = Some(sock);
        break;
      }
      Ok(Err(e)) => last_err = Some(e),
      Err(_) => {
        last_err = Some(std::io::Error::new(
          std::io::ErrorKind::TimedOut,
          format!("connect to {addr} timed out"),
        ))
      }
    }
  }

  let stream = stream.ok_or_else(|| {
    Error::Io(last_err.unwrap_or_else(|| {
      std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("no reachable address for {host}:{port}"),
      )
    }))
  })?;

  client_async_tls_with_config(request, stream, config, None).await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_maps_known_families_and_defaults_to_auto() {
    assert_eq!(IpFamily::parse(Some("ipv4")), IpFamily::V4);
    assert_eq!(IpFamily::parse(Some(" V4 ")), IpFamily::V4);
    assert_eq!(IpFamily::parse(Some("4")), IpFamily::V4);
    assert_eq!(IpFamily::parse(Some("ipv6")), IpFamily::V6);
    assert_eq!(IpFamily::parse(Some("6")), IpFamily::V6);
    assert_eq!(IpFamily::parse(Some("auto")), IpFamily::Auto);
    assert_eq!(IpFamily::parse(Some("nonsense")), IpFamily::Auto);
    assert_eq!(IpFamily::parse(Some("")), IpFamily::Auto);
    assert_eq!(IpFamily::parse(None), IpFamily::Auto);
  }

  fn v4(n: u8) -> SocketAddr {
    format!("10.0.0.{n}:443").parse().unwrap()
  }
  fn v6(n: u8) -> SocketAddr {
    format!("[::{n}]:443").parse().unwrap()
  }

  #[test]
  fn interleave_starts_with_ipv4_and_alternates() {
    let out = interleave(vec![v4(1), v4(2)], vec![v6(1), v6(2)]);
    assert_eq!(out, vec![v4(1), v6(1), v4(2), v6(2)]);
  }

  #[test]
  fn interleave_appends_the_longer_families_remainder() {
    let out = interleave(vec![v4(1)], vec![v6(1), v6(2), v6(3)]);
    assert_eq!(out, vec![v4(1), v6(1), v6(2), v6(3)]);
    let out = interleave(vec![v4(1), v4(2), v4(3)], vec![v6(1)]);
    assert_eq!(out, vec![v4(1), v6(1), v4(2), v4(3)]);
  }

  #[tokio::test]
  async fn resolve_ordered_filters_by_family() {
    // Literal addresses resolve without DNS; the target only needs to name
    // both families. lookup_host on an IP echoes it, so we exercise ordering
    // by resolving a hostname is avoided — use loopback-style literals.
    let only_v4 = resolve_ordered("127.0.0.1", 443, IpFamily::V4)
      .await
      .unwrap();
    assert!(only_v4.iter().all(|a| a.is_ipv4()));

    // Asking for a family the target cannot provide is an error, not a hang.
    let none_v6 = resolve_ordered("127.0.0.1", 443, IpFamily::V6).await;
    assert!(none_v6.is_err());
  }
}
