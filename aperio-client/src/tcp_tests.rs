use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;

/// Installs a max-level tracing subscriber once, so the `info!`/`error!`
/// macros actually evaluate their arguments (otherwise the disabled macros
/// short-circuit and their argument expressions never run).
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

/// A minimal WebSocket server that accepts one connection and echoes every
/// binary frame back. Returns the listening port.
async fn echo_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await
      && let Ok(mut ws) = accept_async(stream).await
    {
      while let Some(Ok(msg)) = ws.next().await {
        match msg {
          Message::Binary(b) => {
            if ws.send(Message::Binary(b)).await.is_err() {
              break;
            }
          }
          Message::Close(_) => break,
          _ => {}
        }
      }
    }
  });
  port
}

/// A WebSocket server that plays the E2E responder: completes the X25519
/// handshake, then echoes each frame (decrypt-then-reencrypt). Returns the port.
async fn responder_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    let Ok((stream, _)) = listener.accept().await else {
      return;
    };
    let Ok(mut ws) = accept_async(stream).await else {
      return;
    };
    // First binary frame is the initiator's handshake.
    let init_frame = loop {
      match ws.next().await {
        Some(Ok(Message::Binary(b))) => break b,
        Some(Ok(_)) => continue,
        _ => return,
      }
    };
    let hs = crate::e2e::Handshake::new(crate::e2e::Role::Responder, None);
    // A leading non-binary frame makes the peer's handshake-wait loop skip a
    // non-binary message before the real handshake frame.
    let _ = ws.send(Message::Text("pre-handshake".to_string())).await;
    if ws.send(Message::Binary(hs.frame.clone())).await.is_err() {
      return;
    }
    let Some(session) = hs.complete(&init_frame) else {
      return;
    };
    let mut sealer = session.sealer;
    let mut opener = session.opener;
    while let Some(Ok(msg)) = ws.next().await {
      match msg {
        Message::Binary(ct) => {
          let Some(plain) = opener.open(&ct) else { break };
          let Some(out) = sealer.seal(&plain) else {
            break;
          };
          if ws.send(Message::Binary(out)).await.is_err() {
            break;
          }
        }
        Message::Close(_) => break,
        _ => {}
      }
    }
  });
  port
}

/// A WebSocket server that answers the initiator's handshake with garbage,
/// so the peer's handshake completion fails. Returns the port.
async fn bad_handshake_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await
      && let Ok(mut ws) = accept_async(stream).await
    {
      // Wait for the initiator frame, then reply with a bogus one.
      loop {
        match ws.next().await {
          Some(Ok(Message::Binary(_))) => {
            let _ = ws.send(Message::Binary(vec![0u8; 10])).await;
            break;
          }
          Some(Ok(_)) => continue,
          _ => return,
        }
      }
      // Drain until closed.
      while let Some(Ok(msg)) = ws.next().await {
        if matches!(msg, Message::Close(_)) {
          break;
        }
      }
    }
  });
  port
}

/// A loopback TCP echo backend that serves one connection. Returns the port.
async fn tcp_echo_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((mut sock, _)) = listener.accept().await {
      let mut buf = [0u8; 4096];
      loop {
        match sock.read(&mut buf).await {
          Ok(0) | Err(_) => break,
          Ok(n) => {
            if sock.write_all(&buf[..n]).await.is_err() {
              break;
            }
          }
        }
      }
    }
  });
  port
}

/// A loopback TCP backend that echoes the first read, then closes the
/// connection — so the backend->tunnel task observes EOF and emits TcpClose.
async fn tcp_echo_once_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((mut sock, _)) = listener.accept().await {
      let mut buf = [0u8; 4096];
      if let Ok(n) = sock.read(&mut buf).await
        && n > 0
      {
        let _ = sock.write_all(&buf[..n]).await;
      }
      // Drop `sock` -> the connection closes.
    }
  });
  port
}

/// Builds a connected loopback TCP pair: returns `(bridge_side, app_side)`.
async fn tcp_pair() -> (TcpStream, TcpStream) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
  let bridge_side = TcpStream::connect(addr).await.unwrap();
  let app_side = accept.await.unwrap();
  (bridge_side, app_side)
}

