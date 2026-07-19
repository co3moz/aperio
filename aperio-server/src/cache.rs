//! Server-side response cache for GET requests (opt-in, `APERIO_CACHE=1`).
//!
//! A client that announces `cache: true` for its service lets the server
//! satisfy repeated GETs from memory instead of a tunnel round-trip. The
//! cache is strictly `Cache-Control`-driven: only responses that explicitly
//! allow shared caching (`max-age`/`s-maxage` without `no-store`/`no-cache`/
//! `private`) are stored, for exactly the advertised lifetime. Total memory
//! is bounded by `APERIO_CACHE_MAX_BYTES`; inserting past the budget evicts
//! the entries closest to expiry.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Strong ETag synthesized from a cached body (hex SHA-256, truncated).
/// Backends that send their own validator are left untouched; this only
/// fills the gap so conditional requests can be answered at the edge.
pub(crate) fn synthesize_etag(body: &[u8]) -> String {
  use sha2::{Digest, Sha256};
  let mut hasher = Sha256::new();
  hasher.update(body);
  let digest = hasher.finalize();
  let hex: String = digest[..16].iter().map(|b| format!("{:02x}", b)).collect();
  format!("\"ap-{}\"", hex)
}

/// True when an `If-None-Match` header value matches `etag`: either `*` or
/// any member of the comma-separated list, compared weakly (a `W/` prefix on
/// either side is ignored, per RFC 9110 conditional-GET semantics).
pub(crate) fn if_none_match_matches(if_none_match: &str, etag: &str) -> bool {
  let strip = |t: &str| t.trim().trim_start_matches("W/").to_string();
  let target = strip(etag);
  if target.is_empty() {
    return false;
  }
  if_none_match
    .split(',')
    .any(|candidate| candidate.trim() == "*" || strip(candidate) == target)
}

/// One cached response.
struct CachedResponse {
  status: u16,
  headers: Vec<(String, String)>,
  body: Vec<u8>,
  stored_at: Instant,
  expires_at: Instant,
  /// The serving client asked for serve-stale resilience when this entry
  /// was stored: it may be served past `expires_at` (up to the max-stale
  /// window) while the route has no healthy client.
  resilient: bool,
}

/// A response served from cache, cloned out of the store.
pub(crate) struct CacheHit {
  pub(crate) status: u16,
  pub(crate) headers: Vec<(String, String)>,
  pub(crate) body: Vec<u8>,
  /// Seconds since the entry was stored (the `Age` header).
  pub(crate) age_secs: u64,
  /// True when the entry is past its advertised lifetime (outage serving).
  pub(crate) stale: bool,
}

/// In-memory bounded response cache, keyed by `host|uri`.
#[derive(Default)]
pub(crate) struct ResponseCache {
  entries: HashMap<String, CachedResponse>,
  total_bytes: u64,
}

/// Cache key for one request.
pub(crate) fn cache_key(host: Option<&str>, uri: &str) -> String {
  format!("{}|{}", host.unwrap_or(""), uri)
}

impl ResponseCache {
  /// Drops every cached entry (used when the cache is disabled at runtime).
  pub(crate) fn clear(&mut self) {
    self.entries.clear();
    self.total_bytes = 0;
  }

  /// Drops every cached entry stored for one request hostname (keys are
  /// `host|uri`). Returns how many entries were removed.
  pub(crate) fn purge_host(&mut self, host: &str) -> usize {
    let prefix = format!("{}|", host);
    let keys: Vec<String> = self
      .entries
      .keys()
      .filter(|k| k.starts_with(&prefix))
      .cloned()
      .collect();
    for key in &keys {
      if let Some(e) = self.entries.remove(key) {
        self.total_bytes = self.total_bytes.saturating_sub(e.body.len() as u64);
      }
    }
    keys.len()
  }

  /// Returns a fresh entry for the key. An expired entry is dropped unless
  /// it is resilient and still inside the `max_stale` window (it stays
  /// stored for [`Self::get_for_outage`]).
  pub(crate) fn get(&mut self, key: &str, max_stale: Duration) -> Option<CacheHit> {
    let now = Instant::now();
    let e = self.entries.get(key)?;
    if e.expires_at <= now {
      if !(e.resilient && now < e.expires_at + max_stale)
        && let Some(e) = self.entries.remove(key)
      {
        self.total_bytes -= e.body.len() as u64;
      }
      return None;
    }
    self.entries.get(key).map(|e| CacheHit {
      status: e.status,
      headers: e.headers.clone(),
      body: e.body.clone(),
      age_secs: now.duration_since(e.stored_at).as_secs(),
      stale: false,
    })
  }

