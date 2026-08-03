//! What these pin down: that stamping adds what it says and changes nothing
//! else, and that a payload it does not recognize survives it untouched.

use super::*;

/// Decodes a length-delimited protobuf into `(field, bytes)` pairs, top level
/// only. Enough to assert what the stamper produced without a schema.
fn fields(buf: &[u8]) -> Vec<(u64, Vec<u8>)> {
  let mut out = Vec::new();
  let mut i = 0;
  while i < buf.len() {
    let (tag, tag_len) = get_varint(&buf[i..]).expect("a tag");
    i += tag_len;
    assert_eq!(tag & 7, 2, "these fixtures are all length-delimited");
    let (len, len_len) = get_varint(&buf[i..]).expect("a length");
    i += len_len;
    let len = len as usize;
    out.push((tag >> 3, buf[i..i + len].to_vec()));
    i += len;
  }
  out
}

/// `ExportTraceServiceRequest { resource_spans: [ ResourceSpans { inner } ] }`.
fn request(entries: &[&[u8]]) -> Bytes {
  let mut out = Vec::new();
  for entry in entries {
    put_bytes_field(&mut out, 1, entry);
  }
  Bytes::from(out)
}

fn attrs() -> Vec<(String, String)> {
  vec![
    ("aperio.token".to_string(), "ops".to_string()),
    ("aperio.org".to_string(), "acme".to_string()),
  ]
}

#[test]
fn adds_a_resource_to_every_entry() {
  // Two ResourceSpans, each with some opaque content of its own.
  let original = request(&[b"\x12\x03abc", b"\x12\x03xyz"]);
  let stamped = stamp(original.clone(), &attrs());
  let entries = fields(&stamped);
  assert_eq!(entries.len(), 2, "still two entries");
  for (field, body) in &entries {
    assert_eq!(*field, 1);
    // The original content is still there, byte for byte, at the front.
    assert!(body.starts_with(b"\x12\x03"), "{body:?}");
    // And a resource (field 1) was appended after it.
    let appended = &body[5..];
    let inner = fields(appended);
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].0, 1, "field 1 is Resource");
    // Two attributes inside it.
    assert_eq!(fields(&inner[0].1).len(), 2);
  }
}

#[test]
fn the_attribute_encoding_is_key_then_string_value() {
  let stamped = stamp(request(&[b""]), &[("k".to_string(), "v".to_string())]);
  let entry = &fields(&stamped)[0].1;
  let resource = &fields(entry)[0].1;
  let kv = &fields(resource)[0].1;
  let parts = fields(kv);
  assert_eq!(parts[0], (1, b"k".to_vec()), "KeyValue.key");
  // KeyValue.value is an AnyValue whose field 1 is the string.
  assert_eq!(parts[1].0, 2);
  assert_eq!(fields(&parts[1].1)[0], (1, b"v".to_vec()));
}

#[test]
fn no_attributes_means_the_payload_is_not_touched() {
  let original = request(&[b"\x12\x03abc"]);
  assert_eq!(stamp(original.clone(), &[]), original);
}

#[test]
fn an_empty_request_stays_empty() {
  // Nothing to attribute: an exporter flushing an empty batch is ordinary.
  assert_eq!(stamp(Bytes::new(), &attrs()), Bytes::new());
}

#[test]
fn a_payload_it_cannot_walk_survives_untouched() {
  // Truncated, and a varint field where a submessage was expected. Both go on
  // as they arrived: it is the collector's business to decide an export is
  // invalid, and a relay that swallowed one would hide the problem the
  // operator needs to see.
  for bad in [
    Bytes::from_static(b"\x0a\x10ab"),
    Bytes::from_static(b"\x08\x96\x01"),
    Bytes::from_static(b"\xff"),
  ] {
    assert_eq!(stamp(bad.clone(), &attrs()), bad, "{bad:?}");
  }
}

#[test]
fn fields_other_than_the_first_are_copied_verbatim() {
  let mut buf = Vec::new();
  put_bytes_field(&mut buf, 1, b"entry");
  put_bytes_field(&mut buf, 7, b"unknown-to-us");
  let stamped = stamp(Bytes::from(buf), &attrs());
  let entries = fields(&stamped);
  assert_eq!(entries.len(), 2);
  // A field this build has never heard of is not a reason to drop anything.
  assert_eq!(entries[1], (7, b"unknown-to-us".to_vec()));
}
