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
    custom_name: None,
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
    reload_drain_secs: 10,
    retry_attempts: 1,
    retry_backoff_ms: 100,
    retry_all_methods: false,
    breaker_failures: 0,
    breaker_open_for_secs: 30,
    max_request_body: None,
    response_timeout: None,
    timeout_secs: 5,
    max_concurrent: None,
    connections: 1,
    priority: 0,
    bandwidth_bps: None,
    bandwidth_declared: None,
    config_notes: Vec::new(),
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
    capture: true,
    webhook_inbox: false,
    denied: None,
    scaling: None,
  }
}

fn test_shared() -> Shared {
  Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    messages: crate::pubsub::MessageBus::new(Vec::new()),
  }
}

#[tokio::test]
async fn shutdown_requested_resolves_for_a_signal_that_already_happened() {
  let shared = test_shared();

  // The common case: the service is already waiting when the signal lands.
  let waiting = {
    let shared = shared.clone();
    tokio::spawn(async move { super::shutdown_requested(&shared).await })
  };
  tokio::task::yield_now().await;
  shared.shutting_down.store(true, Ordering::SeqCst);
  shared.shutdown_notify.notify_waiters();
  tokio::time::timeout(Duration::from_secs(2), waiting)
    .await
    .expect("a waiting service is woken by the notification")
    .unwrap();

  // The case that hung the client: the signal fired while the service was
  // elsewhere (in its reconnect backoff), so `notify_waiters` reached nobody.
  // Waiting on the notification alone would block forever here.
  tokio::time::timeout(Duration::from_secs(2), super::shutdown_requested(&shared))
    .await
    .expect("a service that arrives after the signal must not wait for it");
}

