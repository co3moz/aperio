//! Unit tests for the tunnel-side WebSocket protocol (`ws.rs`).
//!
//! `handle_socket` has no public constructor, so its message-handling and
//! disconnect-cleanup branches are driven through a real in-process axum
//! server and a genuine WebSocket client (`tokio-tungstenite`). Each test
//! connects, discovers the server-assigned client id, seeds `AppState` maps
//! with entries owned by that id (and by a foreign id, to exercise the
//! ownership-gated rejections), sends the relevant tunnel frame, and asserts
//! the effect on the seeded channels/state. `deliver_response_chunk` is also
//! exercised directly.

use super::*;
use crate::protocol::{
  FRAME_RESPONSE_CHUNK, TunnelDecl, TunnelMessage, compress_frame, encode_binary_frame,
};
use crate::state::{
  BodyFrame, PendingRequest, ResponseStreamHandle, TcpConsumerMsg, TcpStreamHandle, TunnelResponse,
  WsStreamHandle, WsStreamMessage,
};
use crate::test_support::*;
use axum::Router;
use axum::routing::get;
use base64::prelude::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

type Client = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// --- harness ----------------------------------------------------------------

/// Spawns an in-process axum server exposing `ws_handler` and returns the
/// `ws://…/ws` URL to connect to.
async fn start_server(state: Arc<AppState>) -> String {
  let app = Router::new()
    .route("/ws", get(ws_handler))
    .with_state(state);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(
      listener,
      app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
  });
  format!("ws://{addr}/ws")
}

fn client_request(url: &str, token: &str) -> axum::http::Request<()> {
  let uri: axum::http::Uri = url.parse().unwrap();
  let host = uri.authority().unwrap().as_str().to_string();
  axum::http::Request::builder()
    .method("GET")
    .uri(url)
    .header("Host", host)
    .header("Connection", "Upgrade")
    .header("Upgrade", "websocket")
    .header("Sec-WebSocket-Version", "13")
    .header("Sec-WebSocket-Key", generate_key())
    .header("Authorization", format!("Bearer {token}"))
    .body(())
    .unwrap()
}

/// Connects a WebSocket client presenting the given bearer token.
async fn connect(url: &str, token: &str) -> Client {
  let (ws, _resp) = tokio_tungstenite::connect_async(client_request(url, token))
    .await
    .unwrap();
  ws
}