/// Reads the next `TunnelMessage` from a tunnel receiver (fails on timeout).
async fn next_tunnel_msg(rx: &mut mpsc::Receiver<Message>) -> TunnelMessage {
  loop {
    let m = tokio::time::timeout(Duration::from_secs(2), rx.recv())
      .await
      .expect("timed out waiting for tunnel message")
      .expect("tunnel channel closed");
    if let Message::Text(json) = m {
      return serde_json::from_str(&json).unwrap();
    }
  }
}

/// A registered stream handle whose channels are unrelated to the ones driven
/// by the test — it exists only so the module's `remove()` has an entry.
fn dummy_handle() -> TcpStreamHandle {
  let (tx, _rx) = mpsc::channel::<Vec<u8>>(1);
  let (abort_tx, _abort_rx) = mpsc::channel::<()>(1);
  TcpStreamHandle { tx, abort_tx }
}

#[tokio::test]
async fn test_handle_tcp_open_relays_plaintext() {
  init_tracing();
  let port = tcp_echo_once_port().await;
  let target = format!("127.0.0.1:{}", port);

  let active: Arc<Mutex<HashMap<String, TcpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(8);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(64);
  active.lock().await.insert("t1".to_string(), dummy_handle());

  let h = tokio::spawn(handle_tcp_open(
    "t1".to_string(),
    target,
    tun_tx,
    active.clone(),
    bytes_rx,
    abort_rx,
    None,
  ));

  // tunnel -> backend -> echoed back -> relayed out as base64 TcpData.
  bytes_tx.send(b"hello".to_vec()).await.unwrap();
  match next_tunnel_msg(&mut tun_rx).await {
    TunnelMessage::TcpData { stream_id, data } => {
      assert_eq!(stream_id, "t1");
      assert_eq!(BASE64_STANDARD.decode(data).unwrap(), b"hello");
    }
    other => panic!("unexpected: {:?}", other),
  }

  // The backend closes after echoing: the backend->tunnel task sees EOF and
  // emits a TcpClose, then the stream is torn down and cleaned up. Keep
  // `bytes_tx` alive so the up task (not the down task) drives the close.
  assert!(matches!(
    next_tunnel_msg(&mut tun_rx).await,
    TunnelMessage::TcpClose { .. }
  ));
  tokio::time::timeout(Duration::from_secs(2), h)
    .await
    .expect("relay did not finish")
    .unwrap();
  assert!(active.lock().await.is_empty());
  drop(bytes_tx);
}

