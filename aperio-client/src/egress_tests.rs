//! What these pin down about the egress proxy: the spellings an operator may
//! write, the fact that a credential never appears in anything printable, and
//! that a refusal names the proxy and the status rather than failing as a
//! generic dial.

use super::*;

#[test]
fn the_spellings_an_operator_may_write_all_parse() {
  for (raw, host, port) in [
    ("http://proxy.corp:3128", "proxy.corp", 3128),
    ("proxy.corp:3128", "proxy.corp", 3128),
    ("HTTP://Proxy.Corp:3128", "Proxy.Corp", 3128),
    ("http://proxy.corp:3128/", "proxy.corp", 3128),
    ("  proxy.corp:3128  ", "proxy.corp", 3128),
    ("10.0.0.9:8080", "10.0.0.9", 8080),
    ("[2001:db8::1]:3128", "2001:db8::1", 3128),
    // No port is the scheme default rather than an error, since that is what
    // every other tool does with the same string.
    ("http://proxy.corp", "proxy.corp", 80),
  ] {
    let proxy = EgressProxy::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
    assert_eq!((proxy.host.as_str(), proxy.port), (host, port), "{raw}");
    assert!(!proxy.has_credentials(), "{raw} carries no credential");
  }
}

#[test]
fn a_credential_is_encoded_and_never_printed() {
  let proxy = EgressProxy::parse("http://alice:s3cret@proxy.corp:3128").unwrap();
  assert_eq!(proxy.host, "proxy.corp");
  assert_eq!(proxy.port, 3128);
  assert!(proxy.has_credentials());

  // The three ways this value can reach a human: the redacted form, Debug,
  // and the CONNECT request. Only the last may carry it.
  assert_eq!(proxy.redacted(), "proxy.corp:3128");
  assert!(!proxy.redacted().contains("s3cret"));
  let debugged = format!("{proxy:?}");
  assert!(!debugged.contains("s3cret"), "Debug leaked it: {debugged}");
  assert!(!debugged.contains("alice"), "Debug leaked it: {debugged}");

  let request = String::from_utf8(proxy.request("tunnel.example.com", 443)).unwrap();
  assert!(
    request.contains("Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n"),
    "{request}"
  );
}

#[test]
fn a_password_containing_an_at_sign_stays_with_the_credential() {
  // Split on the *last* `@`, or a password like `p@ss` takes the host with it.
  let proxy = EgressProxy::parse("http://alice:p@ss@proxy.corp:3128").unwrap();
  assert_eq!(proxy.redacted(), "proxy.corp:3128");
  let request = String::from_utf8(proxy.request("h", 443)).unwrap();
  let expected = base64::engine::general_purpose::STANDARD.encode("alice:p@ss");
  assert!(request.contains(&expected), "{request}");
}

#[test]
fn the_values_that_cannot_work_are_refused_with_the_reason() {
  for (raw, needle) in [
    ("", "empty"),
    ("https://proxy.corp:3128", "https://"),
    ("socks5://proxy.corp:1080", "not supported"),
    ("http://proxy.corp:3128/some/path", "has a path"),
    ("http://proxy.corp:notaport", "no usable port"),
    ("[2001:db8::1", "never closes"),
  ] {
    let err = EgressProxy::parse(raw).unwrap_err();
    assert!(err.contains(needle), "{raw} -> {err}");
  }
}

#[test]
fn a_credential_is_hidden_even_when_the_value_fails_to_parse() {
  // The failure message is printed, so it is one of the places a password can
  // escape, and the value that failed is exactly the one most likely to be a
  // paste of something private.
  let err = EgressProxy::parse("http://alice:s3cret@proxy.corp:3128/path").unwrap_err();
  assert!(!err.contains("s3cret"), "{err}");
  assert!(err.contains("***@"), "{err}");
}

#[test]
fn the_request_is_a_well_formed_connect() {
  let proxy = EgressProxy::parse("proxy.corp:3128").unwrap();
  let request = String::from_utf8(proxy.request("tunnel.example.com", 443)).unwrap();
  assert!(request.starts_with("CONNECT tunnel.example.com:443 HTTP/1.1\r\n"));
  assert!(request.contains("Host: tunnel.example.com:443\r\n"));
  assert!(request.ends_with("\r\n\r\n"));
  assert!(!request.contains("Proxy-Authorization"));
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

#[tokio::test]
async fn a_proxy_that_drops_the_connection_names_itself_too() {
  // The other end of the same story, and the reason the wording is not
  // asserted here: a peer that closes on an unread request produces a reset
  // on some platforms and an end of stream on others. What has to hold either
  // way is that the operator is told which proxy failed them.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    let (sock, _) = listener.accept().await.unwrap();
    drop(sock);
  });
  let sock = TcpStream::connect(addr).await.unwrap();
  let proxy = EgressProxy::parse(&format!("{addr}")).unwrap();
  let err = connect_through(sock, &proxy, "h", 443).await.unwrap_err();
  assert!(err.contains(&format!("{addr}")), "{err}");
  assert!(err.contains("CONNECT"), "{err}");
}