  /// Outage path: returns a resilient entry (fresh or expired) still inside
  /// the `max_stale` window past its lifetime. Used only when the route has
  /// no healthy client, so a stale answer beats a 504.
  pub(crate) fn get_for_outage(&mut self, key: &str, max_stale: Duration) -> Option<CacheHit> {
    let now = Instant::now();
    let e = self.entries.get(key)?;
    if !e.resilient || now >= e.expires_at + max_stale {
      if now >= e.expires_at
        && let Some(e) = self.entries.remove(key)
      {
        self.total_bytes -= e.body.len() as u64;
      }
      return None;
    }
    Some(CacheHit {
      status: e.status,
      headers: e.headers.clone(),
      body: e.body.clone(),
      age_secs: now.duration_since(e.stored_at).as_secs(),
      stale: e.expires_at <= now,
    })
  }

  /// Stores a response for `ttl`. Entries larger than a quarter of the
  /// budget are refused outright (one huge body must not flush the whole
  /// cache); past the budget, entries closest to expiry are evicted first.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn insert(
    &mut self,
    key: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    ttl: Duration,
    max_bytes: u64,
    resilient: bool,
  ) {
    let size = body.len() as u64;
    if size > max_bytes / 4 {
      return;
    }
    if let Some(old) = self.entries.remove(&key) {
      self.total_bytes -= old.body.len() as u64;
    }
    // Evict: expired entries first, then those closest to expiry.
    if self.total_bytes + size > max_bytes {
      let now = Instant::now();
      let mut by_expiry: Vec<(String, Instant)> = self
        .entries
        .iter()
        .map(|(k, e)| (k.clone(), e.expires_at))
        .collect();
      by_expiry.sort_by_key(|(_, exp)| *exp);
      for (k, exp) in by_expiry {
        if self.total_bytes + size <= max_bytes && exp > now {
          break;
        }
        if let Some(e) = self.entries.remove(&k) {
          self.total_bytes -= e.body.len() as u64;
        }
      }
    }
    if self.total_bytes + size > max_bytes {
      return;
    }
    self.total_bytes += size;
    // Fill in a validator when the backend sent none, so conditional GETs
    // can be answered with 304 at the edge without a tunnel round-trip.
    let mut headers = headers;
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("etag")) {
      headers.push(("etag".to_string(), synthesize_etag(&body)));
    }
    let now = Instant::now();
    self.entries.insert(
      key,
      CachedResponse {
        status,
        headers,
        body,
        stored_at: now,
        expires_at: now + ttl,
        resilient,
      },
    );
  }
}

/// Extracts the shared-cache lifetime a response advertises via
/// `Cache-Control`. `None` = must not be cached: no header, `no-store`,
/// `no-cache`, `private`, or no positive `max-age`/`s-maxage`. A `Vary` or
/// `Set-Cookie` header also disqualifies the response (this cache does not
/// key on request headers, and sessions must never be shared).
pub(crate) fn response_cache_ttl(headers: &[(String, String)]) -> Option<Duration> {
  let mut ttl: Option<u64> = None;
  let mut has_cache_control = false;
  for (name, value) in headers {
    match name.to_ascii_lowercase().as_str() {
      "vary" | "set-cookie" => return None,
      "cache-control" => {
        has_cache_control = true;
        for directive in value.split(',') {
          let d = directive.trim().to_ascii_lowercase();
          if d == "no-store" || d == "no-cache" || d == "private" {
            return None;
          }
          // s-maxage (shared caches) wins over max-age.
          if let Some(v) = d.strip_prefix("s-maxage=")
            && let Ok(secs) = v.trim().parse::<u64>()
          {
            return if secs > 0 {
              Some(Duration::from_secs(secs))
            } else {
              None
            };
          }
          if let Some(v) = d.strip_prefix("max-age=")
            && let Ok(secs) = v.trim().parse::<u64>()
          {
            ttl = Some(secs);
          }
        }
      }
      _ => {}
    }
  }
  if !has_cache_control {
    return None;
  }
  ttl.filter(|secs| *secs > 0).map(Duration::from_secs)
}

/// True when the request itself allows a cached answer: a plain GET with no
/// credentials attached (`Authorization`/`Cookie` make responses
/// visitor-specific) and no `Cache-Control: no-cache`/`no-store` override.
pub(crate) fn request_cacheable(method: &str, headers: &axum::http::HeaderMap) -> bool {
  if method != "GET" {
    return false;
  }
  if headers.contains_key("authorization") || headers.contains_key("cookie") {
    return false;
  }
  if let Some(cc) = headers.get("cache-control").and_then(|v| v.to_str().ok()) {
    let cc = cc.to_ascii_lowercase();
    if cc.contains("no-cache") || cc.contains("no-store") {
      return false;
    }
  }
  true
}

#[cfg(test)]
mod tests {
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
    );
    let hit = cache.get_for_outage("h|/f", max_stale).expect("fresh hit");
    assert!(!hit.stale);
  }
}
