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
    timeout_secs: 30,
    target: target.to_string(),
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
    },
    None,
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
    },
    None,
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
    },
    None,
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
    },
    None,
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
    },
    None,
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
    },
    None,
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
    },
    None,
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
    },
    None,
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
