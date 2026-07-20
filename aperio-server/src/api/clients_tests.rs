//! Tests for the dashboard client/stats API: live stats snapshot, traffic
//! logs, uptime summary, traffic history, the SSE live stream, and the
//! per-client override / enable-disable handlers (including org isolation).

use super::*;
use crate::state::RequestLog;
use crate::store::uptime::Availability;
use crate::store::users::Role;
use crate::test_support::{
  admin_headers, cookie_headers, json_body, mock_client, seed_session, test_peer, test_state,
};
use axum::extract::{ConnectInfo, Path, Query, State};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Inserts a client handle under `id`, running `f` to tweak its fields first.
async fn insert_client(
  state: &AppState,
  id: &str,
  f: impl FnOnce(&mut crate::state::ClientHandle),
) {
  let mut handle = mock_client(Some("svc.example.com"), Some("/api"), None, None);
  f(&mut handle);
  state.clients.lock().await.insert(id.to_string(), handle);
}

fn log(id: &str, org: Option<&str>) -> RequestLog {
  RequestLog {
    id: id.to_string(),
    timestamp: "2026-07-20T00:00:00Z".to_string(),
    method: "GET".to_string(),
    uri: "/".to_string(),
    status: Some(200),
    duration_ms: 5,
    error: None,
    host: Some("svc.example.com".to_string()),
    org_id: org.map(|s| s.to_string()),
  }
}

