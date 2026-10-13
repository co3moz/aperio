//! The HTTPS listener end to end: a real handshake picks the certificate for
//! the name asked for and serves the router with the request marked as
//! HTTPS; a name nothing covers, and a proof request with no proof pending,
//! get no handshake. Also the scheme a callback URL is built with, and the
//! plain listener's redirect.

use super::*;
use crate::tls::acme::{Challenges, Issuer};
use crate::tls::certs::{CertStore, names_and_expiry};
use axum::routing::get;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::ServiceExt;

/// Accepts whatever certificate is presented, so a test can look at it.
#[derive(Debug)]
pub(crate) struct AnyCert;

impl ServerCertVerifier for AnyCert {
  fn verify_server_cert(
    &self,
    _: &CertificateDer<'_>,
    _: &[CertificateDer<'_>],
    _: &ServerName<'_>,
    _: &[u8],
    _: UnixTime,
  ) -> Result<ServerCertVerified, rustls::Error> {
    Ok(ServerCertVerified::assertion())
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls12_signature(
      message,
      cert,
      dss,
      &rustls::crypto::ring::default_provider().signature_verification_algorithms,
    )
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(
      message,
      cert,
      dss,
      &rustls::crypto::ring::default_provider().signature_verification_algorithms,
    )
  }

  fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
    rustls::crypto::ring::default_provider()
      .signature_verification_algorithms
      .supported_schemes()
  }
}

pub(crate) type Client = tokio_rustls::client::TlsStream<TcpStream>;

/// A TLS connection to `addr` asking for `sni`, offering `alpn`.
pub(crate) async fn connect(
  addr: SocketAddr,
  sni: &str,
  alpn: &[&[u8]],
) -> std::io::Result<Client> {
  connect_with(addr, sni, alpn, Arc::new(AnyCert)).await
}

/// [`connect`], with the certificate judged by `verifier`.
pub(crate) async fn connect_with(
  addr: SocketAddr,
  sni: &str,
  alpn: &[&[u8]],
  verifier: Arc<dyn ServerCertVerifier>,
) -> std::io::Result<Client> {
  let mut config =
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
      .with_safe_default_protocol_versions()
      .unwrap()
      .dangerous()
      .with_custom_certificate_verifier(verifier)
      .with_no_client_auth();
  config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
  let tcp = TcpStream::connect(addr).await?;
  tokio_rustls::TlsConnector::from(Arc::new(config))
    .connect(ServerName::try_from(sni.to_string()).unwrap(), tcp)
    .await
}

/// The names in the certificate a connection was presented.
pub(crate) fn presented_names(stream: &Client) -> Vec<String> {
  let leaf = &stream.get_ref().1.peer_certificates().unwrap()[0];
  names_and_expiry(leaf).unwrap().0
}

