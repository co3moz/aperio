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
  let clients = state.clients.read().await;
  let connected = clients.len();
  // The client's own announced labels ride along with its counter, so a
  // Prometheus scraping several environments can group by `env` without
  // relabelling rules written against client ids.
  //
  // One series per *service*, because the counter and the labels are both the
  // service's: reading the first one dropped every other service's traffic
  // from the metric entirely, and rendered one service's labels over a
  // connection carrying several.
  //
  // Keyed by the client *process*, not by the connection.
  //
  // It used to be the server's own per-connection UUID, which had two faults
  // at once. The label is called `client_id`, and that was not the client's
  // `client_id`: an operator who set one and searched their metrics for it
  // found nothing. And the value changed on every reconnect, so a flapping
  // client ended one series and started another from zero rather than
  // producing the counter reset `rate()` knows how to read. Asking for one
  // client's rate over an hour was not possible.
  //
  // `instance_group` is the client's own `client_id`, sent on the handshake
  // and shared by every connection of the process, so the label now means
  // what its name says and survives a reconnect. A client too old to send the
  // header keeps the connection id, which leaves those deployments exactly
  // where they were rather than dropping the label.
  //
  // The `service` label appears when a process produces more than one series,
  // which is a wider condition than the old per-connection one and has to be:
  // three services on three connections used to be three different label
  // sets and are now one, and two samples with the same label set in a scrape
  // is a duplicate Prometheus rejects rather than a shape it merges.
  // Connections of the *same* named service sum instead, which is what an
  // operator means by "that service's requests".
  let mut by_process: std::collections::BTreeMap<String, Vec<(Option<String>, String, u64)>> =
    std::collections::BTreeMap::new();
  for (id, c) in clients.iter() {
    let process = c.instance_group.clone().unwrap_or_else(|| id.clone());
    for s in &c.services {
      by_process.entry(process.clone()).or_default().push((
        s.service_name.clone(),
        crate::metrics_labels::render(&s.metrics_labels),
        s.request_count.load(Ordering::SeqCst),
      ));
    }
  }
  let per_client: Vec<(String, u64, String)> = by_process
    .into_iter()
    .flat_map(|(process, services)| {
      // The position fallback is process-wide, because a service with no name
      // on each of two connections would otherwise be `service_0` twice.
      let mut summed: std::collections::BTreeMap<(String, String), u64> =
        std::collections::BTreeMap::new();
      for (i, (name, base_labels, count)) in services.into_iter().enumerate() {
        let (_, value) = crate::metrics_labels::service_label(name.as_deref(), i);
        *summed.entry((value, base_labels)).or_default() += count;
      }
      let needs_service_label = summed.len() > 1;
      summed
        .into_iter()
        .map(move |((service, base_labels), count)| {
          let mut labels = base_labels;
          if needs_service_label {
            labels.push_str(&crate::metrics_labels::render(&[(
              "service".to_string(),
              service,
            )]));
          }
          (process.clone(), count, labels)
        })
        .collect::<Vec<_>>()
    })
    .collect();
  drop(clients);
  let persistent = state.persistent_stats.lock().await.snapshot();
  let pending = state.pending_requests.lock().await.len();
  // Subscribers are counted per client *process*, the way every other
  // per-client view counts them: a client running three services is one
  // subscriber, not three.
  //
  // Filters are counted per process too, and deduplicated: every connection
  // of a client sends the whole set, so counting them raw would report four
  // subscriptions for a client that asked for two, which is an artifact of
  // how the set is kept rather than anything an operator asked about.
  let (subscribers, subscriptions) = {
    let clients = state.clients.read().await;
    let mut per_process: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
      std::collections::HashMap::new();
    for (id, handle) in clients.iter() {
      if handle.subscriptions.is_empty() {
        continue;
      }
      let process = handle.instance_group.as_deref().unwrap_or(id.as_str());
      per_process
        .entry(process)
        .or_default()
        .extend(handle.subscriptions.iter().map(String::as_str));
    }
    let filters: usize = per_process.values().map(|f| f.len()).sum();
    (per_process.len(), filters)
  };
  let awaiting_ack: usize = state
    .pending_messages
    .lock()
    .await
    .values()
    .map(Vec::len)
    .sum();
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
  // Messaging between the clients of an organization. `dropped` is the one
  // to alert on: it means a subscriber's connection was not keeping up and
  // messages did not reach it.
  let messages = &state.message_metrics;
  for (name, help, kind, value) in [
    (
      "aperio_messages_published_total",
      "Messages accepted for publication between clients.",
      "counter",
      messages.published.load(Ordering::Relaxed),
    ),
    (
      "aperio_messages_delivered_total",
      "Message deliveries handed to a client process (one publish to three subscribers counts three).",
      "counter",
      messages.delivered.load(Ordering::Relaxed),
    ),
    (
      "aperio_messages_dropped_total",
      "Message deliveries dropped because a subscriber was not keeping up.",
      "counter",
      messages.dropped.load(Ordering::Relaxed),
    ),
    (
      "aperio_messages_resent_total",
      "QoS 1 deliveries sent again because no acknowledgement came back.",
      "counter",
      messages.resent.load(Ordering::Relaxed),
    ),
    (
      "aperio_messages_abandoned_total",
      "QoS 1 messages given up on after the acknowledgement window elapsed.",
      "counter",
      messages.abandoned.load(Ordering::Relaxed),
    ),
    (
      "aperio_message_subscribers",
      "Client processes holding at least one subscription.",
      "gauge",
      subscribers as u64,
    ),
    (
      "aperio_message_subscriptions",
      "Distinct topic filters subscribed to, summed over client processes.",
      "gauge",
      subscriptions as u64,
    ),
    (
      "aperio_messages_awaiting_ack",
      "QoS 1 deliveries held while their acknowledgement is outstanding.",
      "gauge",
      awaiting_ack as u64,
    ),
  ] {
    out.push_str(&format!(
      "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
    ));
  }
  out.push_str("# HELP aperio_uptime_seconds Server uptime in seconds.\n");
  out.push_str("# TYPE aperio_uptime_seconds gauge\n");
  out.push_str(&format!("aperio_uptime_seconds {}\n", uptime));
  state.duration_histogram.render(&mut out);

  // Refusals by limit. During a load test this is the question being asked,
  // "which ceiling am I hitting", and a header cannot answer it at ten
  // thousand requests a second. Always emitted, including at zero: a series
  // that only appears once it fires is a series nobody has a dashboard for.
  out.push_str(
    "# HELP aperio_rate_limited_total Requests refused with 429, by the limit that refused them.\n",
  );
  out.push_str("# TYPE aperio_rate_limited_total counter\n");
  for limit in crate::limits::ALL_LIMITS {
    out.push_str(&format!(
      "aperio_rate_limited_total{{limit=\"{}\"}} {}\n",
      limit.kind(),
      state.limit_counters.get(limit)
    ));
  }
  out.push_str(
    "# HELP aperio_client_requests_total Requests handled per connected tunnel client.\n",
  );
  out.push_str("# TYPE aperio_client_requests_total counter\n");
  for (id, count, labels) in per_client {
    out.push_str(&format!(
      "aperio_client_requests_total{{client_id=\"{}\"{}}} {}\n",
      id, labels, count
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
    // Biggest consumers first, the billing view's natural order.
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

/// Request volume in fixed slices: the long views of the dashboard's live
/// activity chart.
///
/// The chart's minute-long view is built in the browser from successive polls,
/// which is right for "is it moving right now" and cannot answer "what did the
/// last quarter hour look like": it starts empty on every reload. This series
/// is the server's own, so it survives a reload and two people looking at once
/// see the same picture.
///
/// `range` picks the resolution: `15m` (the default, and what this endpoint
/// returned before the parameter existed), `2h` or `1d`. The slice width grows
/// with the span so every range is about sixty cells; a day at five-second
/// resolution would be seventeen thousand points drawn into a few hundred
/// pixels.
#[utoipa::path(get, path = "/aperio/api/activity", tag = "dashboard",
  params(("range" = Option<String>, Query,
    description = "Span and resolution: 15m (default, 5-second slices), 2h (2-minute slices) or 1d (15-minute slices).")),
  description = "Request volume per bucket (total and failed) over the requested span, for the activity chart's long views.",
  responses((status = 200, description = "Volume buckets, oldest first", body = serde_json::Value)))]
pub(crate) async fn activity_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
  headers: axum::http::HeaderMap,
) -> Json<serde_json::Value> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let now = crate::store::tokens::now_secs();
  let range = crate::state::ActivityRange::parse(params.get("range").map(String::as_str));
  let buckets = state
    .activity
    .lock()
    .await
    .series(org.as_deref(), range, now);
  Json(serde_json::json!({
    "bucket_secs": range.width_secs(),
    "buckets": buckets,
  }))
}

