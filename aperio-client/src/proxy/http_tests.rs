use super::*;

/// Tunnel sender whose receiver is drained in the background, for tests
/// that exercise the buffered (non-streaming) response path.
fn test_tunnel_tx() -> mpsc::Sender<Message> {
  let (tx, mut rx) = mpsc::channel::<Message>(64);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  tx
}

/// Default forwarding context against the given mock target.
fn test_ctx(target: &str, tunnel_tx: mpsc::Sender<Message>) -> ForwardContext {
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

#[test]
fn test_header_transform_apply() {
  // No rules = identity (fast path).
  let noop = HeaderTransform::default();
  let headers = vec![("x-a".to_string(), "1".to_string())];
  assert_eq!(noop.apply(headers.clone()), headers);

  let directives = aperio_config::HeaderDirectives {
    add: [("X-Env".to_string(), "staging".to_string())]
      .into_iter()
      .collect(),
    remove: vec!["Server".to_string()],
  };
  let t = HeaderTransform::compile(Some(&directives));
  let out = t.apply(vec![
    ("server".to_string(), "nginx".to_string()), // removed (case-insensitive)
    ("x-env".to_string(), "prod".to_string()),   // replaced by the add
    ("x-keep".to_string(), "yes".to_string()),
  ]);
  assert_eq!(
    out,
    vec![
      ("x-keep".to_string(), "yes".to_string()),
      ("X-Env".to_string(), "staging".to_string()),
    ]
  );
}

#[test]
fn test_same_site() {
  // Exact and case/dot-insensitive matches.
  assert!(same_site("example.com", "example.com"));
  assert!(same_site("Example.COM.", "example.com"));
  // Same root domain: parent↔child and siblings.
  assert!(same_site("example.com", "test.example.com"));
  assert!(same_site("a.example.com", "b.example.com"));
  assert!(same_site("x.y.example.com", "example.com"));
  // Different domains never match.
  assert!(!same_site("example.com", "evil.com"));
  assert!(!same_site("example.com", "example.org"));
  // IPs and single-label hosts only match exactly.
  assert!(same_site("127.0.0.1", "127.0.0.1"));
  assert!(!same_site("127.0.0.1", "127.0.0.2"));
  assert!(same_site("localhost", "localhost"));
  assert!(!same_site("localhost", "example.com"));
}

/// Minimal HTTP server answering every request with the given response.
async fn mock_server(response: String) -> u16 {
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
async fn test_redirects_followed_same_host() {
  // Target redirects to a second local server on the same host (127.0.0.1);
  // the client must follow it transparently and return the final 200.
  let final_port = mock_server(
    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfinal".to_string(),
  )
  .await;
  let first_port = mock_server(format!(
    "HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:{}/moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    final_port
  ))
  .await;

  let ctx = ForwardContext {
    client: reqwest::Client::builder()
      .redirect(redirect_policy(5))
      .build()
      .unwrap(),
    ..test_ctx(
      &format!("http://127.0.0.1:{}", first_port),
      test_tunnel_tx(),
    )
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-redir-1".to_string(),
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
  .expect("expected buffered response");

  if let TunnelMessage::Response { status, body, .. } = result {
    assert_eq!(status, 200);
    let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "final");
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
async fn test_redirects_passed_through_cross_site() {
  // A redirect to an unrelated domain must NOT be followed: the 301 goes
  // back through the tunnel untouched.
  let first_port = mock_server(
    "HTTP/1.1 301 Moved Permanently\r\nLocation: http://unrelated.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
      .to_string(),
  )
  .await;

  let ctx = ForwardContext {
    client: reqwest::Client::builder()
      .redirect(redirect_policy(5))
      .build()
      .unwrap(),
    ..test_ctx(
      &format!("http://127.0.0.1:{}", first_port),
      test_tunnel_tx(),
    )
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-redir-2".to_string(),
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
  .expect("expected buffered response");

  if let TunnelMessage::Response {
    status, headers, ..
  } = result
  {
    assert_eq!(status, 301);
    let loc = headers
      .iter()
      .find(|(k, _)| k == "location")
      .map(|(_, v)| v.as_str());
    assert_eq!(loc, Some("http://unrelated.invalid/"));
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
async fn test_make_error_response() {
  let response = make_error_response("req-123".to_string(), 502);
  if let TunnelMessage::Response {
    id,
    status,
    headers,
    body,
    ..
  } = response
  {
    assert_eq!(id, "req-123");
    assert_eq!(status, 502);
    let ct = headers
      .iter()
      .find(|(k, _)| k == "content-type")
      .map(|(_, v)| v)
      .unwrap();
    assert_eq!(ct, "text/plain");
    let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
    let decoded_str = String::from_utf8(decoded).unwrap();
    assert!(decoded_str.contains("502 Bad Gateway"));
  } else {
    panic!("Expected Response variant");
  }
}

#[tokio::test]
async fn test_handle_incoming_request() {
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
async fn test_pass_hostname_sends_exactly_one_host_header() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 2048];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_lowercase();
      // The visitor's Host must be forwarded exactly once (a duplicate is a
      // protocol violation that strict backends reject with 400).
      let host_lines = req_str.matches("\r\nhost:").count();
      assert_eq!(
        host_lines, 1,
        "expected exactly one host header, got: {req_str}"
      );
      assert!(
        req_str.contains("host: app.example.com"),
        "visitor host must be passed through, got: {req_str}"
      );

      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      socket.write_all(response.as_bytes()).await.unwrap();
    }
  });

  let mut ctx = test_ctx(&target_url, test_tunnel_tx());
  ctx.pass_hostname = true;

  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-host".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![("host".to_string(), "app.example.com".to_string())],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected response variant");
  };
  // The mock asserts inside its task; a 200 here means the read succeeded
  // (an assert failure in the task would leave the connection unanswered).
  assert_eq!(status, 200);
}

#[tokio::test]
async fn test_handle_incoming_request_header_rules() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 2048];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_lowercase();
      // The request rules injected a header and stripped another.
      assert!(req_str.contains("x-env: staging"), "got: {req_str}");
      assert!(!req_str.contains("x-secret"), "got: {req_str}");

      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nServer: mock\r\nX-Old: 1\r\n\r\nok";
      socket.write_all(response.as_bytes()).await.unwrap();
    }
  });

  let mut ctx = test_ctx(&target_url, test_tunnel_tx());
  ctx.request_headers = HeaderTransform::compile(Some(&aperio_config::HeaderDirectives {
    add: [("X-Env".to_string(), "staging".to_string())]
      .into_iter()
      .collect(),
    remove: vec!["X-Secret".to_string()],
  }));
  ctx.response_headers = HeaderTransform::compile(Some(&aperio_config::HeaderDirectives {
    add: [("X-Served-By".to_string(), "aperio".to_string())]
      .into_iter()
      .collect(),
    remove: vec!["Server".to_string()],
  }));

  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-headers".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![("x-secret".to_string(), "leak-me-not".to_string())],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let TunnelMessage::Response { headers, .. } = result else {
    panic!("Expected response variant");
  };
  assert!(
    headers
      .iter()
      .any(|(k, v)| k == "X-Served-By" && v == "aperio"),
    "got: {headers:?}"
  );
  assert!(
    !headers
      .iter()
      .any(|(k, _)| k.eq_ignore_ascii_case("server")),
    "got: {headers:?}"
  );
  // Untouched backend headers pass through.
  assert!(
    headers.iter().any(|(k, _)| k == "x-old"),
    "got: {headers:?}"
  );
}

