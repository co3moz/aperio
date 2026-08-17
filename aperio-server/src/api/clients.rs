use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
  },
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::info;

use aperio_config::format_bandwidth;

use crate::protocol::PROTOCOL_VERSION;
use crate::routing::{extract_client_ip, normalize_hostname_bind, normalize_path_bind};
use crate::state::{AppState, ClientDetail, EnhancedServerStats, RequestLog};
use crate::store::stats::{self};

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
fn declared_hostnames_of(service: &crate::state::ServiceState) -> Vec<String> {
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
fn uptime_pct(
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
async fn scope_stats_for_org(
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

/// Handler returning recent HTTP logs in JSON, optionally filtered.
///
/// The window is the server's recent-request ring, so this answers "what has
/// been happening" rather than "what happened last Tuesday"; the durable
/// record is the access log file (`APERIO_ACCESS_LOG`). The filters exist so
/// automation does not have to fetch the whole ring and re-implement the
/// matching the dashboard already does client-side.
#[utoipa::path(get, path = "/aperio/api/logs", tag = "dashboard",
  description = "Recent proxied requests (bounded ring buffer), optionally filtered by status, method and path.",
  params(
    ("status" = Option<String>, Query, description = "Exact code (404) or class (4xx, 5xx); a failed request with no status counts as 5xx"),
    ("method" = Option<String>, Query, description = "HTTP method, case-insensitive"),
    ("path" = Option<String>, Query, description = "Case-insensitive substring of the request URI"),
    ("limit" = Option<usize>, Query, description = "Maximum entries, newest first (omit for the whole ring, oldest first)")),
  responses((status = 200, description = "Request log entries", body = Vec<RequestLog>)))]
pub(crate) async fn logs_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
  axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<RequestLog>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let non_empty = |key: &str| {
    params
      .get(key)
      .map(|v| v.trim().to_string())
      .filter(|v| !v.is_empty())
  };
  // `status` takes an exact code (`404`) or a class (`4xx`, `5xx`), because
  // both are what somebody actually asks for. A failed request with no status
  // counts as 5xx, matching how the dashboard buckets it.
  let status = non_empty("status").map(|v| v.to_ascii_lowercase());
  let method = non_empty("method").map(|v| v.to_ascii_uppercase());
  let path = non_empty("path").map(|v| v.to_lowercase());
  let limit = params
    .get("limit")
    .and_then(|v| v.parse::<usize>().ok())
    .unwrap_or(usize::MAX);

  let logs = state.recent_logs.lock().await;
  let mut matched: Vec<RequestLog> = logs
    .iter()
    // Only requests served by a client in the caller's effective org. This
    // runs first and is not one of the predicates: isolation is not something
    // a query parameter gets to widen.
    .filter(|l| l.org_id == org)
    .filter(|l| match status {
      None => true,
      Some(ref want) => {
        let effective = if l.error.is_some() {
          500
        } else {
          l.status.unwrap_or(500)
        };
        match want.strip_suffix("xx").and_then(|d| d.parse::<u16>().ok()) {
          Some(class) => effective / 100 == class,
          None => want.parse::<u16>().ok() == Some(effective),
        }
      }
    })
    .filter(|l| match method {
      None => true,
      Some(ref want) => l.method.eq_ignore_ascii_case(want),
    })
    .filter(|l| match path {
      None => true,
      Some(ref want) => l.uri.to_lowercase().contains(want),
    })
    .cloned()
    .collect();
  // Newest first when a limit is given, so a capped query returns the most
  // recent matches rather than the oldest ones. The unfiltered call keeps its
  // original oldest-first order, which the dashboard's live view relies on.
  if limit != usize::MAX {
    matched.reverse();
    matched.truncate(limit);
  }
  Json(matched)
}