/// Waits until exactly one client is registered and returns its id.
async fn wait_client_id(state: &AppState) -> String {
  for _ in 0..400 {
    {
      let clients = state.clients.lock().await;
      if let Some(k) = clients.keys().next() {
        return k.clone();
      }
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  panic!("client never registered");
}

async fn wait_no_clients(state: &AppState) {
  for _ in 0..400 {
    if state.clients.lock().await.is_empty() {
      return;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  panic!("client never cleaned up");
}

async fn send(ws: &mut Client, msg: &TunnelMessage) {
  ws.send(TMessage::Text(serde_json::to_string(msg).unwrap()))
    .await
    .unwrap();
}

/// Reads the next frame with a timeout.
async fn next_frame(ws: &mut Client) -> Option<TMessage> {
  tokio::time::timeout(Duration::from_secs(2), ws.next())
    .await
    .expect("frame timeout")
    .map(|r| r.expect("ws error"))
}

/// Reads frames until a `Pong` (text or compressed) arrives; returns whether
/// the transport frame was binary (i.e. compression is active).
async fn read_until_pong(ws: &mut Client) -> bool {
  loop {
    match next_frame(ws).await.expect("stream ended before pong") {
      TMessage::Text(t) => {
        if let Ok(TunnelMessage::Pong { .. }) = serde_json::from_str::<TunnelMessage>(&t) {
          return false;
        }
      }
      TMessage::Binary(_) => return true,
      _ => {}
    }
  }
}

/// A default Ping with only the connection id set; individual tests mutate the
/// fields they care about.
fn base_ping() -> TunnelMessage {
  TunnelMessage::Ping {
    client_id: "self".into(),
    timestamp: 1,
    path_bind: None,
    hostname_bind: None,
    hostname_binds: Vec::new(),
    max_concurrent: None,
    tcp: false,
    version: None,
    protocol: None,
    backend_healthy: true,
    backend_probed: true,
    priority: 0,
    bandwidth_bps: None,
    service: None,
    public: false,
    visitor_auth: None,
    allowed_ips: Vec::new(),
    tunnels: Vec::new(),
    cache: false,
    resilience: false,
    max_request_body: None,
    response_timeout: None,
    client_key: None,
    webhook_inbox: false,
    denied: None,
  }
}

/// Creates a dynamic token in the store and returns its secret and record id.
async fn make_dynamic_token(state: &AppState, allow_public: bool) -> (String, String) {
  let mut store = state.token_store.lock().await;
  let (rec, secret) = store.create(
    "dyn".into(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    None,
    None,
    None,
    allow_public,
    false,
    None,
  );
  (secret, rec.id)
}

// --- deliver_response_chunk (direct) ---------------------------------------

#[tokio::test]
async fn deliver_chunk_owned_attributes_bytes() {
  let state = Arc::new(test_state());
  let mut c = mock_client(None, None, None, None);
  c.perms.org_id = Some("org1".into());
  c.perms.token_id = Some("tok1".into());
  state.clients.lock().await.insert("owner".into(), c);

  let (tx, mut rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx,
      client_id: "owner".into(),
    },
  );

  deliver_response_chunk(&state, "owner", "r1", vec![1, 2, 3]).await;

  match rx.recv().await.unwrap().unwrap() {
    BodyFrame::Data(d) => assert_eq!(d, vec![1, 2, 3]),
    _ => panic!("expected data frame"),
  }
  assert_eq!(state.stats.lock().await.total_bytes_transferred, 3);
  assert_eq!(
    *state
      .token_daily_bytes
      .lock()
      .await
      .get("tok1")
      .map(|v| &v.1)
      .unwrap_or(&0),
    3
  );
}

#[tokio::test]
async fn deliver_chunk_not_owned_is_rejected() {
  let state = Arc::new(test_state());
  let (tx, mut rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx,
      client_id: "owner".into(),
    },
  );

  deliver_response_chunk(&state, "intruder", "r1", vec![9]).await;

  assert!(rx.try_recv().is_err());
  assert!(state.response_streams.lock().await.contains_key("r1"));
}

#[tokio::test]
async fn deliver_chunk_unknown_stream_is_noop() {
  let state = Arc::new(test_state());
  deliver_response_chunk(&state, "owner", "missing", vec![1]).await;
  assert!(state.response_streams.lock().await.is_empty());
}

#[tokio::test]
async fn deliver_chunk_consumer_gone_drops_stream() {
  let state = Arc::new(test_state());
  let (tx, rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(1);
  drop(rx); // consumer gone: send fails immediately.
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx,
      client_id: "owner".into(),
    },
  );

  deliver_response_chunk(&state, "owner", "r1", vec![1]).await;

  assert!(!state.response_streams.lock().await.contains_key("r1"));
}

// --- Response / ResponseStart / ResponseChunk / ResponseEnd -----------------

#[tokio::test]
async fn response_frame_owned_resolves_pending() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, rx) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    "req1".into(),
    PendingRequest {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::Response {
      id: "req1".into(),
      status: 201,
      headers: vec![("x".into(), "y".into())],
      body: None,
      trailers: None,
      timings: None,
    },
  )
  .await;

  let resp = tokio::time::timeout(Duration::from_secs(2), rx)
    .await
    .expect("resolve timeout")
    .expect("sender dropped");
  assert_eq!(resp.status, 201);
  assert!(state.pending_requests.lock().await.is_empty());

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn response_frame_not_owned_is_kept() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, _rx) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    "req1".into(),
    PendingRequest {
      tx,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::Response {
      id: "req1".into(),
      status: 200,
      headers: vec![],
      body: None,
      trailers: None,
      timings: None,
    },
  )
  .await;
  // Round-trip a Ping to guarantee the Response frame was processed first.
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;

  assert!(state.pending_requests.lock().await.contains_key("req1"));
}

