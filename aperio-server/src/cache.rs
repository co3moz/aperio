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
  body: axum::body::Bytes,
  stored_at: Instant,
  expires_at: Instant,
  /// Surrogate cache tags (CDN-style `Surrogate-Key` header) for tag-based
  /// purge: a deploy can invalidate every entry carrying a tag at once.
  surrogate_keys: Vec<String>,
  /// The serving client asked for serve-stale resilience when this entry
  /// was stored: it may be served past `expires_at` (up to the max-stale
  /// window) while the route has no healthy client.
  resilient: bool,
  /// `stale-while-revalidate` window the response advertised (RFC 5861):
  /// past `expires_at` the entry may still be served for this long while a
  /// background revalidation refreshes it. Zero = no SWR.
  swr: Duration,
  /// When a background revalidation was last triggered for this entry
  /// (None = none in flight). Prevents a revalidation stampede; retried
  /// after [`REVALIDATE_RETRY`] in case the refresh failed silently.
  revalidate_started: Option<Instant>,
}

/// A stale-while-revalidate leader that has not refreshed the entry within
/// this long is presumed failed; the next stale hit triggers a new one.
const REVALIDATE_RETRY: Duration = Duration::from_secs(15);

/// Outcome of a cache lookup that honours stale-while-revalidate.
pub(crate) enum SwrLookup {
  /// A fresh entry: serve it, nothing else to do.
  Fresh(CacheHit),
  /// An expired entry inside its SWR window: serve it stale. `lead` is true
  /// when this caller should trigger the background revalidation.
  StaleRevalidate { hit: CacheHit, lead: bool },
  /// Nothing servable.
  Miss,
}

/// A response served from cache, cloned out of the store.
pub(crate) struct CacheHit {
  pub(crate) status: u16,
  pub(crate) headers: Vec<(String, String)>,
  pub(crate) body: axum::body::Bytes,
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
  /// Lifetime hit/miss counters for the cache-stats report.
  hits: u64,
  misses: u64,
}

/// A snapshot of cache occupancy and hit rate for the stats endpoint.
#[derive(serde::Serialize)]
pub(crate) struct CacheStats {
  pub(crate) entries: usize,
  pub(crate) bytes: u64,
  pub(crate) hits: u64,
  pub(crate) misses: u64,
  /// Fraction of lookups served from cache (0.0 when there were none).
  pub(crate) hit_ratio: f64,
}

/// Cache key for one request. Tracking query parameters (`utm_*`, `gclid`,
/// `fbclid`, …) are stripped so a URL and its ad-tagged variants share one
/// entry, they never change the response body.
pub(crate) fn cache_key(host: Option<&str>, uri: &str) -> String {
  format!("{}|{}", host.unwrap_or(""), normalize_cache_uri(uri))
}

/// True for query-parameter names that only carry click/analytics tracking and
/// never affect the response, so they can be dropped from the cache key.
fn is_tracking_param(name: &str) -> bool {
  let n = name.to_ascii_lowercase();
  n.starts_with("utm_")
    || matches!(
      n.as_str(),
      "gclid" | "fbclid" | "gbraid" | "wbraid" | "mc_cid" | "mc_eid" | "_ga" | "igshid"
    )
}