#[tokio::test]
async fn test_handle_incoming_request_streams_large_body() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  // Body larger than STREAM_THRESHOLD (256 KB) → must be streamed.
  let body_size = 600 * 1024;

  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let _ = socket.read(&mut buf).await.unwrap();
      let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
        body_size
      );
      socket.write_all(header.as_bytes()).await.unwrap();
      let payload = vec![0xABu8; body_size];
      socket.write_all(&payload).await.unwrap();
      // Do NOT shutdown here: on Windows, queueing a FIN while the tail of
      // the payload is still undelivered (peer window transiently zero) puts
      // the closing connection into zero-window probing, and the stack
      // aborts it with a RST after ~5 probes (~19 s), truncating the body.
      // Keep the socket fully open and wait for the peer to finish reading;
      // the task (and socket) is dropped when the test runtime shuts down.
      let mut sink = [0u8; 1024];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, mut rx) = mpsc::channel::<Message>(256);
  let ctx = ForwardContext {
    max_response_body_size: 10 * 1024 * 1024,
    ..test_ctx(&target_url, tx)
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-stream-1".to_string(),
      method: "GET".to_string(),
      uri: "/big".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await;

  // Streamed responses return None; the messages went through the channel.
  assert!(result.is_none(), "large body should be streamed");

  let mut got_start = false;
  let mut got_end = false;
  let mut total_bytes = 0usize;
  while let Some(Message::Text(json)) = rx.recv().await {
    match serde_json::from_str::<TunnelMessage>(&json).unwrap() {
      TunnelMessage::ResponseStart { id, status, .. } => {
        assert_eq!(id, "req-stream-1");
        assert_eq!(status, 200);
        got_start = true;
      }
      TunnelMessage::ResponseChunk { data, .. } => {
        assert!(got_start, "chunk before start");
        total_bytes += BASE64_STANDARD.decode(data).unwrap().len();
      }
      TunnelMessage::ResponseEnd { .. } => {
        got_end = true;
        break;
      }
      other => panic!("unexpected message: {:?}", other),
    }
  }
  assert!(got_start && got_end);
  assert_eq!(total_bytes, body_size);
}

