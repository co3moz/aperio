use super::*;
use tokio::net::{TcpListener, UdpSocket};
use tokio_tungstenite::accept_async;

/// Installs a max-level tracing subscriber once, so the `info!`/`debug!`/
/// `error!` macros actually evaluate their arguments (otherwise the disabled
/// macros short-circuit and their argument expressions never run).
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
/// binary frame back. Returns the `ws://` URL of the `/aperio/udp` endpoint.
async fn ws_echo_server() -> String {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await
      && let Ok(mut ws) = accept_async(stream).await
    {
      // A leading non-binary frame exercises the bridge's ignore arm.
      let _ = ws.send(Message::Text("non-binary".to_string())).await;
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
  format!("ws://127.0.0.1:{}/aperio/udp", port)
}

/// A WebSocket server that accepts one connection and immediately closes it.
async fn ws_close_server() -> String {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    if let Ok((stream, _)) = listener.accept().await
      && let Ok(mut ws) = accept_async(stream).await
    {
      let _ = ws.send(Message::Close(None)).await;
    }
  });
  format!("ws://127.0.0.1:{}/aperio/udp", port)
}

/// Grabs a currently-free UDP port on loopback by binding then dropping it.
async fn free_udp_port() -> u16 {
  let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  s.local_addr().unwrap().port()
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

fn handle(tx: mpsc::Sender<Vec<u8>>, abort_tx: mpsc::Sender<()>) -> UdpStreamHandle {
  UdpStreamHandle { tx, abort_tx }
}

#[test]
fn test_effective_idle_timeout() {
  init_tracing();
  assert_eq!(effective_idle_timeout(Some(5)), Duration::from_secs(5));
  assert_eq!(effective_idle_timeout(Some(0)), Duration::from_secs(0));
  assert_eq!(effective_idle_timeout(None), UDP_IDLE_TIMEOUT);
}

#[tokio::test]
async fn test_handle_udp_open_relays_and_aborts() {
  init_tracing();
  let backend = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let backend_addr = backend.local_addr().unwrap().to_string();

  let active: Arc<Mutex<HashMap<String, UdpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(8);
  let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(16);

  active
    .lock()
    .await
    .insert("s1".to_string(), handle(dg_tx.clone(), abort_tx.clone()));

  let active2 = active.clone();
  let h = tokio::spawn(handle_udp_open(
    "s1".to_string(),
    backend_addr,
    tun_tx,
    active2,
    dg_rx,
    abort_rx,
    Duration::from_secs(30),
  ));

  // tunnel -> backend
  dg_tx.send(b"ping".to_vec()).await.unwrap();
  let mut buf = [0u8; 64];
  let (n, from) = tokio::time::timeout(Duration::from_secs(2), backend.recv_from(&mut buf))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&buf[..n], b"ping");

  // backend -> tunnel (relayed as a base64 UdpDatagram)
  backend.send_to(b"pong", from).await.unwrap();
  match next_tunnel_msg(&mut tun_rx).await {
    TunnelMessage::UdpDatagram { stream_id, data } => {
      assert_eq!(stream_id, "s1");
      assert_eq!(BASE64_STANDARD.decode(data).unwrap(), b"pong");
    }
    other => panic!("unexpected message: {:?}", other),
  }

  // abort -> cleanup + UdpClose
  abort_tx.send(()).await.unwrap();
  assert!(matches!(
    next_tunnel_msg(&mut tun_rx).await,
    TunnelMessage::UdpClose { .. }
  ));
  h.await.unwrap();
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_handle_udp_open_target_unreachable() {
  init_tracing();
  let active: Arc<Mutex<HashMap<String, UdpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(1);
  let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(8);
  active
    .lock()
    .await
    .insert("s2".to_string(), handle(dg_tx, abort_tx));

  // An unresolvable target makes connect() fail (the .invalid TLD never
  // resolves), driving the "target unreachable" branch.
  handle_udp_open(
    "s2".to_string(),
    "no-such-host.invalid:9".to_string(),
    tun_tx,
    active.clone(),
    dg_rx,
    abort_rx,
    Duration::from_secs(30),
  )
  .await;

  assert!(matches!(
    next_tunnel_msg(&mut tun_rx).await,
    TunnelMessage::UdpClose { .. }
  ));
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_handle_udp_open_idle_expiry() {
  init_tracing();
  let backend = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let addr = backend.local_addr().unwrap().to_string();

  let active: Arc<Mutex<HashMap<String, UdpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(1);
  let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(8);
  active
    .lock()
    .await
    .insert("s3".to_string(), handle(dg_tx.clone(), abort_tx.clone()));

  // No traffic + a tiny idle timeout -> the relay expires on its own.
  handle_udp_open(
    "s3".to_string(),
    addr,
    tun_tx,
    active.clone(),
    dg_rx,
    abort_rx,
    Duration::from_millis(20),
  )
  .await;

  assert!(matches!(
    next_tunnel_msg(&mut tun_rx).await,
    TunnelMessage::UdpClose { .. }
  ));
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_handle_udp_open_datagram_sender_closed() {
  init_tracing();
  let backend = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let addr = backend.local_addr().unwrap().to_string();

  let active: Arc<Mutex<HashMap<String, UdpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(1);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, mut tun_rx) = mpsc::channel::<Message>(8);

  let h = tokio::spawn(handle_udp_open(
    "s4".to_string(),
    addr,
    tun_tx,
    active.clone(),
    dg_rx,
    abort_rx,
    Duration::from_secs(30),
  ));

  // Dropping the only datagram sender closes datagram_rx -> None -> break.
  drop(dg_tx);
  assert!(matches!(
    next_tunnel_msg(&mut tun_rx).await,
    TunnelMessage::UdpClose { .. }
  ));
  h.await.unwrap();
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_handle_udp_open_tunnel_closed_stops_relay() {
  init_tracing();
  let backend = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let addr = backend.local_addr().unwrap().to_string();

  let active: Arc<Mutex<HashMap<String, UdpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(4);
  let (_abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, tun_rx) = mpsc::channel::<Message>(8);
  // Close the tunnel receiver: the next relayed datagram hits TrySendError::Closed.
  drop(tun_rx);

  let h = tokio::spawn(handle_udp_open(
    "s5".to_string(),
    addr,
    tun_tx,
    active.clone(),
    dg_rx,
    abort_rx,
    Duration::from_secs(30),
  ));

  // Prime the connected socket so the backend learns the relay's address,
  // then bounce a datagram back — the relay tries to forward it to the
  // closed tunnel and breaks.
  dg_tx.send(b"x".to_vec()).await.unwrap();
  let mut buf = [0u8; 16];
  let (_n, from) = tokio::time::timeout(Duration::from_secs(2), backend.recv_from(&mut buf))
    .await
    .unwrap()
    .unwrap();
  backend.send_to(b"y", from).await.unwrap();

  tokio::time::timeout(Duration::from_secs(2), h)
    .await
    .expect("relay did not stop")
    .unwrap();
  assert!(active.lock().await.is_empty());
}

#[tokio::test]
async fn test_bridge_udp_session_relays_both_ways() {
  init_tracing();
  let ws_url = ws_echo_server().await;
  let relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
  let peer_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let peer_addr = peer_sock.local_addr().unwrap();
  let (tx, rx) = mpsc::channel::<Vec<u8>>(8);

  let relay2 = relay.clone();
  let h = tokio::spawn(async move {
    bridge_udp_session(
      &ws_url,
      "tok",
      relay2,
      peer_addr,
      rx,
      Duration::from_millis(500),
    )
    .await;
  });

  // peer datagram -> WS -> echoed back -> delivered to the peer socket.
  tx.send(b"hello".to_vec()).await.unwrap();
  let mut buf = [0u8; 64];
  let (n, _) = tokio::time::timeout(Duration::from_secs(2), peer_sock.recv_from(&mut buf))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&buf[..n], b"hello");

  // Dropping the sender closes rx and ends the bridge.
  drop(tx);
  tokio::time::timeout(Duration::from_secs(2), h)
    .await
    .expect("bridge did not end")
    .unwrap();
}

#[tokio::test]
async fn test_bridge_udp_session_idle_timeout() {
  init_tracing();
  let ws_url = ws_echo_server().await;
  let relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
  let (_tx, rx) = mpsc::channel::<Vec<u8>>(1);

  // No traffic + a tiny idle timeout -> returns via the idle branch.
  bridge_udp_session(
    &ws_url,
    "tok",
    relay,
    "127.0.0.1:9".parse().unwrap(),
    rx,
    Duration::from_millis(30),
  )
  .await;
}

#[tokio::test]
async fn test_bridge_udp_session_server_closes() {
  init_tracing();
  let ws_url = ws_close_server().await;
  let relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
  let (_tx, rx) = mpsc::channel::<Vec<u8>>(1);

  // The server closes immediately -> the ws_rx Close arm breaks the loop.
  bridge_udp_session(
    &ws_url,
    "tok",
    relay,
    "127.0.0.1:9".parse().unwrap(),
    rx,
    Duration::from_secs(30),
  )
  .await;
}

#[tokio::test]
async fn test_bridge_udp_session_bad_url() {
  init_tracing();
  let relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
  let (_tx, rx) = mpsc::channel::<Vec<u8>>(1);
  // A malformed URL fails request construction and returns early.
  bridge_udp_session(
    "not a url",
    "tok",
    relay,
    "127.0.0.1:9".parse().unwrap(),
    rx,
    Duration::from_secs(1),
  )
  .await;
}

#[tokio::test]
async fn test_bridge_udp_session_connect_fails() {
  init_tracing();
  let relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
  let (_tx, rx) = mpsc::channel::<Vec<u8>>(1);
  // Nothing listens on port 1 -> connect_async fails and returns.
  bridge_udp_session(
    "ws://127.0.0.1:1/aperio/udp",
    "tok",
    relay,
    "127.0.0.1:9".parse().unwrap(),
    rx,
    Duration::from_secs(1),
  )
  .await;
}

#[tokio::test]
async fn test_run_udp_bind_bind_failure_returns() {
  init_tracing();
  let hog = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let port = hog.local_addr().unwrap().port();
  // The port is already bound by `hog`, so run_udp_bind's bind fails and returns.
  run_udp_bind(
    port,
    "ws://127.0.0.1:1/aperio/udp".to_string(),
    "tok".to_string(),
    Duration::from_secs(1),
  )
  .await;
}

#[tokio::test]
async fn test_run_udp_bind_creates_and_reuses_session() {
  init_tracing();
  // A real echo server keeps the per-peer bridge (and its session entry)
  // alive, so a second datagram from the same peer reuses it.
  let ws_url = ws_echo_server().await;
  let port = free_udp_port().await;
  // Short idle timeout: after the two datagrams the bridge idles out, so the
  // session's cleanup path (remove + log) also runs.
  let handle = tokio::spawn(run_udp_bind(
    port,
    ws_url,
    "tok".to_string(),
    Duration::from_millis(150),
  ));

  let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  // Give the bind a moment, then push a datagram (retry across the bind race).
  tokio::time::sleep(Duration::from_millis(50)).await;
  for _ in 0..20 {
    if client.send_to(b"hi", ("127.0.0.1", port)).await.is_ok() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  // A second datagram from the same peer reuses the existing session entry.
  tokio::time::sleep(Duration::from_millis(40)).await;
  let _ = client.send_to(b"hi-again", ("127.0.0.1", port)).await;

  // The echoed datagrams should come back to this client on the same socket.
  let mut buf = [0u8; 64];
  let _ = tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut buf)).await;
  // Wait past the idle timeout so the bridge ends and the session is cleaned up.
  tokio::time::sleep(Duration::from_millis(300)).await;
  handle.abort();
}

#[tokio::test]
async fn test_handle_udp_open_send_recv_errors() {
  init_tracing();
  // Target a loopback UDP port with nothing bound: the connected socket's
  // send queues an ICMP port-unreachable, and a following send/recv reports
  // the error, driving the best-effort send/recv error arms.
  let dead_port = free_udp_port().await;
  let target = format!("127.0.0.1:{}", dead_port);

  let active: Arc<Mutex<HashMap<String, UdpStreamHandle>>> = Arc::new(Mutex::new(HashMap::new()));
  let (dg_tx, dg_rx) = mpsc::channel::<Vec<u8>>(8);
  let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
  let (tun_tx, _tun_rx) = mpsc::channel::<Message>(16);
  active
    .lock()
    .await
    .insert("se".to_string(), handle(dg_tx.clone(), abort_tx.clone()));

  let h = tokio::spawn(handle_udp_open(
    "se".to_string(),
    target,
    tun_tx,
    active.clone(),
    dg_rx,
    abort_rx,
    Duration::from_secs(2),
  ));

  for _ in 0..5 {
    let _ = dg_tx.send(b"x".to_vec()).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  tokio::time::sleep(Duration::from_millis(100)).await;
  let _ = abort_tx.send(()).await;
  let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}
