//! The wire itself: every frame shape round-trips, a malformed one is refused
//! rather than misread, decoding never panics on bytes a peer chose, and the
//! client's copy of the protocol still describes the same wire and version as this
//! one.

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
  // `tools/fuzz/` explore this far more deeply on nightly): decoding must never
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

#[test]
fn a_full_request_frame_round_trips_through_the_client_side_reader() {
  // v6 is the response frame's mirror, and the two sides are different
  // crates: this encodes with the server's builder and splits with the same
  // reader the client uses, so a disagreement about the layout fails here
  // rather than on someone's upload.
  let json = r#"{"type":"Request","id":"abc","method":"POST","uri":"/upload"}"#;
  let body: Vec<u8> = vec![0x00, 0xff, 0x80, b'"', b'\\', 0x0a, 0x7f];

  let frame = encode_full_request_frame(FRAME_REQUEST_FULL, "req-1", json, &body)
    .expect("a uuid-length id encodes");
  let (tag, id, payload) = decode_binary_frame(&frame).expect("it decodes as a binary frame");
  assert_eq!(tag, FRAME_REQUEST_FULL);
  assert_eq!(id, "req-1");
  let (out_json, out_body) = split_full_response(payload).expect("and splits");
  assert_eq!(out_json, json);
  assert_eq!(out_body, &body[..]);

  // An id too long for the one-byte length prefix is refused rather than
  // wrapped, which would put every frame after it out of step.
  let long = "x".repeat(256);
  assert!(encode_full_request_frame(FRAME_REQUEST_FULL, &long, json, &body).is_none());
}

#[test]
fn a_deflated_full_request_payload_round_trips() {
  // The compressed sibling of the request frame: an upload of compressible
  // text must come back byte for byte, and the tag says which it is.
  let json = r#"{"type":"Request","id":"abc","method":"POST","uri":"/upload"}"#;
  let form: Vec<u8> = b"field=value&".repeat(500);
  let frame = encode_full_request_frame(FRAME_REQUEST_FULL, "req-1", json, &form).unwrap();
  let (_, id, payload) = decode_binary_frame(&frame).unwrap();

  let deflated = deflate_payload(payload).expect("a compressible upload deflates");
  assert!(deflated.len() < payload.len());
  let rewrapped = encode_binary_frame(FRAME_REQUEST_FULL_ZLIB, id, &deflated).unwrap();
  let (tag, _, payload) = decode_binary_frame(&rewrapped).unwrap();
  assert_eq!(tag, FRAME_REQUEST_FULL_ZLIB);

  let inflated = inflate_payload(payload, 1 << 20).expect("it inflates back");
  let (out_json, out_body) = split_full_response(&inflated).expect("and still splits");
  assert_eq!(out_json, json);
  assert_eq!(out_body, &form[..]);
}

#[test]
fn relay_frame_picks_the_shape_the_peer_speaks() {
  use axum::extract::ws::Message;

  // v7: a tagged binary frame carrying the bytes verbatim.
  let frame = relay_frame(7, FRAME_TCP_DATA, "s1", b"raw-bytes", |data| {
    TunnelMessage::TcpData {
      stream_id: "s1".to_string(),
      data,
    }
  })
  .expect("a frame is produced");
  let Message::Binary(bytes) = frame else {
    panic!("v7 gets a binary frame");
  };
  let (tag, id, payload) = decode_binary_frame(&bytes).expect("well-formed");
  assert_eq!(tag, FRAME_TCP_DATA);
  assert_eq!(id, "s1");
  assert_eq!(payload, b"raw-bytes");

  // v6 and below: the JSON message with a base64 payload, the shape every
  // client before this release understands. Handing them the frame above
  // would be silently dropped, which is why one function owns this decision.
  for peer in [1, 2, 5, 6] {
    let frame = relay_frame(peer, FRAME_TCP_DATA, "s1", b"raw-bytes", |data| {
      TunnelMessage::TcpData {
        stream_id: "s1".to_string(),
        data,
      }
    })
    .expect("a frame is produced");
    let Message::Text(json) = frame else {
      panic!("peer v{peer} must get JSON");
    };
    match serde_json::from_str::<TunnelMessage>(&json).expect("valid JSON") {
      TunnelMessage::TcpData { stream_id, data } => {
        assert_eq!(stream_id, "s1");
        use base64::prelude::*;
        assert_eq!(BASE64_STANDARD.decode(data).unwrap(), b"raw-bytes");
      }
      other => panic!("unexpected message: {other:?}"),
    }
  }

  // A stream id too long for the one-byte length prefix cannot be framed;
  // rather than truncate it, the encoder refuses and the caller falls back
  // to the JSON shape, which has no such limit.
  let long_id = "x".repeat(300);
  let frame = relay_frame(7, FRAME_TCP_DATA, &long_id, b"payload", |data| {
    TunnelMessage::TcpData {
      stream_id: long_id.clone(),
      data,
    }
  })
  .expect("a frame is produced");
  assert!(
    matches!(frame, Message::Text(_)),
    "an unframeable id falls back to JSON instead of being dropped"
  );
}

