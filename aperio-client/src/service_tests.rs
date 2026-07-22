use super::*;

#[test]
fn test_reconnect_delay_bounds() {
  // Deterministic cap doubles per attempt: 1s, 2s, 4s ... 60s max; the
  // jittered result must stay within [cap/2, cap].
  for (attempt, cap_ms) in [
    (1u32, 1_000u64),
    (2, 2_000),
    (3, 4_000),
    (7, 60_000),
    (100, 60_000),
  ] {
    for _ in 0..50 {
      let d = reconnect_delay(attempt).as_millis() as u64;
      assert!(
        d >= cap_ms / 2 && d <= cap_ms,
        "attempt {attempt}: delay {d}ms outside [{}ms, {cap_ms}ms]",
        cap_ms / 2
      );
    }
  }
}

#[test]
fn test_fast_reconnect_delay_bounds() {
  // Post-ServerShutdown reconnects skip the backoff: 100–500 ms jitter.
  for _ in 0..50 {
    let d = fast_reconnect_delay().as_millis() as u64;
    assert!(
      (100..=500).contains(&d),
      "fast reconnect delay {d}ms outside [100ms, 500ms]"
    );
  }
}

// ---------------------------------------------------------------------------
// Test harness: a minimal loopback server and a fully-populated ServiceSpec.
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;

/// A ServiceSpec with every field defaulted; tests override the few they care
/// about. `ws_url`/`ws_urls` point at the loopback server, `target` at a
/// (usually unused) backend.
fn test_spec(ws_url: &str, target: &str) -> ServiceSpec {
  ServiceSpec {
    name: None,
    client_id: "test-client".to_string(),
    token: "apr_test".to_string(),
    instance_group: "test-client".to_string(),
    server_addr: "https://tunnel.example.com".to_string(),
    ws_url: ws_url.to_string(),
    ws_urls: vec![ws_url.to_string()],
    target: target.to_string(),
    hostnames: vec!["app.example.com".to_string()],
    path: None,
    trim_bind: false,
    pass_hostname: false,
    max_response_body: 50 * 1024 * 1024,
    max_request_body: None,
    response_timeout: None,
    timeout_secs: 5,
    max_concurrent: None,
    connections: 1,
    priority: 0,
    bandwidth_bps: None,
    max_message_size: 4 * 1024 * 1024,
    max_redirects: 5,
    tcp_target: None,
    target_health: None,
    wait_for_backend: false,
    health_interval: 1,
    health_timeout: 1,
    health_threshold: 1,
    public: false,
    visitor_auth: None,
    allowed_ips: Vec::new(),
    tunnels: Vec::new(),
    headers: None,
    cache: false,
    resilience: false,
    webhook_inbox: false,
    denied: None,
  }
}

fn test_shared() -> Shared {
  Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
  }
}

/// Installs a process-wide TRACE subscriber once so `info!`/`warn!`/`error!`
/// argument expressions are evaluated (and covered). Without a subscriber,
/// tracing skips argument evaluation entirely.
fn init_tracing() {
  use std::sync::Once;
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    let _ = tracing_subscriber::fmt()
      .with_max_level(tracing::Level::TRACE)
      .with_test_writer()
      .try_init();
  });
}

/// Binds a loopback TCP listener and returns it with the matching ws:// URL.
async fn loopback_ws() -> (TcpListener, String) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  (listener, format!("ws://127.0.0.1:{port}/"))
}

/// Serializes and sends a server→client tunnel message on the mock socket.
async fn srv_send(ws: &mut WebSocketStream<TcpStream>, msg: &TunnelMessage) {
  let json = serde_json::to_string(msg).unwrap();
  ws.send(Message::Text(json)).await.unwrap();
}

/// A backend that accepts one TCP connection and replies `200 OK` to the
/// first HTTP request; used to make a health probe pass. Returns the port.
async fn spawn_http_200() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let _ = sock
          .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
          .await;
        let _ = sock.flush().await;
      });
    }
  });
  port
}

// ---------------------------------------------------------------------------
// ServiceSpec::label
// ---------------------------------------------------------------------------

#[test]
fn test_label_variants() {
  let mut spec = test_spec("ws://x/", "http://localhost:3000");
  // A named service labels by name.
  spec.name = Some("web".to_string());
  assert_eq!(spec.label(), "web");
  // No name, non-empty target: labels by target.
  spec.name = None;
  assert_eq!(spec.label(), "http://localhost:3000");
  // No name, empty target (tunnels-only client): a placeholder label.
  spec.target = String::new();
  assert_eq!(spec.label(), "(tunnels only)");
}

