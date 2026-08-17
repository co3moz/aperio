//! The two views that keep arriving after the request: the request log, and
//! the server-sent event stream the dashboard holds open.

use axum::extract::State;
use std::sync::Arc;

use super::numbers::scope_stats_for_org;
use super::*;
use crate::state::AppState;

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

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
