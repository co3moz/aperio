use axum::{
  Json,
  extract::State,
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::auth::constant_time_eq_str;
use crate::state::AppState;
use crate::store::stats::PeriodStats;

/// Escapes a Prometheus label value (backslash, double quote, newline).
fn escape_label(value: &str) -> String {
  value
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace('\n', "\\n")
}

/// Renders one labelled counter family from a label → stats map, sorted by
/// label for a stable scrape output.
fn render_labeled(
  out: &mut String,
  name: &str,
  help: &str,
  label: &str,
  entries: &std::collections::HashMap<String, PeriodStats>,
  value: impl Fn(&PeriodStats) -> u64,
) {
  if entries.is_empty() {
    return;
  }
  out.push_str(&format!("# HELP {} {}\n", name, help));
  out.push_str(&format!("# TYPE {} counter\n", name));
  let mut sorted: Vec<(&String, &PeriodStats)> = entries.iter().collect();
  sorted.sort_by_key(|(k, _)| k.as_str());
  for (key, stats) in sorted {
    out.push_str(&format!(
      "{}{{{}=\"{}\"}} {}\n",
      name,
      label,
      escape_label(key),
      value(stats)
    ));
  }
}

/// Prometheus text-format metrics endpoint (`/aperio/metrics`).
/// Enabled with `APERIO_METRICS=1`. Requires a token, presented either as
/// `?token=<value>` (convenient for Prometheus scrape configs) or as an
/// `Authorization: Bearer <value>` header.
#[utoipa::path(get, path = "/aperio/metrics", tag = "public",
  description = "Prometheus text-format metrics. Requires the metrics token as `?token=` or `Authorization: Bearer`.",
  params(("token" = Option<String>, Query, description = "Metrics token (alternative to the Authorization header)")),
  responses((status = 200, description = "Prometheus exposition", body = String), (status = 401, description = "Missing/invalid token")))]
