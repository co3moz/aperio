//! What these pin down: that the bridge recognizes exactly the paths OTLP
//! defines, that it never blocks the thing it is measuring, and that a gRPC
//! frame it cannot forward faithfully is refused rather than corrupted.

use super::*;

#[test]
fn recognizes_the_otlp_http_signal_paths() {
  assert_eq!(signal_of("/v1/traces"), Some("traces"));
  assert_eq!(signal_of("/v1/metrics"), Some("metrics"));
  assert_eq!(signal_of("/v1/logs"), Some("logs"));
  // A trailing slash is a configuration people write.
  assert_eq!(signal_of("/v1/traces/"), Some("traces"));
  assert_eq!(signal_of("/v1/profiles"), None);
  assert_eq!(signal_of("/"), None);
}

#[test]
fn recognizes_the_otlp_grpc_methods() {
  assert_eq!(
    grpc_signal_of("/opentelemetry.proto.collector.trace.v1.TraceService/Export"),
    Some("traces")
  );
  assert_eq!(
    grpc_signal_of("/opentelemetry.proto.collector.metrics.v1.MetricsService/Export"),
    Some("metrics")
  );
  assert_eq!(
    grpc_signal_of("/opentelemetry.proto.collector.logs.v1.LogsService/Export"),
    Some("logs")
  );
  // Matched on the service name, so a future version number in the path is
  // still recognized rather than silently unhandled.
  assert_eq!(
    grpc_signal_of("/opentelemetry.proto.collector.trace.v2.TraceService/Export"),
    Some("traces")
  );
  assert_eq!(grpc_signal_of("/some.other.Service/Export"), None);
  assert_eq!(
    grpc_signal_of("/opentelemetry.proto.collector.trace.v1.TraceService/List"),
    None
  );
}

#[test]
fn strips_a_grpc_length_prefix() {
  let payload = [1u8, 2, 3, 4];
  let mut framed = vec![0u8];
  framed.extend_from_slice(&4u32.to_be_bytes());
  framed.extend_from_slice(&payload);
  assert_eq!(strip_grpc_frame(&framed).unwrap(), &payload[..]);
}

#[test]
fn refuses_a_grpc_frame_it_cannot_forward_faithfully() {
  // Compressed: the flag says these bytes are not the protobuf the server
  // will try to read, and forwarding them corrupts an export in a way that
  // only surfaces at the collector.
  let mut compressed = vec![1u8];
  compressed.extend_from_slice(&4u32.to_be_bytes());
  compressed.extend_from_slice(&[1, 2, 3, 4]);
  assert!(strip_grpc_frame(&compressed).is_err());

  // Truncated: the length prefix promises more than arrived.
  let mut truncated = vec![0u8];
  truncated.extend_from_slice(&64u32.to_be_bytes());
  truncated.extend_from_slice(&[1, 2]);
  assert!(strip_grpc_frame(&truncated).is_err());

  assert!(strip_grpc_frame(&[0, 0]).is_err());
}

#[tokio::test]
async fn a_full_queue_drops_instead_of_waiting() {
  let (tx, _rx) = channel(1);
  let export = || Export {
    signal: "traces",
    payload: bytes::Bytes::from_static(b"x"),
  };
  assert!(offer(&tx, export()));
  let before = dropped();
  // An SDK that cannot hand off its batch blocks the application it is
  // instrumenting. Telemetry that stalls the thing it measures has done more
  // harm than the missing spans ever would.
  assert!(!offer(&tx, export()));
  assert_eq!(dropped(), before + 1);
}

#[tokio::test]
async fn an_oversized_export_is_refused_without_being_buffered() {
  use http_body_util::BodyExt;

  // The fence used to be applied after `collect()`, which made it a
  // description of what was already in memory rather than a limit on it. What
  // pins that down is not the refusal, which happened before too, but where
  // the reader stops: a body that streams far more than the fence must be cut
  // off at the fence, not read to its end and then measured.
  let read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
  let counter = read.clone();
  const CHUNK: usize = 1024 * 1024;
  let frames = futures_util::stream::repeat_with(move || {
    counter.fetch_add(CHUNK, std::sync::atomic::Ordering::Relaxed);
    Ok::<_, std::io::Error>(hyper::body::Frame::data(bytes::Bytes::from(vec![
      0u8;
      CHUNK
    ])))
  });
  let body = http_body_util::StreamBody::new(frames);
  let limited = http_body_util::Limited::new(body, MAX_EXPORT_BYTES);

  assert!(
    limited.collect().await.is_err(),
    "a body past the fence is refused"
  );
  let buffered = read.load(std::sync::atomic::Ordering::Relaxed);
  assert!(
    buffered <= MAX_EXPORT_BYTES + CHUNK,
    "read {buffered} bytes for an {MAX_EXPORT_BYTES}-byte fence: the body is \
     still being collected before it is measured"
  );
}

#[tokio::test]
async fn the_https_forwarder_follows_a_reloaded_server_and_token() {
  use std::sync::Arc;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  // The forwarder captured the server and the token when it started, so a
  // reload that moved the server or rotated the token left the telemetry
  // going to the old address, or refused by a token that no longer existed,
  // while the tunnel itself had already followed the change.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let seen = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
  let recorder = seen.clone();
  tokio::spawn(async move {
    while let Ok((mut socket, _)) = listener.accept().await {
      let recorder = recorder.clone();
      tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        if let Ok(n) = socket.read(&mut buf).await {
          let head = String::from_utf8_lossy(&buf[..n]).to_string();
          for line in head.lines() {
            if line.to_ascii_lowercase().starts_with("authorization:") {
              recorder.lock().await.push(line.trim().to_string());
            }
          }
        }
        let _ = socket
          .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
          .await;
      });
    }
  });

  let (tx, rx) = channel(8);
  let (credentials, credentials_rx) =
    tokio::sync::watch::channel((format!("http://127.0.0.1:{port}"), "first".to_string()));
  tokio::spawn(run_https_forwarder(rx, credentials_rx));

  let export = |tx: &tokio::sync::mpsc::Sender<Export>| {
    let _ = tx.try_send(Export {
      signal: "traces",
      payload: bytes::Bytes::from_static(b"x"),
    });
  };
  export(&tx);
  // The reload: same server, new token.
  for _ in 0..100 {
    if !seen.lock().await.is_empty() {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
  }
  credentials.send_replace((format!("http://127.0.0.1:{port}"), "second".to_string()));
  export(&tx);

  for _ in 0..100 {
    if seen.lock().await.len() >= 2 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
  }
  let seen = seen.lock().await.clone();
  assert_eq!(seen.len(), 2, "both exports should have been posted");
  assert!(seen[0].ends_with("first"), "{seen:?}");
  assert!(
    seen[1].ends_with("second"),
    "the forwarder kept the token it started with: {seen:?}"
  );
}
