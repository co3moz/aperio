//! The two views that keep arriving after the request: the request log, and
//! the server-sent event stream the dashboard holds open.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
///
/// `?topics=a,b` (or the bare `?a&b`) adds the documents the pages used to
/// poll for (`planned_features.md` #167): each named topic is sent as an
/// event of the same name, carrying exactly what its endpoint answers this
/// caller, on connect and then whenever the store behind it changes or on
/// its own beat of the two-second tick, whichever the topic is. A topic the
/// caller may not read is refused here, by name, rather than silently left
/// out: a page that asked for a list and never received one would show an
/// empty table as if that were the answer.
#[utoipa::path(get, path = "/aperio/api/stream", tag = "dashboard",
  description = "Server-Sent Events stream: named `traffic` events (one per proxied request), periodic `stats` events, `notification` events (server events, as webhooks receive them), and, for every topic named in `?topics=a,b` (or `?a&b`), that endpoint's document as an event of the same name, sent on connect and again whenever it changes: tokens, users, sessions, admin_keys, webhooks, deliveries, inbox, maintenance, scaling, orgs, subscribers, topology, stage_stats, uptime, route_trends, slow_endpoints, tunnels, audit, cache_stats, self_health, session.",
  params(("topics" = Option<String>, Query, description = "Comma-separated topics to add; the bare `?tokens&uptime` spelling works too")),
  responses((status = 200, description = "SSE stream (text/event-stream)"), (status = 400, description = "An unknown topic, named"), (status = 403, description = "A topic the caller's role may not read, named")))]
