//! Right-to-erasure selective purge (`POST /aperio/api/purge`).
//!
//! Deletes persisted traffic records matching a selector — a request
//! hostname, a token label, or a visitor IP — without wiping the whole
//! store. Touched surfaces: the in-memory traffic log, the request
//! inspector captures, the per-hostname/per-token statistics aggregates,
//! per-route stage-latency windows, the response cache, and the structured
//! access log file (rewritten in place). Master super-admin only.
//!
//! Visitor IPs are deliberately not part of access-log lines or statistics
//! (queries are sanitized, no IP field is persisted), so the `ip` selector
//! only matches inspector captures via their forwarded-IP request headers.

use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::extract_client_ip;
use crate::state::AppState;

/// Purge selectors: at least one must be present. All matching is
/// case-insensitive exact (hostname/token) or exact-IP.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PurgeRequest {
  /// Request hostname whose records should be erased.
  pub(crate) hostname: Option<String>,
  /// Token label (name) whose aggregate records should be erased.
  pub(crate) token: Option<String>,
  /// Visitor IP whose inspector captures should be erased.
  pub(crate) ip: Option<String>,
}

/// True when a capture's forwarded-IP request headers name the visitor IP.
fn capture_matches_ip(headers: &[(String, String)], ip: &str) -> bool {
  headers.iter().any(|(k, v)| {
    let k = k.to_ascii_lowercase();
    if k == "x-real-ip" || k == "cf-connecting-ip" {
      return v.trim() == ip;
    }
    if k == "x-forwarded-for" {
      return v.split(',').any(|part| part.trim() == ip);
    }
    false
  })
}

/// True when a capture carries the given Host header.
fn capture_matches_host(headers: &[(String, String)], host: &str) -> bool {
  headers.iter().any(|(k, v)| {
    k.eq_ignore_ascii_case("host") && v.split(':').next().unwrap_or("").eq_ignore_ascii_case(host)
  })
}

/// Rewrites the access log file in place, dropping lines whose `host` or
/// `token` field matches. Returns the number of removed lines (None = no
/// file configured or the rewrite failed).
fn rewrite_access_log(
  state: &AppState,
  hostname: Option<&str>,
  token: Option<&str>,
) -> Option<usize> {
  let path = state.access_log_path.as_deref()?;
  let file_lock = state.access_log.as_ref()?;
  // Hold the append lock across the whole rewrite so no line is written
  // between read and truncate.
  let mut guard = file_lock.lock().ok()?;
  let raw = std::fs::read_to_string(path).ok()?;
  let mut kept = String::with_capacity(raw.len());
  let mut removed = 0usize;
  for line in raw.lines() {
    let matches = serde_json::from_str::<serde_json::Value>(line)
      .map(|v| {
        let field = |key: &str| {
          v.get(key)
            .and_then(|x| x.as_str())
            .map(|s| s.to_ascii_lowercase())
        };
        hostname.is_some_and(|h| field("host").as_deref() == Some(h))
          || token.is_some_and(|t| field("token").as_deref() == Some(t))
      })
      .unwrap_or(false);
    if matches {
      removed += 1;
    } else {
      kept.push_str(line);
      kept.push('\n');
    }
  }
  if removed > 0 {
    std::fs::write(path, kept).ok()?;
    // Reopen the append handle: the old descriptor's offset points past the
    // truncated content.
    *guard = std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
      .ok()?;
  }
  Some(removed)
}

/// Selectors for a response-cache purge; both absent = clear the whole cache.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct CachePurgeRequest {
  /// Hostname whose cached entries should be dropped (exact match).
  pub(crate) hostname: Option<String>,
  /// URI prefix whose cached entries should be dropped (e.g. `/assets/`).
  pub(crate) path_prefix: Option<String>,
  /// Surrogate tag (from a backend `Surrogate-Key` header) whose entries
  /// should be dropped — CDN-style tag-based invalidation.
  pub(crate) surrogate_key: Option<String>,
}

/// Returns cache occupancy and hit-rate statistics (admin only).
#[utoipa::path(get, path = "/aperio/api/cache/stats", tag = "dashboard",
  description = "Response-cache entry count, byte size, and hit/miss rate.",
  responses((status = 200, description = "Cache statistics", body = serde_json::Value)))]
pub(crate) async fn cache_stats_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let stats = state.response_cache.lock().await.stats();
  Json(stats).into_response()
}

/// Purges response-cache entries by hostname and/or URI prefix (both absent
/// = the whole cache). The next request re-fetches from the backend — the
/// tool for "I deployed, drop the old copies now" instead of waiting out
/// max-age.
#[utoipa::path(post, path = "/aperio/api/cache/purge", tag = "dashboard",
  description = "Drops response-cache entries matching a hostname and/or URI prefix; empty body clears the whole cache (admin only).",
  request_body = CachePurgeRequest,
  responses((status = 200, description = "Removed entry count", body = serde_json::Value)))]
