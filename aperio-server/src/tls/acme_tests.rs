//! An order end to end against an ACME authority that checks for real: the
//! proof is read off the HTTPS listener over `acme-tls/1` and compared with
//! the key authorization, the certificate is signed for the key in the CSR,
//! and a handshake afterwards presents it. Also what a refused proof leaves
//! behind: a failure, and a wait before the same name is tried again.
//!
//! The authority is a small fake on loopback rather than Pebble, so the test
//! needs nothing installed. It checks what an authority checks about this
//! server, and nothing about the client's signatures, which are
//! instant-acme's to get right.

use super::*;
use crate::tls::tests::{connect, connect_with, presented_names, serve_https};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Mutex;
use x509_parser::prelude::FromDer;

/// The one order the fake authority is working on.
#[derive(Default)]
struct Authority {
  base: String,
  /// The HTTPS listener the proofs are read from.
  listener: Option<SocketAddr>,
  thumbprint: Option<String>,
  nonce: u64,
  host: String,
  authorization: &'static str,
  order: &'static str,
  certificate: Option<String>,
  /// Names whose proof the authority pretends not to find.
  unreachable: Vec<String>,
  ca: Option<(rcgen::CertificateParams, rcgen::KeyPair)>,
  orders: usize,
}

type Shared = Arc<Mutex<Authority>>;

const TOKEN: &str = "fake-token-1";

fn b64(value: &str) -> Vec<u8> {
  URL_SAFE_NO_PAD.decode(value).unwrap_or_default()
}

/// The protected header and payload of a flattened JWS, unverified.
fn jws(body: &Value) -> (Value, Value) {
  let protected =
    serde_json::from_slice(&b64(body["protected"].as_str().unwrap_or(""))).unwrap_or(Value::Null);
  let payload =
    serde_json::from_slice(&b64(body["payload"].as_str().unwrap_or(""))).unwrap_or(Value::Null);
  (protected, payload)
}

fn answer(shared: &Shared, status: StatusCode, location: Option<String>, body: Value) -> Response {
  let mut authority = shared.lock().unwrap();
  authority.nonce += 1;
  let mut headers = HeaderMap::new();
  headers.insert(
    "replay-nonce",
    format!("nonce-{}", authority.nonce).parse().unwrap(),
  );
  if let Some(location) = location {
    headers.insert("location", location.parse().unwrap());
  }
  (status, headers, Json(body)).into_response()
}

async fn directory(State(shared): State<Shared>) -> Response {
  let base = shared.lock().unwrap().base.clone();
  answer(
    &shared,
    StatusCode::OK,
    None,
    json!({
      "newNonce": format!("{base}/nonce"),
      "newAccount": format!("{base}/account"),
      "newOrder": format!("{base}/order"),
    }),
  )
}

async fn nonce(State(shared): State<Shared>) -> Response {
  answer(&shared, StatusCode::OK, None, json!({}))
}

async fn account(State(shared): State<Shared>, Json(body): Json<Value>) -> Response {
  let (protected, _) = jws(&body);
  // RFC 7638: the required members of an EC key, in lexical order.
  let jwk = &protected["jwk"];
  let canonical = format!(
    r#"{{"crv":"{}","kty":"EC","x":"{}","y":"{}"}}"#,
    jwk["crv"].as_str().unwrap_or(""),
    jwk["x"].as_str().unwrap_or(""),
    jwk["y"].as_str().unwrap_or("")
  );
  let thumb = URL_SAFE_NO_PAD.encode(ring::digest::digest(
    &ring::digest::SHA256,
    canonical.as_bytes(),
  ));
  let base = {
    let mut authority = shared.lock().unwrap();
    authority.thumbprint = Some(thumb);
    authority.base.clone()
  };
  answer(
    &shared,
    StatusCode::CREATED,
    Some(format!("{base}/acct/1")),
    json!({"status": "valid"}),
  )
}

fn order_document(authority: &Authority) -> Value {
  let base = &authority.base;
  let mut order = json!({
    "status": authority.order,
    "identifiers": [{"type": "dns", "value": authority.host}],
    "authorizations": [format!("{base}/authz/1")],
    "finalize": format!("{base}/finalize/1"),
  });
  if authority.certificate.is_some() {
    order["certificate"] = json!(format!("{base}/cert/1"));
  }
  order
}

