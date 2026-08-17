//! Bodies that do not fit in one frame: the coalescer's partial and full
//! frames, the threshold that depends on what the peer can take, streaming in
//! both directions, and the two caps that refuse a body rather than truncating
//! it silently.

use super::super::http_tests::*;
use super::*;

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
      // Do NOT shutdown here: on Windows, queueing a FIN while the tail of
      // the payload is still undelivered (peer window transiently zero) puts
      // the closing connection into zero-window probing, and the stack
      // aborts it with a RST after ~5 probes (~19 s), truncating the body.
      // Keep the socket fully open and wait for the peer to finish reading;
      // the task (and socket) is dropped when the test runtime shuts down.
      let mut sink = [0u8; 1024];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, mut rx) = mpsc::channel::<Message>(256);
  let ctx = ForwardContext {
    max_response_body_size: 10 * 1024 * 1024,
    ..test_ctx(&target_url, tx)
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-stream-1".to_string(),
      method: "GET".to_string(),
      uri: "/big".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
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
async fn test_streamed_request_body_forwarded() {
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
      let mut buf = vec![0u8; 4096];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
      let _ = _tx.send(req_str);
      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      let _ = socket.write_all(response.as_bytes()).await;
    }
  });

  // Feed the request body through the streamed-body channel (v2 path).
  let (btx, brx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
  btx.send(Ok(b"stream-".to_vec().into())).await.unwrap();
  btx.send(Ok(b"payload".to_vec().into())).await.unwrap();
  drop(btx);

  let ctx = test_ctx(&target_url, test_tunnel_tx());
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-sbody".to_string(),
      method: "POST".to_string(),
      uri: "/upload".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    Some(brx),
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let observed = rx.await.unwrap();
  // The body arrives chunk-encoded (transfer-encoding: chunked); assert both
  // parts reached the backend.
  assert!(
    observed.contains("transfer-encoding: chunked")
      && observed.contains("stream-")
      && observed.contains("payload"),
    "streamed body must reach the backend, got: {observed}"
  );
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 200);
}

#[tokio::test]
async fn test_stream_truncated_at_limit() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);
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
      // Written in paced slices so the client genuinely reads several chunks:
      // sent back to back they arrive coalesced into a couple of large ones,
      // and then the cap is passed before streaming ever starts, which is the
      // other test's case rather than this one's.
      let payload = vec![0xCDu8; body_size];
      for part in payload.chunks(32 * 1024) {
        if socket.write_all(part).await.is_err() {
          break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
      }
      let mut sink = [0u8; 1024];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, mut rx) = mpsc::channel::<Message>(256);
  // Above the 256 KB streaming threshold and below the payload, so the
  // response is already streaming when it passes the cap. Below the threshold
  // the cap is reached while still buffering, and nothing has been sent yet:
  // that case is a clean refusal, and it is the test below this one.
  let ctx = ForwardContext {
    max_response_body_size: 400 * 1024,
    ..test_ctx(&target_url, tx)
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-trunc".to_string(),
      method: "GET".to_string(),
      uri: "/big".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await;
  assert!(result.is_none(), "large body streams, got {result:?}");

  let mut got_abort = false;
  while let Some(Message::Text(json)) = rx.recv().await {
    if let TunnelMessage::ResponseAbort { .. } =
      serde_json::from_str::<TunnelMessage>(&json).unwrap()
    {
      got_abort = true;
      break;
    }
  }
  assert!(
    got_abort,
    "a truncated stream must terminate with ResponseAbort, not a clean ResponseEnd"
  );
}

#[tokio::test]
async fn a_body_over_the_cap_before_streaming_is_refused_whole() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  // The cap was only ever consulted in the streaming arm, so it was enforced
  // from the second chunk onwards: a body delivered in one chunk larger than
  // the cap was buffered, made into the head of a stream and sent in full,
  // and the visitor got a successful response several times the size the
  // operator allowed.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);
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
      let _ = socket.write_all(&vec![0xCDu8; body_size]).await;
      let mut sink = [0u8; 1024];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, mut rx) = mpsc::channel::<Message>(256);
  let ctx = ForwardContext {
    max_response_body_size: 16 * 1024,
    ..test_ctx(&target_url, tx)
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-cap".to_string(),
      method: "GET".to_string(),
      uri: "/big".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await;

  // A clean failure rather than a truncated success: nothing had left yet, so
  // there is no stream to abort halfway.
  let Some(TunnelMessage::Response { status, .. }) = result else {
    panic!("expected a buffered error response, got {result:?}");
  };
  assert_eq!(status, 502);
  // And not one byte of the body went out on the tunnel.
  rx.close();
  while let Some(msg) = rx.recv().await {
    if let Message::Text(json) = msg {
      let parsed = serde_json::from_str::<TunnelMessage>(&json).unwrap();
      assert!(
        !matches!(
          parsed,
          TunnelMessage::ResponseStart { .. } | TunnelMessage::ResponseChunk { .. }
        ),
        "a refused body must not have been streamed"
      );
    }
  }
}

#[test]
fn the_streaming_threshold_depends_on_what_the_peer_can_take() {
  // Against a peer that takes binary frames, streaming also means the body
  // stops being base64: worth it from 32 KB up. Against one that cannot, a
  // streamed body is base64 either way, so the only reason left is bounded
  // memory and the threshold stays where it was. Written as a const block so
  // the relationship is checked at compile time, which is when it can break.
  const {
    assert!(BINARY_STREAM_THRESHOLD < STREAM_THRESHOLD);
    assert!(BINARY_STREAM_THRESHOLD == 32 * 1024);
    assert!(STREAM_THRESHOLD == 256 * 1024);
  }
}

#[test]
fn coalescer_holds_partial_frames_and_pops_full_ones() {
  let mut c = ChunkCoalescer::new();
  assert!(c.is_empty());
  c.add(&[1u8; 1000]);
  assert!(!c.is_empty());
  // Under a frame: nothing to pop yet.
  assert!(c.pop_full().is_none());
  // Crossing the frame size pops exactly one full frame, remainder stays.
  c.add(&vec![2u8; STREAM_CHUNK_SIZE]);
  let full = c.pop_full().expect("a full frame accumulated");
  assert_eq!(full.len(), STREAM_CHUNK_SIZE);
  assert_eq!(&full[..1000], &[1u8; 1000]);
  assert!(c.pop_full().is_none());
  let rest = c.take().expect("a remainder is held");
  assert_eq!(rest, vec![2u8; 1000]);
  assert!(c.is_empty());
}

#[test]
fn coalescer_pops_every_full_frame_of_a_large_chunk() {
  // One oversized backend chunk yields multiple full frames in order.
  let mut c = ChunkCoalescer::new();
  c.add(&vec![7u8; STREAM_CHUNK_SIZE * 2 + 5]);
  assert_eq!(c.pop_full().unwrap().len(), STREAM_CHUNK_SIZE);
  assert_eq!(c.pop_full().unwrap().len(), STREAM_CHUNK_SIZE);
  assert!(c.pop_full().is_none());
  assert_eq!(c.take().unwrap(), vec![7u8; 5]);
}

#[test]
fn coalescer_take_on_empty_is_none() {
  let mut c = ChunkCoalescer::new();
  assert!(c.take().is_none());
}
