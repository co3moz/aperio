use super::*;

#[test]
fn test_binary_frame_roundtrip() {
  let frame = encode_binary_frame(FRAME_REQUEST_CHUNK, "req-1", b"payload-bytes");
  let (tag, id, payload) = decode_binary_frame(&frame).expect("frame must decode");
  assert_eq!(tag, FRAME_REQUEST_CHUNK);
  assert_eq!(id, "req-1");
  assert_eq!(payload, b"payload-bytes");
}

#[test]
fn test_binary_frame_malformed() {
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
fn test_decode_never_panics_and_holds_invariants() {
  // A deterministic sweep of adversarial byte patterns (the fuzz targets in
  // `fuzz/` explore this far more deeply on nightly): decoding must never
  // panic, and any decoded frame id must satisfy the `id.len() <= 255`
  // prefix invariant.
  let mut seed = 0x1234_5678u32;
  let mut next = || {
    // Tiny xorshift so the corpus is varied but reproducible (no rng dep).
    seed ^= seed << 13;
    seed ^= seed >> 17;
    seed ^= seed << 5;
    seed
  };
  for _ in 0..2000 {
    let len = (next() % 300) as usize;
    let buf: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
    if let Some((_tag, id, _payload)) = decode_binary_frame(&buf) {
      assert!(
        id.len() <= 255,
        "id length invariant violated: {}",
        id.len()
      );
    }
    // The zlib path must also never panic and must respect the output cap.
    if let Some(out) = decompress_frame(&buf, 4096) {
      assert!(out.len() <= 4096);
    }
  }
}

#[test]
fn test_ping_backward_compat() {
  // Ping messages without the newer optional fields (older clients) parse,
  // and the serde defaults hold: backend_healthy=true, tunnels empty.
  let ping = r#"{"type":"Ping","client_id":"c","timestamp":1,"path_bind":null}"#;
  let msg: TunnelMessage = serde_json::from_str(ping).unwrap();
  match msg {
    TunnelMessage::Ping {
      backend_healthy,
      tunnels,
      ..
    } => {
      assert!(backend_healthy);
      assert!(tunnels.is_empty());
    }
    other => panic!("expected Ping, got {other:?}"),
  }

  let decl: TunnelDecl = serde_json::from_str(r#"{"target":"127.0.0.1:27017"}"#).unwrap();
  assert_eq!(decl.protocol, "tcp");
}