/// The client's copy of `ServiceDecl` declares the same wire shape as this one.
///
/// The protocol lives in two hand-synced files, one per crate, and until now
/// the cost of a drift was bounded: the structs are flat, and a field that
/// went missing on one side was a field that stopped arriving. `ServiceDecl`
/// changes that arithmetic. It is the unit #46 divides a connection by, it
/// carries twenty-five fields, and every one of them is optional with a
/// default, which is exactly the shape where a drift does not fail. A field
/// misspelled on one side deserializes to its default on the other, and the
/// service comes up quietly wrong: no gate, no cache, no limit, no error.
///
/// So this compares the two declarations by the only thing serde actually
/// reads, the field names, their types and their serde attributes, and
/// ignores what the two copies have always been free to differ on: their
/// visibility, their doc comments and their formatting.
#[test]
fn the_two_copies_of_service_decl_describe_the_same_wire() {
  const SERVER: &str = include_str!("protocol.rs");
  const CLIENT: &str = include_str!("../../aperio-client/src/protocol.rs");

  let server = wire_shape_of(SERVER, "ServiceDecl");
  let client = wire_shape_of(CLIENT, "ServiceDecl");
  assert!(
    !server.is_empty(),
    "the server copy still declares ServiceDecl"
  );
  assert_eq!(
    server, client,
    "aperio-server and aperio-client disagree about the ServiceDecl wire. \
     Both copies have to be edited together; a field present on one side \
     only is not a compile error, it is a service that comes up with that \
     setting silently unset."
  );
}

/// The version the two copies announce is the same number.
///
/// A skew here is worse than a missing field: each side would negotiate
/// features against a version the other never claimed.
#[test]
fn the_two_copies_announce_the_same_protocol_version() {
  const CLIENT: &str = include_str!("../../aperio-client/src/protocol.rs");
  let declared = CLIENT
    .lines()
    .find_map(|l| l.split("PROTOCOL_VERSION: u32 = ").nth(1))
    .and_then(|rest| rest.trim_end_matches(';').trim().parse::<u32>().ok())
    .expect("the client copy still declares PROTOCOL_VERSION");
  assert_eq!(
    declared, PROTOCOL_VERSION,
    "the client and the server announce different protocol versions"
  );
}

/// Reduces a struct declaration to what serde reads: field names, their
/// types, and the `#[serde(...)]` attributes attached to them. Everything the
/// two copies are allowed to differ on, visibility, doc comments, line
/// wrapping and indentation, is dropped.
#[cfg(test)]
fn wire_shape_of(source: &str, name: &str) -> Vec<String> {
  let Some(start) = source.find(&format!("struct {name} {{")) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  let mut pending = String::new();
  for line in source[start..].lines().skip(1) {
    let line = line.trim();
    if line == "}" {
      break;
    }
    if line.starts_with("///") || line.starts_with("//") || line.is_empty() {
      continue;
    }
    // A wrapped attribute or field carries on until its terminator.
    pending.push_str(line);
    pending.push(' ');
    if !(line.ends_with(',') || line.ends_with(']')) {
      continue;
    }
    let item = pending.split_whitespace().collect::<Vec<_>>().join(" ");
    pending.clear();
    out.push(item.replace("pub(crate) ", "").replace("pub ", ""));
  }
  out
}
