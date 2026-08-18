//! What reaches the backend and what comes back: header rules in both directions,
//! the redirect policy (same-site followed, cross-site passed through), path-bind
//! trimming at segment boundaries, streaming past the buffer threshold, and the
//! answers a visitor gets when the backend is unreachable.

use super::*;

/// Tunnel sender whose receiver is drained in the background, for tests
/// that exercise the buffered (non-streaming) response path.
pub(crate) fn test_tunnel_tx() -> mpsc::Sender<Message> {
  let (tx, mut rx) = mpsc::channel::<Message>(64);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  tx
}

/// Default forwarding context against the given mock target.
pub(crate) fn test_ctx(target: &str, tunnel_tx: mpsc::Sender<Message>) -> ForwardContext {
  ForwardContext {
    client: reqwest::Client::new(),
    h2_client: None,
    unix_socket: None,
    timeout_secs: 30,
    stream_pauses: Default::default(),
    resilience: crate::proxy::http::BackendResilience::new(1, 100, false, 0, 30),
    target: target.to_string(),
    target_url: url::Url::parse(target).ok(),
    pass_hostname: false,
    path_bind: None,
    trim_bind: false,
    max_response_body_size: 1024 * 1024,
    tunnel_tx,
    request_headers: HeaderTransform::default(),
    response_headers: HeaderTransform::default(),
  }
}

/// Minimal HTTP server answering every request with the given response.
pub(crate) async fn mock_server(response: String) -> u16 {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    while let Ok((mut socket, _)) = listener.accept().await {
      let resp = response.clone();
      tokio::spawn(async move {
        let mut buf = [0; 2048];
        let _ = socket.read(&mut buf).await;
        let _ = socket.write_all(resp.as_bytes()).await;
      });
    }
  });
  port
}

#[tokio::test]
pub(crate) async fn test_handle_incoming_request() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  // Spawn a mock target server
  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]);

      // Check that request contains original path and custom header
      assert!(req_str.contains("GET /test-path"));
      assert!(req_str.contains("x-custom-header: custom-value"));

      // Write back a simple HTTP response
      let response =
        "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nContent-Type: text/plain\r\n\r\nhello from local";
      socket.write_all(response.as_bytes()).await.unwrap();
    }
  });

  let headers = vec![("x-custom-header".to_string(), "custom-value".to_string())];

  let ctx = test_ctx(&target_url, test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-id-123".to_string(),
      method: "GET".to_string(),
      uri: "/test-path".to_string(),
      headers,
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  if let TunnelMessage::Response {
    id,
    status,
    headers,
    body,
    ..
  } = result
  {
    assert_eq!(id, "req-id-123");
    assert_eq!(status, 200);
    let ct = headers
      .iter()
      .find(|(k, _)| k == "content-type")
      .map(|(_, v)| v)
      .unwrap();
    assert_eq!(ct, "text/plain");
    let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "hello from local");
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
pub(crate) async fn test_backend_connection_refused_502() {
  // No server listening on this port → reqwest send() fails → 502.
  let ctx = test_ctx("http://127.0.0.1:1", test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-refused".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 502);
}

#[tokio::test]
pub(crate) async fn test_invalid_method_400() {
  let ctx = test_ctx("http://127.0.0.1:1", test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-badm".to_string(),
      method: "BAD METHOD".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
pub(crate) async fn test_bad_base64_body_400() {
  let ctx = test_ctx("http://127.0.0.1:1", test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-b64".to_string(),
      method: "POST".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: Some("!!not-base64!!".to_string()),
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
pub(crate) async fn test_unparsable_incoming_uri_400() {
  // The incoming URI is spliced into `http://localhost<uri>`; an invalid port
  // makes that URL unparsable → build_dest_url returns 400.
  let ctx = test_ctx("http://127.0.0.1:1", test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-uri".to_string(),
      method: "GET".to_string(),
      uri: ":notaport".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
pub(crate) async fn test_unparsable_target_url_502() {
  // A malformed target URL fails to parse in build_dest_url → 502.
  let ctx = test_ctx("http://[bad", test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-badtarget".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 502);
}

// --- ChunkCoalescer ---------------------------------------------------------

// --- BackendResilience (planned_features #29) -------------------------------

// ---------------------------------------------------------------------------
// tls_floor (planned_features #59)
// ---------------------------------------------------------------------------

/// This path strips everything every path to a backend strips.
///
/// See the h2 twin: the list is shared so that each side asserts it in its own
/// suite, rather than one side policing the other's from a suite it never
/// triggers.
#[test]
fn the_http1_path_strips_the_shared_set() {
  aperio_config::hop_by_hop::strips_the_core(is_hop_by_hop).expect("the shared strip");
  aperio_config::hop_by_hop::leaves_ordinary_headers(is_hop_by_hop).expect("ordinary headers");
  // This path's own two, recorded so a change to either is deliberate.
  assert!(
    is_hop_by_hop("trailer"),
    "on HTTP/1 a visitor can write this framing header"
  );
  assert!(
    !is_hop_by_hop("host"),
    "taken out of the loop and re-added once by pass_hostname, not stripped"
  );
}
