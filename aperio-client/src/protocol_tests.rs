use super::*;

#[test]
fn test_binary_frame_roundtrip() {
  let frame = encode_binary_frame(FRAME_RESPONSE_CHUNK, "req-1", b"payload-bytes");
  let (tag, id, payload) = decode_binary_frame(&frame).expect("frame must decode");
  assert_eq!(tag, FRAME_RESPONSE_CHUNK);
  assert_eq!(id, "req-1");
  assert_eq!(payload, b"payload-bytes");

  // An empty payload is valid.
  let frame = encode_binary_frame(FRAME_REQUEST_CHUNK, "x", b"");
  let (tag, id, payload) = decode_binary_frame(&frame).unwrap();
  assert_eq!((tag, id, payload), (FRAME_REQUEST_CHUNK, "x", &b""[..]));
}

#[test]
fn test_binary_frame_malformed() {
  // Too short for the header.
  assert!(decode_binary_frame(&[]).is_none());
  assert!(decode_binary_frame(&[1]).is_none());
  // Declared id length exceeds the buffer.
  assert!(decode_binary_frame(&[1, 200, b'a']).is_none());
  // Non-UTF-8 id bytes.
  assert!(decode_binary_frame(&[1, 2, 0xff, 0xfe]).is_none());
}

#[test]
fn test_compress_roundtrip() {
  let text = "hello tunnel ".repeat(100);
  let compressed = compress_frame(&text);
  assert!(compressed.len() < text.len());
  // zlib streams start with 0x78 — the property that keeps them
  // distinguishable from v2 binary chunk frames.
  assert_eq!(compressed[0], 0x78);
  assert_eq!(
    decompress_frame(&compressed, 1024 * 1024).as_deref(),
    Some(text.as_str())
  );
  // The output bound rejects frames that inflate beyond the limit.
  assert!(decompress_frame(&compressed, 10).is_none());
}

#[test]
fn test_tunnel_decl_serde_defaults() {
  // The protocol field defaults to tcp when absent (yaml/json alike).
  let decl: TunnelDecl = serde_json::from_str(r#"{"target":"127.0.0.1:27017"}"#).unwrap();
  assert_eq!(decl.protocol, "tcp");

  // Ping messages without a tunnels field (older peers) parse fine.
  let ping = r#"{"type":"Ping","client_id":"c","timestamp":1,"path_bind":null}"#;
  let msg: TunnelMessage = serde_json::from_str(ping).unwrap();
  match msg {
    TunnelMessage::Ping { tunnels, .. } => assert!(tunnels.is_empty()),
    other => panic!("expected Ping, got {other:?}"),
  }

  // TcpOpen without a target (older servers) parses as the legacy form.
  let open = r#"{"type":"TcpOpen","stream_id":"s1"}"#;
  let msg: TunnelMessage = serde_json::from_str(open).unwrap();
  match msg {
    TunnelMessage::TcpOpen { stream_id, target } => {
      assert_eq!(stream_id, "s1");
      assert_eq!(target, None);
    }
    other => panic!("expected TcpOpen, got {other:?}"),
  }
}
