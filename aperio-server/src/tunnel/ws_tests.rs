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
use crate::store::tokens::TokenSpec;
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
      let clients = state.clients.read().await;
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
    if state.clients.write().await.is_empty() {
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
    service_custom_name: None,
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
    cpu_percent: None,
    rss_bytes: None,
    rtt_ms: None,
    jitter_ms: None,
    reconnects: None,
    priority: 0,
    bandwidth_bps: None,
    service: None,
    public: false,
    visitor_auth: None,
    visitor_auth_methods: None,
    allowed_ips: Vec::new(),
    tunnels: Vec::new(),
    cache: false,
    resilience: false,
    no_capture: false,
    max_request_body: None,
    response_timeout: None,
    client_key: None,
    webhook_inbox: false,
    denied: None,
    scaling: None,
    connections: None,
    connections_min: None,
    connections_max: None,
    config_notes: Vec::new(),
    metrics_labels: Default::default(),
    drain_secs: None,
  }
}

/// Creates a dynamic token in the store and returns its secret and record id.
async fn make_dynamic_token(state: &AppState, allow_public: bool) -> (String, String) {
  let mut store = state.token_store.lock().await;
  let (rec, secret) = store.create(TokenSpec {
    name: "dyn".into(),
    allow_public,
    ..Default::default()
  });
  (secret, rec.id)
}

// --- deliver_response_chunk (direct) ---------------------------------------

/// A `ConnCtx` detached from any real socket, for driving the delivery path
/// directly.
fn test_ctx(state: &Arc<AppState>, client_id: &str) -> ConnCtx {
  let (tx_write, _rx) = mpsc::channel::<Message>(8);
  ConnCtx {
    state: state.clone(),
    client_id: client_id.into(),
    client_ip: "127.0.0.1".into(),
    tx_write,
    compress_out: Arc::new(AtomicBool::new(false)),
    perms: ClientPerms::master(),
    server_max_connections: 0,
    max_inflated: 8 * 1024 * 1024,
    stream_cache: std::sync::Mutex::new(HashMap::new()),
  }
}

#[tokio::test]
async fn deliver_chunk_owned_attributes_bytes_when_the_stream_ends() {
  let state = Arc::new(test_state());
  let mut ctx = test_ctx(&state, "owner");
  ctx.perms.org_id = Some("org1".into());
  ctx.perms.token_id = Some("tok1".into());

  let (tx, mut rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: "owner".into(),
    },
  );

  ctx.deliver_response_chunk("r1", vec![1, 2, 3].into()).await;

  match rx.recv().await.unwrap().unwrap() {
    BodyFrame::Data(d) => assert_eq!(d.as_ref(), &[1, 2, 3]),
    _ => panic!("expected data frame"),
  }
  // Accounting is batched: below the flush threshold nothing is charged yet;
  // settling the stream flushes the remainder to every counter.
  assert_eq!(state.stats.lock().await.total_bytes_transferred, 0);
  ctx.finish_stream_accounting("r1").await;
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
async fn deliver_chunk_crossing_the_batch_threshold_flushes_inline() {
  let state = Arc::new(test_state());
  let mut ctx = test_ctx(&state, "owner");
  ctx.perms.org_id = Some("org1".into());
  ctx.perms.token_id = Some("tok1".into());

  let (tx, mut rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: "owner".into(),
    },
  );

  let big = STREAM_ACCOUNT_FLUSH_BYTES as usize;
  ctx
    .deliver_response_chunk("r1", vec![0u8; big].into())
    .await;
  let _ = rx.recv().await.unwrap().unwrap();

  // At the threshold the charge lands without waiting for the stream to end.
  assert_eq!(state.stats.lock().await.total_bytes_transferred, big as u64);
  // Settling afterwards adds nothing twice.
  ctx.finish_stream_accounting("r1").await;
  assert_eq!(state.stats.lock().await.total_bytes_transferred, big as u64);
}