#[tokio::test]
async fn test_handle_tcp_open_connect_fails() {
  init_tracing();
  let active: Arc<Mutex<HashMap<String, TcpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (_bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(1);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(8);
  active.lock().await.insert("t2".to_string(), dummy_handle());

  // Nothing listens on port 1 -> connect is refused -> TcpClose + cleanup.
  handle_tcp_open(
    "t2".to_string(),
    "127.0.0.1:1".to_string(),
    tun_tx,
    active.clone(),
    bytes_rx,
    abort_rx,
    None,
  )
  .await;

  assert!(matches!(
    next_tunnel_msg(&mut tun_rx).await,
    TunnelMessage::TcpClose { .. }
  ));
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_handle_tcp_open_e2e_roundtrip() {
  init_tracing();
  let port = tcp_echo_port().await;
  let target = format!("127.0.0.1:{}", port);

  let active: Arc<Mutex<HashMap<String, TcpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(8);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(16);
  active.lock().await.insert("e1".to_string(), dummy_handle());

  let h = tokio::spawn(handle_tcp_open(
    "e1".to_string(),
    target,
    tun_tx,
    active.clone(),
    bytes_rx,
    abort_rx,
    Some(crate::e2e::E2eParams { psk: None }),
  ));

  // Drive the initiator side of the handshake.
  let hs = crate::e2e::Handshake::new(crate::e2e::Role::Initiator, None);
  bytes_tx.send(hs.frame.clone()).await.unwrap();
  let resp_frame = match next_tunnel_msg(&mut tun_rx).await {
    TunnelMessage::TcpData { data, .. } => BASE64_STANDARD.decode(data).unwrap(),
    other => panic!("expected responder handshake, got {:?}", other),
  };
  let session = hs.complete(&resp_frame).expect("handshake should complete");
  let mut sealer = session.sealer;
  let mut opener = session.opener;

  // Sealed payload -> responder opens -> backend echoes -> responder seals.
  let sealed = sealer.seal(b"ping").unwrap();
  bytes_tx.send(sealed).await.unwrap();
  let ciphertext = match next_tunnel_msg(&mut tun_rx).await {
    TunnelMessage::TcpData { data, .. } => BASE64_STANDARD.decode(data).unwrap(),
    other => panic!("expected sealed echo, got {:?}", other),
  };
  assert_eq!(opener.open(&ciphertext).expect("open"), b"ping");

  drop(bytes_tx);
  let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_handle_tcp_open_e2e_handshake_fails() {
  init_tracing();
  let active: Arc<Mutex<HashMap<String, TcpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(2);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(8);
  active.lock().await.insert("e2".to_string(), dummy_handle());

  let h = tokio::spawn(handle_tcp_open(
    "e2".to_string(),
    "127.0.0.1:1".to_string(),
    tun_tx,
    active.clone(),
    bytes_rx,
    abort_rx,
    Some(crate::e2e::E2eParams { psk: None }),
  ));

  // A bogus peer frame fails handshake completion -> TcpClose + cleanup,
  // before any connect to the target is attempted.
  bytes_tx.send(vec![9u8; 36]).await.unwrap();
  let mut saw_close = false;
  for _ in 0..4 {
    if matches!(
      next_tunnel_msg(&mut tun_rx).await,
      TunnelMessage::TcpClose { .. }
    ) {
      saw_close = true;
      break;
    }
  }
  assert!(saw_close, "expected a TcpClose after handshake failure");
  tokio::time::timeout(Duration::from_secs(2), h)
    .await
    .expect("relay did not finish")
    .unwrap();
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_bridge_connection_plaintext() {
  init_tracing();
  let ws_port = echo_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, mut app) = tcp_pair().await;

  let ws_url2 = ws_url.clone();
  let h = tokio::spawn(async move {
    bridge_connection(bridge_side, &ws_url2, "tok", false, None).await;
  });

  app.write_all(b"hello").await.unwrap();
  let mut buf = [0u8; 5];
  tokio::time::timeout(Duration::from_secs(2), app.read_exact(&mut buf))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&buf, b"hello");

  app.shutdown().await.unwrap();
  let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}

#[tokio::test]
async fn test_bridge_connection_e2e_roundtrip() {
  init_tracing();
  let ws_port = responder_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, mut app) = tcp_pair().await;

  let ws_url2 = ws_url.clone();
  let h = tokio::spawn(async move {
    bridge_connection(bridge_side, &ws_url2, "tok", true, None).await;
  });

  app.write_all(b"secret").await.unwrap();
  let mut buf = [0u8; 6];
  tokio::time::timeout(Duration::from_secs(2), app.read_exact(&mut buf))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&buf, b"secret");

  app.shutdown().await.unwrap();
  let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}

#[tokio::test]
async fn test_bridge_connection_e2e_handshake_fails() {
  init_tracing();
  let ws_port = bad_handshake_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, _app) = tcp_pair().await;

  // The server's bogus handshake reply makes completion fail; the bridge
  // closes and returns.
  bridge_connection(bridge_side, &ws_url, "tok", true, None).await;
}

#[tokio::test]
async fn test_bridge_connection_bad_url() {
  init_tracing();
  let (bridge_side, _app) = tcp_pair().await;
  // A malformed URL fails request construction and returns early.
  bridge_connection(bridge_side, "not a url", "tok", false, None).await;
}

#[tokio::test]
async fn test_bridge_connection_connect_fails() {
  init_tracing();
  let (bridge_side, _app) = tcp_pair().await;
  // Nothing listens on port 1 -> connect_async fails and returns.
  bridge_connection(
    bridge_side,
    "ws://127.0.0.1:1/aperio/tcp",
    "tok",
    false,
    None,
  )
  .await;
}

