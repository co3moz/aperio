//! Server self-observability endpoints: a self-health snapshot (process
//! memory, store size, cache occupancy) and CSV exports of the persisted
//! traffic history.

use axum::{
  Json,
  extract::{Query, State},
  http::{HeaderMap, StatusCode, header},
  response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::state::AppState;

/// Resident-set size of this process in bytes (Linux only; `None` elsewhere).
fn process_rss_bytes() -> Option<u64> {
  #[cfg(target_os = "linux")]
  {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // `sysconf(_SC_PAGESIZE)` is 4 KiB on the platforms Aperio targets.
    Some(rss_pages * 4096)
  }
  #[cfg(not(target_os = "linux"))]
  {
    None
  }
}

/// On-disk footprint of the SQLite store: `aperio.db` plus its WAL/SHM sidecars.
fn store_bytes(data_dir: &std::path::Path) -> u64 {
  ["aperio.db", "aperio.db-wal", "aperio.db-shm"]
    .iter()
    .filter_map(|name| std::fs::metadata(data_dir.join(name)).ok())
    .map(|m| m.len())
    .sum()
}

/// Server self-health snapshot for the dashboard: uptime, connected clients,
/// process RSS, store size, and cache occupancy. Master-admin only.
#[utoipa::path(get, path = "/aperio/api/self-health", tag = "dashboard",
  description = "Server process/memory/store/cache self-health snapshot.",
  responses((status = 200, description = "Self-health snapshot", body = serde_json::Value)))]
pub(crate) async fn self_health_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let data_dir = state
    .settings_path
    .parent()
    .map(|p| p.to_path_buf())
    .unwrap_or_else(|| std::path::PathBuf::from("."));
  let cache = state.response_cache.lock().await.stats();
  let clients = state.clients.lock().await.len();
  Json(serde_json::json!({
    "uptime_seconds": state.server_start_time.elapsed().as_secs(),
    "connected_clients": clients,
    "rss_bytes": process_rss_bytes(),
    "store_bytes": store_bytes(&data_dir),
    "cache": {
      "entries": cache.entries,
      "bytes": cache.bytes,
      "hits": cache.hits,
      "misses": cache.misses,
      "hit_ratio": cache.hit_ratio,
    },
  }))
  .into_response()
}

/// Escapes one CSV field per RFC 4180 (quote when it contains a comma, quote,
/// or newline; double embedded quotes).
fn csv_field(s: &str) -> String {
  if s.contains([',', '"', '\n', '\r']) {
    format!("\"{}\"", s.replace('"', "\"\""))
  } else {
    s.to_string()
  }
}

/// Exports per-period traffic history as CSV for the caller's organization.
/// `unit` = day|week|month|year (default day), `count` = how many recent
/// periods (default 30). Streams `text/csv` with a download filename.
#[utoipa::path(get, path = "/aperio/api/export/traffic.csv", tag = "dashboard",
  description = "Per-period traffic history as CSV (requests, bytes, latency) for the caller's org.",
  responses((status = 200, description = "CSV export")))]
pub(crate) async fn traffic_csv_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(params): Query<HashMap<String, String>>,
) -> Response {
  if crate::auth::dashboard_role(&state, &headers)
    .await
    .is_none()
  {
    return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
  }
  let unit = params
    .get("unit")
    .map(|u| u.as_str())
    .filter(|u| matches!(*u, "day" | "week" | "month" | "year"))
    .unwrap_or("day");
  let count = params
    .get("count")
    .and_then(|c| c.parse::<usize>().ok())
    .unwrap_or(30)
    .clamp(1, 366);

  let org = crate::auth::effective_org(&state, &headers).await;
  let snapshot = state
    .persistent_stats
    .lock()
    .await
    .snapshot_for_org(org.as_deref());
  let keys = crate::store::stats::recent_period_keys(unit, count).unwrap_or_default();

  let mut csv = String::from("period,requests,success,failed,bytes_sent,bytes_received,avg_ms\n");
  for key in keys {
    let p = snapshot.periods.get(&key).cloned().unwrap_or_default();
    let avg = p.duration_ms.checked_div(p.requests).unwrap_or(0);
    // The stored key is prefixed (`d:2026-07-06`); export the bare period.
    let period = key.split_once(':').map(|(_, v)| v).unwrap_or(&key);
    csv.push_str(&format!(
      "{},{},{},{},{},{},{}\n",
      csv_field(period),
      p.requests,
      p.success,
      p.failed,
      p.bytes_sent,
      p.bytes_received,
      avg
    ));
  }

  (
    StatusCode::OK,
    [
      (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
      (
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"aperio-traffic-{unit}.csv\""),
      ),
    ],
    csv,
  )
    .into_response()
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
