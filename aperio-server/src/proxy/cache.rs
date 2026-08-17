//! The server-side response cache on the request path: who gets to refill an
//! entry (one caller, so a cold key does not become a stampede), how a hit and
//! a stale hit are turned back into a visitor response, and the background
//! revalidation that keeps a served-stale entry from staying stale.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Single-flight leadership over one cache key: held by the first request
/// that misses the cache for a coalescable GET while followers wait. Drop
/// removes the key from the in-flight table and (by dropping the watch
/// sender) wakes every waiting follower, on all exit paths.
pub(crate) struct CacheSingleFlight {
  pub(crate) state: Arc<AppState>,
  pub(crate) key: String,
  pub(crate) _done: tokio::sync::watch::Sender<bool>,
}

impl Drop for CacheSingleFlight {
  fn drop(&mut self) {
    if let Ok(mut inflight) = self.state.cache_inflight.lock() {
      inflight.remove(&self.key);
    }
  }
}

/// Background stale-while-revalidate refresh: re-fetches one cacheable GET
/// through the already-selected tunnel client and replaces the cache entry
/// on a cacheable 200. Fire-and-forget, a failure leaves the stale entry
/// serving until its SWR window closes (the leader election retries).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_swr_revalidation(
  state: Arc<AppState>,
  cache_key: String,
  uri: String,
  headers: Vec<(String, String)>,
  client_id: String,
  client_tx: tokio::sync::mpsc::Sender<Message>,
  resilient: bool,
  service_name: Option<String>,
) {
  tokio::spawn(async move {
    let revalidate_id = uuid::Uuid::new_v4().to_string();
    let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();
    state.pending_requests.lock().await.insert(
      revalidate_id.clone(),
      PendingRequest {
        tx: tx_response,
        client_id,
      },
    );
    let msg = TunnelMessage::Request {
      id: revalidate_id.clone(),
      // The refresh is for the same service the stale entry came from.
      service: service_name.clone(),
      method: "GET".to_string(),
      uri,
      headers,
      body: None,
    };
    let Ok(json) = serde_json::to_string(&msg) else {
      state.pending_requests.lock().await.remove(&revalidate_id);
      return;
    };
    if client_tx.send(Message::Text(json.into())).await.is_err() {
      state.pending_requests.lock().await.remove(&revalidate_id);
      return;
    }
    let result = tokio::time::timeout(state.config().gateway_response_timeout, rx_response).await;
    state.pending_requests.lock().await.remove(&revalidate_id);
    if let Ok(Ok(mut tunnel_res)) = result {
      // Streamed bodies never refresh the cache (dropping stream_rx makes
      // the tunnel read loop clean the stream up).
      if tunnel_res.stream_rx.is_none()
        && tunnel_res.status == 200
        && let Some(ttl) = crate::cache::response_cache_ttl(&tunnel_res.headers)
      {
        // Same two shapes as the visitor-facing path: a v5 client sends the
        // body as bytes in the frame, anything older sends base64 in the JSON.
        // Missing this one meant the revalidation refreshed the cache with an
        // empty body, which the e2e suite caught and nothing else would have:
        // the entry is only wrong later, on a request nobody is watching.
        use base64::prelude::*;
        let body = match tunnel_res.body_raw.take() {
          Some(raw) => raw.to_vec(),
          None => tunnel_res
            .body
            .as_deref()
            .and_then(|b| BASE64_STANDARD.decode(b).ok())
            .unwrap_or_default(),
        };
        let swr = crate::cache::response_swr_window(&tunnel_res.headers);
        let surrogate = crate::cache::response_surrogate_keys(&tunnel_res.headers);
        state.response_cache.lock().await.insert(
          cache_key,
          tunnel_res.status,
          tunnel_res.headers,
          body.into(),
          ttl,
          state.config().cache_max_bytes,
          resilient,
          swr,
          surrogate,
        );
      }
    }
  });
}