#[tokio::test]
async fn test_spawn_shutdown_watcher_runs() {
  init_tracing();
  // Just installs the signal handler task; no signal fires during the test.
  spawn_shutdown_watcher();
  tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn test_run_tcp_bridge_relays_one_connection() {
  init_tracing();
  let ws_port = echo_ws_port().await;
  let server = format!("http://127.0.0.1:{}", ws_port);
  let local_port = {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
  };

  let h = tokio::spawn(async move {
    run_tcp_bridge(local_port, &server, "tok").await;
  });

  // Connect once the bridge is listening (retry across the bind race).
  let mut client = None;
  for _ in 0..30 {
    if let Ok(c) = TcpStream::connect(("127.0.0.1", local_port)).await {
      client = Some(c);
      break;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let mut client = client.expect("bridge should be listening");

  client.write_all(b"hey").await.unwrap();
  let mut buf = [0u8; 3];
  tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut buf))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&buf, b"hey");

  // Close cleanly and let the spawned per-connection task run to completion
  // (it logs after bridge_connection returns) before stopping the accept loop.
  let _ = client.shutdown().await;
  drop(client);
  tokio::time::sleep(Duration::from_millis(150)).await;
  h.abort();
}

/// An E2E responder WS server that completes the handshake, then sends one
/// tampered ciphertext so the peer's opener fails.
async fn tamper_responder_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    let Ok((stream, _)) = listener.accept().await else {
      return;
    };
    let Ok(mut ws) = accept_async(stream).await else {
      return;
    };
    let init_frame = loop {
      match ws.next().await {
        Some(Ok(Message::Binary(b))) => break b,
        Some(Ok(_)) => continue,
        _ => return,
      }
    };
    let hs = crate::e2e::Handshake::new(crate::e2e::Role::Responder, None);
    if ws.send(Message::Binary(hs.frame.clone())).await.is_err() {
      return;
    }
    if hs.complete(&init_frame).is_none() {
      return;
    }
    // Wait for the peer's first sealed frame, then answer with garbage that
    // cannot be opened.
    while let Some(Ok(msg)) = ws.next().await {
      if let Message::Binary(_) = msg {
        let _ = ws.send(Message::Binary(vec![0u8; 40])).await;
        break;
      }
    }
    while ws.next().await.is_some() {}
  });
  port
}

/// A plaintext WS server that sends a non-binary frame and then a Close, to
/// exercise the bridge's ignore and close arms on the server->local path.
async fn text_then_close_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await
      && let Ok(mut ws) = accept_async(stream).await
    {
      let _ = ws.send(Message::Text("non-binary".to_string())).await;
      let _ = ws.send(Message::Close(None)).await;
      while ws.next().await.is_some() {}
    }
  });
  port
}

/// An E2E server that reads the initiator's handshake but never replies with
/// a handshake frame (closes instead), so the peer's wait loop ends empty.
async fn no_reply_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await
      && let Ok(mut ws) = accept_async(stream).await
    {
      // Consume the initiator frame, then close without answering.
      loop {
        match ws.next().await {
          Some(Ok(Message::Binary(_))) => break,
          Some(Ok(_)) => continue,
          _ => return,
        }
      }
      let _ = ws.send(Message::Close(None)).await;
    }
  });
  port
}

/// A WS server that accepts the upgrade and then drops the connection, so a
/// subsequent send on the peer fails.
async fn drop_after_accept_ws_port() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await {
      let _ = accept_async(stream).await;
      // Drop immediately.
    }
  });
  port
}

#[tokio::test]
async fn test_bridge_connection_e2e_no_handshake_reply() {
  init_tracing();
  let ws_port = no_reply_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, _app) = tcp_pair().await;

  // The server never sends a handshake frame; the wait loop ends with no
  // peer frame and the handshake fails, so the bridge returns.
  bridge_connection(bridge_side, &ws_url, "tok", true, None).await;
}