/// Drops tracking parameters from a request URI for cache-key purposes,
/// preserving the order of the remaining parameters. The URI sent to the
/// backend is unaffected, only the cache key is normalized.
fn normalize_cache_uri(uri: &str) -> String {
  let Some((path, query)) = uri.split_once('?') else {
    return uri.to_string();
  };
  let kept: Vec<&str> = query
    .split('&')
    .filter(|p| !p.is_empty() && !is_tracking_param(p.split('=').next().unwrap_or("")))
    .collect();
  if kept.is_empty() {
    path.to_string()
  } else {
    format!("{}?{}", path, kept.join("&"))
  }
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

  /// Returns a fresh entry for the key (test convenience over [`Self::lookup`],
  /// which the proxy uses directly for its stale-while-revalidate handling).
  #[cfg(test)]
  pub(crate) fn get(&mut self, key: &str, max_stale: Duration) -> Option<CacheHit> {
    match self.lookup(key, max_stale) {
      SwrLookup::Fresh(hit) => Some(hit),
      _ => None,
    }
  }

  /// Stale-while-revalidate lookup: a fresh entry is served as usual; an
  /// expired entry still inside its advertised SWR window is served stale,
  /// with the first caller since expiry (or since a presumed-failed refresh)
  /// elected to trigger the background revalidation.
  pub(crate) fn lookup(&mut self, key: &str, max_stale: Duration) -> SwrLookup {
    let now = Instant::now();
    let Some(e) = self.entries.get_mut(key) else {
      self.misses += 1;
      return SwrLookup::Miss;
    };
    let hit = |e: &CachedResponse, stale: bool| CacheHit {
      status: e.status,
      headers: e.headers.clone(),
      body: e.body.clone(),
      age_secs: now.duration_since(e.stored_at).as_secs(),
      stale,
    };
    if e.expires_at > now {
      let h = hit(e, false);
      self.hits += 1;
      return SwrLookup::Fresh(h);
    }
    if now < e.expires_at + e.swr {
      let lead = match e.revalidate_started {
        None => true,
        Some(started) => now.duration_since(started) >= REVALIDATE_RETRY,
      };
      if lead {
        e.revalidate_started = Some(now);
      }
      let h = hit(e, true);
      self.hits += 1;
      return SwrLookup::StaleRevalidate { hit: h, lead };
    }
    // Past both windows: drop unless resilient serve-stale still covers it.
    if !(e.resilient && now < e.expires_at + max_stale)
      && let Some(e) = self.entries.remove(key)
    {
      self.total_bytes = self.total_bytes.saturating_sub(e.body.len() as u64);
    }
    self.misses += 1;
    SwrLookup::Miss
  }

  /// A snapshot of cache occupancy and hit rate for the stats endpoint.
  pub(crate) fn stats(&self) -> CacheStats {
    let total = self.hits + self.misses;
    CacheStats {
      entries: self.entries.len(),
      bytes: self.total_bytes,
      hits: self.hits,
      misses: self.misses,
      hit_ratio: if total == 0 {
        0.0
      } else {
        self.hits as f64 / total as f64
      },
    }
  }

  /// Purges every entry tagged with the given surrogate key (CDN-style
  /// tag-based invalidation). Returns how many entries were removed.
  pub(crate) fn purge_by_surrogate(&mut self, tag: &str) -> usize {
    let keys: Vec<String> = self
      .entries
      .iter()
      .filter(|(_, e)| e.surrogate_keys.iter().any(|t| t == tag))
      .map(|(k, _)| k.clone())
      .collect();
    for key in &keys {
      if let Some(e) = self.entries.remove(key) {
        self.total_bytes = self.total_bytes.saturating_sub(e.body.len() as u64);
      }
    }
    keys.len()
  }

  /// Purges entries by selector: `host` matches the key's hostname part
  /// exactly, `path_prefix` the start of its URI part; both absent = clear
  /// everything. Returns removed entries.
  pub(crate) fn purge_matching(&mut self, host: Option<&str>, path_prefix: Option<&str>) -> usize {
    if host.is_none() && path_prefix.is_none() {
      let removed = self.entries.len();
      self.clear();
      return removed;
    }
    let keys: Vec<String> = self
      .entries
      .keys()
      .filter(|k| {
        let (key_host, key_uri) = k.split_once('|').unwrap_or(("", k));
        host.is_none_or(|h| key_host.eq_ignore_ascii_case(h))
          && path_prefix.is_none_or(|p| key_uri.starts_with(p))
      })
      .cloned()
      .collect();
    for key in &keys {
      if let Some(e) = self.entries.remove(key) {
        self.total_bytes = self.total_bytes.saturating_sub(e.body.len() as u64);
      }
    }
    keys.len()
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
        self.total_bytes = self.total_bytes.saturating_sub(e.body.len() as u64);
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
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn insert(
    &mut self,
    key: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: axum::body::Bytes,
    ttl: Duration,
    max_bytes: u64,
    resilient: bool,
    swr: Duration,
    surrogate_keys: Vec<String>,
  ) {
    let size = body.len() as u64;
    if size > max_bytes / 4 {
      return;
    }
    if let Some(old) = self.entries.remove(&key) {
      self.total_bytes = self.total_bytes.saturating_sub(old.body.len() as u64);
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
          self.total_bytes = self.total_bytes.saturating_sub(e.body.len() as u64);
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
        surrogate_keys,
        resilient,
        swr,
        revalidate_started: None,
      },
    );
  }
}

