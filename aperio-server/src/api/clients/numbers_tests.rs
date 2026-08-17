//! The numbers the dashboard renders, and the fence around every one of
//! them: the statistics snapshot, the uptime summary and its percentages, and
//! the history buckets, each scoped to the caller's organization.

use super::super::clients_tests::*;
use super::*;
use crate::store::uptime::Availability;
use crate::test_support::*;
use axum::extract::{Query, State};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn stats_snapshot_reports_active_clients_and_shared_instances() {
  let state = Arc::new(test_state());

  // Two clients sharing one reported instance id (flagged as shared) with a
  // declared hostname missing from the assigned set (so it gets appended),
  // a mismatched protocol, and non-zero bandwidth.
  insert_client(&state, "c1", |h| {
    h.reported_instance_id = Some("iid-1".to_string());
    h.sole_mut().declared_hostname = Some("extra.example.com".to_string());
    h.sole_mut().assigned_hostnames = vec!["assigned.example.com".to_string()];
    h.client_protocol = Some(crate::protocol::PROTOCOL_VERSION.wrapping_add(1));
    h.sole().bandwidth_bps.store(1234, Ordering::Relaxed);
    h.sole().request_count.store(7, Ordering::SeqCst);
    h.sole_mut().service_name = Some("svc".to_string());
  })
  .await;
  insert_client(&state, "c2", |h| {
    h.reported_instance_id = Some("iid-1".to_string());
    // declared hostname already present in the assigned set → not appended.
    h.sole_mut().declared_hostname = Some("dup.example.com".to_string());
    h.sole_mut().assigned_hostnames = vec!["dup.example.com".to_string()];
    // assigned_path used because declared_path is cleared.
    h.sole_mut().declared_path = None;
    h.sole_mut().assigned_path = Some("/assigned".to_string());
    h.sole().bandwidth_bps.store(0, Ordering::Relaxed);
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
async fn stats_lists_declared_hostnames_before_assigned_ones() {
  let state = Arc::new(test_state());

  // A multi-hostname client with a server-assigned random subdomain: every
  // declared name must be reported (not only the first), declared names lead
  // the bind list, and the random one is called out separately.
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames =
      vec!["app.example.com".to_string(), "www.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec!["wild-fox.tunnel.example.com".to_string()];
    h.sole_mut().random_hostname = Some("wild-fox.tunnel.example.com".to_string());
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = stats_handler(State(state.clone()), headers).await;
  let body = serde_json::to_value(&resp.0).unwrap();
  let c1 = body["active_clients"]
    .as_array()
    .unwrap()
    .iter()
    .find(|c| c["id"] == "c1")
    .unwrap();

  assert_eq!(
    c1["hostname_binds"],
    serde_json::json!([
      "app.example.com",
      "www.example.com",
      "wild-fox.tunnel.example.com"
    ]),
    "declared names lead, the assigned one trails"
  );
  assert_eq!(
    c1["declared_hostnames"],
    serde_json::json!(["app.example.com", "www.example.com"])
  );
  assert_eq!(c1["random_hostname"], "wild-fox.tunnel.example.com");
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