#[tokio::test]
async fn test_bridge_connection_ws_send_failure() {
  init_tracing();
  let ws_port = drop_after_accept_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, mut app) = tcp_pair().await;

  let ws_url2 = ws_url.clone();
  let h = tokio::spawn(async move {
    bridge_connection(bridge_side, &ws_url2, "tok", false, None).await;
  });

  // Let the server-side drop propagate, then push data: the local->server
  // send then fails and the direction breaks.
  tokio::time::sleep(Duration::from_millis(100)).await;
  let _ = app.write_all(b"data-after-close").await;
  let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
  drop(app);
}

#[tokio::test]
async fn test_bridge_connection_ignores_text_then_closes() {
  init_tracing();
  let ws_port = text_then_close_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, app) = tcp_pair().await;

  // The server sends a non-binary frame (ignored) then a Close (breaks the
  // server->local loop). Drop the local end so the local->server direction
  // also ends; the bridge then returns.
  let h = tokio::spawn(async move {
    bridge_connection(bridge_side, &ws_url, "tok", false, None).await;
  });
  drop(app);
  tokio::time::timeout(Duration::from_secs(3), h)
    .await
    .expect("bridge did not return")
    .unwrap();
}

#[tokio::test]
async fn test_bridge_connection_e2e_decrypt_failure() {
  init_tracing();
  let ws_port = tamper_responder_ws_port().await;
  let ws_url = format!("ws://127.0.0.1:{}/aperio/tcp", ws_port);
  let (bridge_side, mut app) = tcp_pair().await;

  let ws_url2 = ws_url.clone();
  let h = tokio::spawn(async move {
    bridge_connection(bridge_side, &ws_url2, "tok", true, None).await;
  });

  // Drive one plaintext byte through: the bridge seals it, the server replies
  // with a tampered frame, and the bridge's opener fails -> the loop closes.
  app.write_all(b"z").await.unwrap();
  let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
  drop(app);
}

#[tokio::test]
async fn test_handle_tcp_open_e2e_decrypt_failure() {
  init_tracing();
  let port = tcp_echo_port().await;
  let target = format!("127.0.0.1:{}", port);

  let active: Arc<Mutex<HashMap<String, TcpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(8);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(16);
  active.lock().await.insert("ed".to_string(), dummy_handle());

  let h = tokio::spawn(handle_tcp_open(
    "ed".to_string(),
    target,
    tun_tx,
    active.clone(),
    bytes_rx,
    abort_rx,
    Some(crate::e2e::E2eParams { psk: None }),
  ));

  // Complete the handshake as the initiator.
  let hs = crate::e2e::Handshake::new(crate::e2e::Role::Initiator, None);
  bytes_tx.send(hs.frame.clone()).await.unwrap();
  let resp_frame = match next_tunnel_msg(&mut tun_rx).await {
    TunnelMessage::TcpData { data, .. } => BASE64_STANDARD.decode(data).unwrap(),
    other => panic!("expected responder handshake, got {:?}", other),
  };
  let _session = hs.complete(&resp_frame).expect("handshake should complete");

  // Feed a frame the opener cannot decrypt -> the tunnel->backend task fails
  // closed and tears the stream down.
  bytes_tx.send(vec![0u8; 40]).await.unwrap();
  let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
  assert!(active.lock().await.is_empty());
  drop(bytes_tx);
}

#[tokio::test]
async fn test_handle_tcp_open_up_task_send_failure() {
  init_tracing();
  let port = tcp_echo_port().await;
  let target = format!("127.0.0.1:{}", port);

  let active: Arc<Mutex<HashMap<String, TcpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(8);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, tun_rx) = mpsc::channel::<Message>(16);
  // Drop the tunnel receiver: when the backend echoes, the backend->tunnel
  // task's send fails and the task breaks.
  drop(tun_rx);
  active.lock().await.insert("us".to_string(), dummy_handle());

  let h = tokio::spawn(handle_tcp_open(
    "us".to_string(),
    target,
    tun_tx,
    active.clone(),
    bytes_rx,
    abort_rx,
    None,
  ));

  bytes_tx.send(b"echo-me".to_vec()).await.unwrap();
  let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
  assert!(active.lock().await.is_empty());
  drop(bytes_tx);
}