pub(crate) async fn metrics_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
  headers: HeaderMap,
) -> Response {
  if let Some(ref token) = state.config().metrics_token {
    let bearer_ok = headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.strip_prefix("Bearer "))
      .is_some_and(|t| constant_time_eq_str(t, token));
    let query_ok = query
      .get("token")
      .is_some_and(|t| constant_time_eq_str(t, token));
    if !bearer_ok && !query_ok {
      return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
  }

  let stats = state.stats.lock().await.clone();
  let clients = state.clients.lock().await;
  let connected = clients.len();
  let per_client: Vec<(String, u64)> = clients
    .iter()
    .map(|(id, c)| (id.clone(), c.request_count.load(Ordering::SeqCst)))
    .collect();
  drop(clients);
  let persistent = state.persistent_stats.lock().await.snapshot();
  let pending = state.pending_requests.lock().await.len();
  let ws_streams = state.ws_streams.lock().await.len();
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut out = String::with_capacity(1024);
  out.push_str("# HELP aperio_requests_total Total proxied requests received.\n");
  out.push_str("# TYPE aperio_requests_total counter\n");
  out.push_str(&format!("aperio_requests_total {}\n", stats.total_requests));
  out.push_str("# HELP aperio_requests_success_total Successfully proxied requests.\n");
  out.push_str("# TYPE aperio_requests_success_total counter\n");
  out.push_str(&format!(
    "aperio_requests_success_total {}\n",
    stats.successful_requests
  ));
  out.push_str(
    "# HELP aperio_requests_failed_total Failed proxied requests (5xx / gateway errors).\n",
  );
  out.push_str("# TYPE aperio_requests_failed_total counter\n");
  out.push_str(&format!(
    "aperio_requests_failed_total {}\n",
    stats.failed_requests
  ));
  out.push_str("# HELP aperio_bytes_transferred_total Total payload bytes transferred.\n");
  out.push_str("# TYPE aperio_bytes_transferred_total counter\n");
  out.push_str(&format!(
    "aperio_bytes_transferred_total {}\n",
    stats.total_bytes_transferred
  ));
  out.push_str("# HELP aperio_connected_clients Currently connected tunnel clients.\n");
  out.push_str("# TYPE aperio_connected_clients gauge\n");
  out.push_str(&format!("aperio_connected_clients {}\n", connected));
  out.push_str("# HELP aperio_pending_requests Requests currently awaiting a client response.\n");
  out.push_str("# TYPE aperio_pending_requests gauge\n");
  out.push_str(&format!("aperio_pending_requests {}\n", pending));
  out.push_str("# HELP aperio_ws_streams_active Active proxied WebSocket streams.\n");
  out.push_str("# TYPE aperio_ws_streams_active gauge\n");
  out.push_str(&format!("aperio_ws_streams_active {}\n", ws_streams));
  out.push_str("# HELP aperio_uptime_seconds Server uptime in seconds.\n");
  out.push_str("# TYPE aperio_uptime_seconds gauge\n");
  out.push_str(&format!("aperio_uptime_seconds {}\n", uptime));
  state.duration_histogram.render(&mut out);
  out.push_str(
    "# HELP aperio_client_requests_total Requests handled per connected tunnel client.\n",
  );
  out.push_str("# TYPE aperio_client_requests_total counter\n");
  for (id, count) in per_client {
    out.push_str(&format!(
      "aperio_client_requests_total{{client_id=\"{}\"}} {}\n",
      id, count
    ));
  }

  // Per-token and per-hostname counters (restart-surviving, from the
  // persistent stats store) for quota/billing dashboards. Labels beyond the
  // store's cap are folded into `__other`.
  render_labeled(
    &mut out,
    "aperio_token_requests_total",
    "Proxied requests attributed to a token (label `master` = the master token).",
    "token",
    &persistent.by_token,
    |p| p.requests,
  );
  render_labeled(
    &mut out,
    "aperio_token_requests_failed_total",
    "Failed (5xx / gateway error) proxied requests attributed to a token.",
    "token",
    &persistent.by_token,
    |p| p.failed,
  );
  render_labeled(
    &mut out,
    "aperio_token_bytes_received_total",
    "Request body bytes received from visitors, attributed to a token.",
    "token",
    &persistent.by_token,
    |p| p.bytes_received,
  );
  render_labeled(
    &mut out,
    "aperio_token_bytes_sent_total",
    "Response body bytes sent to visitors, attributed to a token.",
    "token",
    &persistent.by_token,
    |p| p.bytes_sent,
  );
  render_labeled(
    &mut out,
    "aperio_hostname_requests_total",
    "Proxied requests attributed to a request hostname.",
    "hostname",
    &persistent.by_hostname,
    |p| p.requests,
  );
  render_labeled(
    &mut out,
    "aperio_hostname_requests_failed_total",
    "Failed (5xx / gateway error) proxied requests attributed to a request hostname.",
    "hostname",
    &persistent.by_hostname,
    |p| p.failed,
  );
  render_labeled(
    &mut out,
    "aperio_hostname_bytes_received_total",
    "Request body bytes received from visitors, attributed to a request hostname.",
    "hostname",
    &persistent.by_hostname,
    |p| p.bytes_received,
  );
  render_labeled(
    &mut out,
    "aperio_hostname_bytes_sent_total",
    "Response body bytes sent to visitors, attributed to a request hostname.",
    "hostname",
    &persistent.by_hostname,
    |p| p.bytes_sent,
  );

  (
    StatusCode::OK,
    [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
    out,
  )
    .into_response()
}

/// Per-stage latency statistics per route, from the timeline data of recent
/// buffered requests (rolling window, in-memory).
#[utoipa::path(get, path = "/aperio/api/stage-stats", tag = "dashboard",
  description = "Rolling per-stage latency statistics (mean/stddev/last, µs) per route, with anomaly verdicts.",
  responses((status = 200, description = "Stage statistics", body = serde_json::Value)))]
pub(crate) async fn stage_stats_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let stats = state.stage_stats.lock().await;
  let mut routes: Vec<serde_json::Value> = stats
    .routes
    .iter()
    // Only routes served by the caller's effective organization.
    .filter(|(_, window)| window.org_id == org)
    .map(|(host, window)| {
      let stages: Vec<serde_json::Value> = window
        .stats()
        .into_iter()
        .map(|row| {
          serde_json::json!({
            "stage": row.stage,
            "count": row.count,
            "mean_us": row.mean.round() as u64,
            "stddev_us": row.stddev.round() as u64,
            "last_us": row.last,
            "anomalous": row.anomalous,
          })
        })
        .collect();
      serde_json::json!({ "host": host, "stages": stages })
    })
    .collect();
  routes.sort_by(|a, b| a["host"].as_str().cmp(&b["host"].as_str()));
  Json(routes)
}

/// Top-N slowest endpoints over the recent latency window (in-memory).
#[utoipa::path(get, path = "/aperio/api/slow-endpoints", tag = "dashboard",
  description = "Slowest endpoints by recent-window p95 latency (host|path, avg/p50/p95/max, request and 5xx counts).",
  responses((status = 200, description = "Slowest endpoints, worst first", body = serde_json::Value)))]
