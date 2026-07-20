//! Micro-benchmarks for cache hot-path helpers. The cache module is
//! self-contained (no `crate::` references), so it is included directly here
//! and compiled into the bench binary — the same trick the fuzz targets use to
//! reach a bin-crate's internals. Run with `cargo bench -p aperio-server`.

// The included cache module exposes more than this bench exercises, and its
// in-file test module rides along; neither is dead code in the real crate.
#![allow(dead_code, unused_imports)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

#[path = "../src/cache.rs"]
mod cache;

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

criterion_group!(
  benches,
  bench_cache_key,
  bench_response_cache_ttl,
  bench_evaluate_range
);
criterion_main!(benches);
