//! Stamping identity onto an OTLP export, without understanding OTLP.
//!
//! An export arriving over the bridge has to be attributable, or it is worse
//! than useless: telemetry filed under the wrong tenant is believed. The
//! client cannot be the one to say whose it is, so the server writes the
//! attributes itself, here.
//!
//! ## Why this does not decode the payload
//!
//! Decoding an OTLP request means owning a copy of the schema and re-encoding
//! everything through it, which turns "a field this build has never heard of"
//! into "a field this build silently drops". A relay that loses a span because
//! the SDK is newer than the server is a bad relay.
//!
//! So this understands exactly one field. All three export requests have the
//! same top-level shape:
//!
//! ```text
//! ExportTraceServiceRequest  { repeated ResourceSpans   resource_spans  = 1 }
//! ExportMetricsServiceRequest{ repeated ResourceMetrics resource_metrics= 1 }
//! ExportLogsServiceRequest   { repeated ResourceLogs    resource_logs   = 1 }
//! ```
//!
//! and each of those carries `Resource resource = 1`, whose field 1 is a
//! repeated `KeyValue`. So the whole job is: for every top-level field 1,
//! append a second `resource` submessage holding only the new attributes.
//! Protobuf merges repeated occurrences of an embedded message field and
//! concatenates the repeated fields inside them, so appending is exactly
//! equivalent to adding the attributes to the resource that is already there,
//! and every other byte of the payload is copied untouched.

use axum::body::Bytes;

/// Appends a varint.
fn put_varint(out: &mut Vec<u8>, mut value: u64) {
  loop {
    let byte = (value & 0x7F) as u8;
    value >>= 7;
    if value == 0 {
      out.push(byte);
      return;
    }
    out.push(byte | 0x80);
  }
}

/// Reads a varint, returning it and how many bytes it took.
fn get_varint(buf: &[u8]) -> Option<(u64, usize)> {
  let mut value = 0u64;
  let mut shift = 0u32;
  for (i, byte) in buf.iter().enumerate() {
    if shift >= 64 {
      return None;
    }
    value |= u64::from(byte & 0x7F) << shift;
    if byte & 0x80 == 0 {
      return Some((value, i + 1));
    }
    shift += 7;
  }
  None
}

/// Appends a length-delimited field.
fn put_bytes_field(out: &mut Vec<u8>, field: u64, payload: &[u8]) {
  put_varint(out, (field << 3) | 2);
  put_varint(out, payload.len() as u64);
  out.extend_from_slice(payload);
}

/// Encodes `Resource { attributes: [KeyValue { key, value: AnyValue { string_value } }] }`.
fn resource_with(attributes: &[(String, String)]) -> Vec<u8> {
  let mut resource = Vec::new();
  for (key, value) in attributes {
    let mut any = Vec::new();
    // AnyValue.string_value = 1
    put_bytes_field(&mut any, 1, value.as_bytes());
    let mut kv = Vec::new();
    // KeyValue.key = 1, KeyValue.value = 2
    put_bytes_field(&mut kv, 1, key.as_bytes());
    put_bytes_field(&mut kv, 2, &any);
    // Resource.attributes = 1
    put_bytes_field(&mut resource, 1, &kv);
  }
  resource
}

/// Adds `attributes` to the resource of every top-level entry.
///
/// A payload this cannot walk is returned untouched rather than rejected: it
/// is the collector's business to decide whether an export is valid, and a
/// relay that swallowed a malformed export would hide the very problem the
/// operator needs to see.
pub(crate) fn stamp(payload: Bytes, attributes: &[(String, String)]) -> Bytes {
  if attributes.is_empty() {
    return payload;
  }
  let resource = resource_with(attributes);
  // `ResourceSpans.resource = 1`, appended as a second occurrence.
  let mut addition = Vec::new();
  put_bytes_field(&mut addition, 1, &resource);

  let buf = payload.as_ref();
  let mut out = Vec::with_capacity(buf.len() + addition.len() * 2);
  let mut i = 0;
  while i < buf.len() {
    let Some((tag, tag_len)) = get_varint(&buf[i..]) else {
      return payload;
    };
    let field = tag >> 3;
    let wire = tag & 7;
    let start = i;
    i += tag_len;
    // Only length-delimited field 1 is understood; anything else means this is
    // not the shape assumed above, and the payload goes on as it arrived.
    let value_len = match wire {
      2 => {
        let Some((len, len_len)) = get_varint(&buf[i..]) else {
          return payload;
        };
        i += len_len;
        len as usize
      }
      _ => return payload,
    };
    if i + value_len > buf.len() {
      return payload;
    }
    let value = &buf[i..i + value_len];
    i += value_len;
    if field == 1 {
      // Re-emit the entry with the extra resource appended inside it.
      put_varint(&mut out, tag);
      put_varint(&mut out, (value_len + addition.len()) as u64);
      out.extend_from_slice(value);
      out.extend_from_slice(&addition);
    } else {
      out.extend_from_slice(&buf[start..i]);
    }
  }
  Bytes::from(out)
}

#[cfg(test)]
#[path = "otlp_identity_tests.rs"]
mod tests;