/// Server-Sent Events stream powering the dashboard's live view, so it doesn't
/// poll: named `traffic` events (one per proxied request, as it completes),
/// periodic `stats` events (the same snapshot as `/api/stats`, pushed every 2s
/// and once immediately on connect), and `notification` events (every server
/// event that also feeds webhooks, for the notification bell). A subscriber
/// that falls behind either buffer skips the lagged span rather than closing
/// the stream.
#[utoipa::path(get, path = "/aperio/api/stream", tag = "dashboard",
  description = "Server-Sent Events stream: named `traffic` events (one per proxied request), periodic `stats` events, and `notification` events (server events, as webhooks receive them).",
  responses((status = 200, description = "SSE stream (text/event-stream)")))]
pub(crate) async fn live_stream_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
  use std::time::Duration;
  use tokio::sync::broadcast::error::RecvError;
  use tokio::time::MissedTickBehavior;

  // The caller's effective org is fixed for the life of the connection.
  let org = crate::auth::effective_org(&state, &headers).await;
  let rx = state.traffic_tx.subscribe();
  let events = state.events_tx.subscribe();
  let shutdown = state.shutdown.subscribe();
  let mut interval = tokio::time::interval(Duration::from_secs(2));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  let stream = futures_util::stream::unfold(
    (state, rx, events, interval, shutdown, org, headers),
    |(state, mut rx, mut events, mut interval, mut shutdown, org, headers)| async move {
      loop {
        tokio::select! {
          // The first tick fires immediately, seeding the initial snapshot.
          _ = interval.tick() => {
            // The session middleware runs once, when the stream is opened,
            // and this connection then lives for hours. Signing out, "sign
            // out everywhere", an expiring session or a disabled user would
            // all leave it emitting traffic and statistics to a caller who no
            // longer has a session. Re-checking on each tick bounds that to
            // one tick, and costs one read of the session store every two
            // seconds per open stream.
            crate::auth::dashboard_role(&state, &headers).await?;
            let mut snapshot = compute_stats(&state).await;
            filter_stats_for_org(&mut snapshot, &org);
            scope_stats_for_org(&state, &mut snapshot, &org).await;
            let event = Event::default()
              .event("stats")
              .json_data(&snapshot)
              .unwrap_or_else(|_| Event::default());
            return Some((Ok(event), (state, rx, events, interval, shutdown, org, headers)));
          }
          recv = rx.recv() => match recv {
            Ok(log) => {
              // Only stream traffic served by a client in the subscriber's org.
              if log.org_id != org {
                continue;
              }
              let event = Event::default()
                .event("traffic")
                .json_data(&log)
                .unwrap_or_else(|_| Event::default());
              return Some((Ok(event), (state, rx, events, interval, shutdown, org, headers)));
            }
            // Slow subscriber: drop the missed span and keep streaming.
            Err(RecvError::Lagged(_)) => continue,
            // Sender gone: end the stream.
            Err(RecvError::Closed) => return None,
          },
          recv = events.recv() => match recv {
            Ok(ev) => {
              // Same fence as traffic: an event belongs to one organization
              // and is only ever seen by dashboards of that organization.
              if ev.org != org {
                continue;
              }
              let event = Event::default()
                .event("notification")
                .json_data(&ev)
                .unwrap_or_else(|_| Event::default());
              return Some((Ok(event), (state, rx, events, interval, shutdown, org, headers)));
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return None,
          },
          // Server shutting down: end the stream so graceful shutdown can
          // complete (an open SSE connection would otherwise hold it forever).
          _ = shutdown.changed() => {
            if *shutdown.borrow() {
              return None;
            }
          }
        }
      }
    },
  );
  Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Request payload for the dashboard client override (overrule) endpoint.
/// Each field fully replaces the corresponding override: a non-empty value
/// sets it, an empty string/list or `null` clears it. Overrides are in-memory
/// only and disappear when the client reconnects or the server restarts.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ClientOverrideRequest {
  /// Single hostname to route this connection on, replacing every declared and
  /// assigned name. Superseded by `hostname_binds` when both are present.
  pub(crate) hostname_bind: Option<String>,
  /// Hostnames to route this connection on, replacing every declared and
  /// assigned name. Lets an operator retarget one of a client's names while
  /// keeping the others (blank entries are dropped).
  pub(crate) hostname_binds: Option<Vec<String>>,
  pub(crate) path_bind: Option<String>,
}