pub(crate) async fn slow_endpoints_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  const TOP_N: usize = 20;
  let org = crate::auth::effective_org(&state, &headers).await;
  let stats = state.endpoint_stats.lock().await;
  let mut rows: Vec<serde_json::Value> = stats
    .endpoints
    .iter()
    // Only endpoints served by the caller's effective organization, with
    // enough recent samples for the percentiles to mean anything.
    .filter(|(_, w)| w.org_id == org && w.samples() >= crate::state::ENDPOINT_MIN_SAMPLES)
    .map(|(key, w)| {
      let (host, path) = key.split_once('|').unwrap_or(("*", key));
      let (avg, p50, p95, max) = w.summary();
      serde_json::json!({
        "host": host,
        "path": path,
        "samples": w.samples(),
        "count": w.count,
        "errors": w.errors,
        "avg_ms": avg.round() as u64,
        "p50_ms": p50,
        "p95_ms": p95,
        "max_ms": max,
      })
    })
    .collect();
  rows.sort_by(|a, b| {
    b["p95_ms"]
      .as_u64()
      .cmp(&a["p95_ms"].as_u64())
      .then(b["avg_ms"].as_u64().cmp(&a["avg_ms"].as_u64()))
  });
  rows.truncate(TOP_N);
  Json(rows)
}

/// Bandwidth accounting: per-token and per-hostname bytes bucketed per day
/// or per month (billing-style report).
#[utoipa::path(get, path = "/aperio/api/bandwidth", tag = "dashboard",
  description = "Bytes in/out per token and hostname, bucketed per day or month (unit=day|month, count).",
  params(
    ("unit" = Option<String>, Query, description = "Bucket granularity: day (default) or month"),
    ("count" = Option<usize>, Query, description = "Buckets to return (default 14 days / 6 months, max 62)")
  ),
  responses((status = 200, description = "Bandwidth rows per label", body = serde_json::Value)))]
pub(crate) async fn bandwidth_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
  axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
  let unit = params.get("unit").map(String::as_str).unwrap_or("day");
  if !matches!(unit, "day" | "month") {
    return (
      axum::http::StatusCode::BAD_REQUEST,
      "unit must be day or month",
    )
      .into_response();
  }
  let count = params
    .get("count")
    .and_then(|c| c.parse::<usize>().ok())
    .unwrap_or(if unit == "day" { 14 } else { 6 })
    .clamp(1, 62);
  let Some(keys) = crate::store::stats::recent_period_keys(unit, count) else {
    return (axum::http::StatusCode::BAD_REQUEST, "invalid unit").into_response();
  };

  let org = crate::auth::effective_org(&state, &headers).await;
  let snapshot = state
    .persistent_stats
    .lock()
    .await
    .snapshot_for_org(org.as_deref());

  // One row per label: the label's counters for each requested bucket, in
  // chronological order (missing buckets are zeroed).
  let rows = |periods: &std::collections::HashMap<
    String,
    std::collections::HashMap<String, crate::store::stats::PeriodStats>,
  >| {
    let mut labels: Vec<String> = periods
      .iter()
      .filter(|(k, _)| keys.contains(k))
      .flat_map(|(_, m)| m.keys().cloned())
      .collect();
    labels.sort();
    labels.dedup();
    let mut out: Vec<serde_json::Value> = labels
      .into_iter()
      .map(|label| {
        let buckets: Vec<serde_json::Value> = keys
          .iter()
          .map(|key| {
            let p = periods.get(key).and_then(|m| m.get(&label));
            serde_json::json!({
              "period": key,
              "bytes_sent": p.map(|p| p.bytes_sent).unwrap_or(0),
              "bytes_received": p.map(|p| p.bytes_received).unwrap_or(0),
              "requests": p.map(|p| p.requests).unwrap_or(0),
            })
          })
          .collect();
        let total: u64 = buckets
          .iter()
          .map(|b| {
            b["bytes_sent"].as_u64().unwrap_or(0) + b["bytes_received"].as_u64().unwrap_or(0)
          })
          .sum();
        serde_json::json!({ "label": label, "total_bytes": total, "buckets": buckets })
      })
      .collect();
    // Biggest consumers first — the billing view's natural order.
    out.sort_by(|a, b| b["total_bytes"].as_u64().cmp(&a["total_bytes"].as_u64()));
    out
  };

  Json(serde_json::json!({
    "unit": unit,
    "periods": keys,
    "by_token": rows(&snapshot.by_token_periods),
    "by_hostname": rows(&snapshot.by_hostname_periods),
  }))
  .into_response()
}