pub(crate) async fn cache_purge_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<CachePurgeRequest>,
) -> Response {
  // The cache is server-global (keys are host|uri, not org-scoped), so the
  // purge is master-admin only like the other global mutations.
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  let hostname = payload
    .hostname
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_ascii_lowercase);
  let path_prefix = payload
    .path_prefix
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string);
  let surrogate = payload
    .surrogate_key
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string);
  // A surrogate-key purge is tag-based and independent of host/path.
  let removed = if let Some(tag) = &surrogate {
    state.response_cache.lock().await.purge_by_surrogate(tag)
  } else {
    state
      .response_cache
      .lock()
      .await
      .purge_matching(hostname.as_deref(), path_prefix.as_deref())
  };
  info!(
    "Cache purge by {}: hostname={:?} path_prefix={:?} surrogate={:?} → {} entr(ies) dropped",
    actor_ip, hostname, path_prefix, surrogate, removed
  );
  state
    .audit_session(
      "cache_purged",
      &headers,
      &actor_ip,
      &format!(
        "hostname={:?} path_prefix={:?} surrogate={:?} removed={}",
        hostname, path_prefix, surrogate, removed
      ),
    )
    .await;
  Json(serde_json::json!({"status": "ok", "removed": removed})).into_response()
}

/// Erases persisted records matching a hostname, token label, or visitor IP.
#[utoipa::path(post, path = "/aperio/api/purge", tag = "dashboard",
  description = "Right-to-erasure: deletes traffic records (logs, inspector captures, stats aggregates, cache, access-log lines) matching a hostname, token label, or visitor IP (admin only).",
  request_body = PurgeRequest,
  responses((status = 200, description = "Per-surface removal counts", body = serde_json::Value), (status = 400, description = "No selector given")))]
pub(crate) async fn purge_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<PurgeRequest>,
) -> Response {
  // Purge crosses org boundaries (stats aggregates are global), so it is
  // restricted to the master super-admin like export/import.
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  let norm = |v: &Option<String>| {
    v.as_ref()
      .map(|s| s.trim().to_ascii_lowercase())
      .filter(|s| !s.is_empty())
  };
  let hostname = norm(&payload.hostname);
  let token = norm(&payload.token);
  let ip = payload
    .ip
    .as_ref()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
  if hostname.is_none() && token.is_none() && ip.is_none() {
    return (
      StatusCode::BAD_REQUEST,
      "Provide at least one selector: hostname, token, or ip",
    )
      .into_response();
  }

  // In-memory traffic log (hostname only — entries carry no token/IP).
  let mut logs_removed = 0usize;
  if let Some(host) = hostname.as_deref() {
    let mut logs = state.recent_logs.lock().await;
    let before = logs.len();
    logs.retain(|l| l.host.as_deref().map(str::to_ascii_lowercase).as_deref() != Some(host));
    logs_removed = before - logs.len();
  }

  // Inspector captures: match the Host header, or the forwarded visitor IP.
  let captures_removed = {
    let mut captures = state.captured_requests.lock().await;
    let before = captures.len();
    captures.retain(|c| {
      let host_hit = hostname
        .as_deref()
        .is_some_and(|h| capture_matches_host(&c.req_headers, h));
      let ip_hit = ip
        .as_deref()
        .is_some_and(|i| capture_matches_ip(&c.req_headers, i));
      !(host_hit || ip_hit)
    });
    before - captures.len()
  };

  // Persistent per-hostname / per-token statistics aggregates.
  let mut stats_removed = 0usize;
  {
    let mut stats = state.persistent_stats.lock().await;
    if let Some(host) = hostname.as_deref() {
      stats_removed += stats.purge_hostname(host);
    }
    if let Some(token) = token.as_deref() {
      stats_removed += stats.purge_token(token);
    }
  }

  // Per-route stage-latency windows and cached responses (hostname-keyed).
  let mut stage_removed = 0usize;
  let mut cache_removed = 0usize;
  if let Some(host) = hostname.as_deref() {
    stage_removed = state
      .stage_stats
      .lock()
      .await
      .routes
      .remove(host)
      .map(|_| 1)
      .unwrap_or(0);
    cache_removed = state.response_cache.lock().await.purge_host(host);
  }

  // Structured access log file (host/token fields; no IPs are persisted).
  let access_log_removed = rewrite_access_log(&state, hostname.as_deref(), token.as_deref());

  info!(
    "Selective purge by {}: hostname={:?} token={:?} ip={:?} → logs={} captures={} stats_rows={} stage_windows={} cache_entries={} access_log_lines={:?}",
    actor_ip,
    hostname,
    token,
    ip,
    logs_removed,
    captures_removed,
    stats_removed,
    stage_removed,
    cache_removed,
    access_log_removed
  );
  state
    .audit_session(
      "data_purged",
      &headers,
      &actor_ip,
      &format!(
        "hostname={:?} token={:?} ip={:?} logs={} captures={} stats_rows={} stage_windows={} cache_entries={} access_log_lines={:?}",
        hostname,
        token,
        ip,
        logs_removed,
        captures_removed,
        stats_removed,
        stage_removed,
        cache_removed,
        access_log_removed
      ),
    )
    .await;

  Json(serde_json::json!({
    "status": "ok",
    "removed": {
      "traffic_log": logs_removed,
      "inspector_captures": captures_removed,
      "stats_rows": stats_removed,
      "stage_windows": stage_removed,
      "cache_entries": cache_removed,
      "access_log_lines": access_log_removed,
    }
  }))
  .into_response()
}

#[cfg(test)]
#[path = "purge_tests.rs"]
mod tests;