#[tokio::test]
async fn test_handle_incoming_request_trim_bind() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  use tokio::sync::oneshot;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  // Channel to receive the observed request line from the mock server.
  let (tx, rx) = oneshot::channel::<String>();

  tokio::spawn(async move {
    let _tx = tx;
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
      let request_line = req_str.lines().next().unwrap_or("").to_string();
      // Send the observed request line back, then write a minimal response.
      let _ = _tx.send(request_line);
      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      let _ = socket.write_all(response.as_bytes()).await;
    }
  });

  // path_bind = "/api", trim_bind = true → /api/hello should become /hello
  let ctx = ForwardContext {
    path_bind: Some("/api".to_string()),
    trim_bind: true,
    ..test_ctx(&target_url, test_tunnel_tx())
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-trim-1".to_string(),
      method: "GET".to_string(),
      uri: "/api/hello".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let observed = rx.await.unwrap();
  // The mock server should have received the trimmed path "/hello".
  assert!(
    observed.contains("GET /hello"),
    "expected trimmed path '/hello' in request line, got: {}",
    observed
  );

  if let TunnelMessage::Response { status, .. } = result {
    assert_eq!(status, 200);
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
async fn test_build_dest_url_trims_only_at_segment_boundary() {
  let ctx = ForwardContext {
    path_bind: Some("/api".to_string()),
    trim_bind: true,
    ..test_ctx("http://127.0.0.1:1", test_tunnel_tx())
  };
  let path = |uri: &str| build_dest_url(&ctx, "id", uri).unwrap().path().to_string();
  // Exact bind and a sub-path are trimmed.
  assert_eq!(path("/api/hello"), "/hello");
  assert_eq!(path("/api"), "/");
  // A different route that merely shares the `api` prefix must NOT be trimmed.
  assert_eq!(path("/apiv2/hello"), "/apiv2/hello");
  assert_eq!(path("/apitrash"), "/apitrash");
}

#[tokio::test]
async fn test_handle_incoming_request_trim_bind_disabled() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  use tokio::sync::oneshot;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  let (tx, rx) = oneshot::channel::<String>();

  tokio::spawn(async move {
    let _tx = tx;
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
      let request_line = req_str.lines().next().unwrap_or("").to_string();
      let _ = _tx.send(request_line);
      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      let _ = socket.write_all(response.as_bytes()).await;
    }
  });

  // path_bind = "/api", trim_bind = false → path should NOT be stripped
  let ctx = ForwardContext {
    path_bind: Some("/api".to_string()),
    trim_bind: false,
    ..test_ctx(&target_url, test_tunnel_tx())
  };
  let _result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-trim-2".to_string(),
      method: "GET".to_string(),
      uri: "/api/hello".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await;

  let observed = rx.await.unwrap();
  assert!(
    observed.contains("GET /api/hello"),
    "expected untrimmed path '/api/hello' in request line, got: {}",
    observed
  );
}

