//! What the dashboard's numbers are made of: the per-client statistics it
//! renders, the uptime history behind the availability strip, and the traffic
//! buckets behind the chart. Every one of them is fenced to the caller's
//! organization before it is returned.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Which service of a connection an endpoint is about.
///
/// A query parameter rather than a second path segment, and defaulted, so
/// every existing caller keeps working: a connection carrying one service is
/// every client before protocol v8, and for those `?service=0` and no
/// parameter at all are the same request.
#[derive(serde::Deserialize, Default)]
pub(crate) struct ServiceQuery {
  #[serde(default)]
  pub(crate) service: usize,
}

/// Hostnames this client asked for itself, in declaration order and without
/// duplicates. `declared_hostname` is the first entry of the client's own list,
/// but a client that predates multi-hostname binds only sends that one.
pub(crate) fn declared_hostnames_of(service: &crate::state::ServiceState) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  for h in service
    .declared_hostname
    .iter()
    .chain(service.declared_hostnames.iter())
  {
    if !out.contains(h) {
      out.push(h.clone());
    }
  }
  out
}

/// Computes the live statistics + active-connection snapshot shared by the
/// `/api/stats` endpoint and the SSE live stream.
pub(crate) async fn compute_stats(state: &AppState) -> EnhancedServerStats {
  let raw_stats = state.stats.lock().await.clone();
  let clients = state.clients.read().await;

  // Instance ids reported by more than one live connection: a
  // misconfiguration worth flagging (`--bind-tunnels` / failover `wait`
  // lookups become ambiguous).
  let mut instance_counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
  for handle in clients.values() {
    if let Some(iid) = handle.reported_instance_id.as_deref() {
      *instance_counts.entry(iid).or_insert(0) += 1;
    }
  }

  // One row per service, not per connection. A connection carrying two
  // services is two things an operator manages: each has its own binds, its
  // own gate, its own health. Showing the first and hiding the second would
  // be worse than showing neither, because nothing on the page would say a
  // second existed.
  //
  // Rows still carry the connection id, because that is what the socket is
  // and what several columns describe. What makes a row addressable is the
  // pair, which is why `service_index` sits beside it.
  let active_clients = clients
    .iter()
    .flat_map(|(id, handle)| {
      handle
        .services
        .iter()
        .enumerate()
        .map(move |(service_index, service)| (id, handle, service_index, service))
    })
    .map(|(id, handle, service_index, service)| ClientDetail {
      id: id.clone(),
      service_index,
      ip: handle.client_ip.clone(),
      connected_for_seconds: handle.connected_at.elapsed().as_secs(),
      request_count: service.request_count.load(Ordering::SeqCst),
      path_bind: service
        .declared_path
        .clone()
        .or_else(|| service.assigned_path.clone()),
      // Declared names first: the dashboard shows the head of this list as
      // the client's hostname and folds the rest away, and a name the
      // operator chose is the one worth showing.
      hostname_binds: {
        let mut set = declared_hostnames_of(service);
        for h in &service.assigned_hostnames {
          if !set.contains(h) {
            set.push(h.clone());
          }
        }
        set
      },
      declared_hostnames: declared_hostnames_of(service),
      random_hostname: service.random_hostname.clone(),
      token_name: handle.perms.token_name.clone(),
      org_id: handle.perms.org_id.clone(),
      override_path_bind: service.override_path_bind.clone(),
      override_hostname_binds: service.override_hostname_binds.clone(),
      last_ping_seconds_ago: handle.last_ping_at.map(|t| t.elapsed().as_secs()),
      max_concurrent: service.max_concurrent,
      version: handle.client_version.clone(),
      service: service.service_name.clone(),
      service_custom_name: service.service_custom_name.clone(),
      public: service.public,
      visitor_auth: service.visitor_auth.is_some(),
      allowed_ips: service.allowed_ips.clone(),
      protocol: handle.client_protocol,
      protocol_mismatch: handle
        .client_protocol
        .is_some_and(|p| p != PROTOCOL_VERSION),
      backend_healthy: service.backend_healthy,
      backend_probed: service.backend_probed,
      cpu_percent: handle.cpu_percent,
      rss_bytes: handle.rss_bytes,
      rtt_ms: handle.rtt_ms,
      jitter_ms: handle.jitter_ms,
      reconnects: handle.reconnects,
      priority: service.priority,
      bandwidth_bps: match service.bandwidth_bps.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
      },
      healthy: handle.is_healthy(state.config().client_down_threshold),
      draining: handle.draining,
      ejected: service
        .ejected_until
        .is_some_and(|until| std::time::Instant::now() < until),
      enabled: service.admin_enabled,
      cache_ignored: service.cache && !state.config().cache_enabled,
      capture: service.capture,
      capture_disabled_by_server: service.capture && !state.config().inspector,
      instance_id: handle.reported_instance_id.clone(),
      instance_id_shared: handle
        .reported_instance_id
        .as_deref()
        .is_some_and(|iid| instance_counts.get(iid).copied().unwrap_or(0) > 1),
      instance_group: handle.instance_group.clone(),
    })
    .collect();

  let pending_count = state.pending_requests.lock().await.len();
  let persistent = state.persistent_stats.lock().await.snapshot();
  let avg_response_ms = persistent.avg_response_ms();
  let today = persistent
    .periods
    .get(&stats::period_keys()[0])
    .cloned()
    .unwrap_or_default();

  EnhancedServerStats {
    total_requests: raw_stats.total_requests,
    successful_requests: raw_stats.successful_requests,
    failed_requests: raw_stats.failed_requests,
    total_bytes_transferred: raw_stats.total_bytes_transferred,
    connected_clients_count: clients.len(),
    uptime_seconds: state.server_start_time.elapsed().as_secs(),
    pending_requests_count: pending_count,
    active_clients,
    persistent,
    avg_response_ms,
    today,
  }
}