/// Applies a temporary hostname/path bind override to a connected client.
/// Protected by the dashboard session middleware.
#[utoipa::path(post, path = "/aperio/api/clients/{id}/override", tag = "dashboard",
  description = "Temporarily overrule a client's hostname/path bind server-side (empty values clear the override).",
  params(("id" = String, Path, description = "Client connection id")),
  request_body = ClientOverrideRequest,
  responses((status = 200, description = "Override applied"), (status = 404, description = "No such client")))]
pub(crate) async fn client_override_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  axum::extract::Query(which): axum::extract::Query<ServiceQuery>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientOverrideRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Validate before mutating: reject invalid values with 400. `hostname_binds`
  // wins when both forms are sent; the singular form stays accepted so older
  // callers (and `aperio-client api client override`) keep working.
  let raw_hostnames: Vec<String> = match payload.hostname_binds {
    Some(ref list) => list.clone(),
    None => payload.hostname_bind.clone().into_iter().collect(),
  };
  let mut new_hostnames: Vec<String> = Vec::new();
  for raw in raw_hostnames.iter().filter(|r| !r.trim().is_empty()) {
    match normalize_hostname_bind(raw) {
      Some(h) => {
        if !new_hostnames.contains(&h) {
          new_hostnames.push(h);
        }
      }
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid hostname_bind value").into_response();
      }
    }
  }
  let new_path = match payload.path_bind.as_deref() {
    None | Some("") => None,
    Some(raw) => match normalize_path_bind(raw) {
      Some(p) => Some(p),
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid path_bind value").into_response();
      }
    },
  };

  // Org isolation: a caller may only overrule a client of their effective org.
  // A cross-org (or unknown) client is indistinguishable, both 404, so a
  // client's existence never leaks across orgs.
  let org = crate::auth::effective_org(&state, &headers).await;
  // Organization fence: an overrule is the one place a bind is set without a
  // token permission behind it, so a fenced org must not be able to point one
  // of its clients at a hostname it does not own.
  if !new_hostnames.is_empty() {
    let allowlist = state.org_store.lock().await.hostnames_of(org.as_deref());
    for host in &new_hostnames {
      if !crate::store::orgs::hostname_in_org_allowlist(host, &allowlist) {
        return (
          StatusCode::FORBIDDEN,
          format!(
            "hostname {} is outside this organization's allowlist ({})",
            host,
            allowlist.join(", ")
          ),
        )
          .into_response();
      }
    }
  }
  let found = {
    let mut clients = state.clients.write().await;
    match clients.get_mut(&client_id) {
      Some(handle) if handle.perms.org_id == org => {
        let Some(service) = handle.services.get_mut(which.service) else {
          return (StatusCode::NOT_FOUND, "No such service on this client").into_response();
        };
        service.override_hostname_binds = new_hostnames.clone();
        service.override_path_bind = new_path.clone();
        true
      }
      _ => false,
    }
  };
  if found {
    info!(
      "Dashboard overrule applied to client {}: hostname_bind={:?} path_bind={:?}",
      client_id, new_hostnames, new_path
    );
    state
      .audit_session(
        "client_overrule",
        &headers,
        &actor_ip,
        &format!(
          "client={} hostname={:?} path={:?}",
          client_id, new_hostnames, new_path
        ),
      )
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

/// One setting whose effective value differs from what was configured.
#[derive(serde::Serialize, utoipa::ToSchema, Clone)]
pub(crate) struct ConfigNoteView {
  /// Config key the note is about (`bandwidth`, `connections`, `cache`, …).
  pub(crate) field: String,
  /// What was configured, as written (empty = nothing was configured).
  pub(crate) declared: String,
  /// What is actually in effect.
  pub(crate) effective: String,
  /// Why the two differ, one sentence.
  pub(crate) reason: String,
  /// `client` = the client resolved it before announcing it (only the client
  /// knows both sides, so it reports these itself); `server` = it was
  /// announced as configured but the server is not honoring it.
  pub(crate) source: String,
}

/// A connection's effective configuration for the dashboard's config view.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ClientConfigView {
  /// The effective configuration as a YAML document, with each adjusted
  /// setting carrying its note as a trailing comment.
  pub(crate) yaml: String,
  /// Every setting whose effective value differs from what was configured.
  pub(crate) notes: Vec<ConfigNoteView>,
}