#[tokio::test]
async fn test_backend_connection_refused_502() {
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
async fn test_invalid_method_400() {
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
async fn test_bad_base64_body_400() {
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
async fn test_unparsable_incoming_uri_400() {
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
async fn test_unparsable_target_url_502() {
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

#[tokio::test]
async fn test_streamed_request_body_forwarded() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  use tokio::sync::oneshot;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);
  let (tx, rx) = oneshot::channel::<String>();

  tokio::spawn(async move {
    let _tx = tx;
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = vec![0u8; 4096];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
      let _ = _tx.send(req_str);
      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      let _ = socket.write_all(response.as_bytes()).await;
    }
  });

  // Feed the request body through the streamed-body channel (v2 path).
  let (btx, brx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
  btx.send(Ok(b"stream-".to_vec())).await.unwrap();
  btx.send(Ok(b"payload".to_vec())).await.unwrap();
  drop(btx);

  let ctx = test_ctx(&target_url, test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-sbody".to_string(),
      method: "POST".to_string(),
      uri: "/upload".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    Some(brx),
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let observed = rx.await.unwrap();
  // The body arrives chunk-encoded (transfer-encoding: chunked); assert both
  // parts reached the backend.
  assert!(
    observed.contains("transfer-encoding: chunked")
      && observed.contains("stream-")
      && observed.contains("payload"),
    "streamed body must reach the backend, got: {observed}"
  );
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 200);
}

#[tokio::test]
async fn test_redirect_stops_after_max_hops() {
  // A→B→C chain with max_hops=1: the second hop exceeds the limit, so the
  // redirect is passed through instead of followed (attempt.stop()).
  let c_port =
    mock_server("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nend".to_string())
      .await;
  let b_port = mock_server(format!(
    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/c\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    c_port
  ))
  .await;
  let a_port = mock_server(format!(
    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/b\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    b_port
  ))
  .await;

  let ctx = ForwardContext {
    client: reqwest::Client::builder()
      .redirect(redirect_policy(1))
      .build()
      .unwrap(),
    ..test_ctx(&format!("http://127.0.0.1:{}", a_port), test_tunnel_tx())
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-hops".to_string(),
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
  .expect("expected buffered response");
  // The second redirect (B→C) is not followed → the 302 is returned as-is.
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 302);
}

#[tokio::test]
async fn test_stream_truncated_at_limit() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);
  let body_size = 600 * 1024;

  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let _ = socket.read(&mut buf).await.unwrap();
      let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
        body_size
      );
      socket.write_all(header.as_bytes()).await.unwrap();
      // Write the payload in slices so more chunks arrive after streaming
      // begins, letting the size cap trip mid-stream.
      let payload = vec![0xCDu8; body_size];
      for part in payload.chunks(64 * 1024) {
        if socket.write_all(part).await.is_err() {
          break;
        }
      }
      let mut sink = [0u8; 1024];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, mut rx) = mpsc::channel::<Message>(256);
  // Cap below the payload so the streamed response is truncated.
  let ctx = ForwardContext {
    max_response_body_size: 300 * 1024,
    ..test_ctx(&target_url, tx)
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-trunc".to_string(),
      method: "GET".to_string(),
      uri: "/big".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await;
  assert!(result.is_none(), "large body streams");

  let mut got_abort = false;
  while let Some(Message::Text(json)) = rx.recv().await {
    if let TunnelMessage::ResponseAbort { .. } =
      serde_json::from_str::<TunnelMessage>(&json).unwrap()
    {
      got_abort = true;
      break;
    }
  }
  assert!(
    got_abort,
    "a truncated stream must terminate with ResponseAbort, not a clean ResponseEnd"
  );
}

#[test]
fn test_redirect_policy_none_when_zero() {
  // max_hops == 0 yields a no-follow policy (smoke test of the early return).
  let _ = redirect_policy(0);
}