/// Handler returning the live statistics + active connections detail in JSON.
/// One day of a service entity's availability history.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct UptimeDay {
  pub(crate) date: String,
  pub(crate) up_secs: u64,
  pub(crate) degraded_secs: u64,
  pub(crate) down_secs: u64,
}

/// Availability summary of one service entity.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct UptimeEntry {
  /// Service name (or stable client id when the client names no service).
  pub(crate) name: String,
  /// Current status: `up`, `degraded`, or `down`.
  pub(crate) status: crate::store::uptime::Availability,
  /// Unix seconds the entity was last observed connected.
  pub(crate) last_seen: u64,
  /// Uptime percentage over today's observed time (null = nothing observed).
  pub(crate) pct_today: Option<f64>,
  /// Uptime percentage over the last 7 days of observed time.
  pub(crate) pct_7d: Option<f64>,
  /// Uptime percentage over the last 30 days of observed time.
  pub(crate) pct_30d: Option<f64>,
  /// Last 30 day buckets, chronological (missing days are absent).
  pub(crate) days: Vec<UptimeDay>,
}

/// Percentage of observed seconds spent `up` across the given day keys.
pub(crate) fn uptime_pct(
  days: &std::collections::HashMap<String, crate::store::uptime::DayAvailability>,
  keys: &[String],
) -> Option<f64> {
  let (mut up, mut total) = (0u64, 0u64);
  for key in keys {
    if let Some(d) = days.get(key.strip_prefix("d:").unwrap_or(key)) {
      up += d.up_secs;
      total += d.observed_secs();
    }
  }
  (total > 0).then(|| up as f64 / total as f64 * 100.0)
}

/// Returns the availability history of every tracked service entity.
#[utoipa::path(get, path = "/aperio/api/uptime", tag = "dashboard",
  description = "Uptime/SLA summary per service entity: current status, uptime percentages for today / 7 days / 30 days of observed time, and the last 30 daily buckets (seconds up/degraded/down). Time while the server itself was not running is not counted.",
  responses((status = 200, description = "Availability per service entity", body = Vec<UptimeEntry>)))]
