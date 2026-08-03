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