// ---------------------------------------------------------------------------
// resolve_device_key / device_key
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_device_key_env_and_file() {
  // Serialize env mutation within this test; no other test reads these vars.
  // SAFETY: single-threaded within this test; the vars are unique to it.
  unsafe {
    std::env::remove_var("APERIO_DEVICE_KEY");
    std::env::remove_var("APERIO_DEVICE_KEY_FILE");
  }
  // Neither set: nothing announced.
  assert_eq!(resolve_device_key(), None);

  // Explicit value wins and is trimmed.
  unsafe { std::env::set_var("APERIO_DEVICE_KEY", "  explicit-key  ") };
  assert_eq!(resolve_device_key().as_deref(), Some("explicit-key"));

  // An empty explicit value falls through to the file path.
  unsafe { std::env::set_var("APERIO_DEVICE_KEY", "   ") };
  let path = std::env::temp_dir().join(format!("aperio-devkey-{}", uuid::Uuid::new_v4()));
  let path_str = path.to_string_lossy().into_owned();
  unsafe { std::env::set_var("APERIO_DEVICE_KEY_FILE", &path_str) };
  // First call: the file does not exist, so a fresh key is generated and
  // persisted.
  let generated = resolve_device_key().expect("a key is generated");
  assert!(!generated.is_empty());
  assert_eq!(
    std::fs::read_to_string(&path).unwrap().trim(),
    generated,
    "the generated key is persisted"
  );
  // Second call: the existing file's contents are reused verbatim.
  assert_eq!(resolve_device_key().as_deref(), Some(generated.as_str()));

  let _ = std::fs::remove_file(&path);
  unsafe {
    std::env::remove_var("APERIO_DEVICE_KEY");
    std::env::remove_var("APERIO_DEVICE_KEY_FILE");
  }
  // device_key() memoizes resolve_device_key() and returns a stable value.
  let a = device_key();
  let b = device_key();
  assert_eq!(a, b);
}

// ---------------------------------------------------------------------------
// backend_accepts_connections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_backend_accepts_connections() {
  // A listening TCP backend accepts.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  assert!(backend_accepts_connections(&format!("http://127.0.0.1:{port}")).await);
  // The h2c:// scheme is rewritten to http:// before dialing.
  assert!(backend_accepts_connections(&format!("h2c://127.0.0.1:{port}")).await);

  // A fresh unused port is refused.
  drop(listener);
  let free = {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
  };
  assert!(!backend_accepts_connections(&format!("http://127.0.0.1:{free}")).await);

  // Unparseable and address-less targets are refused.
  assert!(!backend_accepts_connections("::::not a url").await);
  assert!(!backend_accepts_connections("http:///no-host").await);
  // Parses but has no host component.
  assert!(!backend_accepts_connections("mailto:foo@bar").await);
  // Has a host but an unknown scheme with no default port.
  assert!(!backend_accepts_connections("foo://host/").await);
}

#[cfg(unix)]
#[tokio::test]
async fn test_backend_accepts_connections_unix() {
  // Short path: unix domain socket paths must stay under SUN_LEN (~104).
  let id = uuid::Uuid::new_v4().simple().to_string();
  let sock = std::path::PathBuf::from(format!("/tmp/ap-{}.sock", &id[..8]));
  let _ = std::fs::remove_file(&sock);
  let listener = tokio::net::UnixListener::bind(&sock).unwrap();
  tokio::spawn(async move {
    let _ = listener.accept().await;
  });
  assert!(backend_accepts_connections(&format!("unix://{}", sock.display())).await);
  // A unix path with nothing listening is refused.
  let missing = std::path::PathBuf::from(format!("/tmp/ap-{}-missing.sock", &id[..8]));
  assert!(!backend_accepts_connections(&format!("unix://{}", missing.display())).await);
  let _ = std::fs::remove_file(&sock);
}