pub(crate) async fn uptime_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
) -> Json<Vec<UptimeEntry>> {
  // Hide services that have been continuously down for longer than this: a
  // one-off/experimental connection that errored and was closed leaves an
  // uptime record whose `last_seen` (only advanced while up/degraded) stops
  // advancing, so it drops out of the view once stale, while a service that
  // was up recently, or is up now, stays. The record itself lingers in the
  // store until its own 30-day GC.
  const HIDE_STALE_DOWN_SECS: u64 = 24 * 60 * 60;
  let now = crate::store::sessions::now_secs();
  let org = crate::auth::effective_org(&state, &headers).await;
  let snapshot = state.uptime.lock().await.snapshot();
  let today = stats::recent_period_keys("day", 1).unwrap_or_default();
  let last7 = stats::recent_period_keys("day", 7).unwrap_or_default();
  let last30 = stats::recent_period_keys("day", 30).unwrap_or_default();

  let mut entries: Vec<UptimeEntry> = snapshot
    .into_iter()
    // Only service entities served by the caller's effective organization, and
    // hide long-dead ones (down and not seen for over a day).
    .filter(|(_, entity)| {
      entity.org_id == org
        && !(entity.status == crate::store::uptime::Availability::Down
          && now.saturating_sub(entity.last_seen) > HIDE_STALE_DOWN_SECS)
    })
    .map(|(name, entity)| {
      let mut days: Vec<UptimeDay> = last30
        .iter()
        .filter_map(|key| {
          let date = key.strip_prefix("d:").unwrap_or(key);
          entity.days.get(date).map(|d| UptimeDay {
            date: date.to_string(),
            up_secs: d.up_secs,
            degraded_secs: d.degraded_secs,
            down_secs: d.down_secs,
          })
        })
        .collect();
      days.sort_by(|a, b| a.date.cmp(&b.date));
      UptimeEntry {
        pct_today: uptime_pct(&entity.days, &today),
        pct_7d: uptime_pct(&entity.days, &last7),
        pct_30d: uptime_pct(&entity.days, &last30),
        status: entity.status,
        last_seen: entity.last_seen,
        days,
        name,
      }
    })
    .collect();
  // Most-recently-active first (by last successful ping), then by name, so
  // live/recently-up services lead and stale ones sink to the bottom.
  entries.sort_by(|a, b| {
    b.last_seen
      .cmp(&a.last_seen)
      .then_with(|| a.name.cmp(&b.name))
  });
  Json(entries)
}

/// Query for the traffic-history endpoint: either a rolling window
/// (`unit` + `count`) or a custom day range (`from` + `to`).
#[derive(Deserialize, utoipa::IntoParams)]
pub(crate) struct HistoryQuery {
  /// Bucket unit: `day` (default), `week`, `month`, or `year`.
  pub(crate) unit: Option<String>,
  /// Number of buckets, newest last (clamped to the retention window).
  pub(crate) count: Option<usize>,
  /// Custom range start, `YYYY-MM-DD` (day buckets; overrides unit/count).
  pub(crate) from: Option<String>,
  /// Custom range end, `YYYY-MM-DD` (defaults to today).
  pub(crate) to: Option<String>,
}

/// One period bucket of the traffic history (zero-filled when no traffic).
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct HistoryBucket {
  /// Period label: `2026-07-06`, `2026-W27`, `2026-07`, or `2026`.
  pub(crate) period: String,
  pub(crate) requests: u64,
  pub(crate) success: u64,
  pub(crate) failed: u64,
  pub(crate) bytes_sent: u64,
  pub(crate) bytes_received: u64,
  /// Average response time of the bucket in milliseconds.
  pub(crate) avg_ms: f64,
}

/// Returns zero-filled traffic history buckets from the persistent stats.
#[utoipa::path(get, path = "/aperio/api/stats/history", tag = "dashboard",
  description = "Chronological traffic buckets (requests, success/failed, bytes, latency) for a rolling window (unit=day|week|month|year + count) or a custom day range (from/to, YYYY-MM-DD). Buckets outside the retention window (60 days / 26 weeks / 24 months / 10 years) are zero.",
  params(HistoryQuery),
  responses((status = 200, description = "Traffic history buckets", body = Vec<HistoryBucket>), (status = 400, description = "Invalid unit or date range")))]