async fn new_order(State(shared): State<Shared>, Json(body): Json<Value>) -> Response {
  let (_, payload) = jws(&body);
  let (location, document) = {
    let mut authority = shared.lock().unwrap();
    authority.host = payload["identifiers"][0]["value"]
      .as_str()
      .unwrap_or("")
      .to_string();
    authority.authorization = "pending";
    authority.order = "pending";
    authority.certificate = None;
    authority.orders += 1;
    (
      format!("{}/order/1", authority.base),
      order_document(&authority),
    )
  };
  answer(&shared, StatusCode::CREATED, Some(location), document)
}

async fn order(State(shared): State<Shared>, Path(_): Path<String>) -> Response {
  let document = order_document(&shared.lock().unwrap());
  answer(&shared, StatusCode::OK, None, document)
}

fn challenge_document(authority: &Authority) -> Value {
  json!({
    "type": "tls-alpn-01",
    "url": format!("{}/chall/1", authority.base),
    "token": TOKEN,
    "status": match authority.authorization { "valid" => "valid", "invalid" => "invalid", _ => "pending" },
  })
}

async fn authorization(State(shared): State<Shared>, Path(_): Path<String>) -> Response {
  let document = {
    let authority = shared.lock().unwrap();
    json!({
      "identifier": {"type": "dns", "value": authority.host},
      "status": authority.authorization,
      "challenges": [challenge_document(&authority)],
    })
  };
  answer(&shared, StatusCode::OK, None, document)
}

/// What an authority does on `set_ready`: connects to the name's port 443,
/// here the listener, asking for `acme-tls/1`, and reads the proof.
async fn challenge(State(shared): State<Shared>, Path(_): Path<String>) -> Response {
  let (listener, host, thumb, unreachable) = {
    let authority = shared.lock().unwrap();
    (
      authority
        .listener
        .expect("the listener is up before an order"),
      authority.host.clone(),
      authority.thumbprint.clone().unwrap_or_default(),
      authority.unreachable.contains(&authority.host),
    )
  };
  let expected = ring::digest::digest(&ring::digest::SHA256, format!("{TOKEN}.{thumb}").as_bytes());
  let proved = !unreachable && proof_on(listener, &host, expected.as_ref()).await;
  let document = {
    let mut authority = shared.lock().unwrap();
    authority.authorization = if proved { "valid" } else { "invalid" };
    authority.order = if proved { "ready" } else { "invalid" };
    challenge_document(&authority)
  };
  answer(&shared, StatusCode::OK, None, document)
}

/// Reads a proof certificate. The signature check of the handshake goes
/// through webpki, which refuses any certificate with an unknown critical
/// extension, and the acmeIdentifier extension is critical by design: an
/// authority judges the proof by the extension, not by a chain.
#[derive(Debug)]
struct ProofReader;

impl ServerCertVerifier for ProofReader {
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
    _: &[u8],
    _: &CertificateDer<'_>,
    _: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    Ok(HandshakeSignatureValid::assertion())
  }

  fn verify_tls13_signature(
    &self,
    _: &[u8],
    _: &CertificateDer<'_>,
    _: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    Ok(HandshakeSignatureValid::assertion())
  }

  fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
    rustls::crypto::ring::default_provider()
      .signature_verification_algorithms
      .supported_schemes()
  }
}

/// Whether the listener answers `acme-tls/1` for `host` with a certificate
/// carrying `digest` in its acmeIdentifier extension (RFC 8737).
async fn proof_on(listener: SocketAddr, host: &str, digest: &[u8]) -> bool {
  let alpn = [crate::tls::listener::ACME_TLS_ALPN];
  let Ok(stream) = connect_with(listener, host, &alpn, Arc::new(ProofReader)).await else {
    return false;
  };
  let leaf = stream.get_ref().1.peer_certificates().unwrap()[0].clone();
  let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref()).unwrap();
  let oid = x509_parser::oid_registry::Oid::from(&[1, 3, 6, 1, 5, 5, 7, 1, 31]).unwrap();
  cert.extensions().iter().any(|ext| {
    // The value is an OCTET STRING of the 32-byte digest.
    ext.oid == oid
      && ext.value.len() == 34
      && ext.value[..2] == [0x04, 0x20]
      && &ext.value[2..] == digest
  })
}

