//! Tests for the response cache: what may be stored, for how long, and
//! what a stored entry is allowed to be handed back for.

use super::*;

#[test]
fn test_response_cache_ttl() {
  let h = |v: &str| vec![("Cache-Control".to_string(), v.to_string())];
  // No Cache-Control (or no lifetime) → not cacheable.
  assert_eq!(response_cache_ttl(&[]), None);
  assert_eq!(response_cache_ttl(&h("public")), None);
  assert_eq!(response_cache_ttl(&h("max-age=0")), None);
  // Explicit lifetimes.
  assert_eq!(
    response_cache_ttl(&h("max-age=60")),
    Some(Duration::from_secs(60))
  );
  assert_eq!(
    response_cache_ttl(&h("public, max-age=60, s-maxage=120")),
    Some(Duration::from_secs(120))
  );
  // Refusals.
  assert_eq!(response_cache_ttl(&h("no-store")), None);
  assert_eq!(response_cache_ttl(&h("private, max-age=60")), None);
  assert_eq!(response_cache_ttl(&h("no-cache, max-age=60")), None);
  // Vary / Set-Cookie disqualify.
  assert_eq!(
    response_cache_ttl(&[
      ("cache-control".to_string(), "max-age=60".to_string()),
      ("vary".to_string(), "Accept-Encoding".to_string()),
    ]),
    None
  );
  assert_eq!(
    response_cache_ttl(&[
      ("cache-control".to_string(), "max-age=60".to_string()),
      ("set-cookie".to_string(), "sid=1".to_string()),
    ]),
    None
  );
}

#[test]
fn test_request_cacheable() {
  use axum::http::{HeaderMap, HeaderValue};
  let empty = HeaderMap::new();
  assert!(request_cacheable("GET", &empty));
  assert!(!request_cacheable("POST", &empty));
  let mut with_auth = HeaderMap::new();
  with_auth.insert("authorization", HeaderValue::from_static("Bearer x"));
  assert!(!request_cacheable("GET", &with_auth));
  let mut with_cookie = HeaderMap::new();
  with_cookie.insert("cookie", HeaderValue::from_static("sid=1"));
  assert!(!request_cacheable("GET", &with_cookie));
  let mut no_cache = HeaderMap::new();
  no_cache.insert("cache-control", HeaderValue::from_static("no-cache"));
  assert!(!request_cacheable("GET", &no_cache));
}