#[test]
fn the_streaming_threshold_depends_on_what_the_peer_can_take() {
  // Against a peer that takes binary frames, streaming also means the body
  // stops being base64: worth it from 32 KB up. Against one that cannot, a
  // streamed body is base64 either way, so the only reason left is bounded
  // memory and the threshold stays where it was. Written as a const block so
  // the relationship is checked at compile time, which is when it can break.
  const {
    assert!(BINARY_STREAM_THRESHOLD < STREAM_THRESHOLD);
    assert!(BINARY_STREAM_THRESHOLD == 32 * 1024);
    assert!(STREAM_THRESHOLD == 256 * 1024);
  }
}

#[test]
fn splitting_the_uri_agrees_with_parsing_it_as_a_url() {
  // `build_dest_url` used to parse `http://localhost{uri}` just to reach the
  // path and the query. Parsing also normalizes (`..` collapses, stray
  // characters get encoded), and an SSRF check sits underneath, so the cheap
  // split may only replace it where the two agree exactly. This is that
  // proof, and it stays as the thing that fails if `set_path` ever stops
  // normalizing on its own.
  let base = url::Url::parse("http://127.0.0.1:3000").unwrap();
  for uri in [
    "/",
    "/a/b",
    "/a/../b",
    "/a/./b",
    "/a//b",
    "/..%2f..%2fetc/passwd",
    "/%2e%2e/x",
    "/a?x=1&y=2",
    "/?q",
    "/a?x=/../b",
    "/a%20b",
    "/ünicode/yol",
    "/a?",
    "/trailing/",
  ] {
    let parsed = url::Url::parse(&format!("http://localhost{uri}")).unwrap();

    let (raw_path, raw_query) = match uri.split_once('?') {
      Some((p, q)) => (p, Some(q)),
      None => (uri, None),
    };

    let mut from_parse = base.clone();
    from_parse.set_path(parsed.path());
    from_parse.set_query(parsed.query());

    let mut from_split = base.clone();
    from_split.set_path(raw_path);
    from_split.set_query(raw_query);

    assert_eq!(
      from_parse.as_str(),
      from_split.as_str(),
      "the two ways of reading {uri:?} disagree"
    );
  }
}

#[test]
fn an_absolute_form_uri_reaches_the_backend_as_its_path() {
  // HTTP/2 visitors send `:scheme` and `:authority`, so the URI arrives
  // rebuilt as `http://host/path`. The old expression prefixed
  // `http://localhost` to it, which made the whole thing the path: the
  // backend saw `/127.0.0.1:18110/echo` where it should see `/echo`.
  let (tx, _rx) = mpsc::channel(4);
  let ctx = test_ctx("http://127.0.0.1:3000", tx);

  let dest = build_dest_url(&ctx, "req-1", "http://127.0.0.1:18110/echo").unwrap();
  assert_eq!(dest.as_str(), "http://127.0.0.1:3000/echo");

  let dest = build_dest_url(&ctx, "req-2", "http://host/a?b=1").unwrap();
  assert_eq!(dest.as_str(), "http://127.0.0.1:3000/a?b=1");

  // Origin-form is unchanged, and neither shape reaches the backend when it
  // is not a URI at all.
  let dest = build_dest_url(&ctx, "req-3", "/plain?x=1").unwrap();
  assert_eq!(dest.as_str(), "http://127.0.0.1:3000/plain?x=1");
  assert_eq!(build_dest_url(&ctx, "req-4", ":notaport").unwrap_err(), 400);
}

// --- ChunkCoalescer ---------------------------------------------------------

#[test]
fn coalescer_holds_partial_frames_and_pops_full_ones() {
  let mut c = ChunkCoalescer::new();
  assert!(c.is_empty());
  c.add(&[1u8; 1000]);
  assert!(!c.is_empty());
  // Under a frame: nothing to pop yet.
  assert!(c.pop_full().is_none());
  // Crossing the frame size pops exactly one full frame, remainder stays.
  c.add(&vec![2u8; STREAM_CHUNK_SIZE]);
  let full = c.pop_full().expect("a full frame accumulated");
  assert_eq!(full.len(), STREAM_CHUNK_SIZE);
  assert_eq!(&full[..1000], &[1u8; 1000]);
  assert!(c.pop_full().is_none());
  let rest = c.take().expect("a remainder is held");
  assert_eq!(rest, vec![2u8; 1000]);
  assert!(c.is_empty());
}

