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
  Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
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

/// How long the TLS and WebSocket handshakes together may take, once a socket
/// is open.
///
/// Only the TCP connect used to be bounded, which left the case a reconnect
/// loop cannot recover from: a peer that accepts the connection and then says
/// nothing. `connect` returns, the handshake blocks forever, and the loop
/// never gets to retry, so the service is down with no error and no attempt
/// on the next candidate server. Anything that is not a hung peer finishes a
/// handshake in well under this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

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

/// The TLS floor, and optionally the exact cipher suites, the tunnel dial
/// offers.
///
/// Rustls' defaults are good, and this exists anyway, because "we use the
/// library's defaults" is a true answer that a compliance questionnaire asking
/// for the floor *in writing* cannot accept. Pinning it changes nothing about
/// what a correct peer negotiates and makes the answer a line in a file rather
/// than a claim about a dependency.
///
/// Unset is not the same as "set to the default": with nothing configured the
/// connector is left alone entirely, so the dial is byte for byte what it was
/// before this existed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TlsPolicy {
  /// Lowest protocol version offered. `None` leaves rustls' own set (1.2 and
  /// 1.3) in place.
  pub(crate) min_version: Option<TlsFloor>,
  /// Exact cipher suites offered, by their IANA names. Empty leaves rustls'
  /// own preference order, which is the better choice unless something
  /// external requires otherwise.
  pub(crate) cipher_suites: Vec<String>,
}

/// A protocol floor an operator can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TlsFloor {
  V12,
  V13,
}

impl TlsPolicy {
  /// Reads the two settings, refusing anything it does not understand.
  ///
  /// **A misspelling is an error, not a default.** Every other value in this
  /// file falls back when it cannot be read, which is right for a preference
  /// and wrong for a floor: an operator who writes `tls_min_version: 1.4`
  /// wants a stricter dial than they are getting, and quietly handing them
  /// the default is how a control ends up existing only on paper.
  pub(crate) fn parse(
    min_version: Option<&str>,
    cipher_suites: Option<&str>,
  ) -> Result<Self, String> {
    let min_version = match min_version.map(str::trim).filter(|v| !v.is_empty()) {
      None => None,
      Some(raw) => {
        let normalized = raw
          .to_ascii_lowercase()
          .replace("tlsv", "")
          .replace("tls", "");
        match normalized.trim() {
          "1.2" | "12" => Some(TlsFloor::V12),
          "1.3" | "13" => Some(TlsFloor::V13),
          _ => {
            return Err(format!(
              "tls_min_version: `{raw}` is not a TLS version this client can offer; use `1.2` or `1.3`"
            ));
          }
        }
      }
    };

    let mut names = Vec::new();
    for name in cipher_suites
      .unwrap_or_default()
      .split(&[',', ' '][..])
      .map(str::trim)
      .filter(|n| !n.is_empty())
    {
      let known = all_cipher_suites()
        .iter()
        .any(|s| s.eq_ignore_ascii_case(name));
      if !known {
        return Err(format!(
          "tls_cipher_suites: `{name}` is not a cipher suite this build has; the ones it has are {}",
          all_cipher_suites().join(", ")
        ));
      }
      names.push(name.to_string());
    }

    let policy = TlsPolicy {
      min_version,
      cipher_suites: names,
    };
    // Built here and thrown away, for its errors. The one contradiction the
    // two fields can describe together, a 1.3 floor with only 1.2 suites
    // named, is invisible to each half on its own and rustls only reports it
    // when the config is assembled. Finding it now means it is a startup
    // refusal with the file in front of the operator, rather than a dial that
    // fails later somewhere they are not looking.
    if !policy.is_default() {
      build_connector(&policy)?;
    }
    Ok(policy)
  }

  /// True when nothing was asked for, which is the case that must leave the
  /// dial untouched.
  fn is_default(&self) -> bool {
    self.min_version.is_none() && self.cipher_suites.is_empty()
  }
}

/// Every cipher suite this build can offer, by IANA name.
fn all_cipher_suites() -> Vec<String> {
  rustls::crypto::ring::ALL_CIPHER_SUITES
    .iter()
    .map(|s| format!("{:?}", s.suite()))
    .collect()
}

/// Process-wide TLS policy, resolved once at startup like the dialing family.
static TLS_POLICY: OnceLock<TlsPolicy> = OnceLock::new();