pub(crate) async fn stats_history_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
  axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  let keys = if let Some(from) = query.from.as_deref() {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let to = query.to.as_deref().unwrap_or(&today);
    stats::day_keys_between(from, to)
  } else {
    let unit = query.unit.as_deref().unwrap_or("day");
    let max = match unit {
      "day" => 60,
      "week" => 26,
      "month" => 24,
      "year" => 10,
      _ => 0,
    };
    if max == 0 {
      None
    } else {
      let count = query.count.unwrap_or(30).clamp(1, max);
      stats::recent_period_keys(unit, count)
    }
  };
  let Some(keys) = keys else {
    return (
      StatusCode::BAD_REQUEST,
      "unit must be day/week/month/year; from/to must be YYYY-MM-DD with from <= to",
    )
      .into_response();
  };

  // History is scoped to the caller's effective organization.
  let snapshot = state
    .persistent_stats
    .lock()
    .await
    .snapshot_for_org(org.as_deref());
  let buckets: Vec<HistoryBucket> = keys
    .into_iter()
    .map(|key| {
      let period = key
        .split_once(':')
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| key.clone());
      let p = snapshot.periods.get(&key).cloned().unwrap_or_default();
      let avg_ms = if p.requests == 0 {
        0.0
      } else {
        p.duration_ms as f64 / p.requests as f64
      };
      HistoryBucket {
        period,
        requests: p.requests,
        success: p.success,
        failed: p.failed,
        bytes_sent: p.bytes_sent,
        bytes_received: p.bytes_received,
        avg_ms,
      }
    })
    .collect();
  Json(buckets).into_response()
}

/// Restricts a stats snapshot to the caller's effective organization: only
/// clients whose token belongs to that org remain visible, and the connected
/// count follows. Aggregate counters stay global. The master super-admin sees
/// the org currently selected on their session.
pub(crate) fn filter_stats_for_org(stats: &mut EnhancedServerStats, org: &Option<String>) {
  stats.active_clients.retain(|c| &c.org_id == org);
  stats.connected_clients_count = stats.active_clients.len();
}

/// Replaces the aggregate counters, `today`, average, and persistent
/// breakdown of a stats snapshot with the effective organization's own slice,
/// so each org sees only its own traffic totals (the master org's slice covers
/// org-`None` traffic; the true server-wide grand total stays in Prometheus).
pub(crate) async fn scope_stats_for_org(
  state: &AppState,
  stats: &mut EnhancedServerStats,
  org: &Option<String>,
) {
  let persistent = state
    .persistent_stats
    .lock()
    .await
    .snapshot_for_org(org.as_deref());
  stats.total_requests = persistent.total_requests;
  stats.successful_requests = persistent.total_success;
  stats.failed_requests = persistent.total_failed;
  stats.total_bytes_transferred = persistent.total_bytes_sent + persistent.total_bytes_received;
  stats.avg_response_ms = persistent.avg_response_ms();
  stats.today = persistent
    .periods
    .get(&stats::period_keys()[0])
    .cloned()
    .unwrap_or_default();
  stats.persistent = persistent;
}

#[utoipa::path(get, path = "/aperio/api/stats", tag = "dashboard",
  description = "Live statistics snapshot: counters, persistent stats, and the active client connections.",
  responses((status = 200, description = "Current statistics", body = EnhancedServerStats)))]
pub(crate) async fn stats_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
) -> Json<EnhancedServerStats> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let mut snapshot = compute_stats(&state).await;
  filter_stats_for_org(&mut snapshot, &org);
  scope_stats_for_org(&state, &mut snapshot, &org).await;
  Json(snapshot)
}

#[cfg(test)]
#[path = "numbers_tests.rs"]
mod tests;