/// An HTTP/1.1 GET over an open connection, the whole answer as text.
pub(crate) async fn get_over(mut stream: Client, host: &str, path: &str) -> String {
  stream
    .write_all(
      format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
  let mut out = Vec::new();
  let _ = stream.read_to_end(&mut out).await;
  String::from_utf8_lossy(&out).into_owned()
}

/// The listener, serving a router that says whether a request was marked as
/// having come over HTTPS.
pub(crate) async fn serve_https(
  certs: Arc<CertStore>,
  challenges: Arc<Challenges>,
  issuer: Option<Arc<Issuer>>,
) -> SocketAddr {
  let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let handshakes = Arc::new(listener::Handshakes::new(certs, challenges, issuer));
  let listener = listener::listen(tcp, handshakes).unwrap();
  let addr = axum::serve::Listener::local_addr(&listener).unwrap();
  let app = Router::new()
    .route(
      "/hello",
      get(|tls: Option<axum::Extension<ServedOverTls>>| async move {
        if tls.is_some() { "over https" } else { "plain" }
      }),
    )
    .layer(axum::Extension(ServedOverTls));
  tokio::spawn(async move {
    let _ = axum::serve(
      listener.tap_io(|_| {}),
      app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await;
  });
  addr
}

fn temp_dir(name: &str) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-tls-{name}-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

fn file_pair(
  dir: &std::path::Path,
  stem: &str,
  names: &[&str],
) -> aperio_config::server_settings::CertificateFiles {
  let key = rcgen::KeyPair::generate().unwrap();
  let cert = rcgen::CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
    .unwrap()
    .self_signed(&key)
    .unwrap();
  let cert_path = dir.join(format!("{stem}.pem"));
  let key_path = dir.join(format!("{stem}.key"));
  std::fs::write(&cert_path, cert.pem()).unwrap();
  std::fs::write(&key_path, key.serialize_pem()).unwrap();
  aperio_config::server_settings::CertificateFiles {
    cert: cert_path.to_string_lossy().into_owned(),
    key: key_path.to_string_lossy().into_owned(),
  }
}

#[tokio::test]
async fn a_handshake_gets_the_certificate_for_its_name_and_the_request_is_https() {
  crate::ensure_crypto_provider();
  let dir = temp_dir("serve");
  let files = [
    file_pair(&dir, "wild", &["*.example.com"]),
    file_pair(&dir, "api", &["api.example.com"]),
  ];
  let certs = Arc::new(CertStore::new(&files, None));
  let addr = serve_https(certs, Arc::default(), None).await;

  let api = connect(addr, "api.example.com", &[b"http/1.1"])
    .await
    .unwrap();
  assert_eq!(presented_names(&api), ["api.example.com"]);
  let answer = get_over(api, "api.example.com", "/hello").await;
  assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
  assert!(answer.ends_with("over https"), "{answer}");

  let other = connect(addr, "www.example.com", &[b"http/1.1"])
    .await
    .unwrap();
  assert_eq!(presented_names(&other), ["*.example.com"]);

  // Nothing covers it, and there is no issuer: no handshake at all, rather
  // than a certificate for somebody else's name.
  assert!(connect(addr, "example.org", &[b"http/1.1"]).await.is_err());
  // An authority asking for a proof nobody is serving gets nothing either.
  assert!(
    connect(addr, "api.example.com", &[listener::ACME_TLS_ALPN])
      .await
      .is_err()
  );
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_callback_is_https_over_the_listener_or_when_a_trusted_proxy_says_so() {
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-proto", "https".parse().unwrap());

  let untrusting = crate::test_support::test_state_with(crate::test_support::test_config());
  assert_eq!(scheme(&untrusting, &headers, false), "http");
  assert_eq!(scheme(&untrusting, &HeaderMap::new(), true), "https");

  let mut config = crate::test_support::test_config();
  config.trust_proxy = true;
  let trusting = crate::test_support::test_state_with(config);
  assert_eq!(scheme(&trusting, &headers, false), "https");
  headers.insert("x-forwarded-proto", "gopher".parse().unwrap());
  assert_eq!(scheme(&trusting, &headers, false), "http");
}

#[tokio::test]
async fn plain_http_is_sent_to_https_except_the_probes() {
  let app = Router::new()
    .route("/page", get(|| async { "page" }))
    .route("/aperio/healthz", get(|| async { "ok" }))
    .layer(axum::middleware::from_fn_with_state(
      8443u16,
      redirect_to_https,
    ));
  let request = |uri: &str| {
    axum::http::Request::builder()
      .uri(uri)
      .header("host", "app.example.com:8080")
      .body(axum::body::Body::empty())
      .unwrap()
  };

  let answer = app.clone().oneshot(request("/page?x=1")).await.unwrap();
  assert_eq!(answer.status(), StatusCode::PERMANENT_REDIRECT);
  assert_eq!(
    answer.headers()[header::LOCATION],
    "https://app.example.com:8443/page?x=1"
  );

  let probe = app
    .clone()
    .oneshot(request("/aperio/healthz"))
    .await
    .unwrap();
  assert_eq!(probe.status(), StatusCode::OK);

  let default_port =
    Router::new()
      .route("/", get(|| async { "page" }))
      .layer(axum::middleware::from_fn_with_state(
        443u16,
        redirect_to_https,
      ));
  let answer = default_port.oneshot(request("/")).await.unwrap();
  assert_eq!(
    answer.headers()[header::LOCATION],
    "https://app.example.com/"
  );
}

#[tokio::test]
async fn a_websocket_upgrade_on_the_plain_listener_is_told_to_dial_wss() {
  // A tunnel client dialing `ws://` against a server that redirects to HTTPS
  // cannot follow the redirect, and used to log nothing but "HTTP 308". It
  // gets a `426` whose body names the change to make, which is what the
  // client prints for that status.
  let app = Router::new()
    .route("/aperio/ws", get(|| async { "tunnel" }))
    .layer(axum::middleware::from_fn_with_state(
      443u16,
      redirect_to_https,
    ));
  let request = axum::http::Request::builder()
    .uri("/aperio/ws")
    .header("host", "tunnel.example.com")
    .header("upgrade", "websocket")
    .header("connection", "Upgrade")
    .body(axum::body::Body::empty())
    .unwrap();
  let answer = app.oneshot(request).await.unwrap();
  assert_eq!(answer.status(), StatusCode::UPGRADE_REQUIRED);
  let body = axum::body::to_bytes(answer.into_body(), 4096)
    .await
    .unwrap();
  let text = String::from_utf8_lossy(&body);
  assert!(text.contains("wss://"), "{text}");
}
