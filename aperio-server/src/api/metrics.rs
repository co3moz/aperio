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