// ---------------------------------------------------------------------------
// compute_stats / stats_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stats_snapshot_reports_active_clients_and_shared_instances() {
  let state = Arc::new(test_state());

  // Two clients sharing one reported instance id (flagged as shared) with a
  // declared hostname missing from the assigned set (so it gets appended),
  // a mismatched protocol, and non-zero bandwidth.
  insert_client(&state, "c1", |h| {
    h.reported_instance_id = Some("iid-1".to_string());
    h.declared_hostname = Some("extra.example.com".to_string());
    h.assigned_hostnames = vec!["assigned.example.com".to_string()];
    h.client_protocol = Some(crate::protocol::PROTOCOL_VERSION.wrapping_add(1));
    h.bandwidth_bps.store(1234, Ordering::Relaxed);
    h.request_count.store(7, Ordering::SeqCst);
    h.service_name = Some("svc".to_string());
  })
  .await;
  insert_client(&state, "c2", |h| {
    h.reported_instance_id = Some("iid-1".to_string());
    // declared hostname already present in the assigned set → not appended.
    h.declared_hostname = Some("dup.example.com".to_string());
    h.assigned_hostnames = vec!["dup.example.com".to_string()];
    // assigned_path used because declared_path is cleared.
    h.declared_path = None;
    h.assigned_path = Some("/assigned".to_string());
    h.bandwidth_bps.store(0, Ordering::Relaxed);
    h.client_protocol = None;
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = stats_handler(State(state.clone()), headers).await;
  let body = serde_json::to_value(&resp.0).unwrap();

  assert_eq!(body["connected_clients_count"], 2);
  let clients = body["active_clients"].as_array().unwrap();
  assert_eq!(clients.len(), 2);

  let c1 = clients.iter().find(|c| c["id"] == "c1").unwrap();
  assert_eq!(c1["instance_id_shared"], true);
  assert!(
    c1["hostname_binds"]
      .as_array()
      .unwrap()
      .iter()
      .any(|v| v == "extra.example.com"),
    "declared hostname appended"
  );
  assert_eq!(c1["protocol_mismatch"], true);
  assert_eq!(c1["bandwidth_bps"], 1234);
  assert_eq!(c1["request_count"], 7);

  let c2 = clients.iter().find(|c| c["id"] == "c2").unwrap();
  assert_eq!(c2["path_bind"], "/assigned");
  assert_eq!(c2["bandwidth_bps"], serde_json::Value::Null);
  assert_eq!(
    c2["hostname_binds"].as_array().unwrap().len(),
    1,
    "duplicate declared hostname not appended"
  );
}

#[tokio::test]
async fn stats_filtered_and_scoped_by_org() {
  let state = Arc::new(test_state());
  // A client belonging to another org must not appear for the master admin
  // (whose selected org is None).
  insert_client(&state, "other", |h| {
    h.perms.org_id = Some("acme".to_string());
  })
  .await;
  insert_client(&state, "mine", |h| {
    h.perms.org_id = None;
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = stats_handler(State(state.clone()), headers).await;
  let body = serde_json::to_value(&resp.0).unwrap();
  let clients = body["active_clients"].as_array().unwrap();
  assert_eq!(clients.len(), 1);
  assert_eq!(clients[0]["id"], "mine");
  assert_eq!(body["connected_clients_count"], 1);
}

// ---------------------------------------------------------------------------
// logs_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logs_filtered_by_effective_org() {
  let state = Arc::new(test_state());
  {
    let mut logs = state.recent_logs.lock().await;
    logs.push_back(log("a", None));
    logs.push_back(log("b", Some("acme")));
    logs.push_back(log("c", None));
  }
  let headers = admin_headers(&state).await;
  let resp = logs_handler(State(state.clone()), headers).await;
  let entries = resp.0;
  assert_eq!(entries.len(), 2, "only master-org logs visible to admin");
  assert!(entries.iter().all(|l| l.org_id.is_none()));
}

// ---------------------------------------------------------------------------
// uptime_handler / uptime_pct
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uptime_summary_scoped_to_org_with_percentages() {
  let state = Arc::new(test_state());
  let now = crate::store::sessions::now_secs();
  // Seed two entities via two ticks 100s apart: elapsed time accrues under the
  // "up" status into today's bucket, giving non-null percentages.
  {
    let mut up = state.uptime.lock().await;
    let mut live: HashMap<String, (Availability, Option<String>)> = HashMap::new();
    live.insert("mine".to_string(), (Availability::Up, None));
    live.insert(
      "theirs".to_string(),
      (Availability::Up, Some("acme".to_string())),
    );
    up.tick(now - 100, live.clone());
    up.tick(now, live);
  }

  let headers = admin_headers(&state).await;
  let resp = uptime_handler(State(state.clone()), headers).await;
  let entries = resp.0;
  assert_eq!(entries.len(), 1, "only the master-org entity is visible");
  let e = &entries[0];
  assert_eq!(e.name, "mine");
  assert_eq!(e.status, Availability::Up);
  assert!(e.pct_today.unwrap() > 99.0);
  assert!(e.pct_7d.is_some());
  assert!(e.pct_30d.is_some());
  assert!(!e.days.is_empty(), "today's bucket present");
}

#[tokio::test]
async fn uptime_pct_is_none_without_observations() {
  let state = Arc::new(test_state());
  // A single tick records status but accrues no elapsed time (no previous
  // tick), so there are no observed seconds and percentages are null.
  {
    let mut up = state.uptime.lock().await;
    let mut live: HashMap<String, (Availability, Option<String>)> = HashMap::new();
    live.insert("fresh".to_string(), (Availability::Up, None));
    up.tick(crate::store::sessions::now_secs(), live);
  }
  let headers = admin_headers(&state).await;
  let resp = uptime_handler(State(state.clone()), headers).await;
  let e = &resp.0[0];
  assert!(e.pct_today.is_none());
  assert!(e.days.is_empty());
}

// ---------------------------------------------------------------------------
// stats_history_handler
// ---------------------------------------------------------------------------

fn hquery(
  unit: Option<&str>,
  count: Option<usize>,
  from: Option<&str>,
  to: Option<&str>,
) -> HistoryQuery {
  HistoryQuery {
    unit: unit.map(|s| s.to_string()),
    count,
    from: from.map(|s| s.to_string()),
    to: to.map(|s| s.to_string()),
  }
}

async fn history(state: &Arc<AppState>, q: HistoryQuery) -> Response {
  let headers = admin_headers(state).await;
  stats_history_handler(State(state.clone()), headers, Query(q)).await
}

#[tokio::test]
async fn history_default_window_ok() {
  let state = Arc::new(test_state());
  let resp = history(&state, hquery(None, None, None, None)).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body.as_array().unwrap().len(), 30, "default 30 day buckets");
}

#[tokio::test]
async fn history_week_month_year_units_ok() {
  let state = Arc::new(test_state());
  for (unit, count) in [("week", 5usize), ("month", 3), ("year", 2)] {
    let resp = history(&state, hquery(Some(unit), Some(count), None, None)).await;
    assert_eq!(resp.status(), StatusCode::OK, "unit {unit}");
    let body = json_body(resp).await;
    assert_eq!(body.as_array().unwrap().len(), count);
  }
}

#[tokio::test]
async fn history_custom_range_ok() {
  let state = Arc::new(test_state());
  // Explicit from/to range.
  let resp = history(
    &state,
    hquery(None, None, Some("2026-07-01"), Some("2026-07-03")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await.as_array().unwrap().len(), 3);

  // from only → to defaults to today.
  let resp = history(&state, hquery(None, None, Some("2026-07-18"), None)).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn history_rejects_bad_unit_and_range() {
  let state = Arc::new(test_state());
  let resp = history(&state, hquery(Some("decade"), None, None, None)).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  let resp = history(
    &state,
    hquery(None, None, Some("2026-07-10"), Some("2026-07-01")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// client_override_handler
// ---------------------------------------------------------------------------

fn override_req(hostname: Option<&str>, path: Option<&str>) -> Json<ClientOverrideRequest> {
  Json(ClientOverrideRequest {
    hostname_bind: hostname.map(|s| s.to_string()),
    path_bind: path.map(|s| s.to_string()),
  })
}

#[tokio::test]
async fn override_unknown_client_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = client_override_handler(
    State(state.clone()),
    Path("nope".to_string()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some("h.example.com"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn override_set_then_clear() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let headers = admin_headers(&state).await;

  // Set both binds.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_req(Some("New.Example.com"), Some("api/v2")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  {
    let clients = state.clients.lock().await;
    let h = clients.get("c1").unwrap();
    assert_eq!(h.override_hostname_bind.as_deref(), Some("new.example.com"));
    assert_eq!(h.override_path_bind.as_deref(), Some("/api/v2"));
  }

  // Clear both (empty string and null).
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some(""), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  {
    let clients = state.clients.lock().await;
    let h = clients.get("c1").unwrap();
    assert!(h.override_hostname_bind.is_none());
    assert!(h.override_path_bind.is_none());
  }
}

#[tokio::test]
async fn override_rejects_invalid_values() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let headers = admin_headers(&state).await;

  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_req(Some("bad_host!"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
    override_req(None, Some("/foo/../bar")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn override_cross_org_client_is_404() {
  let state = Arc::new(test_state());
  // Client belongs to org "acme"; the master admin's effective org is None.
  insert_client(&state, "c1", |h| {
    h.perms.org_id = Some("acme".to_string());
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some("h.example.com"), None),
  )
  .await;
  assert_eq!(
    resp.status(),
    StatusCode::NOT_FOUND,
    "cross-org client hidden as 404"
  );
  // The override must not have been applied.
  let clients = state.clients.lock().await;
  assert!(clients.get("c1").unwrap().override_hostname_bind.is_none());
}

// ---------------------------------------------------------------------------
// client_enabled_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enabled_toggle_and_unknown() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| h.admin_enabled = true).await;
  let headers = admin_headers(&state).await;

  // Disable.
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(!state.clients.lock().await.get("c1").unwrap().admin_enabled);

  // Re-enable.
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(ClientEnabledRequest { enabled: true }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(state.clients.lock().await.get("c1").unwrap().admin_enabled);

  // Unknown client.
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("ghost".to_string()),
    ConnectInfo(test_peer()),
    headers,
    Json(ClientEnabledRequest { enabled: true }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn enabled_cross_org_client_is_404() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.perms.org_id = Some("acme".to_string());
    h.admin_enabled = true;
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
  // Untouched.
  assert!(state.clients.lock().await.get("c1").unwrap().admin_enabled);
}

// ---------------------------------------------------------------------------
// live_stream_handler (SSE)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_stream_emits_stats_traffic_and_ends_on_shutdown() {
  use axum::response::IntoResponse;
  use futures_util::StreamExt;
  use std::time::Duration;
  use tokio::time::timeout;

  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;

  // A viewer session scoped to the master org (None).
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let headers = cookie_headers(&token);

  let sse = live_stream_handler(State(state.clone()), headers).await;
  let resp = sse.into_response();
  let mut body = resp.into_body().into_data_stream();

  // First frame: the immediate `stats` event.
  let first = timeout(Duration::from_secs(2), body.next())
    .await
    .expect("stats frame in time")
    .expect("some frame")
    .expect("ok bytes");
  let text = String::from_utf8_lossy(&first);
  assert!(text.contains("event: stats"), "got: {text}");

  // Publish a mismatched-org log (skipped) then a matching one (streamed).
  let _ = state.traffic_tx.send(log("skip", Some("acme")));
  let _ = state.traffic_tx.send(log("keep", None));

  // Read frames until the matching traffic event arrives.
  let mut saw_traffic = false;
  for _ in 0..4 {
    let frame = timeout(Duration::from_secs(3), body.next())
      .await
      .expect("frame in time");
    let Some(Ok(bytes)) = frame else { break };
    if String::from_utf8_lossy(&bytes).contains("event: traffic") {
      saw_traffic = true;
      break;
    }
  }
  assert!(saw_traffic, "matching traffic event streamed");

  // Signal shutdown → the stream terminates.
  let _ = state.shutdown.send(true);
  let ended = timeout(Duration::from_secs(3), body.next())
    .await
    .expect("stream ends promptly");
  assert!(ended.is_none(), "stream closed on shutdown");
}
