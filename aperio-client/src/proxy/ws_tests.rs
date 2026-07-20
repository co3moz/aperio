use super::*;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;

/// Receives the next tunnel message, decoded from JSON, within a short bound
/// so a hung relay fails the test instead of blocking forever.
async fn next_tunnel_msg(rx: &mut mpsc::Receiver<Message>) -> TunnelMessage {
  let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
    .await
    .expect("timed out waiting for tunnel message")
    .expect("tunnel channel closed");
  match msg {
    Message::Text(json) => serde_json::from_str(&json).unwrap(),
    other => panic!("expected text tunnel message, got {:?}", other),
  }
}

/// Waits until the stream handle is registered and returns its sender/abort.
async fn wait_for_handle(
  streams: &Arc<Mutex<HashMap<String, WsStreamHandle>>>,
  id: &str,
) -> (mpsc::Sender<Message>, mpsc::Sender<()>) {
  for _ in 0..100 {
    {
      let map = streams.lock().await;
      if let Some(h) = map.get(id) {
        return (h.tx.clone(), h.abort_tx.clone());
      }
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  panic!("stream handle never registered");
}

/// Starts a WebSocket backend. `mode` selects behaviour:
/// "echo" mirrors text/binary frames; "close" closes with a coded frame right
/// after handshake; "stall" accepts the TCP connection but never completes the
/// WebSocket handshake (drives the connect timeout).
async fn start_ws_backend(mode: &'static str) -> u16 {
  use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    while let Ok((stream, _)) = listener.accept().await {
      tokio::spawn(async move {
        if mode == "stall" {
          // Hold the TCP connection open without answering the handshake.
          tokio::time::sleep(Duration::from_secs(3600)).await;
          drop(stream);
          return;
        }
        let ws = match tokio_tungstenite::accept_async(stream).await {
          Ok(ws) => ws,
          Err(_) => return,
        };
        let (mut writer, mut reader) = ws.split();
        match mode {
          "close" => {
            let frame = CloseFrame {
              code: CloseCode::Away,
              reason: "backend-bye".into(),
            };
            let _ = writer.send(Message::Close(Some(frame))).await;
          }
          _ => {
            while let Some(Ok(msg)) = reader.next().await {
              match msg {
                Message::Text(_) | Message::Binary(_) => {
                  if writer.send(msg).await.is_err() {
                    break;
                  }
                }
                Message::Close(_) => {
                  let _ = writer.send(Message::Close(None)).await;
                  break;
                }
                _ => {}
              }
            }
          }
        }
      });
    }
  });
  port
}

fn new_streams() -> Arc<Mutex<HashMap<String, WsStreamHandle>>> {
  Arc::new(Mutex::new(HashMap::new()))
}

#[tokio::test]
async fn test_ws_echo_text_and_binary() {
  let port = start_ws_backend("echo").await;
  let target = format!("http://127.0.0.1:{}", port);
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(64);
  let streams = new_streams();

  let streams_c = streams.clone();
  let handle = tokio::spawn(async move {
    handle_upgrade_request(
      "ws-1".to_string(),
      "GET".to_string(),
      "/socket".to_string(),
      vec![("origin".to_string(), "http://example.com".to_string())],
      &target,
      None,
      false,
      tunnel_tx,
      streams_c,
      10,
    )
    .await;
  });

  // First message must be the 101 upgrade response.
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::UpgradeResponse { id, status, .. } => {
      assert_eq!(id, "ws-1");
      assert_eq!(status, 101);
    }
    other => panic!("expected UpgradeResponse, got {:?}", other),
  }

  let (backend_tx, abort_tx) = wait_for_handle(&streams, "ws-1").await;

  // Text frame round-trip.
  backend_tx
    .send(Message::Text("hi there".into()))
    .await
    .unwrap();
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::WsData {
      stream_id,
      data,
      is_text,
    } => {
      assert_eq!(stream_id, "ws-1");
      assert!(is_text);
      assert_eq!(data, "hi there");
    }
    other => panic!("expected text WsData, got {:?}", other),
  }

  // Binary frame round-trip.
  backend_tx
    .send(Message::Binary(vec![1, 2, 3, 4]))
    .await
    .unwrap();
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::WsData { data, is_text, .. } => {
      assert!(!is_text);
      assert_eq!(BASE64_STANDARD.decode(data).unwrap(), vec![1, 2, 3, 4]);
    }
    other => panic!("expected binary WsData, got {:?}", other),
  }

  // Abort → writer sends Close to backend, backend echoes Close, relay ends.
  abort_tx.send(()).await.unwrap();
  let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

  // Handle removed on teardown.
  assert!(streams.lock().await.get("ws-1").is_none());
}