/// Per-route status-class trends: one-minute buckets over the last window,
/// for the dashboard sparklines.
#[utoipa::path(get, path = "/aperio/api/route-trends", tag = "dashboard",
  description = "Per-route status-code trend: one-minute buckets (2xx/3xx/4xx/5xx counts) over the last 30 minutes.",
  responses((status = 200, description = "Route trends", body = serde_json::Value)))]
pub(crate) async fn route_trends_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  const WINDOW_MINUTES: usize = 30;
  let org = crate::auth::effective_org(&state, &headers).await;
  let now_minute = crate::store::tokens::now_secs() / 60;
  let trends = state.route_trends.lock().await;
  let mut routes: Vec<serde_json::Value> = trends
    .routes
    .iter()
    .filter(|(_, t)| t.org_id == org)
    .map(|(host, trend)| {
      let series = trend.series(WINDOW_MINUTES, now_minute);
      let (mut total, mut errors) = (0u64, 0u64);
      let buckets: Vec<serde_json::Value> = series
        .iter()
        .map(|b| {
          total += b.total as u64;
          errors += b.s5xx as u64;
          serde_json::json!({
            "total": b.total,
            "s2xx": b.s2xx,
            "s3xx": b.s3xx,
            "s4xx": b.s4xx,
            "s5xx": b.s5xx,
          })
        })
        .collect();
      let error_rate = if total > 0 {
        errors as f64 / total as f64
      } else {
        0.0
      };
      serde_json::json!({
        "host": host,
        "total": total,
        "error_rate": (error_rate * 1000.0).round() / 1000.0,
        "buckets": buckets,
      })
    })
    .filter(|r| r["total"].as_u64().unwrap_or(0) > 0)
    .collect();
  routes.sort_by(|a, b| b["total"].as_u64().cmp(&a["total"].as_u64()));
  Json(routes)
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