/// Builds the visitor response for a cache hit: a full 200 with the stored
/// body, or a bodyless 304 when the request's `If-None-Match` matches the
/// entry's validator (synthesized at store time when the backend sent none).
/// Returns the status actually sent, for stats and the access log.
pub(crate) fn cache_hit_response(
  hit: crate::cache::CacheHit,
  request_headers: &HeaderMap,
) -> (u16, u64, Response) {
  let etag = hit
    .headers
    .iter()
    .find(|(n, _)| n.eq_ignore_ascii_case("etag"))
    .map(|(_, v)| v.clone());
  let not_modified = request_headers
    .get("if-none-match")
    .and_then(|v| v.to_str().ok())
    .zip(etag.as_deref())
    .is_some_and(|(inm, tag)| crate::cache::if_none_match_matches(inm, tag));

  // Range requests (video scrubbing, resumable downloads) are satisfied
  // straight from the cached full body, a 304 still wins, and an `If-Range`
  // validator that no longer matches degrades to the full 200 per RFC 9110.
  let total_len = hit.body.len();
  let range_outcome = if not_modified || hit.status != 200 {
    crate::cache::RangeOutcome::Full
  } else {
    let if_range_ok = match request_headers
      .get("if-range")
      .and_then(|v| v.to_str().ok())
    {
      // An If-Range with a date (or a stale validator) means "full body".
      Some(validator) => etag.as_deref() == Some(validator.trim()),
      None => true,
    };
    match request_headers.get("range").and_then(|v| v.to_str().ok()) {
      Some(range) if if_range_ok => crate::cache::evaluate_range(range, total_len),
      _ => crate::cache::RangeOutcome::Full,
    }
  };

  let status = match range_outcome {
    _ if not_modified => 304,
    crate::cache::RangeOutcome::Partial(_, _) => 206,
    crate::cache::RangeOutcome::Unsatisfiable => 416,
    crate::cache::RangeOutcome::Full => hit.status,
  };
  let mut builder = Response::builder()
    .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
    .header("x-aperio-cache", "hit")
    .header("age", hit.age_secs.to_string());
  if hit.stale {
    builder = builder.header("x-aperio-stale", "true");
  }
  if hit.status == 200 && !not_modified {
    builder = builder.header("accept-ranges", "bytes");
  }
  match range_outcome {
    crate::cache::RangeOutcome::Partial(start, end) => {
      builder = builder.header(
        "content-range",
        format!("bytes {}-{}/{}", start, end, total_len),
      );
    }
    crate::cache::RangeOutcome::Unsatisfiable => {
      builder = builder.header("content-range", format!("bytes */{}", total_len));
    }
    crate::cache::RangeOutcome::Full => {}
  }
  for (k, v) in hit.headers.iter() {
    let k_lower = k.to_ascii_lowercase();
    // A 304 carries only the metadata a client may need to update its own
    // cache entry, never entity headers describing a body that isn't there.
    if not_modified
      && !matches!(
        k_lower.as_str(),
        "etag" | "cache-control" | "expires" | "last-modified" | "vary"
      )
    {
      continue;
    }
    // Never copy the cached body's Content-Length: a range/304 response sends
    // a different number of bytes than the full entity, so a stale value would
    // desync the framing (curl sees a truncated reply). Hyper derives the
    // correct Content-Length from the actual body below.
    if k_lower == "content-length" {
      continue;
    }
    if let (Ok(name), Ok(value)) = (
      HeaderName::from_bytes(k.as_bytes()),
      HeaderValue::from_str(v),
    ) {
      builder = builder.header(name, value);
    }
  }
  let (bytes, body) = if not_modified {
    (0u64, Body::empty())
  } else {
    match range_outcome {
      crate::cache::RangeOutcome::Partial(start, end) => {
        let slice = hit.body.slice(start..=end);
        (slice.len() as u64, Body::from(slice))
      }
      crate::cache::RangeOutcome::Unsatisfiable => (0u64, Body::empty()),
      crate::cache::RangeOutcome::Full => (hit.body.len() as u64, Body::from(hit.body)),
    }
  };
  let response = builder
    .body(body)
    .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "cache error").into_response());
  (status, bytes, response)
}

/// Serve-stale fallback: when a route has no client to dispatch to, a
/// resilient cached response (fresh, or expired within the max-stale window)
/// beats a 504. Entries qualify only if the client that produced them
/// announced `resilience`, so a normal service never serves stale content;
/// the moment a healthy client reconnects, the regular proxy path takes over.
pub(crate) async fn stale_cache_response(
  state: &Arc<AppState>,
  method: &str,
  uri: &str,
  headers: &HeaderMap,
  start_time: Instant,
) -> Option<Response> {
  let cfg = state.config();
  if !cfg.cache_enabled || !crate::cache::request_cacheable(method, headers) {
    return None;
  }
  let host = extract_request_host(headers);
  let key = crate::cache::cache_key(host.as_deref(), uri);
  let max_stale = std::time::Duration::from_secs(cfg.cache_max_stale);
  let hit = state
    .response_cache
    .lock()
    .await
    .get_for_outage(&key, max_stale)?;

  let duration = start_time.elapsed();
  let (status, body_len, response) = cache_hit_response(hit, headers);
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
    stats.successful_requests += 1;
    stats.total_bytes_transferred += body_len;
  }
  state.persistent_stats.lock().await.record_request_labeled(
    true,
    0,
    body_len,
    duration.as_millis() as u64,
    None,
    host.as_deref(),
    None,
  );
  log_request_success(
    state,
    uuid::Uuid::new_v4().to_string(),
    method,
    uri,
    status,
    duration,
    host.as_deref(),
    None,
    None,
    None,
  )
  .await;
  telemetry::record_status(&tracing::Span::current(), status);
  Some(response)
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