/// A CSR's public key, handed to rcgen as it is.
struct CsrKey(Vec<u8>);

impl rcgen::PublicKeyData for CsrKey {
  fn der_bytes(&self) -> &[u8] {
    &self.0
  }

  fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
    &rcgen::PKCS_ECDSA_P256_SHA256
  }
}

async fn finalize(
  State(shared): State<Shared>,
  Path(_): Path<String>,
  Json(body): Json<Value>,
) -> Response {
  let (_, payload) = jws(&body);
  let csr = b64(payload["csr"].as_str().unwrap_or(""));
  let (_, request) =
    x509_parser::certification_request::X509CertificationRequest::from_der(&csr).unwrap();
  let key = CsrKey(
    request
      .certification_request_info
      .subject_pki
      .subject_public_key
      .data
      .to_vec(),
  );
  let document = {
    let mut authority = shared.lock().unwrap();
    let host = authority.host.clone();
    let (ca_params, ca_key) = authority.ca.as_ref().unwrap();
    let ca = rcgen::Issuer::from_params(ca_params, ca_key);
    let leaf = rcgen::CertificateParams::new(vec![host])
      .unwrap()
      .signed_by(&key, &ca)
      .unwrap();
    let ca_cert = ca_params.self_signed(ca_key).unwrap();
    authority.certificate = Some(format!("{}{}", leaf.pem(), ca_cert.pem()));
    authority.order = "valid";
    order_document(&authority)
  };
  answer(&shared, StatusCode::OK, None, document)
}

async fn certificate(State(shared): State<Shared>, Path(_): Path<String>) -> Response {
  let pem = shared
    .lock()
    .unwrap()
    .certificate
    .clone()
    .unwrap_or_default();
  let mut response = answer(&shared, StatusCode::OK, None, Value::Null);
  *response.body_mut() = axum::body::Body::from(pem);
  response
}

/// Starts the fake authority; its directory URL, and its state for the test
/// to set the listener on and look into.
async fn start_authority() -> (String, Shared) {
  let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let base = format!("http://{}", tcp.local_addr().unwrap());
  let ca_key = rcgen::KeyPair::generate().unwrap();
  let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
  ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
  ca_params
    .distinguished_name
    .push(rcgen::DnType::CommonName, "Fake ACME CA");
  let shared: Shared = Arc::new(Mutex::new(Authority {
    base: base.clone(),
    ca: Some((ca_params, ca_key)),
    ..Authority::default()
  }));
  let app = axum::Router::new()
    .route("/directory", get(directory))
    .route("/nonce", get(nonce))
    .route("/account", post(account))
    .route("/order", post(new_order))
    .route("/order/{id}", post(order))
    .route("/authz/{id}", post(authorization))
    .route("/chall/{id}", post(challenge))
    .route("/finalize/{id}", post(finalize))
    .route("/cert/{id}", post(certificate))
    .with_state(shared.clone());
  tokio::spawn(async move {
    let _ = axum::serve(tcp, app).await;
  });
  (format!("{base}/directory"), shared)
}

