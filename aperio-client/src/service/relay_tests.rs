//! The read loop's hand-off to a stream, and the writer's drain: that a
//! consumer which stops reading cannot stall the loop that feeds every other
//! stream. The writer's own drain is in `writer_tests.rs`.

use super::*;

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