/// Quotes a value as a YAML double-quoted scalar.
fn yaml_str(v: &str) -> String {
  format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Appends `key: value`, with the note for that key (when there is one) as a
/// trailing comment, so the document still explains itself once copied out.
fn push_line(out: &mut String, notes: &[ConfigNoteView], key: &str, value: &str) {
  out.push_str(key);
  out.push_str(": ");
  out.push_str(value);
  if let Some(note) = notes.iter().find(|n| n.field == key) {
    let declared = if note.declared.is_empty() {
      "not set".to_string()
    } else {
      note.declared.clone()
    };
    out.push_str(&format!("  # declared {}: {}", declared, note.reason));
  }
  out.push('\n');
}

/// Builds the effective configuration of one connection: what the client
/// announced over its heartbeat, plus what the server applies on top.
///
/// This is deliberately not the client's `aperio.yaml`. Settings the client
/// never announces (its target, request timeouts, header rules, health probe)
/// are not knowable here and are left out rather than guessed at, which the
/// document's own header says.
fn client_config_view(
  id: &str,
  handle: &crate::state::ClientHandle,
  service: &crate::state::ServiceState,
  cache_enabled: bool,
) -> ClientConfigView {
  // What the client resolved differently before announcing it: only it knows
  // both sides, so it reports these in its Ping.
  let mut notes: Vec<ConfigNoteView> = service
    .config_notes
    .iter()
    .map(|n| ConfigNoteView {
      field: n.field.clone(),
      declared: n.declared.clone(),
      effective: n.effective.clone(),
      reason: n.reason.clone(),
      source: "client".to_string(),
    })
    .collect();

  // What the server is not honoring, though the client announced it as
  // configured. Each of these already logs a warning server-side; the point
  // here is that an operator should not have to find that line.
  let mut server_note = |field: &str, declared: &str, effective: &str, reason: &str| {
    notes.push(ConfigNoteView {
      field: field.to_string(),
      declared: declared.to_string(),
      effective: effective.to_string(),
      reason: reason.to_string(),
      source: "server".to_string(),
    })
  };
  if service.cache && !cache_enabled {
    server_note(
      "cache",
      "true",
      "false",
      "the server's response cache is disabled (APERIO_CACHE off), so the opt-in does nothing",
    );
  }
  if service.public_denied_warned {
    server_note(
      "public",
      "true",
      "false",
      "this client's token does not permit publishing public services, so the visitor auth gate stays on",
    );
  }
  if service.visitor_auth_denied_warned {
    server_note(
      "auth",
      "set",
      "ignored",
      "the token does not permit controlling the visitor gate, the value was not user:password, or the server sets APERIO_IGNORE_CLIENT_AUTH",
    );
  }
  let declared_hostnames = declared_hostnames_of(service);
  if !service.override_hostname_binds.is_empty() {
    let mut before = declared_hostnames.clone();
    for h in &service.assigned_hostnames {
      if !before.contains(h) {
        before.push(h.clone());
      }
    }
    server_note(
      "hostname",
      &before.join(", "),
      &service.override_hostname_binds.join(", "),
      "a dashboard overrule is in effect; it is in-memory only and reverts when the client reconnects",
    );
  }
  if let Some(path) = &service.override_path_bind {
    server_note(
      "path",
      service
        .declared_path
        .as_deref()
        .or(service.assigned_path.as_deref())
        .unwrap_or(""),
      path,
      "a dashboard overrule is in effect; it is in-memory only and reverts when the client reconnects",
    );
  }

  let mut y = String::new();
  y.push_str(&format!(
    "# Effective configuration of connection {}. Settings a client never\n\
     # announces (target, timeouts, header rules, health probes) are not shown.\n",
    id
  ));
  if let Some(name) = &service.service_name {
    push_line(&mut y, &notes, "name", &yaml_str(name));
  }
  if let Some(iid) = &handle.reported_instance_id {
    push_line(&mut y, &notes, "client_id", &yaml_str(iid));
  }
  // An elastic pool is written as the range the file wrote, with the size it
  // is running right now beside it. Printing only the current size read as a
  // fixed `connections: 3` next to four live connections, because the number
  // was a snapshot of a pool that had since grown.
  match (service.connections_min, service.connections_max) {
    (Some(min), Some(max)) => {
      let open = service
        .connections
        .map(|n| format!("  # {n} open right now"))
        .unwrap_or_default();
      push_line(
        &mut y,
        &notes,
        "connections",
        &format!("{{ min: {min}, max: {max} }}{open}"),
      );
    }
    _ => {
      if let Some(n) = service.connections {
        push_line(&mut y, &notes, "connections", &n.to_string());
      }
    }
  }

  // Hostnames, each labeled with where it came from; an active overrule
  // replaces the set, exactly as routing does.
  let effective_hosts: Vec<String> = service.effective_hostnames().into_iter().cloned().collect();
  if effective_hosts.is_empty() {
    push_line(&mut y, &notes, "hostname", "[]");
  } else {
    push_line(&mut y, &notes, "hostname", "");
    // `hostname:` above ends with the note comment, so the list follows it.
    for host in &effective_hosts {
      let origin = if !service.override_hostname_binds.is_empty() {
        "dashboard overrule"
      } else if declared_hostnames.contains(host) {
        "requested by the client"
      } else if service.random_hostname.as_deref() == Some(host.as_str()) {
        "random subdomain, assigned by the server"
      } else {
        "granted by the token"
      };
      y.push_str(&format!("  - {}  # {}\n", yaml_str(host), origin));
    }
  }
  // This service's bind, like every other line in this view. Read off the
  // connection it showed the first service's path for all of them, in the one
  // place an operator goes to check what a service is actually configured as.
  if let Some(path) = service.effective_path_bind() {
    push_line(&mut y, &notes, "path", &yaml_str(path));
  }
  if let Some(n) = service.max_concurrent {
    push_line(&mut y, &notes, "max_concurrent", &n.to_string());
  }
  match service.bandwidth_bps.load(Ordering::Relaxed) {
    0 => {}
    bps => push_line(
      &mut y,
      &notes,
      "bandwidth",
      &yaml_str(&format_bandwidth(bps)),
    ),
  }
  if service.priority > 0 {
    push_line(&mut y, &notes, "priority", &service.priority.to_string());
  }
  if service.public {
    push_line(&mut y, &notes, "public", "true");
  }
  if service.visitor_auth.is_some() {
    // The credentials themselves never leave the server.
    y.push_str("auth: \"<set by the client>\"\n");
  }
  if !service.allowed_ips.is_empty() {
    push_line(
      &mut y,
      &notes,
      "allowed_ips",
      &format!(
        "[{}]",
        service
          .allowed_ips
          .iter()
          .map(|ip| yaml_str(ip))
          .collect::<Vec<_>>()
          .join(", ")
      ),
    );
  }
  if let Some(denied) = &service.denied {
    push_line(&mut y, &notes, "denied", &yaml_str(denied));
  }
  if service.cache {
    push_line(&mut y, &notes, "cache", "true");
  }
  if service.resilience {
    push_line(&mut y, &notes, "resilience", "true");
  }
  if service.webhook_inbox {
    push_line(&mut y, &notes, "webhook_inbox", "true");
  }
  if let Some(n) = service.max_request_body {
    push_line(&mut y, &notes, "max_request_body", &n.to_string());
  }
  if let Some(n) = service.response_timeout {
    push_line(&mut y, &notes, "response_timeout", &n.to_string());
  }
  if service.tcp_enabled {
    y.push_str("tcp_target: \"<set by the client>\"\n");
  }
  if !service.tunnels.is_empty() {
    y.push_str("tunnels:\n");
    for t in &service.tunnels {
      y.push_str(&format!(
        "  - target: {}\n    protocol: {}\n",
        yaml_str(&t.target),
        yaml_str(&t.protocol)
      ));
      if t.encrypt {
        y.push_str("    encrypt: true\n");
      }
    }
  }

  ClientConfigView { yaml: y, notes }
}

/// Returns the effective configuration of one connected client.
#[utoipa::path(get, path = "/aperio/api/clients/{id}/config", tag = "dashboard",
  description = "Effective configuration of one connection as a YAML document, plus every setting whose effective value differs from what was configured (a bandwidth budget divided across parallel connections, a cache opt-in the server ignores, an active overrule).",
  params(("id" = String, Path, description = "Client connection id")),
  responses((status = 200, description = "Effective configuration", body = ClientConfigView),
    (status = 404, description = "No such client")))]
