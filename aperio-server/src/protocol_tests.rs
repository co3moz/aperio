use super::*;

#[test]
fn test_binary_frame_roundtrip() {
  let frame = encode_binary_frame(FRAME_REQUEST_CHUNK, "req-1", b"payload-bytes")
    .expect("a uuid id always fits");
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
  // zlib streams start with 0x78, the property that keeps them
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

#[test]
fn a_full_response_frame_round_trips() {
  // The v5 payload shape: the envelope's length, the envelope, the body. The
  // body is bytes, so it must survive being something JSON could not carry.
  let json = r#"{"type":"Response","id":"abc","status":200}"#;
  let body: Vec<u8> = vec![0x00, 0xff, 0x80, b'"', b'\\', 0x0a];

  let payload = join_full_response(json, &body);
  let (out_json, out_body) = split_full_response(&payload).expect("round trip");
  assert_eq!(out_json, json);
  assert_eq!(out_body, &body[..]);

  // An empty body is a legitimate frame: a 204 has an envelope and nothing
  // after it.
  let payload = join_full_response(json, &[]);
  let (out_json, out_body) = split_full_response(&payload).expect("empty body");
  assert_eq!(out_json, json);
  assert!(out_body.is_empty());
}

#[test]
fn a_truncated_full_response_frame_is_refused() {
  // A length prefix that does not describe the frame is corruption, and the
  // reader has to say so rather than index past the end.
  let payload = join_full_response(r#"{"type":"Response"}"#, b"body");
  for cut in [0, 1, 3, 4, 6] {
    assert!(
      split_full_response(&payload[..cut]).is_none(),
      "a frame cut to {cut} bytes was accepted"
    );
  }
  // A prefix claiming more JSON than the frame holds.
  let mut lying = u32::MAX.to_le_bytes().to_vec();
  lying.extend_from_slice(b"{}");
  assert!(split_full_response(&lying).is_none());
}

#[test]
fn a_deflated_full_response_payload_round_trips() {
  // The compressed sibling: a compressible body must come back byte for byte,
  // and an incompressible one must not be sent compressed at all, since
  // deflating it costs CPU to produce more bytes.
  let json = r#"{"type":"Response","id":"abc","status":200}"#;
  let compressible: Vec<u8> = b"the quick brown fox ".repeat(400);
  let payload = join_full_response(json, &compressible);

  let deflated = deflate_payload(&payload).expect("a compressible payload deflates");
  assert!(deflated.len() < payload.len());
  let inflated = inflate_payload(&deflated, 1 << 20).expect("it inflates back");
  let (out_json, out_body) = split_full_response(&inflated).expect("and still splits");
  assert_eq!(out_json, json);
  assert_eq!(out_body, &compressible[..]);

  // Random bytes: deflating makes it bigger, so it is refused and the frame
  // goes out as it is.
  let random: Vec<u8> = (0..4096u32)
    .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
    .collect();
  let payload = join_full_response(json, &random);
  assert!(
    deflate_payload(&payload).is_none() || deflate_payload(&payload).unwrap().len() < payload.len()
  );

  // The bound holds: a payload that inflates past the limit is refused rather
  // than allocated.
  let big = join_full_response(json, &vec![0u8; 200_000]);
  let deflated = deflate_payload(&big).expect("zeros deflate");
  assert!(
    inflate_payload(&deflated, 1000).is_none(),
    "the limit was not enforced"
  );
  assert!(inflate_payload(&deflated, 1 << 20).is_some());
}