#[tokio::test]
async fn shutdown_requested_keeps_waiting_until_the_flag_is_set() {
  let shared = test_shared();
  // No shutdown yet: the future must not resolve on its own.
  assert!(
    tokio::time::timeout(
      Duration::from_millis(200),
      super::shutdown_requested(&shared)
    )
    .await
    .is_err()
  );
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
  ws.send(Message::Text(json.into())).await.unwrap();
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
  // No name, empty target: the placeholder says which of the two reasons a
  // connection with no service exists for, rather than guessing at one.
  spec.target = String::new();
  assert_eq!(spec.label(), "(no service)");
  spec.tunnels = vec![aperio_config::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:5432".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }];
  assert_eq!(spec.label(), "(tunnels only)");
}

// ---------------------------------------------------------------------------
// resolve_device_key / device_key
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_device_key_value_and_file() {
  // Nothing configured: nothing announced.
  assert_eq!(resolve_device_key(None, None), None);

  // An explicit value wins and is trimmed.
  assert_eq!(
    resolve_device_key(Some("  explicit-key  ".into()), None).as_deref(),
    Some("explicit-key")
  );

  // A blank explicit value falls through to the file.
  let path = std::env::temp_dir().join(format!("aperio-devkey-{}", uuid::Uuid::new_v4()));
  let path_str = path.to_string_lossy().into_owned();
  // First call: the file does not exist, so a fresh key is generated and
  // persisted.
  let generated =
    resolve_device_key(Some("   ".into()), Some(path_str.clone())).expect("a key is generated");
  assert!(!generated.is_empty());
  assert_eq!(
    std::fs::read_to_string(&path).unwrap().trim(),
    generated,
    "the generated key is persisted"
  );
  // Second call: the existing file's contents are reused verbatim.
  assert_eq!(
    resolve_device_key(None, Some(path_str.clone())).as_deref(),
    Some(generated.as_str())
  );
  // A blank path is treated as unset.
  assert_eq!(resolve_device_key(None, Some("  ".into())), None);

  let _ = std::fs::remove_file(&path);

  // device_key() memoizes and returns a stable value.
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
      custom_name: None,
      name: None,
      target: "127.0.0.1:5432".to_string(),
      protocol: "tcp".to_string(),
      encrypt: false,
      psk: None,
      idle_timeout: None,
      expose: None,
    },
    TunnelDecl {
      custom_name: None,
      name: None,
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));

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
  ws.send(Message::Binary(compressed.into())).await.unwrap();

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
  ws.send(Message::Binary(
    crate::protocol::encode_binary_frame(FRAME_REQUEST_CHUNK, "r2", b"world")
      .unwrap()
      .into(),
  ))
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));

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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));

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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
  tokio::time::sleep(Duration::from_millis(400)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[test]
fn a_heartbeat_never_reports_healthy_but_unprobed() {
  // `healthy && !probed` says the backend is up and that nobody has looked.
  // It is not a state that exists, but it was reachable: the probe announced
  // the healthy transition and set `probed` after, so a heartbeat woken in
  // between sent exactly that, and the dashboard showed CHECKING for a backend
  // already serving. One e2e run in many caught it, which is the wrong way to
  // find out.
  //
  // The window is two instructions wide, so this pins the property instead of
  // trying to observe the race: the pair is derived in one place, and being
  // healthy is itself evidence a probe completed.
  let mut spec = test_spec("ws://127.0.0.1:9/", "http://127.0.0.1:9");
  spec.target_health = Some("/healthz".to_string());
  let health = BackendHealth::for_spec(&spec);
  assert_eq!(
    health.report(),
    (false, false),
    "gated: down, not yet probed"
  );

  // Exactly the interleaving the probe used to expose.
  health.healthy.store(true, Ordering::SeqCst);
  assert_eq!(
    health.report(),
    (true, true),
    "healthy must never be reported without probed"
  );

  health.probed.store(true, Ordering::SeqCst);
  assert_eq!(health.report(), (true, true));

  // Unhealthy after a probe stays honest in the other direction: down, and
  // known to be down, which is what the dashboard draws as DOWN not CHECKING.
  health.healthy.store(false, Ordering::SeqCst);
  assert_eq!(health.report(), (false, true));

  // An ungated service is up and probed from the start; nothing to report but
  // the truth.
  let plain = BackendHealth::for_spec(&test_spec("ws://127.0.0.1:9/", "http://127.0.0.1:9"));
  assert_eq!(plain.report(), (true, true));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
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
  let svc = tokio::spawn(run_service(
    spec.clone(),
    shared,
    cancel_rx,
    BackendHealth::for_spec(&spec),
    true,
    1,
    ConnectionCeiling::new(),
  ));
  tokio::time::sleep(Duration::from_millis(200)).await;
  cancel_tx.send(true).unwrap();
  tokio::time::timeout(Duration::from_secs(5), svc)
    .await
    .expect("exits")
    .unwrap();
}

#[test]
fn test_backend_health_for_spec_initial_state() {
  let mut spec = test_spec("ws://x/", "http://localhost:3000");
  // No gating: healthy and probed immediately.
  let h = BackendHealth::for_spec(&spec);
  assert!(h.healthy.load(Ordering::SeqCst));
  assert!(h.probed.load(Ordering::SeqCst));
  // A target_health check starts the service out of routing (unhealthy,
  // unprobed) until the first probe passes.
  spec.target_health = Some("/healthz".to_string());
  let h = BackendHealth::for_spec(&spec);
  assert!(!h.healthy.load(Ordering::SeqCst));
  assert!(!h.probed.load(Ordering::SeqCst));
  // wait_for_backend (without a health check) also starts gated.
  spec.target_health = None;
  spec.wait_for_backend = true;
  let h = BackendHealth::for_spec(&spec);
  assert!(!h.healthy.load(Ordering::SeqCst));
  assert!(!h.probed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn mark_request_activity_stamps_the_idle_clock() {
  let shared = test_shared();
  // Zero means "never served anything", which the idle watcher treats as
  // "do not retire yet" rather than "idle forever".
  assert_eq!(shared.last_request_at.load(Ordering::SeqCst), 0);

  shared.mark_request_activity();

  // Every inbound work item calls this, streamed uploads, WebSocket
  // upgrades and raw TCP/UDP opens as well as buffered requests, so a client
  // busy with any of them cannot decide it is idle and shut down mid-traffic.
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs();
  let stamped = shared.last_request_at.load(Ordering::SeqCst);
  assert!(stamped > 0);
  assert!(
    now.saturating_sub(stamped) <= 2,
    "stamped {stamped}, now {now}"
  );

  // The relays stamp the same clock through the shared handle.
  let clock = shared.activity_clock();
  clock.stamp();
  assert_eq!(clock.secs(), shared.last_request_at.load(Ordering::SeqCst));
}

#[test]
fn should_retire_idle_covers_inflight_and_cold_start() {
  // Never served anything: a freshly started client must not retire before
  // it has had the chance to be used.
  assert!(!should_retire_idle(0, 1_000, 300, 0));
  // Quiet for the full window with nothing in flight: retire.
  assert!(should_retire_idle(700, 1_000, 300, 0));
  // Not quiet for long enough yet.
  assert!(!should_retire_idle(800, 1_000, 300, 0));
  // A stale clock with work still in flight is not idleness: a slow backend
  // or a response streaming for longer than the window produces exactly this
  // state, and retiring would cut it off at the drain deadline.
  assert!(!should_retire_idle(700, 1_000, 300, 1));
}

// --- One upload's consumer must not be able to stop the tunnel ---

/// The map the read loop consults, and the backend end of the one stream in it.
type StreamMap = Arc<Mutex<HashMap<String, RequestBodyFeeder>>>;
type BackendEnd = mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>;

/// A stream map holding one feeder, with the buffer size given.
fn one_stream(capacity: usize) -> (StreamMap, BackendEnd) {
  let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(capacity);
  let map = Arc::new(Mutex::new(HashMap::from([("req-1".to_string(), tx)])));
  (map, rx)
}

#[tokio::test]
async fn a_chunk_reaches_the_backend_and_the_lock_is_free_while_it_does() {
  let (streams, mut rx) = one_stream(4);
  feed_request_chunk(&streams, "req-1", b"hello".to_vec().into()).await;
  assert_eq!(rx.recv().await.unwrap().unwrap(), b"hello".to_vec());
  // The map is not held across the send: another task can read it right after.
  assert!(streams.lock().await.contains_key("req-1"));
}

#[tokio::test]
async fn a_chunk_for_an_unknown_stream_is_dropped_quietly() {
  let (streams, _rx) = one_stream(4);
  // A late chunk for a request that already ended is normal, not an error.
  feed_request_chunk(&streams, "gone", b"x".to_vec().into()).await;
}

#[tokio::test(start_paused = true)]
async fn a_consumer_that_stops_reading_loses_its_upload_not_the_tunnel() {
  // The bug this covers: the send blocked forever on a full channel while
  // holding the stream map, so the read loop stopped, no Pong went out, and
  // fifteen seconds later the liveness check tore down every request on the
  // connection because of one slow backend.
  let (streams, _rx) = one_stream(1);
  feed_request_chunk(&streams, "req-1", b"first".to_vec().into()).await; // fills it

  let start = tokio::time::Instant::now();
  feed_request_chunk(&streams, "req-1", b"second".to_vec().into()).await;
  let waited = start.elapsed();

  assert!(
    waited >= STREAM_STALL_BUDGET && waited < STREAM_STALL_BUDGET * 2,
    "the loop waited {waited:?}, it must be bounded by the stall budget"
  );
  assert!(
    !streams.lock().await.contains_key("req-1"),
    "the abandoned upload is dropped, so later chunks cost nothing"
  );
}

#[tokio::test(start_paused = true)]
async fn a_consumer_that_catches_up_keeps_its_upload() {
  // Merely slow is not abandoned: the chunk lands as soon as there is room.
  let (streams, mut rx) = one_stream(1);
  feed_request_chunk(&streams, "req-1", b"first".to_vec().into()).await;

  let reader = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut out = Vec::new();
    while let Some(Ok(chunk)) = rx.recv().await {
      out.extend_from_slice(&chunk);
      if out.len() >= 11 {
        break;
      }
    }
    out
  });
  feed_request_chunk(&streams, "req-1", b"second".to_vec().into()).await;
  assert!(
    streams.lock().await.contains_key("req-1"),
    "a backend that catches up keeps its stream"
  );
  assert_eq!(reader.await.unwrap(), b"firstsecond".to_vec());
}

// --- The relay arms take the same bounded hand-off ---

#[tokio::test]
async fn a_relay_frame_is_delivered_when_the_consumer_has_room() {
  let (tx, mut rx) = mpsc::channel::<bytes::Bytes>(2);
  assert!(deliver_to_relay(&tx, "TCP", "s1", b"first".to_vec().into()).await);
  assert_eq!(rx.recv().await.unwrap(), b"first".to_vec());
}

#[tokio::test]
async fn a_relay_whose_consumer_is_gone_is_finished() {
  let (tx, rx) = mpsc::channel::<bytes::Bytes>(1);
  drop(rx);
  assert!(!deliver_to_relay(&tx, "TCP", "s1", b"x".to_vec().into()).await);
}

#[tokio::test(start_paused = true)]
async fn a_relay_consumer_that_is_merely_slow_keeps_its_stream() {
  // The regression this covers: `try_send` alone dropped a lossless stream the
  // moment its buffer filled, so a large file over a tunneled socket died on a
  // burst its backend would have absorbed a moment later.
  let (tx, mut rx) = mpsc::channel::<bytes::Bytes>(1);
  assert!(deliver_to_relay(&tx, "TCP", "s1", b"first".to_vec().into()).await); // fills it

  let reader = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut seen = Vec::new();
    while let Some(chunk) = rx.recv().await {
      seen.push(chunk);
      if seen.len() == 2 {
        break;
      }
    }
    seen
  });
  assert!(
    deliver_to_relay(&tx, "TCP", "s1", b"second".to_vec().into()).await,
    "a consumer that catches up inside the budget keeps its stream"
  );
  assert_eq!(reader.await.unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_relay_consumer_that_stops_reading_loses_its_stream_not_the_tunnel() {
  let (tx, _rx) = mpsc::channel::<bytes::Bytes>(1);
  assert!(deliver_to_relay(&tx, "WebSocket", "s1", b"first".to_vec().into()).await);

  let start = tokio::time::Instant::now();
  let alive = deliver_to_relay(&tx, "WebSocket", "s1", b"second".to_vec().into()).await;
  let waited = start.elapsed();

  assert!(!alive, "the stalled stream is finished");
  assert!(
    waited >= STREAM_STALL_BUDGET && waited < STREAM_STALL_BUDGET * 2,
    "the read loop waited {waited:?}, it must be bounded by the budget"
  );
}

// --- reload drain budget (planned_features #33) -----------------------------

#[tokio::test]
async fn a_zero_budget_returns_at_once_even_with_requests_in_flight() {
  // `reload_drain: 0` is the pre-#33 behavior, an immediate drop. It must not
  // wait, and must not depend on the counter reaching zero.
  let shared = test_shared();
  shared.inflight_requests.store(3, Ordering::SeqCst);
  let start = Instant::now();
  drain_inflight_for(&shared, Duration::from_secs(0)).await;
  assert!(start.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn a_drain_returns_as_soon_as_the_last_request_finishes() {
  let shared = test_shared();
  shared.inflight_requests.store(1, Ordering::SeqCst);
  let done = shared.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(150)).await;
    done.inflight_requests.store(0, Ordering::SeqCst);
  });
  let start = Instant::now();
  // A generous budget: what ends the wait is the work finishing, not the cap.
  drain_inflight_for(&shared, Duration::from_secs(30)).await;
  let waited = start.elapsed();
  assert!(
    waited >= Duration::from_millis(140),
    "it waited for the request"
  );
  assert!(
    waited < Duration::from_secs(5),
    "it did not wait out the budget: {waited:?}"
  );
}

#[tokio::test]
async fn a_stalled_request_cannot_hold_a_reload_past_the_budget() {
  let shared = test_shared();
  shared.inflight_requests.store(1, Ordering::SeqCst);
  let start = Instant::now();
  drain_inflight_for(&shared, Duration::from_millis(300)).await;
  let waited = start.elapsed();
  assert!(
    waited >= Duration::from_millis(300),
    "the budget was honored"
  );
  assert!(
    waited < Duration::from_secs(3),
    "and it did give up: {waited:?}"
  );
}