pub(crate) async fn client_config_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  axum::extract::Query(which): axum::extract::Query<ServiceQuery>,
  headers: HeaderMap,
) -> Response {
  // Org isolation, as everywhere else: a cross-org client is a 404, so its
  // existence never leaks.
  let org = crate::auth::effective_org(&state, &headers).await;
  let cache_enabled = state.config().cache_enabled;
  let clients = state.clients.read().await;
  match clients.get(&client_id) {
    Some(handle) if handle.perms.org_id == org => {
      // An index past the end is a service this connection does not carry,
      // which is a 404 for the same reason an unknown id is: the answer to
      // "show me that" is that there is no that.
      match handle.services.get(which.service) {
        Some(service) => Json(client_config_view(
          &client_id,
          handle,
          service,
          cache_enabled,
        ))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "No such service on this client").into_response(),
      }
    }
    _ => (StatusCode::NOT_FOUND, "Client not found").into_response(),
  }
}

/// Payload for the client enable/disable toggle.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ClientEnabledRequest {
  pub(crate) enabled: bool,
}

/// Dashboard kill switch: temporarily removes a connected client from the
/// routing pool (or puts it back). In-flight requests always complete.
#[utoipa::path(post, path = "/aperio/api/clients/{id}/enabled", tag = "dashboard",
  description = "Kill switch: enable/disable routing to one client without dropping its tunnel.",
  params(("id" = String, Path, description = "Client connection id")),
  request_body = ClientEnabledRequest,
  responses((status = 200, description = "State changed"), (status = 404, description = "No such client")))]
pub(crate) async fn client_enabled_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  axum::extract::Query(which): axum::extract::Query<ServiceQuery>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientEnabledRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Org isolation: a caller may only enable/disable a client of their org.
  let org = crate::auth::effective_org(&state, &headers).await;
  let found = {
    let mut clients = state.clients.write().await;
    match clients.get_mut(&client_id) {
      Some(handle) if handle.perms.org_id == org => {
        let Some(service) = handle.services.get_mut(which.service) else {
          return (StatusCode::NOT_FOUND, "No such service on this client").into_response();
        };
        service.admin_enabled = payload.enabled;
        true
      }
      _ => false,
    }
  };
  if found {
    info!(
      "Client {} {} via dashboard",
      client_id,
      if payload.enabled {
        "enabled"
      } else {
        "disabled"
      }
    );
    state
      .audit_session(
        if payload.enabled {
          "client_enabled"
        } else {
          "client_disabled"
        },
        &headers,
        &actor_ip,
        &format!("client={}", client_id),
      )
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

#[cfg(test)]
#[path = "clients_tests.rs"]
mod tests;
