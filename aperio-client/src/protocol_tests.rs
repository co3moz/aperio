use super::*;

#[test]
fn test_binary_frame_roundtrip() {
  let frame = encode_binary_frame(FRAME_RESPONSE_CHUNK, "req-1", b"payload-bytes")
    .expect("a uuid id always fits");
  let (tag, id, payload) = decode_binary_frame(&frame).expect("frame must decode");
  assert_eq!(tag, FRAME_RESPONSE_CHUNK);
  assert_eq!(id, "req-1");
  assert_eq!(payload, b"payload-bytes");

  // An empty payload is valid.
  let frame = encode_binary_frame(FRAME_REQUEST_CHUNK, "x", b"").expect("a uuid id always fits");
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

#[test]
fn an_id_too_long_to_frame_is_refused_rather_than_wrapped() {
  // The length prefix is one byte. A peer's id is echoed back into the frame,
  // so a peer sending a longer one would have the cast wrap: a length that
  // does not describe the frame, and every frame after it out of step.
  let long = "x".repeat(256);
  assert!(encode_binary_frame(FRAME_RESPONSE_CHUNK, &long, b"body").is_none());
  let edge = "x".repeat(255);
  let frame = encode_binary_frame(FRAME_RESPONSE_CHUNK, &edge, b"body").expect("255 still fits");
  let (_, id, payload) = decode_binary_frame(&frame).unwrap();
  assert_eq!(id, edge);
  assert_eq!(payload, b"body");
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

#[test]
fn a_full_request_frame_is_read_the_way_the_server_writes_it() {
  // The client's half of v6: the server encodes `[tag][id_len][id]` then
  // `[json_len][json][body]`, and this is the reader that has to agree. The
  // frame is built here by hand rather than imported, because the two sides
  // are different crates and a shared helper would prove nothing.
  let json = r#"{"type":"Request","id":"abc","method":"POST","uri":"/upload"}"#;
  let body: Vec<u8> = vec![0x00, 0xff, 0x80, b'"', 0x0a];
  let id = "req-1";

  let mut frame = vec![FRAME_REQUEST_FULL, id.len() as u8];
  frame.extend_from_slice(id.as_bytes());
  frame.extend_from_slice(&(json.len() as u32).to_le_bytes());
  frame.extend_from_slice(json.as_bytes());
  frame.extend_from_slice(&body);

  let (tag, out_id, payload) = decode_binary_frame(&frame).expect("decodes");
  assert_eq!(tag, FRAME_REQUEST_FULL);
  assert_eq!(out_id, id);
  let (out_json, out_body) = split_full_response(payload).expect("splits");
  assert_eq!(out_json, json);
  assert_eq!(out_body, &body[..]);

  // The deflated tag carries the same payload through inflate first. A body
  // worth compressing, since a handful of bytes never deflates smaller and
  // the writer would send it as it is.
  let form: Vec<u8> = b"field=value&".repeat(500);
  let mut payload = (json.len() as u32).to_le_bytes().to_vec();
  payload.extend_from_slice(json.as_bytes());
  payload.extend_from_slice(&form);
  let deflated = deflate_payload(&payload).expect("compressible");
  let inflated = inflate_payload(&deflated, 1 << 20).expect("inflates");
  let (out_json, out_body) = split_full_response(&inflated).expect("splits");
  assert_eq!(out_json, json);
  assert_eq!(out_body, &form[..]);
  assert_eq!(FRAME_REQUEST_FULL_ZLIB, 6, "the tag the server sends");
}