/// Extracts the surrogate cache tags a response advertises via `Surrogate-Key`
/// (space-separated, the CDN convention) for tag-based purge.
pub(crate) fn response_surrogate_keys(headers: &[(String, String)]) -> Vec<String> {
  headers
    .iter()
    .filter(|(n, _)| n.eq_ignore_ascii_case("surrogate-key"))
    .flat_map(|(_, v)| v.split_whitespace().map(|s| s.to_string()))
    .collect()
}

/// True when a response must not be cached regardless of a positive TTL:
/// it carries `Vary`/`Set-Cookie`, or a `Cache-Control` `no-store`/`no-cache`/
/// `private`. Used to gate negative caching (404/410), which otherwise skips
/// the `Cache-Control`-driven checks that the 200 path gets for free.
pub(crate) fn response_uncacheable(headers: &[(String, String)]) -> bool {
  for (name, value) in headers {
    match name.to_ascii_lowercase().as_str() {
      "vary" | "set-cookie" => return true,
      "cache-control" => {
        for directive in value.split(',') {
          let d = directive.trim().to_ascii_lowercase();
          if d == "no-store" || d == "no-cache" || d == "private" {
            return true;
          }
        }
      }
      _ => {}
    }
  }
  false
}

/// Short negative-cache TTL for 404/410 responses
/// (`APERIO_CACHE_NEGATIVE_TTL`, seconds; 0/unset = negative caching off).
pub(crate) fn negative_cache_ttl() -> Duration {
  use std::sync::OnceLock;
  static TTL: OnceLock<Duration> = OnceLock::new();
  *TTL.get_or_init(|| {
    Duration::from_secs(
      std::env::var("APERIO_CACHE_NEGATIVE_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0),
    )
  })
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

/// Outcome of evaluating a request's `Range` header against a cached body.
pub(crate) enum RangeOutcome {
  /// Serve the full body (no/unsupported/multi range).
  Full,
  /// Serve `body[start..=end]` as a 206 Partial Content.
  Partial(usize, usize),
  /// The range lies entirely outside the body: 416 with `bytes */len`.
  Unsatisfiable,
}

/// Evaluates a `Range` header value against a body of `len` bytes. Only
/// single `bytes=` ranges are honored, multipart ranges and other units are
/// answered with the full body, which RFC 9110 explicitly permits.
pub(crate) fn evaluate_range(range: &str, len: usize) -> RangeOutcome {
  let Some(spec) = range.trim().strip_prefix("bytes=") else {
    return RangeOutcome::Full;
  };
  if spec.contains(',') || len == 0 {
    return RangeOutcome::Full;
  }
  let Some((start_raw, end_raw)) = spec.split_once('-') else {
    return RangeOutcome::Full;
  };
  let (start_raw, end_raw) = (start_raw.trim(), end_raw.trim());
  if start_raw.is_empty() {
    // Suffix form: the last N bytes.
    let Ok(suffix) = end_raw.parse::<usize>() else {
      return RangeOutcome::Full;
    };
    if suffix == 0 {
      return RangeOutcome::Unsatisfiable;
    }
    let start = len.saturating_sub(suffix);
    return RangeOutcome::Partial(start, len - 1);
  }
  let Ok(start) = start_raw.parse::<usize>() else {
    return RangeOutcome::Full;
  };
  if start >= len {
    return RangeOutcome::Unsatisfiable;
  }
  let end = if end_raw.is_empty() {
    len - 1
  } else {
    match end_raw.parse::<usize>() {
      Ok(e) => e.min(len - 1),
      Err(_) => return RangeOutcome::Full,
    }
  };
  if end < start {
    return RangeOutcome::Full;
  }
  RangeOutcome::Partial(start, end)
}

/// Extracts the `stale-while-revalidate` window (RFC 5861) a response
/// advertises via `Cache-Control`. Zero = none.
pub(crate) fn response_swr_window(headers: &[(String, String)]) -> Duration {
  for (name, value) in headers {
    if name.eq_ignore_ascii_case("cache-control") {
      for directive in value.split(',') {
        let d = directive.trim().to_ascii_lowercase();
        if let Some(v) = d.strip_prefix("stale-while-revalidate=")
          && let Ok(secs) = v.trim().parse::<u64>()
        {
          return Duration::from_secs(secs);
        }
      }
    }
  }
  Duration::ZERO
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
#[path = "cache_tests.rs"]
mod tests;
