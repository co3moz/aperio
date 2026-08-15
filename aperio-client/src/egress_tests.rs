//! What these pin down about the client's half of the egress proxy: the
//! `CONNECT` request it writes, and that a refusal names the proxy and the
//! status rather than failing as a generic dial.
//!
//! Parsing, redaction and the bypass rule are not here: they live in
//! `aperio-config::egress` with the type, because the server needs the same
//! value and a second copy of a redaction rule is one too many.

use super::*;

#[test]
fn the_request_is_a_well_formed_connect() {
  let proxy = EgressProxy::parse("proxy.corp:3128").unwrap();
  let request = String::from_utf8(connect_request(&proxy, "tunnel.example.com", 443)).unwrap();
  assert!(request.starts_with("CONNECT tunnel.example.com:443 HTTP/1.1\r\n"));
  assert!(request.contains("Host: tunnel.example.com:443\r\n"));
  assert!(request.ends_with("\r\n\r\n"));
  assert!(!request.contains("Proxy-Authorization"));
}

#[test]
fn a_credential_is_encoded_into_the_request_and_nowhere_else() {
  let proxy = EgressProxy::parse("http://alice:s3cret@proxy.corp:3128").unwrap();
  let request = String::from_utf8(connect_request(&proxy, "tunnel.example.com", 443)).unwrap();
  assert!(
    request.contains("Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n"),
    "{request}"
  );
  // The request is the only place it may appear; everything printable about
  // the proxy is host and port.
  assert!(!proxy.redacted().contains("s3cret"));
  assert!(!format!("{proxy:?}").contains("s3cret"));
}

/// A proxy that answers `answer` to whatever it is sent, once.
async fn proxy_answering(
  answer: &'static [u8],
) -> (std::net::SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let handle = tokio::spawn(async move {
    let (mut sock, _) = listener.accept().await.unwrap();
    let mut seen = vec![0u8; 512];
    let n = sock.read(&mut seen).await.unwrap_or(0);
    seen.truncate(n);
    sock.write_all(answer).await.unwrap();
    // Held open so the caller's stream stays usable on the success path.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    seen
  });
  (addr, handle)
}

#[tokio::test]
async fn an_established_tunnel_hands_back_the_stream() {
  let (addr, handle) = proxy_answering(b"HTTP/1.1 200 Connection established\r\n\r\n").await;
  let sock = TcpStream::connect(addr).await.unwrap();
  let proxy = EgressProxy::parse(&format!("{addr}")).unwrap();
  let out = connect_through(sock, &proxy, "tunnel.example.com", 443).await;
  assert!(out.is_ok(), "{:?}", out.err());
  let sent = String::from_utf8(handle.await.unwrap()).unwrap();
  assert!(sent.starts_with("CONNECT tunnel.example.com:443"), "{sent}");
}

#[tokio::test]
async fn a_refusal_names_the_proxy_and_the_status() {
  let (addr, _h) = proxy_answering(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
  let sock = TcpStream::connect(addr).await.unwrap();
  let proxy = EgressProxy::parse(&format!("{addr}")).unwrap();
  let err = connect_through(sock, &proxy, "tunnel.example.com", 443)
    .await
    .unwrap_err();
  assert!(err.contains(&format!("{addr}")), "{err}");
  assert!(err.contains("403"), "{err}");
  assert!(err.contains("tunnel.example.com:443"), "{err}");
  assert!(err.contains("refused this destination"), "{err}");
}

#[tokio::test]
async fn a_407_says_which_half_of_the_credential_problem_it_is() {
  // Without a credential configured, the answer is "add one"; with one, it is
  // "the one you have was rejected". Same status, different work.
  let (addr, _h) = proxy_answering(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
  let sock = TcpStream::connect(addr).await.unwrap();
  let bare = EgressProxy::parse(&format!("{addr}")).unwrap();
  let err = connect_through(sock, &bare, "h", 443).await.unwrap_err();
  assert!(err.contains("wants a credential"), "{err}");

  let (addr, _h) = proxy_answering(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
  let sock = TcpStream::connect(addr).await.unwrap();
  let with = EgressProxy::parse(&format!("alice:pw@{addr}")).unwrap();
  let err = connect_through(sock, &with, "h", 443).await.unwrap_err();
  assert!(err.contains("rejected the credential"), "{err}");
  assert!(
    !err.contains("pw"),
    "the credential leaked into the error: {err}"
  );
}

#[test]
fn a_status_line_is_read_out_of_the_response_head() {
  assert_eq!(
    connect_status(b"HTTP/1.1 200 Connection established\r\n\r\n"),
    Some(200)
  );
  assert_eq!(connect_status(b"HTTP/1.0 200 OK\r\n\r\n"), Some(200));
  assert_eq!(
    connect_status(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"),
    Some(407)
  );
  // Not a status line at all: a proxy that answers with something else must
  // be reported as such rather than read as a success.
  assert_eq!(connect_status(b"<html>no</html>\r\n\r\n"), None);
  assert_eq!(connect_status(b"200 OK\r\n\r\n"), None);
}

#[tokio::test]
async fn a_proxy_that_never_answers_is_given_up_on_rather_than_waited_on_forever() {
  // The socket is open by the time CONNECT is sent, so nothing above this is
  // watching the clock: the dial's connect timeout is already spent and its
  // handshake timeout has not started. Without a budget here the reconnect
  // loop gets no error to act on and the service stays down in silence, which
  // is the failure HANDSHAKE_TIMEOUT exists to prevent one layer down.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let held = tokio::spawn(async move {
    let (sock, _) = listener.accept().await.unwrap();
    // Accepted and kept, deliberately unanswered, for longer than the budget.
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    drop(sock);
  });

  let sock = TcpStream::connect(addr).await.unwrap();
  let proxy = EgressProxy::parse(&format!("{addr}")).unwrap();
  let started = std::time::Instant::now();
  let err = connect_through_within(
    sock,
    &proxy,
    "tunnel.example.com",
    443,
    std::time::Duration::from_millis(300),
  )
  .await
  .unwrap_err();

  assert!(
    started.elapsed() < std::time::Duration::from_secs(5),
    "it gave up promptly"
  );
  assert!(err.contains("did not answer CONNECT"), "{err}");
  assert!(err.contains(&format!("{addr}")), "{err}");
  held.abort();
}

#[tokio::test]
async fn a_proxy_that_accepts_and_then_says_nothing_is_reported() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    let (mut sock, _) = listener.accept().await.unwrap();
    // Read the CONNECT before closing, so the peer sees an orderly end of
    // stream. Closing on an unread request is a reset instead, which is a
    // different failure and not the one this is about.
    let mut buf = [0u8; 512];
    let _ = sock.read(&mut buf).await;
    drop(sock);
  });
  let sock = TcpStream::connect(addr).await.unwrap();
  let proxy = EgressProxy::parse(&format!("{addr}")).unwrap();
  let err = connect_through(sock, &proxy, "h", 443).await.unwrap_err();
  assert!(err.contains("closed the connection"), "{err}");
  assert!(err.contains(&format!("{addr}")), "{err}");
}