// ---------------------------------------------------------------------------
// run_service: full message loop against a mock server.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_run_service_message_loop() {
  init_tracing();
  let (listener, ws_url) = loopback_ws().await;
  // Point the backend at an unused port: request/tcp/udp forwarding fails,
  // but every dispatch arm still executes.
  let mut spec = test_spec(&ws_url, "h2c://127.0.0.1:9");
  // Exercise the h2-target + pass_hostname warning.
  spec.pass_hostname = true;
  // A local concurrency limit exercises the semaphore-permit branch.
  spec.max_concurrent = Some(4);
  spec.tcp_target = Some("127.0.0.1:9".to_string());
  spec.tunnels = vec![
    TunnelDecl {
      target: "127.0.0.1:5432".to_string(),
      protocol: "tcp".to_string(),
      encrypt: false,
      psk: None,
      idle_timeout: None,
      expose: None,
    },
    TunnelDecl {
      target: "127.0.0.1:5353".to_string(),
      protocol: "udp".to_string(),
      encrypt: false,
      psk: None,
      idle_timeout: None,
      expose: None,
    },
  ];

  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));

  // Mock server: accept, then push one of each server→client frame, close.
  let (stream, _) = listener.accept().await.unwrap();
  let mut ws = accept_async(stream).await.unwrap();

  // Pong with a skewed protocol → version-skew warning + protocol store.
  srv_send(
    &mut ws,
    &TunnelMessage::Pong {
      timestamp: 1,
      version: Some("9.9.9".to_string()),
      protocol: Some(1),
    },
  )
  .await;
  // A second Pong (no further skew warning) and one with no protocol field.
  srv_send(
    &mut ws,
    &TunnelMessage::Pong {
      timestamp: 2,
      version: None,
      protocol: Some(2),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::Pong {
      timestamp: 3,
      version: None,
      protocol: None,
    },
  )
  .await;

  // A zlib-compressed Pong frame exercises the decompress path.
  let compressed = crate::protocol::compress_frame(
    &serde_json::to_string(&TunnelMessage::Pong {
      timestamp: 4,
      version: None,
      protocol: Some(2),
    })
    .unwrap(),
  );
  ws.send(Message::Binary(compressed)).await.unwrap();

  // An unhandled (client-bound-irrelevant) message hits the catch-all arm.
  srv_send(&mut ws, &TunnelMessage::CompressionAck {}).await;

  // A plain proxied request.
  srv_send(
    &mut ws,
    &TunnelMessage::Request {
      id: "r1".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
    },
  )
  .await;

  // Streamed request body: start, a Base64 chunk, a binary chunk, then end.
  srv_send(
    &mut ws,
    &TunnelMessage::RequestStart {
      id: "r2".to_string(),
      method: "POST".to_string(),
      uri: "/upload".to_string(),
      headers: vec![],
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::RequestChunk {
      id: "r2".to_string(),
      data: BASE64_STANDARD.encode(b"hello"),
    },
  )
  .await;
  // A malformed Base64 chunk exercises the decode-error warning.
  srv_send(
    &mut ws,
    &TunnelMessage::RequestChunk {
      id: "r2".to_string(),
      data: "!!!not-base64!!!".to_string(),
    },
  )
  .await;
  // Binary v2 chunk frame for the same request id.
  ws.send(Message::Binary(crate::protocol::encode_binary_frame(
    FRAME_REQUEST_CHUNK,
    "r2",
    b"world",
  )))
  .await
  .unwrap();
  srv_send(
    &mut ws,
    &TunnelMessage::RequestEnd {
      id: "r2".to_string(),
    },
  )
  .await;

  // An upgrade (WebSocket) request.
  srv_send(
    &mut ws,
    &TunnelMessage::UpgradeRequest {
      id: "u1".to_string(),
      method: "GET".to_string(),
      uri: "/ws".to_string(),
      headers: vec![],
    },
  )
  .await;
  // WsData/WsClose for an unknown stream (no backend WS established).
  srv_send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "u1".to_string(),
      data: "hi".to_string(),
      is_text: true,
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::WsClose {
      stream_id: "u1".to_string(),
      code: 1000,
      reason: "bye".to_string(),
    },
  )
  .await;

  // TCP: open a declared target, feed data, close. Then open an undeclared
  // target (refused) and the legacy no-target form (uses tcp_target).
  srv_send(
    &mut ws,
    &TunnelMessage::TcpOpen {
      stream_id: "t1".to_string(),
      target: Some("127.0.0.1:5432".to_string()),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".to_string(),
      data: BASE64_STANDARD.encode(b"ping"),
    },
  )
  .await;
  // Malformed Base64 TcpData warning.
  srv_send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".to_string(),
      data: "!!!".to_string(),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::TcpClose {
      stream_id: "t1".to_string(),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::TcpOpen {
      stream_id: "t2".to_string(),
      target: Some("127.0.0.1:9999".to_string()),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::TcpOpen {
      stream_id: "t3".to_string(),
      target: None,
    },
  )
  .await;

  // UDP: declared target, datagram (+ malformed), close; then undeclared.
  srv_send(
    &mut ws,
    &TunnelMessage::UdpOpen {
      stream_id: "d1".to_string(),
      target: "127.0.0.1:5353".to_string(),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "d1".to_string(),
      data: BASE64_STANDARD.encode(b"dgram"),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "d1".to_string(),
      data: "!!!".to_string(),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::UdpClose {
      stream_id: "d1".to_string(),
    },
  )
  .await;
  srv_send(
    &mut ws,
    &TunnelMessage::UdpOpen {
      stream_id: "d2".to_string(),
      target: "127.0.0.1:6666".to_string(),
    },
  )
  .await;

  // Compression offer → the client acks and flips outgoing compression on.
  srv_send(&mut ws, &TunnelMessage::CompressionStart {}).await;
  // Hostname assignment and a graceful-shutdown announcement.
  srv_send(
    &mut ws,
    &TunnelMessage::HostnameAssigned {
      hostname: "auto.example.com".to_string(),
    },
  )
  .await;
  srv_send(&mut ws, &TunnelMessage::ServerShutdown {}).await;

  // Let the client drain the frames, then close the socket so the read loop
  // ends and the service enters its (fast) reconnect wait.
  tokio::time::sleep(Duration::from_millis(300)).await;
  let _ = ws.close(None).await;
  drop(ws);

  // Cancel so the reconnect wait breaks out of the outer loop.
  tokio::time::sleep(Duration::from_millis(150)).await;
  cancel_tx.send(true).unwrap();

  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("run_service exits after cancel")
    .unwrap();
}

// ---------------------------------------------------------------------------
// run_service: cancel while connected → drops the connection via the ping
// task's abort path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_run_service_cancel_while_connected() {
  init_tracing();
  let (listener, ws_url) = loopback_ws().await;
  let spec = test_spec(&ws_url, "http://127.0.0.1:9");
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));

  let (stream, _) = listener.accept().await.unwrap();
  let mut ws = accept_async(stream).await.unwrap();
  // Keep the connection alive: drain client frames in the background.
  tokio::spawn(async move { while ws.next().await.is_some() {} });

  // Request a config-reload style cancel; the ping task notices it at the top
  // of its loop and aborts the socket. This waits out one ping cycle (~5s).
  tokio::time::sleep(Duration::from_millis(200)).await;
  cancel_tx.send(true).unwrap();

  tokio::time::timeout(Duration::from_secs(10), svc)
    .await
    .expect("run_service exits after cancel-drop")
    .unwrap();
}