fn temp_dir(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-acme-{name}-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

/// An issuer for `hostnames` against the fake authority, the listener it
/// proves on, and where it keeps things.
async fn issuer_against(
  directory: &str,
  shared: &Shared,
  hostnames: &[&str],
  name: &str,
) -> (Arc<Issuer>, SocketAddr, PathBuf) {
  let dir = temp_dir(name);
  let certs = Arc::new(CertStore::new(&[], Some(dir.clone())));
  let challenges = Arc::new(Challenges::default());
  let mut issuer = Issuer::new(
    AcmePlan {
      directory: directory.to_string(),
      email: Some("ops@example.com".into()),
      hostnames: hostnames.iter().map(|h| h.to_string()).collect(),
      on_demand: true,
    },
    dir.clone(),
    certs.clone(),
    challenges.clone(),
    None,
  );
  issuer.patience = Duration::from_secs(10);
  let issuer = Arc::new(issuer);
  let listener = serve_https(certs, challenges, Some(issuer.clone())).await;
  shared.lock().unwrap().listener = Some(listener);
  (issuer, listener, dir)
}

#[tokio::test]
async fn a_certificate_is_obtained_over_tls_alpn_and_then_presented() {
  crate::ensure_crypto_provider();
  let (directory, shared) = start_authority().await;
  let (issuer, listener, dir) = issuer_against(&directory, &shared, &[], "issue").await;

  issuer
    .ensure("app.example.com", Why::Configured)
    .await
    .unwrap();

  let stream = connect(listener, "app.example.com", &[b"http/1.1"])
    .await
    .unwrap();
  assert_eq!(presented_names(&stream), ["app.example.com"]);
  // Kept beside the account, for the next start.
  assert!(dir.join("app.example.com.pem").exists());
  assert!(
    std::fs::read_dir(&dir)
      .unwrap()
      .flatten()
      .any(|e| e.file_name().to_string_lossy().starts_with("account-"))
  );
  // Held now, so asking again places no second order.
  issuer
    .ensure("app.example.com", Why::Configured)
    .await
    .unwrap();
  assert_eq!(shared.lock().unwrap().orders, 1);
  let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_handshake_for_a_configured_name_waits_for_its_certificate() {
  crate::ensure_crypto_provider();
  let (directory, shared) = start_authority().await;
  let (_issuer, listener, dir) =
    issuer_against(&directory, &shared, &["tunnel.example.com"], "wait").await;

  // Nothing is held yet: the first visitor's handshake is what obtains it.
  let stream = connect(listener, "tunnel.example.com", &[b"http/1.1"])
    .await
    .unwrap();
  assert_eq!(presented_names(&stream), ["tunnel.example.com"]);
  // A name that is neither configured nor served, with no state to ask, is
  // never ordered.
  assert!(
    connect(listener, "other.example.com", &[b"http/1.1"])
      .await
      .is_err()
  );
  assert_eq!(shared.lock().unwrap().orders, 1);
  let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_refused_proof_is_a_failure_and_the_name_waits_before_another_order() {
  crate::ensure_crypto_provider();
  let (directory, shared) = start_authority().await;
  shared
    .lock()
    .unwrap()
    .unreachable
    .push("down.example.com".into());
  let (issuer, _listener, dir) = issuer_against(&directory, &shared, &[], "refused").await;

  let first = issuer
    .ensure("down.example.com", Why::Configured)
    .await
    .unwrap_err();
  assert!(
    first.contains("could not verify down.example.com"),
    "{first}"
  );
  let second = issuer
    .ensure("down.example.com", Why::Configured)
    .await
    .unwrap_err();
  assert!(second.contains("the next is in"), "{second}");
  assert_eq!(
    shared.lock().unwrap().orders,
    1,
    "the wait placed no second order"
  );
  // And the proof was withdrawn with the order.
  assert!(issuer.challenges.get("down.example.com").is_none());
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn on_demand_orders_are_capped_per_hour() {
  let issuer = Issuer::new(
    AcmePlan {
      directory: "https://acme.example.com/directory".into(),
      email: None,
      hostnames: Vec::new(),
      on_demand: true,
    },
    std::env::temp_dir(),
    Arc::new(CertStore::new(&[], None)),
    Arc::default(),
    None,
  );
  for _ in 0..ON_DEMAND_PER_HOUR {
    issuer.take_on_demand_slot().unwrap();
  }
  assert!(
    issuer
      .take_on_demand_slot()
      .unwrap_err()
      .contains("in the last hour")
  );
}

#[tokio::test]
async fn a_panels_certificate_is_a_configured_one_not_an_on_demand_one() {
  // The dashboard's hostname and an organization's panel are names the
  // operator wrote, so they are obtained like `tls.acme.hostnames`, outside
  // the hourly on-demand allowance; a name a client merely serves is not.
  let mut cfg = crate::test_support::test_config();
  cfg.dashboard_hostname = Some("panel.example.com".to_string());
  let state = Arc::new(crate::test_support::test_state_with(cfg));
  let issuer = Issuer::new(
    AcmePlan {
      directory: "https://acme.example.com/directory".into(),
      email: None,
      hostnames: vec!["app.example.com".into()],
      on_demand: false,
    },
    std::env::temp_dir(),
    Arc::new(CertStore::new(&[], None)),
    Arc::default(),
    Some(state),
  );
  assert_eq!(issuer.reason_for("app.example.com").await, Why::Configured);
  assert_eq!(
    issuer.reason_for("panel.example.com").await,
    Why::Configured
  );
  assert_eq!(issuer.reason_for("other.example.com").await, Why::OnDemand);
}