#[tokio::test]
async fn deliver_chunk_not_owned_is_rejected() {
  let state = Arc::new(test_state());
  let (tx, mut rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: "owner".into(),
    },
  );

  let ctx = test_ctx(&state, "intruder");
  ctx.deliver_response_chunk("r1", vec![9].into()).await;

  assert!(rx.try_recv().is_err());
  assert!(state.response_streams.lock().await.contains_key("r1"));
  // A rejected sender must not gain a cached handle to the stream either.
  assert!(ctx.stream_cache.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deliver_chunk_unknown_stream_is_noop() {
  let state = Arc::new(test_state());
  let ctx = test_ctx(&state, "owner");
  ctx.deliver_response_chunk("missing", vec![1].into()).await;
  assert!(state.response_streams.lock().await.is_empty());
}

#[tokio::test]
async fn deliver_chunk_consumer_gone_drops_stream() {
  let state = Arc::new(test_state());
  let (tx, rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(1);
  drop(rx); // consumer gone: the pump ends as soon as it tries to forward.
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: "owner".into(),
    },
  );

  // The first chunk may still be accepted into the pump's queue before the
  // pump notices the dead consumer; the stream must be gone shortly after.
  let ctx = test_ctx(&state, "owner");
  for _ in 0..100 {
    ctx.deliver_response_chunk("r1", vec![1].into()).await;
    if !state.response_streams.lock().await.contains_key("r1") {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert!(!state.response_streams.lock().await.contains_key("r1"));
  // The failed push evicted the cached handle too.
  assert!(ctx.stream_cache.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stalled_consumer_never_blocks_the_read_loop() {
  let state = Arc::new(test_state());
  state
    .clients
    .write()
    .await
    .insert("owner".into(), mock_client(None, None, None, None));

  // A visitor that has stopped reading: the consumer channel is never
  // drained. A short stall timeout stands in for gateway_response_timeout.
  let (tx, _held_rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(2);
  let tx = crate::state::spawn_consumer_pump(
    tx,
    Duration::from_millis(100),
    crate::state::StreamFlow::detached("r1"),
  );
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx,
      client_id: "owner".into(),
    },
  );

  // Every chunk must be handled promptly regardless. Before the pump, the
  // read loop waited here for gateway_response_timeout once the channel
  // filled, stalling every other stream on the same tunnel. The chunks are
  // buffered (well under the backlog cap), NOT dropped: a slow-but-alive
  // visitor must not lose its stream just because the producer ran ahead.
  let ctx = test_ctx(&state, "owner");
  for _ in 0..10 {
    tokio::time::timeout(
      Duration::from_secs(5),
      ctx.deliver_response_chunk("r1", vec![1, 2, 3].into()),
    )
    .await
    .expect("a stalled visitor must not block the tunnel read loop");
  }
  assert!(
    state.response_streams.lock().await.contains_key("r1"),
    "buffered chunks under the cap must not kill the stream"
  );

  // Only once the consumer has accepted nothing for the whole stall timeout
  // does the pump give up; the next chunk then reports the stream gone and
  // removes it, exactly as the old blocking send's timeout did.
  tokio::time::sleep(Duration::from_millis(300)).await;
  for _ in 0..100 {
    ctx.deliver_response_chunk("r1", vec![1, 2, 3].into()).await;
    if !state.response_streams.lock().await.contains_key("r1") {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert!(!state.response_streams.lock().await.contains_key("r1"));
}

#[tokio::test]
async fn backlog_cap_drops_a_producer_that_cannot_be_paused() {
  let state = Arc::new(test_state());
  state
    .clients
    .write()
    .await
    .insert("owner".into(), mock_client(None, None, None, None));

  // A stalled consumer and a pre-v3 producer (detached flow: pause cannot be
  // delivered): the only guard left is the hard byte cap.
  let (tx, _held_rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(2);
  let tx = crate::state::spawn_consumer_pump(
    tx,
    Duration::from_secs(30),
    crate::state::StreamFlow::detached("r1"),
  );
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx,
      client_id: "owner".into(),
    },
  );

  // Push past STREAM_BACKLOG_LIMIT (16 MiB) in 4 MiB chunks; the read loop
  // stays unblocked throughout and the stream is dropped at the cap.
  let ctx = test_ctx(&state, "owner");
  for _ in 0..6 {
    tokio::time::timeout(
      Duration::from_secs(5),
      ctx.deliver_response_chunk("r1", vec![0u8; 4 * 1024 * 1024].into()),
    )
    .await
    .expect("the backlog cap must not block the read loop");
  }
  assert!(!state.response_streams.lock().await.contains_key("r1"));
}

#[tokio::test]
async fn slow_consumer_pauses_and_resumes_a_v3_producer() {
  use crate::state::{PumpCost, StreamFlow, spawn_consumer_pump};

  // The producing client's tunnel writer, observed by the test.
  let (client_tx, mut client_rx) = mpsc::channel::<Message>(8);
  // A consumer that accepts two chunks and then sits on them.
  let (out_tx, mut out_rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(2);
  let pumped = spawn_consumer_pump(
    out_tx,
    Duration::from_secs(30),
    StreamFlow::new(
      "r1".into(),
      client_tx,
      true,
      crate::state::StreamLimits::default(),
    ),
  );

  // 1 MiB chunks: two land in the consumer channel, the next two build up
  // 2 MiB of backlog and cross STREAM_PAUSE_BYTES -> StreamPause goes out.
  let chunk = || Ok(BodyFrame::Data(vec![0u8; 1024 * 1024].into()));
  assert_eq!(chunk().cost(), 1024 * 1024);
  for _ in 0..4 {
    pumped.push(chunk()).unwrap();
    // Give the pump a moment to move what it can into the consumer channel,
    // so backlog only counts what is genuinely stuck.
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let paused = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
    .await
    .expect("expected a StreamPause")
    .expect("client channel open");
  match paused {
    Message::Text(json) => assert!(json.contains("StreamPause"), "got {json}"),
    other => panic!("expected a text frame, got {other:?}"),
  }

  // The visitor drains everything: the backlog falls below the resume mark
  // and the producer is released.
  for _ in 0..4 {
    let _ = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
      .await
      .expect("consumer must receive the buffered chunks");
  }
  let resumed = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
    .await
    .expect("expected a StreamResume")
    .expect("client channel open");
  match resumed {
    Message::Text(json) => assert!(json.contains("StreamResume"), "got {json}"),
    other => panic!("expected a text frame, got {other:?}"),
  }
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
      timings: None,
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
  ws.send(TMessage::Binary(
    encode_binary_frame(FRAME_RESPONSE_CHUNK, "s1", &[1, 2]).unwrap(),
  ))
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
      timings: None,
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
      tx: crate::state::test_pump(tx),
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
      tx: crate::state::test_pump(tx),
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
      tx: crate::state::test_pump(tx),
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
      tx: crate::state::test_pump(tx),
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
    crate::state::UdpStreamHandle {
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
    crate::state::UdpStreamHandle {
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
      tx: crate::state::test_pump(tx),
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
      tx: crate::state::test_pump(tx),
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
  assert!(state.clients.write().await.get(&cid).unwrap().draining);

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
      custom_name: None,
      name: None,
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
    let clients = state.clients.read().await;
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
      .write()
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

  let clients = state.clients.read().await;
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

  let clients = state.clients.read().await;
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
      tx: crate::state::test_pump(rs_tx),
      client_id: cid.clone(),
    },
  );
  let (tcp_tx, mut tcp_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(tcp_tx),
      client_id: cid.clone(),
    },
  );
  let (udp_tx, mut udp_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.udp_streams.lock().await.insert(
    "d1".into(),
    crate::state::UdpStreamHandle {
      tx: udp_tx,
      client_id: cid.clone(),
    },
  );
  let (wss_tx, mut wss_rx) = mpsc::channel::<WsStreamMessage>(4);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx: crate::state::test_pump(wss_tx),
      client_id: cid.clone(),
    },
  );
  // A foreign entry that must survive.
  let (foreign_tx, _foreign_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.tcp_streams.lock().await.insert(
    "keep".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(foreign_tx),
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

// --- tunnel slot accounting ------------------------------------------------

#[tokio::test]
async fn tunnel_slot_released_when_upgrade_never_runs() {
  use std::sync::atomic::Ordering;
  let state = Arc::new(test_state());
  state.active_tunnel_count.store(1, Ordering::SeqCst);

  // axum drops the on_upgrade callback uncalled when the handshake dies, so
  // handle_socket never runs and the slot has to come back by itself.
  drop(TunnelSlot {
    state: state.clone(),
    armed: true,
  });
  assert_eq!(state.active_tunnel_count.load(Ordering::SeqCst), 0);

  // Once the callback runs, handle_socket owns the slot and the guard must
  // keep its hands off it.
  state.active_tunnel_count.store(1, Ordering::SeqCst);
  TunnelSlot {
    state: state.clone(),
    armed: true,
  }
  .handed_off();
  assert_eq!(state.active_tunnel_count.load(Ordering::SeqCst), 1);
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

// ---------------------------------------------------------------------------
// The writer task's two pure pieces, split out by the #21 decomposition.
// ---------------------------------------------------------------------------

#[test]
fn writer_transform_compresses_what_it_should_and_only_that() {
  use crate::protocol::{FRAME_REQUEST_FULL, FRAME_REQUEST_FULL_ZLIB, encode_full_request_frame};

  // Off: everything passes through untouched.
  let text = Message::Text("{\"type\":\"Pong\"}".to_string().into());
  assert!(matches!(writer_transform(text, false), Message::Text(_)));

  // On: a text frame goes out deflated as binary.
  let text = Message::Text("{\"type\":\"Pong\"}".to_string().into());
  let out = writer_transform(text, true);
  let Message::Binary(b) = out else {
    panic!("a compressed text frame is binary");
  };
  assert_eq!(b.first(), Some(&0x78), "a zlib stream, tag byte and all");

  // A v6 full-request frame with a compressible body is re-tagged zlib.
  let body = vec![b'a'; 4096];
  let frame = encode_full_request_frame(FRAME_REQUEST_FULL, "req-1", "{}", &body).unwrap();
  let out = writer_transform(Message::Binary(frame.into()), true);
  let Message::Binary(b) = out else {
    panic!("still binary");
  };
  assert_eq!(b.first(), Some(&FRAME_REQUEST_FULL_ZLIB));
  assert!(b.len() < 4096, "deflate won: {} bytes", b.len());

  // An incompressible body keeps its plain tag: paying for the bytes twice
  // helps nobody.
  let mut x: u32 = 0x9E37_79B9;
  let noise: Vec<u8> = (0..4096)
    .map(|_| {
      // xorshift32: cheap, deterministic, and dense enough that deflate
      // cannot win against its own header overhead.
      x ^= x << 13;
      x ^= x >> 17;
      x ^= x << 5;
      (x >> 24) as u8
    })
    .collect();
  let frame = encode_full_request_frame(FRAME_REQUEST_FULL, "req-2", "{}", &noise).unwrap();
  let out = writer_transform(Message::Binary(frame.clone().into()), true);
  let Message::Binary(b) = out else {
    panic!("still binary");
  };
  assert_eq!(b.first(), Some(&FRAME_REQUEST_FULL));
  assert_eq!(b.len(), frame.len(), "unchanged");

  // A binary chunk frame (any other tag) is never touched.
  let chunk =
    crate::protocol::encode_binary_frame(crate::protocol::FRAME_RESPONSE_CHUNK, "id", b"data")
      .unwrap();
  let out = writer_transform(Message::Binary(chunk.clone().into()), true);
  let Message::Binary(b) = out else {
    panic!("still binary");
  };
  assert_eq!(b.as_ref(), chunk.as_slice());
}

#[test]
fn the_pacer_charges_debt_only_past_the_burst() {
  let start = Instant::now();
  let mut pacer = SendPacer::new(start);
  let rate = 1000u64; // bytes per second

  // The first second's burst is free... once earned: a fresh bucket starts
  // empty, so the very first frame already owes its own transmission time.
  let debt = pacer.debt(500, rate, start).expect("an empty bucket owes");
  assert!((debt.as_secs_f64() - 0.5).abs() < 0.001, "{debt:?}");

  // A second later the bucket has refilled to the full burst; a small frame
  // inside it owes nothing.
  let later = start + Duration::from_secs(2);
  assert!(pacer.debt(500, rate, later).is_none());

  // A frame larger than the whole burst drives it negative and pays the
  // remainder as sleep: 2500 bytes against a 500-token bucket at 1000 B/s
  // owes two seconds.
  let debt = pacer.debt(2500, rate, later).expect("past the burst");
  assert!((debt.as_secs_f64() - 2.0).abs() < 0.001, "{debt:?}");
}

/// Sends one raw binary frame (a v7 relay payload) over the tunnel socket.
async fn send_frame(ws: &mut Client, tag: u8, stream_id: &str, payload: &[u8]) {
  let frame = crate::protocol::encode_binary_frame(tag, stream_id, payload).expect("encodable");
  ws.send(TMessage::Binary(frame)).await.unwrap();
}

#[tokio::test]
async fn v7_relay_frames_deliver_and_keep_their_ownership_fence() {
  // The v7 shapes of TcpData/UdpDatagram/WsData(binary): same delivery, same
  // ownership rule. The fence is the point: a client must not be able to
  // write into another client's relay stream by switching frame shape.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tcp_tx, mut tcp_rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "t7".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(tcp_tx),
      client_id: cid.clone(),
    },
  );
  let (udp_tx, mut udp_rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.udp_streams.lock().await.insert(
    "u7".into(),
    crate::state::UdpStreamHandle {
      tx: udp_tx,
      client_id: cid.clone(),
    },
  );
  let (ws_tx, mut ws_rx) = mpsc::channel::<crate::state::WsStreamMessage>(8);
  state.ws_streams.lock().await.insert(
    "w7".into(),
    crate::state::WsStreamHandle {
      tx: crate::state::test_pump(ws_tx),
      client_id: cid.clone(),
    },
  );
  // A stream this connection does not own, one of each kind.
  let (foreign_tcp, mut foreign_rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "foreign".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(foreign_tcp),
      client_id: "somebody-else".into(),
    },
  );

  send_frame(&mut ws, crate::protocol::FRAME_TCP_DATA, "t7", &[4u8, 5]).await;
  match tokio::time::timeout(Duration::from_secs(2), tcp_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d, vec![4, 5]),
    _ => panic!("expected a data frame"),
  }

  send_frame(&mut ws, crate::protocol::FRAME_UDP_DATAGRAM, "u7", b"dgram").await;
  match tokio::time::timeout(Duration::from_secs(2), udp_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d.as_ref(), b"dgram"),
    _ => panic!("expected a datagram"),
  }

  send_frame(&mut ws, crate::protocol::FRAME_WS_DATA_BIN, "w7", &[9u8, 9]).await;
  match tokio::time::timeout(Duration::from_secs(2), ws_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    crate::state::WsStreamMessage::Data(Message::Binary(b)) => assert_eq!(b.as_ref(), &[9, 9]),
    _ => panic!("expected a binary WS frame"),
  }

  // The fence: a v7 frame for a stream owned by another connection delivers
  // nothing, exactly as its JSON shape does not.
  send_frame(&mut ws, crate::protocol::FRAME_TCP_DATA, "foreign", b"x").await;
  // Round-trip a legitimate frame afterwards: if the foreign one had been
  // delivered it would have arrived before this.
  send_frame(&mut ws, crate::protocol::FRAME_TCP_DATA, "t7", b"after").await;
  match tokio::time::timeout(Duration::from_secs(2), tcp_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d.as_ref(), b"after"),
    _ => panic!("expected the owned delivery"),
  }
  assert!(
    foreign_rx.try_recv().is_err(),
    "a frame for another client's stream delivers nothing"
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// --- self-reported client health (planned_features #37) ---------------------

#[tokio::test]
async fn a_ping_carrying_client_health_stores_it_on_the_handle() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut cpu_percent,
    ref mut rss_bytes,
    ref mut rtt_ms,
    ref mut jitter_ms,
    ref mut reconnects,
    ..
  } = ping
  {
    *cpu_percent = Some(12.5);
    *rss_bytes = Some(48 * 1024 * 1024);
    *rtt_ms = Some(23);
    *jitter_ms = Some(4);
    *reconnects = Some(2);
  }
  send(&mut ws, &ping).await;

  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;
  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("the client is registered");
  assert_eq!(handle.cpu_percent, Some(12.5));
  assert_eq!(handle.rss_bytes, Some(48 * 1024 * 1024));
  assert_eq!(handle.rtt_ms, Some(23));
  assert_eq!(handle.jitter_ms, Some(4));
  assert_eq!(handle.reconnects, Some(2));
}

#[tokio::test]
async fn a_client_that_stops_reporting_shows_nothing_rather_than_a_stale_value() {
  // An older client, or a platform where a figure cannot be read, omits it.
  // Keeping the last value would let a number age silently while looking live.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut p = base_ping();
  if let TunnelMessage::Ping {
    ref mut rtt_ms,
    ref mut cpu_percent,
    ..
  } = p
  {
    *rtt_ms = Some(99);
    *cpu_percent = Some(50.0);
  }
  send(&mut ws, &p).await;
  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;
  assert_eq!(
    state.clients.read().await.get(&id).unwrap().rtt_ms,
    Some(99)
  );

  send(&mut ws, &base_ping()).await;
  let _ = read_until_pong(&mut ws).await;
  let clients = state.clients.read().await;
  let handle = clients.get(&id).unwrap();
  assert_eq!(handle.rtt_ms, None, "the absence is stored, not ignored");
  assert_eq!(handle.cpu_percent, None);
}

// ---------------------------------------------------------------------------
// Alternate servers announced on the handshake (planned_features #52)
// ---------------------------------------------------------------------------

#[test]
fn alternates_keep_only_what_a_tunnel_can_be_dialed_with() {
  // The list is announced to every client and tried by every client, so a
  // typo reaches further here than most, and dropping what cannot be a tunnel
  // URL is cheaper than every client discovering it one reconnect at a time.
  assert_eq!(
    parse_alternates("wss://eu.example.com/tunnel, https://not-a-tunnel, , ws://b/x"),
    vec![
      "wss://eu.example.com/tunnel".to_string(),
      "ws://b/x".to_string()
    ]
  );
  assert!(parse_alternates("").is_empty());
  assert!(parse_alternates("   ").is_empty());
}

#[test]
fn alternates_are_capped() {
  let many = (0..40)
    .map(|i| format!("wss://s{i}.example.com"))
    .collect::<Vec<_>>()
    .join(",");
  // Clients walk this list in rotation; an unbounded one turns every
  // reconnect into a long walk through addresses nobody chose.
  assert_eq!(parse_alternates(&many).len(), MAX_ALTERNATES);
}