// ---------------------------------------------------------------------------
// run_service: connection-level failures.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_run_service_invalid_token_header() {
  init_tracing();
  // A token with control characters cannot form an Authorization header.
  let mut spec = test_spec("ws://127.0.0.1:9/", "http://127.0.0.1:9");
  spec.token = "bad\ntoken".to_string();
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  // Let the header-build error fire once, then cancel out of the reconnect.
  tokio::time::sleep(Duration::from_millis(150)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_server_shutdown_fast_reconnect() {
  init_tracing();
  // A ServerShutdown before the socket drops switches the client to the
  // fast (no-backoff) reconnect path.
  let (listener, ws_url) = loopback_ws().await;
  let spec = test_spec(&ws_url, "http://127.0.0.1:9");
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));

  let (stream, _) = listener.accept().await.unwrap();
  let mut ws = accept_async(stream).await.unwrap();
  srv_send(&mut ws, &TunnelMessage::ServerShutdown {}).await;
  tokio::time::sleep(Duration::from_millis(150)).await;
  let _ = ws.close(None).await;
  drop(ws);
  drop(listener);
  // Wait past the fast-reconnect delay (100–500 ms) so the fast branch and a
  // follow-up (failed) reconnect attempt both run, then cancel.
  tokio::time::sleep(Duration::from_millis(800)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_connection_refused_failover() {
  init_tracing();
  // Two unreachable servers: the connect fails and failover rotates the URL
  // index before the cancel breaks the loop.
  let free = {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
  };
  let mut spec = test_spec(&format!("ws://127.0.0.1:{free}/"), "http://127.0.0.1:9");
  spec.ws_urls = vec![
    format!("ws://127.0.0.1:{free}/"),
    "ws://127.0.0.1:9/".to_string(),
  ];
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  // Let one failed connect + failover rotation happen, then cancel.
  tokio::time::sleep(Duration::from_millis(200)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_http_401_rejection() {
  init_tracing();
  // A server that answers the WebSocket upgrade with 401 exercises the
  // authentication-failure branch.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((mut sock, _)) = listener.accept().await {
      let mut buf = [0u8; 1024];
      let _ = sock.read(&mut buf).await;
      let _ = sock
        .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
        .await;
      let _ = sock.flush().await;
    }
  });
  let spec = test_spec(&format!("ws://127.0.0.1:{port}/"), "http://127.0.0.1:9");
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  tokio::time::sleep(Duration::from_millis(300)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

// ---------------------------------------------------------------------------
// run_service: backend health probe and wait-for-backend gate.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_run_service_http_500_rejection() {
  init_tracing();
  // A non-auth rejection status hits the generic "server rejected" branch.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((mut sock, _)) = listener.accept().await {
      let mut buf = [0u8; 1024];
      let _ = sock.read(&mut buf).await;
      let _ = sock
        .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")
        .await;
      let _ = sock.flush().await;
    }
  });
  let spec = test_spec(&format!("ws://127.0.0.1:{port}/"), "http://127.0.0.1:9");
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  tokio::time::sleep(Duration::from_millis(300)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_health_probe_flap() {
  init_tracing();
  // A backend that fails, recovers, then fails again exercises the health
  // transitions: first-probe failure, "restored", and healthy→unhealthy.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let counter = Arc::new(AtomicUsize::new(0));
  let c2 = counter.clone();
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let n = c2.fetch_add(1, Ordering::SeqCst);
      tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        // Probe 0: fail, probe 1: succeed (restored), later: fail again.
        let resp: &[u8] = if n == 1 {
          b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
        } else {
          b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n"
        };
        let _ = sock.write_all(resp).await;
        let _ = sock.flush().await;
      });
    }
  });
  let mut spec = test_spec("ws://127.0.0.1:9/", &format!("http://127.0.0.1:{port}"));
  spec.target_health = Some("healthz".to_string());
  spec.health_interval = 1;
  spec.health_threshold = 1;
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  // Span three probes (t≈0,1,2s): fail → restored → unhealthy.
  tokio::time::sleep(Duration::from_millis(2400)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_health_probe_healthy() {
  init_tracing();
  // A 200 backend makes the health probe report healthy (routable). The ws
  // server is unreachable, but the probe task runs independently.
  let port = spawn_http_200().await;
  let mut spec = test_spec("ws://127.0.0.1:9/", &format!("http://127.0.0.1:{port}"));
  // Relative health path → built from the target base.
  spec.target_health = Some("healthz".to_string());
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  tokio::time::sleep(Duration::from_millis(400)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_health_probe_absolute_url_unhealthy() {
  init_tracing();
  // An absolute health URL is used verbatim; an unreachable one stays
  // unhealthy (first-probe failure branch).
  let mut spec = test_spec("ws://127.0.0.1:9/", "h2c://127.0.0.1:9");
  spec.target_health = Some("http://127.0.0.1:9/health".to_string());
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  tokio::time::sleep(Duration::from_millis(300)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_wait_for_backend() {
  init_tracing();
  // wait_for_backend with a live backend: the gate marks the service routable
  // as soon as the backend accepts a connection.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      if listener.accept().await.is_err() {
        return;
      }
    }
  });
  let mut spec = test_spec("ws://127.0.0.1:9/", &format!("http://127.0.0.1:{port}"));
  spec.wait_for_backend = true;
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  tokio::time::sleep(Duration::from_millis(400)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[tokio::test]
async fn test_run_service_wait_for_backend_implied_by_health() {
  init_tracing();
  // wait_for_backend together with target_health logs that the health check
  // already gates startup (the gate itself is a no-op).
  let mut spec = test_spec("ws://127.0.0.1:9/", "http://127.0.0.1:9");
  spec.wait_for_backend = true;
  spec.target_health = Some("http://127.0.0.1:9/health".to_string());
  let shared = test_shared();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let svc = tokio::spawn(run_service(spec, shared, cancel_rx));
  tokio::time::sleep(Duration::from_millis(200)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}