#[test]
fn coalescer_pops_every_full_frame_of_a_large_chunk() {
  // One oversized backend chunk yields multiple full frames in order.
  let mut c = ChunkCoalescer::new();
  c.add(&vec![7u8; STREAM_CHUNK_SIZE * 2 + 5]);
  assert_eq!(c.pop_full().unwrap().len(), STREAM_CHUNK_SIZE);
  assert_eq!(c.pop_full().unwrap().len(), STREAM_CHUNK_SIZE);
  assert!(c.pop_full().is_none());
  assert_eq!(c.take().unwrap(), vec![7u8; 5]);
}

#[test]
fn coalescer_take_on_empty_is_none() {
  let mut c = ChunkCoalescer::new();
  assert!(c.take().is_none());
}

// --- BackendResilience (planned_features #29) -------------------------------

#[test]
fn retry_is_limited_to_idempotent_methods_unless_opted_in() {
  let cautious = BackendResilience::new(3, 10, false, 0, 30);
  assert!(cautious.may_retry_method("GET"));
  assert!(cautious.may_retry_method("head"), "the check ignores case");
  assert!(cautious.may_retry_method("DELETE"));
  // A retried write may reach the backend twice, so it is opt-in.
  assert!(!cautious.may_retry_method("POST"));
  assert!(!cautious.may_retry_method("PATCH"));

  let eager = BackendResilience::new(3, 10, true, 0, 30);
  assert!(eager.may_retry_method("POST"));
}

#[test]
fn a_disabled_breaker_never_opens() {
  let r = BackendResilience::new(1, 10, false, 0, 30);
  for _ in 0..100 {
    assert!(!r.record_failure(), "failures are not counted when off");
    assert!(matches!(r.check(), BreakerVerdict::Proceed));
  }
}

#[test]
fn the_breaker_opens_on_consecutive_failures_and_reports_it_once() {
  let r = BackendResilience::new(1, 10, false, 3, 30);
  assert!(!r.record_failure());
  assert!(!r.record_failure());
  assert!(matches!(r.check(), BreakerVerdict::Proceed), "still closed");
  assert!(
    r.record_failure(),
    "the third failure opens it, and says so"
  );
  // Further failures keep it open but do not re-announce it, so a flood
  // produces one line rather than one per request.
  assert!(!r.record_failure());
  match r.check() {
    BreakerVerdict::Open(left) => assert!(left.as_secs() <= 30),
    BreakerVerdict::Proceed => panic!("expected the breaker to be open"),
  }
}

#[test]
fn a_success_resets_the_failure_run() {
  let r = BackendResilience::new(1, 10, false, 3, 30);
  r.record_failure();
  r.record_failure();
  r.record_success();
  // The count restarted, so two more failures are not enough to open it.
  assert!(!r.record_failure());
  assert!(!r.record_failure());
  assert!(matches!(r.check(), BreakerVerdict::Proceed));
}

#[test]
fn the_open_window_lets_exactly_one_request_probe_the_backend() {
  // A one-second window, so the test can wait it out without being slow.
  let r = BackendResilience::new(1, 10, false, 1, 1);
  assert!(r.record_failure(), "one failure is the threshold here");
  assert!(matches!(r.check(), BreakerVerdict::Open(_)));
  std::thread::sleep(std::time::Duration::from_millis(1100));
  // The first caller after the window is the probe...
  assert!(matches!(r.check(), BreakerVerdict::Proceed));
  // ...and until it reports back, everyone else is let through too, because
  // the window was cleared. What keeps a dead backend from being hammered is
  // that the probe's failure opens a fresh window.
  assert!(r.record_failure());
  assert!(matches!(r.check(), BreakerVerdict::Open(_)));
}