pub(crate) async fn live_stream_handler(
  State(state): State<Arc<AppState>>,
  headers: axum::http::HeaderMap,
  axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
  use super::topics::{Cadence, Change, Topic};
  use std::collections::{BTreeSet, VecDeque};
  use std::time::Duration;
  use tokio::sync::broadcast::error::RecvError;
  use tokio::time::{Instant, MissedTickBehavior};

  let topics = match super::topics::topics_from_query(&params) {
    Ok(t) => t,
    Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
  };
  // Refused at the upgrade, with the topic named. The role floor is the
  // endpoint's own, and the super-admin's topics ask the same question the
  // handler would.
  if !topics.is_empty() {
    let Some(role) = crate::auth::dashboard_role(&state, &headers).await else {
      return (
        StatusCode::UNAUTHORIZED,
        "a dashboard session or admin key is required",
      )
        .into_response();
    };
    for t in &topics {
      if role < t.required_role() {
        return (
          StatusCode::FORBIDDEN,
          format!(
            "stream topic `{}` requires the {} role (you are {})",
            t.name(),
            t.required_role().as_str(),
            role.as_str()
          ),
        )
          .into_response();
      }
      if t.master_only()
        && crate::auth::require_master_admin(&state, &headers)
          .await
          .is_err()
      {
        return (
          StatusCode::FORBIDDEN,
          format!("stream topic `{}` is the master super-admin's", t.name()),
        )
          .into_response();
      }
    }
  }

  /// Everything one open stream holds.
  struct Live {
    state: Arc<AppState>,
    rx: tokio::sync::broadcast::Receiver<RequestLog>,
    events: tokio::sync::broadcast::Receiver<crate::state::ServerEvent>,
    changes: tokio::sync::broadcast::Receiver<Change>,
    interval: tokio::time::Interval,
    shutdown: tokio::sync::watch::Receiver<bool>,
    org: Option<String>,
    headers: axum::http::HeaderMap,
    topics: Vec<Topic>,
    /// Ticks so far; the connect tick is 0 and sends every topic once.
    tick: u64,
    /// Topics a change touched, waiting for the short coalescing window.
    dirty: BTreeSet<Topic>,
    /// When the window closes, if one is open.
    flush_at: Option<Instant>,
    /// Topics to send next, one per frame.
    pending: VecDeque<Topic>,
  }

  /// The coalescing window: a burst of changes (an import touching every
  /// store) sends each topic once, a quarter second after the first.
  async fn window(at: Option<Instant>) {
    match at {
      Some(at) => tokio::time::sleep_until(at).await,
      None => std::future::pending::<()>().await,
    }
  }

  // The caller's effective org is fixed for the life of the connection.
  let org = crate::auth::effective_org(&state, &headers).await;
  let mut interval = tokio::time::interval(Duration::from_secs(2));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let live = Live {
    rx: state.traffic_tx.subscribe(),
    events: state.events_tx.subscribe(),
    changes: state.changes_tx.subscribe(),
    shutdown: state.shutdown.subscribe(),
    state,
    interval,
    org,
    headers,
    topics,
    tick: 0,
    dirty: BTreeSet::new(),
    flush_at: None,
    pending: VecDeque::new(),
  };

  let stream = futures_util::stream::unfold(live, |mut live| async move {
    loop {
      // A topic document waiting to go out is sent before anything else is
      // waited for; one frame per turn, so a queue of several drains over
      // consecutive turns.
      if let Some(topic) = live.pending.pop_front() {
        let Some(doc) = topic.payload(&live.state, &live.headers).await else {
          continue;
        };
        let event = Event::default()
          .event(topic.name())
          .json_data(&doc)
          .unwrap_or_else(|_| Event::default());
        return Some((Ok::<Event, std::convert::Infallible>(event), live));
      }
      tokio::select! {
        // The first tick fires immediately, seeding the initial snapshot.
        _ = live.interval.tick() => {
          // The session middleware runs once, when the stream is opened,
          // and this connection then lives for hours. Signing out, "sign
          // out everywhere", an expiring session or a disabled user would
          // all leave it emitting traffic and statistics to a caller who no
          // longer has a session. Re-checking on each tick bounds that to
          // one tick, and costs one read of the session store every two
          // seconds per open stream.
          crate::auth::dashboard_role(&live.state, &live.headers).await?;
          let tick = live.tick;
          live.tick += 1;
          for t in &live.topics {
            if t.due_on(tick) {
              live.pending.push_back(*t);
            }
          }
          let mut snapshot = compute_stats(&live.state).await;
          filter_stats_for_org(&mut snapshot, &live.org);
          scope_stats_for_org(&live.state, &mut snapshot, &live.org).await;
          let event = Event::default()
            .event("stats")
            .json_data(&snapshot)
            .unwrap_or_else(|_| Event::default());
          return Some((Ok(event), live));
        }
        recv = live.rx.recv() => match recv {
          Ok(log) => {
            // Only stream traffic served by a client in the subscriber's org.
            if log.org_id != live.org {
              continue;
            }
            let event = Event::default()
              .event("traffic")
              .json_data(&log)
              .unwrap_or_else(|_| Event::default());
            return Some((Ok(event), live));
          }
          // Slow subscriber: drop the missed span and keep streaming.
          Err(RecvError::Lagged(_)) => continue,
          // Sender gone: end the stream.
          Err(RecvError::Closed) => return None,
        },
        recv = live.events.recv() => match recv {
          Ok(ev) => {
            // Same fence as traffic: an event belongs to one organization
            // and is only ever seen by dashboards of that organization.
            if ev.org != live.org {
              continue;
            }
            let event = Event::default()
              .event("notification")
              .json_data(&ev)
              .unwrap_or_else(|_| Event::default());
            return Some((Ok(event), live));
          }
          Err(RecvError::Lagged(_)) => continue,
          Err(RecvError::Closed) => return None,
        },
        recv = live.changes.recv() => match recv {
          Ok(change) => {
            // The same fence again, unless the change is everyone's (the
            // organization list itself).
            if !(change.everyone || change.org == live.org) {
              continue;
            }
            if live.topics.contains(&change.topic) {
              live.dirty.insert(change.topic);
            }
            // The caller's own session is what its users and sessions rows
            // decide, so it is re-sent when either of those moved.
            if matches!(change.topic, Topic::Users | Topic::Sessions)
              && live.topics.contains(&Topic::Session)
            {
              live.dirty.insert(Topic::Session);
            }
            if !live.dirty.is_empty() && live.flush_at.is_none() {
              live.flush_at = Some(Instant::now() + Duration::from_millis(250));
            }
            continue;
          }
          // Missed a span of changes: whatever they touched, every
          // change-driven topic is sent again rather than guessed at.
          Err(RecvError::Lagged(_)) => {
            for t in &live.topics {
              if matches!(t.cadence(), Cadence::OnChange | Cadence::Both(_)) {
                live.dirty.insert(*t);
              }
            }
            if live.flush_at.is_none() {
              live.flush_at = Some(Instant::now() + Duration::from_millis(250));
            }
            continue;
          }
          Err(RecvError::Closed) => return None,
        },
        _ = window(live.flush_at) => {
          live.flush_at = None;
          let dirty = std::mem::take(&mut live.dirty);
          live.pending.extend(dirty);
          continue;
        }
        // Server shutting down: end the stream so graceful shutdown can
        // complete (an open SSE connection would otherwise hold it forever).
        _ = live.shutdown.changed() => {
          if *live.shutdown.borrow() {
            return None;
          }
        }
      }
    }
  });
  Sse::new(stream)
    .keep_alive(KeepAlive::default())
    .into_response()
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