#[test]
fn test_cache_store_and_expiry() {
  let mut cache = ResponseCache::default();
  let headers = vec![("content-type".to_string(), "text/plain".to_string())];
  cache.insert(
    "h|/a".to_string(),
    200,
    headers.clone(),
    b"hello".to_vec(),
    Duration::from_secs(60),
    1024,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  let hit = cache.get("h|/a", Duration::ZERO).expect("hit");
  assert_eq!(hit.status, 200);
  assert_eq!(hit.body, b"hello");
  assert!(cache.get("h|/b", Duration::ZERO).is_none());

  // Zero-TTL entries expire immediately.
  cache.insert(
    "h|/z".to_string(),
    200,
    headers.clone(),
    b"gone".to_vec(),
    Duration::from_secs(0),
    1024,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  assert!(cache.get("h|/z", Duration::ZERO).is_none());

  // An entry larger than a quarter of the budget is refused.
  cache.insert(
    "h|/big".to_string(),
    200,
    headers.clone(),
    vec![0u8; 512],
    Duration::from_secs(60),
    1024,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  assert!(cache.get("h|/big", Duration::ZERO).is_none());
}

#[test]
fn test_cache_eviction_respects_budget() {
  let mut cache = ResponseCache::default();
  let headers: Vec<(String, String)> = Vec::new();
  // Budget 1000: four 200-byte entries fit, the fifth evicts the one
  // closest to expiry.
  for (i, ttl) in [60u64, 30, 90, 120].iter().enumerate() {
    cache.insert(
      format!("h|/{}", i),
      200,
      headers.clone(),
      vec![0u8; 200],
      Duration::from_secs(*ttl),
      1000,
      false,
      Duration::ZERO,
      Vec::new(),
    );
  }
  cache.insert(
    "h|/new".to_string(),
    200,
    headers.clone(),
    vec![0u8; 240],
    Duration::from_secs(60),
    1000,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  assert!(
    cache.get("h|/new", Duration::ZERO).is_some(),
    "new entry must be stored"
  );
  // The soonest-expiring entry (ttl 30) was evicted; the rest survive.
  assert!(
    cache.get("h|/1", Duration::ZERO).is_none(),
    "closest-to-expiry evicted"
  );
  assert!(cache.get("h|/3", Duration::ZERO).is_some());
}

#[test]
fn test_etag_synthesis_and_matching() {
  // Deterministic, quoted, distinct per body.
  let a = synthesize_etag(b"hello");
  let b = synthesize_etag(b"world");
  assert!(a.starts_with("\"ap-") && a.ends_with('"'));
  assert_ne!(a, b);
  assert_eq!(a, synthesize_etag(b"hello"));

  // If-None-Match semantics: exact, list, wildcard, weak comparison.
  assert!(if_none_match_matches(&a, &a));
  assert!(if_none_match_matches(&format!("{}, {}", b, a), &a));
  assert!(if_none_match_matches("*", &a));
  assert!(if_none_match_matches(&format!("W/{}", a), &a));
  assert!(!if_none_match_matches(&b, &a));
  assert!(!if_none_match_matches("", &a));

  // insert() adds a validator only when the backend sent none.
  let mut cache = ResponseCache::default();
  cache.insert(
    "h|/no-etag".to_string(),
    200,
    Vec::new(),
    b"hello".to_vec(),
    Duration::from_secs(60),
    4096,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  let hit = cache.get("h|/no-etag", Duration::ZERO).unwrap();
  let etag = hit
    .headers
    .iter()
    .find(|(n, _)| n.eq_ignore_ascii_case("etag"))
    .map(|(_, v)| v.clone())
    .expect("etag synthesized");
  assert_eq!(etag, synthesize_etag(b"hello"));

  cache.insert(
    "h|/has-etag".to_string(),
    200,
    vec![("ETag".to_string(), "\"origin\"".to_string())],
    b"hello".to_vec(),
    Duration::from_secs(60),
    4096,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  let hit = cache.get("h|/has-etag", Duration::ZERO).unwrap();
  let etags: Vec<_> = hit
    .headers
    .iter()
    .filter(|(n, _)| n.eq_ignore_ascii_case("etag"))
    .collect();
  assert_eq!(etags.len(), 1, "origin validator must not be duplicated");
  assert_eq!(etags[0].1, "\"origin\"");
}

#[test]
fn test_serve_stale_outage_semantics() {
  let mut cache = ResponseCache::default();
  let headers: Vec<(String, String)> = Vec::new();
  let max_stale = Duration::from_secs(3600);

  // Resilient zero-TTL entry: expired immediately for the fresh path, but
  // still servable through the outage path within the stale window.
  cache.insert(
    "h|/r".to_string(),
    200,
    headers.clone(),
    b"stale-ok".to_vec(),
    Duration::from_secs(0),
    1024,
    true,
    Duration::ZERO,
    Vec::new(),
  );
  assert!(cache.get("h|/r", max_stale).is_none(), "fresh path misses");
  let hit = cache.get_for_outage("h|/r", max_stale).expect("stale hit");
  assert!(hit.stale);
  assert_eq!(hit.body, b"stale-ok");
  // The fresh-path miss must not have dropped the resilient entry.
  assert!(cache.get_for_outage("h|/r", max_stale).is_some());

  // Non-resilient entries never serve through the outage path once expired.
  cache.insert(
    "h|/n".to_string(),
    200,
    headers.clone(),
    b"plain".to_vec(),
    Duration::from_secs(0),
    1024,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  assert!(cache.get_for_outage("h|/n", max_stale).is_none());

  // A zero max-stale window disables outage serving for expired entries.
  assert!(cache.get_for_outage("h|/r", Duration::ZERO).is_none());

  // A fresh resilient entry is servable on both paths, unmarked.
  cache.insert(
    "h|/f".to_string(),
    200,
    headers,
    b"fresh".to_vec(),
    Duration::from_secs(60),
    1024,
    true,
    Duration::ZERO,
    Vec::new(),
  );
  let hit = cache.get_for_outage("h|/f", max_stale).expect("fresh hit");
  assert!(!hit.stale);
}

#[test]
fn test_swr_lookup_and_leader_election() {
  let mut cache = ResponseCache::default();
  let headers = vec![(
    "cache-control".to_string(),
    "max-age=1, stale-while-revalidate=60".to_string(),
  )];
  assert_eq!(response_swr_window(&headers), Duration::from_secs(60));

  // Zero TTL + a 60s SWR window: expired immediately, but still servable.
  cache.insert(
    "h|/swr".to_string(),
    200,
    headers.clone(),
    b"stale-ok".to_vec(),
    Duration::ZERO,
    1024,
    false,
    Duration::from_secs(60),
    Vec::new(),
  );
  // First stale hit leads the revalidation; followers do not.
  match cache.lookup("h|/swr", Duration::ZERO) {
    SwrLookup::StaleRevalidate { hit, lead } => {
      assert!(lead);
      assert!(hit.stale);
      assert_eq!(hit.body, b"stale-ok");
    }
    _ => panic!("expected a stale-while-revalidate hit"),
  }
  match cache.lookup("h|/swr", Duration::ZERO) {
    SwrLookup::StaleRevalidate { lead, .. } => assert!(!lead),
    _ => panic!("expected a follower stale hit"),
  }
  // A refresh replaces the entry and clears the revalidation marker.
  cache.insert(
    "h|/swr".to_string(),
    200,
    headers,
    b"fresh".to_vec(),
    Duration::from_secs(60),
    1024,
    false,
    Duration::from_secs(60),
    Vec::new(),
  );
  match cache.lookup("h|/swr", Duration::ZERO) {
    SwrLookup::Fresh(hit) => assert_eq!(hit.body, b"fresh"),
    _ => panic!("expected a fresh hit after the refresh"),
  }
  // Entries without an SWR window still miss once expired.
  cache.insert(
    "h|/plain".to_string(),
    200,
    vec![],
    b"x".to_vec(),
    Duration::ZERO,
    1024,
    false,
    Duration::ZERO,
    Vec::new(),
  );
  assert!(matches!(
    cache.lookup("h|/plain", Duration::ZERO),
    SwrLookup::Miss
  ));
}

#[test]
fn test_purge_matching() {
  let mut cache = ResponseCache::default();
  for key in ["a.com|/x", "a.com|/assets/1", "b.com|/x"] {
    cache.insert(
      key.to_string(),
      200,
      vec![],
      b"y".to_vec(),
      Duration::from_secs(60),
      1024,
      false,
      Duration::ZERO,
      Vec::new(),
    );
  }
  // Prefix within one hostname.
  assert_eq!(cache.purge_matching(Some("a.com"), Some("/assets/")), 1);
  assert!(cache.get("a.com|/x", Duration::ZERO).is_some());
  // Hostname-wide.
  assert_eq!(cache.purge_matching(Some("a.com"), None), 1);
  assert!(cache.get("b.com|/x", Duration::ZERO).is_some());
  // No selectors = clear everything.
  assert_eq!(cache.purge_matching(None, None), 1);
  assert!(cache.get("b.com|/x", Duration::ZERO).is_none());
}

#[test]
fn test_evaluate_range() {
  use RangeOutcome::*;
  let len = 10;
  // Plain ranges.
  assert!(matches!(evaluate_range("bytes=0-3", len), Partial(0, 3)));
  assert!(matches!(evaluate_range("bytes=4-", len), Partial(4, 9)));
  assert!(matches!(evaluate_range("bytes=-3", len), Partial(7, 9)));
  // An end past the body is clamped.
  assert!(matches!(evaluate_range("bytes=8-99", len), Partial(8, 9)));
  // Out-of-range start is unsatisfiable; so is a zero-length suffix.
  assert!(matches!(evaluate_range("bytes=10-", len), Unsatisfiable));
  assert!(matches!(evaluate_range("bytes=-0", len), Unsatisfiable));
  // Multi-range, other units, and garbage degrade to the full body.
  assert!(matches!(evaluate_range("bytes=0-1,4-5", len), Full));
  assert!(matches!(evaluate_range("items=0-1", len), Full));
  assert!(matches!(evaluate_range("bytes=5-2", len), Full));
  assert!(matches!(evaluate_range("bytes=x-y", len), Full));
  // An empty body cannot satisfy any range.
  assert!(matches!(evaluate_range("bytes=0-1", 0), Full));
}

#[test]
fn cache_key_strips_tracking_params() {
  // Tracking params drop out; real params and order are preserved.
  assert_eq!(
    cache_key(Some("h"), "/p?utm_source=x&q=1&fbclid=abc&sort=asc"),
    "h|/p?q=1&sort=asc"
  );
  // A URL with only tracking params collapses to the bare path.
  assert_eq!(cache_key(Some("h"), "/p?utm_medium=y"), "h|/p");
  // No query string is untouched.
  assert_eq!(cache_key(Some("h"), "/p"), "h|/p");
}

#[test]
fn stats_track_hits_and_misses() {
  let mut cache = ResponseCache::default();
  assert!(cache.get("h|/a", Duration::ZERO).is_none()); // miss
  cache.insert(
    "h|/a".to_string(),
    200,
    vec![],
    b"x".to_vec(),
    Duration::from_secs(60),
    4096,
    false,
    Duration::ZERO,
    vec!["tagA".to_string()],
  );
  assert!(cache.get("h|/a", Duration::ZERO).is_some()); // hit
  let s = cache.stats();
  assert_eq!(s.entries, 1);
  assert_eq!(s.hits, 1);
  assert_eq!(s.misses, 1);
  assert!((s.hit_ratio - 0.5).abs() < 1e-9);
}

#[test]
fn purge_by_surrogate_drops_tagged_entries() {
  let mut cache = ResponseCache::default();
  let ins = |cache: &mut ResponseCache, key: &str, tag: &str| {
    cache.insert(
      key.to_string(),
      200,
      vec![],
      b"x".to_vec(),
      Duration::from_secs(60),
      4096,
      false,
      Duration::ZERO,
      vec![tag.to_string()],
    );
  };
  ins(&mut cache, "h|/a", "product-1");
  ins(&mut cache, "h|/b", "product-1");
  ins(&mut cache, "h|/c", "product-2");
  assert_eq!(cache.purge_by_surrogate("product-1"), 2);
  assert!(cache.get("h|/a", Duration::ZERO).is_none());
  assert!(cache.get("h|/c", Duration::ZERO).is_some());
  assert_eq!(cache.purge_by_surrogate("nope"), 0);
}

#[test]
fn uncacheable_detects_no_store_and_cookies() {
  let cc = |v: &str| vec![("cache-control".to_string(), v.to_string())];
  assert!(response_uncacheable(&cc("no-store")));
  assert!(response_uncacheable(&cc("public, no-cache")));
  assert!(response_uncacheable(&cc("private, max-age=60")));
  assert!(response_uncacheable(&[(
    "Set-Cookie".to_string(),
    "s=1".to_string()
  )]));
  assert!(response_uncacheable(&[(
    "Vary".to_string(),
    "Accept".to_string()
  )]));
  // A plain cacheable 404 (short max-age) is fine to negatively cache.
  assert!(!response_uncacheable(&cc("public, max-age=30")));
  assert!(!response_uncacheable(&[]));
}

#[test]
fn surrogate_keys_parsed_from_header() {
  let h = vec![("Surrogate-Key".to_string(), "a b  c".to_string())];
  assert_eq!(response_surrogate_keys(&h), vec!["a", "b", "c"]);
  assert!(response_surrogate_keys(&[]).is_empty());
}