/// Records the resolved policy. Idempotent; a second call is ignored.
pub(crate) fn set_tls_policy(policy: TlsPolicy) {
  let _ = TLS_POLICY.set(policy);
}

/// The connector the dial should use, or `None` to leave tokio-tungstenite's
/// own default in place.
///
/// Built once and reused: a `ClientConfig` owns the parsed root store, and
/// rebuilding it per reconnect would parse every webpki root again on a path
/// that runs whenever the tunnel drops.
fn tls_connector() -> Result<Option<Connector>, String> {
  static CONNECTOR: OnceLock<Result<Option<Connector>, String>> = OnceLock::new();
  CONNECTOR
    .get_or_init(|| {
      let Some(policy) = TLS_POLICY.get() else {
        return Ok(None);
      };
      if policy.is_default() {
        return Ok(None);
      }
      build_connector(policy).map(Some)
    })
    .clone()
}

/// Turns a policy into a rustls connector, or says why it cannot.
fn build_connector(policy: &TlsPolicy) -> Result<Connector, String> {
  let base = rustls::crypto::ring::default_provider();
  let provider = if policy.cipher_suites.is_empty() {
    base
  } else {
    let suites = rustls::crypto::ring::ALL_CIPHER_SUITES
      .iter()
      .filter(|s| {
        let name = format!("{:?}", s.suite());
        policy
          .cipher_suites
          .iter()
          .any(|asked| asked.eq_ignore_ascii_case(&name))
      })
      .copied()
      .collect::<Vec<_>>();
    rustls::crypto::CryptoProvider {
      cipher_suites: suites,
      ..base
    }
  };

  let versions: &[&'static rustls::SupportedProtocolVersion] = match policy.min_version {
    Some(TlsFloor::V13) => &[&rustls::version::TLS13],
    Some(TlsFloor::V12) | None => rustls::ALL_VERSIONS,
  };

  let mut roots = rustls::RootCertStore::empty();
  roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

  let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(provider))
    .with_protocol_versions(versions)
    .map_err(|e| {
      format!(
        "the configured TLS floor and cipher suites cannot be offered together ({e}); \
         a 1.3 floor needs at least one TLS 1.3 suite"
      )
    })?
    .with_root_certificates(roots)
    .with_no_client_auth();

  Ok(Connector::Rustls(std::sync::Arc::new(config)))
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
/// unchanged from `connect_async`, the default webpki-roots connector is used.
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

  // The tunnel carries request/response messages, not a bulk stream: every
  // write is a message somebody is waiting on the other side of. Nagle would
  // hold a small one back for an acknowledgement that has nothing to do with
  // it, which on this socket is latency added to a visitor's request.
  // Failure is not fatal, an un-tuned socket still works.
  let _ = stream.set_nodelay(true);

  handshake(request, stream, config, HANDSHAKE_TIMEOUT, &host, port).await
}

/// The TLS and WebSocket handshakes, under a budget.
///
/// Separate from `connect_ws` so the budget can be handed in: the case worth
/// testing is a peer that accepts and then says nothing, and waiting out the
/// real budget to see it is twenty seconds of nothing happening.
async fn handshake(
  request: tokio_tungstenite::tungstenite::handshake::client::Request,
  stream: TcpStream,
  config: Option<WebSocketConfig>,
  budget: Duration,
  host: &str,
  port: u16,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), Error> {
  // A policy that cannot be honoured fails the dial. Connecting anyway would
  // be the worst of the three outcomes: the tunnel is up, the floor the
  // operator wrote down is not in force, and nothing in the running system
  // disagrees with the file. `TlsPolicy::parse` already refuses this at
  // startup, so reaching it here means something built the policy another way.
  let connector = match tls_connector() {
    Ok(connector) => connector,
    Err(e) => {
      return Err(Error::Io(std::io::Error::other(format!(
        "refusing to dial {host}:{port} without the configured TLS policy: {e}"
      ))));
    }
  };

  match tokio::time::timeout(
    budget,
    client_async_tls_with_config(request, stream, config, connector),
  )
  .await
  {
    Ok(result) => result,
    Err(_) => Err(Error::Io(std::io::Error::new(
      std::io::ErrorKind::TimedOut,
      format!("TLS/WebSocket handshake with {host}:{port} timed out"),
    ))),
  }
}

#[cfg(test)]
#[path = "dial_tests.rs"]
mod tests;