#[tokio::test]
async fn test_ws_backend_closes_emits_wsclose() {
  let port = start_ws_backend("close").await;
  let target = format!("http://127.0.0.1:{}", port);
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(64);
  let streams = new_streams();

  let streams_c = streams.clone();
  tokio::spawn(async move {
    handle_upgrade_request(
      "ws-close".to_string(),
      "GET".to_string(),
      "/".to_string(),
      vec![],
      &target,
      None,
      false,
      tunnel_tx,
      streams_c,
      10,
    )
    .await;
  });

  // 101 first.
  assert!(matches!(
    next_tunnel_msg(&mut rx).await,
    TunnelMessage::UpgradeResponse { status: 101, .. }
  ));

  // Backend closes with a coded frame → the code/reason are relayed.
  let mut saw_close = false;
  for _ in 0..4 {
    if let TunnelMessage::WsClose {
      stream_id,
      code,
      reason,
    } = next_tunnel_msg(&mut rx).await
    {
      assert_eq!(stream_id, "ws-close");
      // First WsClose carries the backend's coded close frame (1001 Away).
      assert_eq!(code, 1001);
      assert_eq!(reason, "backend-bye");
      saw_close = true;
      break;
    }
  }
  assert!(saw_close, "expected a WsClose after backend closed");
}

#[tokio::test]
async fn test_ws_backend_handshake_timeout() {
  // Backend accepts TCP but never completes the WS handshake → 504.
  let port = start_ws_backend("stall").await;
  let target = format!("http://127.0.0.1:{}", port);
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(8);
  handle_upgrade_request(
    "ws-timeout".to_string(),
    "GET".to_string(),
    "/".to_string(),
    vec![],
    &target,
    None,
    false,
    tunnel_tx,
    new_streams(),
    1,
  )
  .await;
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::UpgradeResponse { status, .. } => assert_eq!(status, 504),
    other => panic!("expected 504 UpgradeResponse, got {:?}", other),
  }
}

#[tokio::test]
async fn test_ws_unix_target_rejected() {
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(8);
  handle_upgrade_request(
    "ws-unix".to_string(),
    "GET".to_string(),
    "/".to_string(),
    vec![],
    "unix:///var/run/app.sock",
    None,
    false,
    tunnel_tx,
    new_streams(),
    10,
  )
  .await;
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::UpgradeResponse { status, .. } => assert_eq!(status, 502),
    other => panic!("expected 502 UpgradeResponse, got {:?}", other),
  }
}

#[tokio::test]
async fn test_ws_invalid_target_rejected() {
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(8);
  handle_upgrade_request(
    "ws-bad".to_string(),
    "GET".to_string(),
    "/".to_string(),
    vec![],
    "::::not a url::::",
    None,
    false,
    tunnel_tx,
    new_streams(),
    10,
  )
  .await;
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::UpgradeResponse { status, .. } => assert_eq!(status, 502),
    other => panic!("expected 502 UpgradeResponse, got {:?}", other),
  }
}

#[tokio::test]
async fn test_ws_bad_incoming_uri_rejected() {
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(8);
  // A bad port in the spliced `http://localhost<uri>` URL fails to parse → 400.
  handle_upgrade_request(
    "ws-uri".to_string(),
    "GET".to_string(),
    ":notaport".to_string(),
    vec![],
    "http://127.0.0.1:65535",
    None,
    false,
    tunnel_tx,
    new_streams(),
    10,
  )
  .await;
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::UpgradeResponse { status, .. } => assert_eq!(status, 400),
    other => panic!("expected 400 UpgradeResponse, got {:?}", other),
  }
}

#[tokio::test]
async fn test_ws_backend_unreachable() {
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(8);
  // Port 1 has no listener → connect fails → 502.
  handle_upgrade_request(
    "ws-refused".to_string(),
    "GET".to_string(),
    "/".to_string(),
    vec![],
    "http://127.0.0.1:1",
    None,
    false,
    tunnel_tx,
    new_streams(),
    10,
  )
  .await;
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::UpgradeResponse { status, .. } => assert_eq!(status, 502),
    other => panic!("expected 502 UpgradeResponse, got {:?}", other),
  }
}

#[tokio::test]
async fn test_ws_trim_bind_path() {
  // trim_bind rewrites `/api/socket` → `/socket`; the echo backend accepts any
  // path, so success (101 + round-trip) confirms the branch executed cleanly.
  let port = start_ws_backend("echo").await;
  let target = format!("http://127.0.0.1:{}", port);
  let (tunnel_tx, mut rx) = mpsc::channel::<Message>(64);
  let streams = new_streams();

  let streams_c = streams.clone();
  tokio::spawn(async move {
    handle_upgrade_request(
      "ws-trim".to_string(),
      "GET".to_string(),
      "/api/socket?x=1".to_string(),
      vec![("origin".to_string(), "http://example.com".to_string())],
      &target,
      Some("/api".to_string()),
      true,
      tunnel_tx,
      streams_c,
      10,
    )
    .await;
  });

  assert!(matches!(
    next_tunnel_msg(&mut rx).await,
    TunnelMessage::UpgradeResponse { status: 101, .. }
  ));
  let (backend_tx, abort_tx) = wait_for_handle(&streams, "ws-trim").await;
  backend_tx.send(Message::Text("ping".into())).await.unwrap();
  match next_tunnel_msg(&mut rx).await {
    TunnelMessage::WsData { data, .. } => assert_eq!(data, "ping"),
    other => panic!("expected WsData, got {:?}", other),
  }
  abort_tx.send(()).await.unwrap();
}
