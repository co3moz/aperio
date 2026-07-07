use super::*;

/// Tunnel sender whose receiver is drained in the background, for tests
/// that exercise the buffered (non-streaming) response path.
fn test_tunnel_tx() -> mpsc::Sender<Message> {
  let (tx, mut rx) = mpsc::channel::<Message>(64);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  tx
}

#[tokio::test]
async fn test_make_error_response() {
  let response = make_error_response("req-123".to_string(), 502);
  if let TunnelMessage::Response {
    id,
    status,
    headers,
    body,
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

  let client = reqwest::Client::new();
  let headers = vec![("x-custom-header".to_string(), "custom-value".to_string())];

  let result = handle_incoming_request(
    client,
    "req-id-123".to_string(),
    "GET".to_string(),
    "/test-path".to_string(),
    headers,
    None,
    &target_url,
    false,
    None,
    false,
    1024 * 1024,
    None,
    false,
    test_tunnel_tx(),
  )
  .await
  .expect("expected buffered response");

  if let TunnelMessage::Response {
    id,
    status,
    headers,
    body,
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
      // Close gracefully and wait for the peer to finish reading; an
      // abrupt drop can RST the connection and truncate in-flight bytes
      // (a flake seen under parallel test load).
      let _ = socket.shutdown().await;
      let mut sink = [0u8; 1024];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, mut rx) = mpsc::channel::<Message>(256);
  let client = reqwest::Client::new();
  let result = handle_incoming_request(
    client,
    "req-stream-1".to_string(),
    "GET".to_string(),
    "/big".to_string(),
    vec![],
    None,
    &target_url,
    false,
    None,
    false,
    10 * 1024 * 1024,
    None,
    false,
    tx,
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

  let client = reqwest::Client::new();
  // path_bind = "/api", trim_bind = true → /api/hello should become /hello
  let result = handle_incoming_request(
    client,
    "req-trim-1".to_string(),
    "GET".to_string(),
    "/api/hello".to_string(),
    vec![],
    None,
    &target_url,
    false,
    Some("/api".to_string()),
    true,
    1024 * 1024,
    None,
    false,
    test_tunnel_tx(),
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

  let client = reqwest::Client::new();
  // path_bind = "/api", trim_bind = false → path should NOT be stripped
  let _result = handle_incoming_request(
    client,
    "req-trim-2".to_string(),
    "GET".to_string(),
    "/api/hello".to_string(),
    vec![],
    None,
    &target_url,
    false,
    Some("/api".to_string()),
    false,
    1024 * 1024,
    None,
    false,
    test_tunnel_tx(),
  )
  .await;

  let observed = rx.await.unwrap();
  assert!(
    observed.contains("GET /api/hello"),
    "expected untrimmed path '/api/hello' in request line, got: {}",
    observed
  );
}
