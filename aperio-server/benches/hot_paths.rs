//! Micro-benchmarks for cache hot-path helpers. The cache module is
//! self-contained (no `crate::` references), so it is included directly here
//! and compiled into the bench binary, the same trick the fuzz targets use to
//! reach a bin-crate's internals. Run with `cargo bench -p aperio-server`.

// The included cache module exposes more than this bench exercises, and its
// in-file test module rides along; neither is dead code in the real crate.
#![allow(dead_code, unused_imports)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

#[path = "../src/cache.rs"]
mod cache;

#[path = "../src/protocol.rs"]
mod protocol;

fn bench_cache_key(c: &mut Criterion) {
  // A URL with tracking params exercises the query normalization path.
  let uri = "/products/list?utm_source=news&page=3&fbclid=xyz&sort=price&gclid=abc";
  c.bench_function("cache_key_with_tracking_params", |b| {
    b.iter(|| cache::cache_key(black_box(Some("app.example.com")), black_box(uri)))
  });
  // A plain URL skips normalization entirely.
  c.bench_function("cache_key_plain", |b| {
    b.iter(|| cache::cache_key(black_box(Some("app.example.com")), black_box("/index.html")))
  });
}

fn bench_response_cache_ttl(c: &mut Criterion) {
  let headers = vec![
    ("content-type".to_string(), "text/html".to_string()),
    (
      "cache-control".to_string(),
      "public, max-age=300, stale-while-revalidate=60".to_string(),
    ),
  ];
  c.bench_function("response_cache_ttl", |b| {
    b.iter(|| cache::response_cache_ttl(black_box(&headers)))
  });
}

fn bench_evaluate_range(c: &mut Criterion) {
  c.bench_function("evaluate_range", |b| {
    b.iter(|| cache::evaluate_range(black_box("bytes=1024-4095"), black_box(1_000_000)))
  });
}

/// What the tunnel envelope costs to encode and decode, JSON against
/// MessagePack, on the two messages that carry every request.
///
/// The envelope is what is left after the body moved into its own binary
/// frame (protocol v5): a method, a URI, an id and a dozen or so header
/// pairs. Published benchmarks measure a codec on their own datasets; this
/// measures ours, which is the only question that decides anything here.
fn bench_envelope_codec(c: &mut Criterion) {
  use protocol::TunnelMessage;

  let headers: Vec<(String, String)> = [
    ("host", "app.example.com"),
    (
      "user-agent",
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
    ),
    (
      "accept",
      "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    ),
    ("accept-encoding", "gzip, deflate, br"),
    ("accept-language", "en-US,en;q=0.9,tr;q=0.8"),
    (
      "cookie",
      "session=6f1a9c2e4b7d8f03a1b2c3d4e5f60718; theme=dark; lang=en",
    ),
    ("referer", "https://app.example.com/dashboard?tab=overview"),
    ("x-request-id", "9f8e7d6c-5b4a-3210-fedc-ba9876543210"),
    ("x-forwarded-for", "203.0.113.7"),
    ("x-forwarded-proto", "https"),
    ("sec-fetch-site", "same-origin"),
    ("sec-fetch-mode", "navigate"),
  ]
  .iter()
  .map(|(k, v)| (k.to_string(), v.to_string()))
  .collect();

  let request = TunnelMessage::Request {
    id: "9f8e7d6c-5b4a-3210-fedc-ba9876543210".to_string(),
    method: "GET".to_string(),
    uri: "/products/list?page=3&sort=price".to_string(),
    headers: headers.clone(),
    body: None,
  };
  let response = TunnelMessage::Response {
    id: "9f8e7d6c-5b4a-3210-fedc-ba9876543210".to_string(),
    status: 200,
    headers,
    body: None,
    trailers: None,
    timings: None,
  };

  for (name, msg) in [("request", &request), ("response", &response)] {
    let json = serde_json::to_string(msg).unwrap();
    let pack = rmp_serde::to_vec_named(msg).unwrap();
    println!(
      "envelope {name}: json {} bytes, msgpack {} bytes",
      json.len(),
      pack.len()
    );

    c.bench_function(&format!("envelope_{name}_encode_json"), |b| {
      b.iter(|| serde_json::to_string(black_box(msg)).unwrap())
    });
    c.bench_function(&format!("envelope_{name}_encode_msgpack"), |b| {
      b.iter(|| rmp_serde::to_vec_named(black_box(msg)).unwrap())
    });
    c.bench_function(&format!("envelope_{name}_decode_json"), |b| {
      b.iter(|| serde_json::from_str::<TunnelMessage>(black_box(&json)).unwrap())
    });
    c.bench_function(&format!("envelope_{name}_decode_msgpack"), |b| {
      b.iter(|| rmp_serde::from_slice::<TunnelMessage>(black_box(&pack)).unwrap())
    });
  }
}

criterion_group!(
  benches,
  bench_cache_key,
  bench_response_cache_ttl,
  bench_evaluate_range,
  bench_envelope_codec
);
criterion_main!(benches);
