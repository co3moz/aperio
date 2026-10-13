//! The HTTPS listener: TCP accepted here, the TLS handshake finished on a
//! task of its own, and the connection handed to axum like any other.
//!
//! The handshake is not done inside `accept`. axum calls `accept` in a loop
//! and serves nothing new while it waits, so a handshake done there would let
//! one slow visitor, or one waiting on a certificate being issued, hold up
//! every other connection. Each runs on its own task instead, and `accept`
//! takes the finished ones from a channel.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls::server::{Acceptor, ResolvesServerCert};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::LazyConfigAcceptor;
use tokio_rustls::server::TlsStream;
use tracing::{debug, warn};

use super::acme::{ChallengeResolver, Challenges, Issuer};
use super::certs::{CertStore, StoreResolver};

/// The protocol an ACME authority asks for when it checks a TLS-ALPN-01
/// proof (RFC 8737).
pub(crate) const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";
/// How long a visitor has to send its ClientHello, and then to finish.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a handshake waits for a certificate being obtained on demand.
/// The order itself goes on if this runs out; the next handshake finds it.
const ISSUE_WAIT: Duration = Duration::from_secs(60);

/// Builds a TLS server configuration on the ring provider, presenting what
/// `resolver` picks and offering `alpn`.
pub(crate) fn server_config(
  resolver: Arc<dyn ResolvesServerCert>,
  alpn: &[&[u8]],
) -> Arc<ServerConfig> {
  let mut config =
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
      .with_safe_default_protocol_versions()
      .expect("ring supports TLS 1.2 and 1.3")
      .with_no_client_auth()
      .with_cert_resolver(resolver);
  config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
  Arc::new(config)
}

/// What a handshake needs: the certificates, the proofs being served, and
/// the issuer that can obtain one it lacks.
pub(crate) struct Handshakes {
  certs: Arc<CertStore>,
  issuer: Option<Arc<Issuer>>,
  main: Arc<ServerConfig>,
  proof: Arc<ServerConfig>,
}

impl Handshakes {
  pub(crate) fn new(
    certs: Arc<CertStore>,
    challenges: Arc<Challenges>,
    issuer: Option<Arc<Issuer>>,
  ) -> Self {
    Handshakes {
      main: server_config(
        Arc::new(StoreResolver(certs.clone())),
        &[b"h2", b"http/1.1"],
      ),
      proof: server_config(Arc::new(ChallengeResolver(challenges)), &[ACME_TLS_ALPN]),
      certs,
      issuer,
    }
  }

  /// Finishes one handshake, or `None` when there is nothing to serve: it
  /// failed, timed out, or was an authority checking a proof.
  async fn handshake(&self, stream: TcpStream, peer: SocketAddr) -> Option<TlsStream<TcpStream>> {
    let start = tokio::time::timeout(
      HANDSHAKE_TIMEOUT,
      LazyConfigAcceptor::new(Acceptor::default(), stream),
    )
    .await
    .ok()?
    .map_err(|e| debug!("TLS handshake from {peer} failed: {e}"))
    .ok()?;
    let hello = start.client_hello();
    let sni = hello.server_name().map(str::to_ascii_lowercase);
    let checking_proof = hello
      .alpn()
      .is_some_and(|mut protocols| protocols.any(|p| p == ACME_TLS_ALPN));
    if checking_proof {
      // An authority reading a proof: answer, and serve nothing over it.
      let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, start.into_stream(self.proof.clone())).await;
      return None;
    }
    if let (Some(issuer), Some(host)) = (&self.issuer, &sni)
      && self.certs.lookup(Some(host)).is_none()
      && issuer.wants(host).await
    {
      let why = issuer.reason_for(host).await;
      // On a task of its own, so a visitor who gives up does not cancel an
      // order halfway through.
      let issuer = issuer.clone();
      let host = host.clone();
      let order = tokio::spawn(async move { issuer.ensure(&host, why).await });
      let _ = tokio::time::timeout(ISSUE_WAIT, order).await;
    }
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, start.into_stream(self.main.clone())).await {
      Ok(Ok(stream)) => Some(stream),
      Ok(Err(e)) => {
        debug!(
          "TLS handshake from {peer} for {} failed: {e}",
          sni.as_deref().unwrap_or("no name")
        );
        None
      }
      Err(_) => None,
    }
  }
}

/// TLS connections, finished, for `axum::serve`.
pub(crate) struct TlsListener {
  ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
  local: SocketAddr,
}

/// Accepts on `tcp` and handshakes each connection on its own task.
pub(crate) fn listen(
  tcp: TcpListener,
  handshakes: Arc<Handshakes>,
) -> std::io::Result<TlsListener> {
  let local = tcp.local_addr()?;
  let (ready_tx, ready) = mpsc::channel(256);
  tokio::spawn(async move {
    loop {
      let (stream, peer) = match tcp.accept().await {
        Ok(accepted) => accepted,
        Err(e) => {
          // Out of descriptors, mostly: back off rather than spin on it.
          warn!("HTTPS accept failed: {e}");
          tokio::time::sleep(Duration::from_millis(50)).await;
          continue;
        }
      };
      if ready_tx.is_closed() {
        break;
      }
      let _ = stream.set_nodelay(true);
      let handshakes = handshakes.clone();
      let ready_tx = ready_tx.clone();
      tokio::spawn(async move {
        if let Some(tls) = handshakes.handshake(stream, peer).await {
          let _ = ready_tx.send((tls, peer)).await;
        }
      });
    }
  });
  Ok(TlsListener { ready, local })
}

impl axum::serve::Listener for TlsListener {
  type Io = TlsStream<TcpStream>;
  type Addr = SocketAddr;

  async fn accept(&mut self) -> (Self::Io, Self::Addr) {
    match self.ready.recv().await {
      Some(connection) => connection,
      // The accept loop is gone, which only happens when the listener is
      // being dropped; there is nothing more to hand out.
      None => std::future::pending().await,
    }
  }

  fn local_addr(&self) -> std::io::Result<Self::Addr> {
    Ok(self.local)
  }
}