#[tokio::test]
async fn response_frame_dropped_receiver_warns() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, rx) = oneshot::channel::<TunnelResponse>();
  drop(rx);
  state.pending_requests.lock().await.insert(
    "req1".into(),
    PendingRequest {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::Response {
      id: "req1".into(),
      status: 200,
      headers: vec![],
      body: None,
      trailers: None,
      timings: None,
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(state.pending_requests.lock().await.is_empty());
}

#[tokio::test]
async fn response_stream_lifecycle() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, rx) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    "s1".into(),
    PendingRequest {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::ResponseStart {
      id: "s1".into(),
      status: 200,
      headers: vec![],
    },
  )
  .await;
  let mut resp = tokio::time::timeout(Duration::from_secs(2), rx)
    .await
    .expect("start timeout")
    .expect("dropped");
  let mut body_rx = resp.stream_rx.take().expect("expected stream_rx");
  assert!(state.response_streams.lock().await.contains_key("s1"));

  // Base64 chunk path.
  send(
    &mut ws,
    &TunnelMessage::ResponseChunk {
      id: "s1".into(),
      data: BASE64_STANDARD.encode([7u8, 8, 9]),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), body_rx.recv())
    .await
    .unwrap()
    .unwrap()
    .unwrap()
  {
    BodyFrame::Data(d) => assert_eq!(d, vec![7, 8, 9]),
    _ => panic!("expected data"),
  }

  // Binary frame chunk path (FRAME_RESPONSE_CHUNK).
  ws.send(TMessage::Binary(encode_binary_frame(
    FRAME_RESPONSE_CHUNK,
    "s1",
    &[1, 2],
  )))
  .await
  .unwrap();
  match tokio::time::timeout(Duration::from_secs(2), body_rx.recv())
    .await
    .unwrap()
    .unwrap()
    .unwrap()
  {
    BodyFrame::Data(d) => assert_eq!(d, vec![1, 2]),
    _ => panic!("expected data"),
  }

  // End with trailers.
  send(
    &mut ws,
    &TunnelMessage::ResponseEnd {
      id: "s1".into(),
      trailers: Some(vec![("grpc-status".into(), "0".into())]),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), body_rx.recv())
    .await
    .unwrap()
    .unwrap()
    .unwrap()
  {
    BodyFrame::Trailers(t) => assert_eq!(t[0].0, "grpc-status"),
    _ => panic!("expected trailers"),
  }
  assert!(!state.response_streams.lock().await.contains_key("s1"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn response_start_dropped_receiver_removes_stream() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, rx) = oneshot::channel::<TunnelResponse>();
  drop(rx);
  state.pending_requests.lock().await.insert(
    "s1".into(),
    PendingRequest {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::ResponseStart {
      id: "s1".into(),
      status: 200,
      headers: vec![],
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(!state.response_streams.lock().await.contains_key("s1"));
}

#[tokio::test]
async fn response_chunk_bad_base64_removes_stream() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, _rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "s1".into(),
    ResponseStreamHandle {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::ResponseChunk {
      id: "s1".into(),
      data: "not base64!!!".into(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(!state.response_streams.lock().await.contains_key("s1"));
}

#[tokio::test]
async fn response_end_not_owned_is_reinserted() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, _rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "s1".into(),
    ResponseStreamHandle {
      tx,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::ResponseEnd {
      id: "s1".into(),
      trailers: None,
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(state.response_streams.lock().await.contains_key("s1"));
}

// --- compressed frame + CompressionAck --------------------------------------

#[tokio::test]
async fn compressed_ping_is_decoded() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let json = serde_json::to_string(&base_ping()).unwrap();
  ws.send(TMessage::Binary(compress_frame(&json)))
    .await
    .unwrap();
  // A decoded Ping still yields a Pong.
  assert!(!read_until_pong(&mut ws).await);

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn compression_ack_compresses_outgoing() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  send(&mut ws, &TunnelMessage::CompressionAck {}).await;
  send(&mut ws, &base_ping()).await;
  // After the ack the writer compresses the Pong into a binary frame.
  assert!(read_until_pong(&mut ws).await);

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// --- TCP / UDP data frames --------------------------------------------------

#[tokio::test]
async fn tcp_data_and_close_owned() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".into(),
      data: BASE64_STANDARD.encode([4u8, 5]),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d, vec![4, 5]),
    _ => panic!("expected data"),
  }

  // Bad base64: ignored, stream kept.
  send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".into(),
      data: "###".into(),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::TcpClose {
      stream_id: "t1".into(),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Close => {}
    _ => panic!("expected close"),
  }
  assert!(!state.tcp_streams.lock().await.contains_key("t1"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn tcp_data_and_close_not_owned() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".into(),
      data: BASE64_STANDARD.encode([1u8]),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::TcpClose {
      stream_id: "t1".into(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(rx.try_recv().is_err());
  // Not owned: TcpClose reinserts it.
  assert!(state.tcp_streams.lock().await.contains_key("t1"));
}

#[tokio::test]
async fn udp_datagram_and_close_owned() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.udp_streams.lock().await.insert(
    "u1".into(),
    TcpStreamHandle {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "u1".into(),
      data: BASE64_STANDARD.encode([6u8]),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d, vec![6]),
    _ => panic!("expected data"),
  }
  // Bad base64 for udp is ignored.
  send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "u1".into(),
      data: "%%%".into(),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::UdpClose {
      stream_id: "u1".into(),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Close => {}
    _ => panic!("expected close"),
  }
  assert!(!state.udp_streams.lock().await.contains_key("u1"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn udp_not_owned_rejected() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.udp_streams.lock().await.insert(
    "u1".into(),
    TcpStreamHandle {
      tx,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "u1".into(),
      data: BASE64_STANDARD.encode([1u8]),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::UdpClose {
      stream_id: "u1".into(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(rx.try_recv().is_err());
  assert!(state.udp_streams.lock().await.contains_key("u1"));
}

// --- WebSocket relay frames -------------------------------------------------

#[tokio::test]
async fn ws_data_text_and_binary_and_close() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<WsStreamMessage>(8);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: "hello".into(),
      is_text: true,
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    WsStreamMessage::Data(Message::Text(t)) => assert_eq!(t, "hello"),
    _ => panic!("expected text data"),
  }

  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: BASE64_STANDARD.encode([1u8, 2, 3]),
      is_text: false,
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    WsStreamMessage::Data(Message::Binary(b)) => assert_eq!(b, vec![1, 2, 3]),
    _ => panic!("expected binary data"),
  }

  // Bad base64 binary: skipped without closing the stream.
  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: "@@@".into(),
      is_text: false,
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::WsClose {
      stream_id: "w1".into(),
      code: 1000,
      reason: "bye".into(),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    WsStreamMessage::Close => {}
    _ => panic!("expected close"),
  }

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn ws_data_not_owned_ignored() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<WsStreamMessage>(8);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: "hi".into(),
      is_text: true,
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::WsClose {
      stream_id: "w1".into(),
      code: 1000,
      reason: String::new(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(rx.try_recv().is_err());
}

// --- UpgradeResponse --------------------------------------------------------

#[tokio::test]
async fn upgrade_response_owned_and_dropped() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, rx) = oneshot::channel::<TunnelResponse>();
  state.pending_upgrades.lock().await.insert(
    "up1".into(),
    PendingRequest {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UpgradeResponse {
      id: "up1".into(),
      status: 101,
      headers: vec![],
    },
  )
  .await;
  let resp = tokio::time::timeout(Duration::from_secs(2), rx)
    .await
    .expect("timeout")
    .expect("dropped");
  assert_eq!(resp.status, 101);

  // Not-owned variant is rejected and kept.
  let (tx2, _rx2) = oneshot::channel::<TunnelResponse>();
  state.pending_upgrades.lock().await.insert(
    "up2".into(),
    PendingRequest {
      tx: tx2,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UpgradeResponse {
      id: "up2".into(),
      status: 101,
      headers: vec![],
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(state.pending_upgrades.lock().await.contains_key("up2"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// --- Draining ---------------------------------------------------------------

#[tokio::test]
async fn draining_marks_client() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  send(&mut ws, &TunnelMessage::Draining {}).await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(state.clients.lock().await.get(&cid).unwrap().draining);

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// --- Ping handler -----------------------------------------------------------

#[tokio::test]
async fn ping_master_applies_all_binds() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut path_bind,
    ref mut hostname_bind,
    ref mut hostname_binds,
    ref mut max_concurrent,
    ref mut tcp,
    ref mut version,
    ref mut protocol,
    ref mut priority,
    ref mut bandwidth_bps,
    ref mut service,
    ref mut public,
    ref mut visitor_auth,
    ref mut allowed_ips,
    ref mut tunnels,
    ref mut cache,
    ref mut resilience,
    ref mut max_request_body,
    ref mut response_timeout,
    ref mut webhook_inbox,
    ref mut denied,
    ref mut backend_healthy,
    ..
  } = ping
  {
    *path_bind = Some("/api".into());
    *hostname_bind = Some("example.com".into());
    *hostname_binds = vec!["a.example.com".into(), "b.example.com".into()];
    *max_concurrent = Some(4);
    *tcp = true;
    *version = Some("9.9.9".into());
    *protocol = Some(9999);
    *priority = 7;
    *bandwidth_bps = Some(1_000_000);
    *service = Some("svc".into());
    *public = true;
    *visitor_auth = Some("user:pass".into());
    *allowed_ips = vec!["127.0.0.1".into(), "bogus".into()];
    *tunnels = vec![TunnelDecl {
      target: "127.0.0.1:9".into(),
      protocol: "tcp".into(),
      encrypt: false,
      idle_timeout: None,
      expose: None,
    }];
    *cache = true;
    *resilience = true;
    *max_request_body = Some(1000);
    *response_timeout = Some(30);
    *webhook_inbox = true;
    *denied = Some("https://example.com/denied".into());
    *backend_healthy = false;
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;

  {
    let clients = state.clients.lock().await;
    let h = clients.get(&cid).unwrap();
    assert_eq!(h.declared_path.as_deref(), Some("/api"));
    assert_eq!(h.declared_hostnames.len(), 2);
    assert_eq!(h.max_concurrent, Some(4));
    assert!(h.tcp_enabled);
    assert!(h.cache);
    assert!(h.resilience);
    assert!(h.webhook_inbox);
    assert!(h.public);
    assert!(h.visitor_auth.is_some());
    assert_eq!(h.allowed_ips, vec!["127.0.0.1".to_string()]);
    assert!(h.denied.is_some());
    assert_eq!(h.response_timeout, Some(30));
    assert_eq!(h.max_request_body, Some(1000));
    assert_eq!(h.priority, 7);
    assert_eq!(h.service_name.as_deref(), Some("svc"));
    assert_eq!(h.reported_instance_id.as_deref(), Some("self"));
    assert!(!h.backend_healthy);
  }

  // A second, identical Ping exercises the "no change" / warn-once branches
  // and the healthy-again transition.
  let mut ping2 = ping.clone();
  if let TunnelMessage::Ping {
    ref mut backend_healthy,
    ..
  } = ping2
  {
    *backend_healthy = true;
  }
  send(&mut ws, &ping2).await;
  read_until_pong(&mut ws).await;
  assert!(
    state
      .clients
      .lock()
      .await
      .get(&cid)
      .unwrap()
      .backend_healthy
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn ping_master_invalid_visitor_and_denied() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut visitor_auth,
    ref mut denied,
    ..
  } = ping
  {
    *visitor_auth = Some("no-colon-here".into()); // invalid creds
    *denied = Some("ftp://bad".into()); // not http(s) -> filtered
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;

  let clients = state.clients.lock().await;
  let h = clients.get(&cid).unwrap();
  assert!(h.visitor_auth.is_none());
  assert!(h.denied.is_none());
}

#[tokio::test]
async fn ping_dynamic_token_denies_public_and_visitor_auth() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let (secret, _id) = make_dynamic_token(&state, false).await;
  let mut ws = connect(&url, &secret).await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut public,
    ref mut visitor_auth,
    ref mut allowed_ips,
    ..
  } = ping
  {
    *public = true;
    *visitor_auth = Some("user:pass".into());
    *allowed_ips = vec!["10.0.0.0/8".into(), "junk".into()];
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;
  // Second ping to hit the warned-once guards.
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;

  let clients = state.clients.lock().await;
  let h = clients.get(&cid).unwrap();
  assert!(!h.public);
  assert!(h.visitor_auth.is_none());
  assert!(h.public_denied_warned);
  assert!(h.visitor_auth_denied_warned);
  assert_eq!(h.allowed_ips, vec!["10.0.0.0/8".to_string()]);
}

// --- Token pinning ----------------------------------------------------------

#[tokio::test]
async fn token_pinning_pins_then_rejects_mismatch() {
  let mut cfg = test_config();
  cfg.token_pinning = true;
  let state = Arc::new(test_state_with(cfg));
  let url = start_server(state.clone()).await;
  let (secret, _id) = make_dynamic_token(&state, false).await;

  // First connection pins the device key.
  let mut ws = connect(&url, &secret).await;
  let _cid = wait_client_id(&state).await;
  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut client_key, ..
  } = ping
  {
    *client_key = Some("device-key-1".into());
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;
  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;

  // Second connection with no key: pinning fails closed and disconnects.
  let mut ws2 = connect(&url, &secret).await;
  wait_client_id(&state).await;
  send(&mut ws2, &base_ping()).await; // no client_key -> Mismatch -> break
  // The server force-closes (an abrupt reset counts as a disconnect); we must
  // never receive a Pong before the connection ends.
  loop {
    let frame = tokio::time::timeout(Duration::from_secs(2), ws2.next())
      .await
      .expect("frame timeout");
    match frame {
      None | Some(Err(_)) | Some(Ok(TMessage::Close(_))) => break,
      Some(Ok(TMessage::Text(t))) => {
        if let Ok(msg) = serde_json::from_str::<TunnelMessage>(&t) {
          assert!(
            !matches!(msg, TunnelMessage::Pong { .. }),
            "unexpected pong after pin mismatch"
          );
        }
      }
      Some(Ok(_)) => {}
    }
  }
  wait_no_clients(&state).await;
}

// --- disconnect cleanup -----------------------------------------------------

#[tokio::test]
async fn disconnect_drains_all_owned_state() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  // Seed owned entries across every map.
  let (preq_tx, _preq_rx) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    "p1".into(),
    PendingRequest {
      tx: preq_tx,
      client_id: cid.clone(),
    },
  );
  let (pup_tx, _pup_rx) = oneshot::channel::<TunnelResponse>();
  state.pending_upgrades.lock().await.insert(
    "u1".into(),
    PendingRequest {
      tx: pup_tx,
      client_id: cid.clone(),
    },
  );
  let (rs_tx, _rs_rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx: rs_tx,
      client_id: cid.clone(),
    },
  );
  let (tcp_tx, mut tcp_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx: tcp_tx,
      client_id: cid.clone(),
    },
  );
  let (udp_tx, mut udp_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.udp_streams.lock().await.insert(
    "d1".into(),
    TcpStreamHandle {
      tx: udp_tx,
      client_id: cid.clone(),
    },
  );
  let (wss_tx, mut wss_rx) = mpsc::channel::<WsStreamMessage>(4);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx: wss_tx,
      client_id: cid.clone(),
    },
  );
  // A foreign entry that must survive.
  let (foreign_tx, _foreign_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.tcp_streams.lock().await.insert(
    "keep".into(),
    TcpStreamHandle {
      tx: foreign_tx,
      client_id: "foreign".into(),
    },
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;

  // Give cleanup a moment to drain the maps.
  for _ in 0..200 {
    if state.pending_requests.lock().await.is_empty()
      && state.pending_upgrades.lock().await.is_empty()
      && state.response_streams.lock().await.is_empty()
      && state.udp_streams.lock().await.is_empty()
      && state.ws_streams.lock().await.is_empty()
      && !state.tcp_streams.lock().await.contains_key("t1")
    {
      break;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }

  assert!(state.pending_requests.lock().await.is_empty());
  assert!(state.pending_upgrades.lock().await.is_empty());
  assert!(state.response_streams.lock().await.is_empty());
  assert!(!state.tcp_streams.lock().await.contains_key("t1"));
  assert!(state.tcp_streams.lock().await.contains_key("keep")); // foreign kept
  assert!(state.udp_streams.lock().await.is_empty());
  assert!(state.ws_streams.lock().await.is_empty());
  // Consumers were signalled Close.
  assert!(matches!(tcp_rx.recv().await, Some(TcpConsumerMsg::Close)));
  assert!(matches!(udp_rx.recv().await, Some(TcpConsumerMsg::Close)));
  assert!(matches!(wss_rx.recv().await, Some(WsStreamMessage::Close)));
  // Tunnel slot released.
  assert_eq!(
    state
      .active_tunnel_count
      .load(std::sync::atomic::Ordering::SeqCst),
    0
  );
}

// --- ws_handler rejection paths --------------------------------------------

#[tokio::test]
async fn ws_handler_rejects_unauthorized() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let res = tokio_tungstenite::connect_async(client_request(&url, "wrong-token")).await;
  assert!(res.is_err(), "bad token must fail the handshake");
}

#[tokio::test]
async fn ws_handler_rejects_when_tunnels_full() {
  let mut cfg = test_config();
  cfg.max_tunnels = 0;
  let state = Arc::new(test_state_with(cfg));
  let url = start_server(state.clone()).await;
  let res = tokio_tungstenite::connect_async(client_request(&url, "test")).await;
  assert!(res.is_err(), "full tunnel table must reject the upgrade");
}
