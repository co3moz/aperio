//! The streamed-response path as a client drives it: one chunk delivered
//! directly, then every response frame in sequence, and the compressed frame
//! the writer produces once a connection has negotiated it.

use super::super::tests::*;
use super::super::*;
use crate::protocol::TunnelMessage;
use crate::protocol::encode_binary_frame;
use crate::state::*;
use crate::test_support::*;
use base64::prelude::*;
use futures_util::SinkExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as TMessage;

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
    encode_binary_frame(FRAME_RESPONSE_CHUNK, "s1", &[1, 2])
      .unwrap()
      .into(),
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
  ws.send(TMessage::Binary(compress_frame(&json).into()))
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
