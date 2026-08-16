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
  state.clients.write().await.insert(id.to_string(), handle);
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
  let resp = logs_handler(
    State(state.clone()),
    headers,
    axum::extract::Query(Default::default()),
  )
  .await;
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
// client_config_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_elastic_pool_is_rendered_as_its_range_and_its_current_size() {
  // The count is what the pool has open right now, and on its own it read as
  // a fixed setting: a dashboard showing `connections: 3` beside four live
  // connections, because 3 was the size the pool happened to be when that
  // connection announced itself. The range is what says the number moves.
  let state = Arc::new(test_state());
  insert_client(&state, "pool", |h| {
    h.sole_mut().service_name = Some("axum".to_string());
    h.sole_mut().connections = Some(4);
    h.sole_mut().connections_min = Some(1);
    h.sole_mut().connections_max = Some(5);
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_config_handler(State(state.clone()), Path("pool".to_string()), headers).await;
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();
  assert!(
    yaml.contains("connections: { min: 1, max: 5 }  # 4 open right now"),
    "got:\n{yaml}"
  );

  // A fixed pool announces no range and is written the way the file wrote it.
  insert_client(&state, "fixed", |h| {
    h.sole_mut().service_name = Some("api".to_string());
    h.sole_mut().connections = Some(3);
  })
  .await;
  let headers = admin_headers(&state).await;
  let resp = client_config_handler(State(state.clone()), Path("fixed".to_string()), headers).await;
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();
  assert!(yaml.contains("connections: 3"), "got:\n{yaml}");
  assert!(!yaml.contains("min:"), "got:\n{yaml}");
}

#[tokio::test]
async fn client_config_renders_yaml_with_declared_vs_effective_notes() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().service_name = Some("api".to_string());
    h.reported_instance_id = Some("my-box-0".to_string());
    h.sole_mut().connections = Some(10);
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec![
      "app.example.com".to_string(),
      "wild-fox.example.com".to_string(),
    ];
    h.sole_mut().random_hostname = Some("wild-fox.example.com".to_string());
    h.sole_mut().max_concurrent = Some(32);
    h.sole().bandwidth_bps.store(125_000, Ordering::Relaxed);
    // Opted into caching while the test server has its cache disabled.
    h.sole_mut().cache = true;
    // What the client itself resolved differently before announcing it.
    h.sole_mut().config_notes = vec![crate::protocol::ConfigNote {
      field: "bandwidth".to_string(),
      declared: "10mbit".to_string(),
      effective: "1mbit".to_string(),
      reason: "split across 10 parallel connections".to_string(),
    }];
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(State(state.clone()), Path("c1".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();

  assert!(yaml.contains("name: \"api\""), "got:\n{yaml}");
  assert!(yaml.contains("connections: 10"), "got:\n{yaml}");
  assert!(
    yaml.contains("  - \"app.example.com\"  # requested by the client"),
    "each hostname is labeled with where it came from:\n{yaml}"
  );
  assert!(
    yaml.contains("  - \"wild-fox.example.com\"  # random subdomain, assigned by the server"),
    "got:\n{yaml}"
  );
  // The client-reported difference rides along as a trailing comment.
  assert!(
    yaml.contains("bandwidth: \"1mbit\"  # declared 10mbit: split across 10 parallel connections"),
    "got:\n{yaml}"
  );
  assert!(
    yaml.contains("cache: true  # declared true: the server's response cache is disabled"),
    "a server-side adjustment is annotated too:\n{yaml}"
  );

  let notes = body["notes"].as_array().unwrap();
  assert_eq!(notes.len(), 2, "one from the client, one from the server");
  let bw = notes.iter().find(|n| n["field"] == "bandwidth").unwrap();
  assert_eq!(bw["declared"], "10mbit");
  assert_eq!(bw["effective"], "1mbit");
  assert_eq!(bw["source"], "client");
  let cache = notes.iter().find(|n| n["field"] == "cache").unwrap();
  assert_eq!(cache["effective"], "false");
  assert_eq!(cache["source"], "server");
}

#[tokio::test]
async fn client_config_renders_an_empty_hostname_list() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = None;
    h.sole_mut().declared_hostnames = Vec::new();
    h.sole_mut().assigned_hostnames = Vec::new();
    h.sole_mut().random_hostname = None;
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(State(state.clone()), Path("c1".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();

  // Serving no hostname is a state worth stating outright; omitting the key
  // would read as "not rendered yet" rather than "this connection has none".
  assert!(yaml.contains("hostname: []"), "got:\n{yaml}");
}

#[tokio::test]
async fn client_config_reports_an_active_overrule_and_hides_other_orgs() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().override_hostname_binds = vec!["moved.example.com".to_string()];
  })
  .await;
  insert_client(&state, "other", |h| {
    h.perms.org_id = Some("acme".to_string());
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(
    State(state.clone()),
    Path("c1".to_string()),
    headers.clone(),
  )
  .await;
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();
  // Routing follows the overrule, so the document does too.
  assert!(
    yaml.contains("  - \"moved.example.com\"  # dashboard overrule"),
    "got:\n{yaml}"
  );
  assert!(
    !yaml.contains("  - \"app.example.com\""),
    "the overruled name no longer routes:\n{yaml}"
  );
  let note = body["notes"]
    .as_array()
    .unwrap()
    .iter()
    .find(|n| n["field"] == "hostname")
    .unwrap()
    .clone();
  assert_eq!(note["declared"], "app.example.com");
  assert_eq!(note["effective"], "moved.example.com");

  // A client of another organization is a 404, like everywhere else.
  let resp = client_config_handler(State(state.clone()), Path("other".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// client_override_handler
// ---------------------------------------------------------------------------

fn override_req(hostname: Option<&str>, path: Option<&str>) -> Json<ClientOverrideRequest> {
  Json(ClientOverrideRequest {
    hostname_bind: hostname.map(|s| s.to_string()),
    hostname_binds: None,
    path_bind: path.map(|s| s.to_string()),
  })
}

/// The list form of the payload, as the dashboard's overrule dialog sends it.
fn override_list_req(hostnames: &[&str]) -> Json<ClientOverrideRequest> {
  Json(ClientOverrideRequest {
    hostname_bind: None,
    hostname_binds: Some(hostnames.iter().map(|s| s.to_string()).collect()),
    path_bind: None,
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
    let clients = state.clients.read().await;
    let h = clients.get("c1").unwrap();
    assert_eq!(
      h.sole().override_hostname_binds,
      vec!["new.example.com".to_string()]
    );
    assert_eq!(h.sole().override_path_bind.as_deref(), Some("/api/v2"));
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
    let clients = state.clients.read().await;
    let h = clients.get("c1").unwrap();
    assert!(h.sole().override_hostname_binds.is_empty());
    assert!(h.sole().override_path_bind.is_none());
  }
}

#[tokio::test]
async fn override_accepts_a_list_of_hostnames() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let headers = admin_headers(&state).await;

  // The dashboard sends one entry per bind row, so an operator can retarget
  // the name the client declared while keeping the random subdomain. Entries
  // are normalized, blanks dropped, and duplicates collapsed.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_list_req(&[
      "New.Example.com",
      "",
      "wild-fox.tunnel.example.com",
      "new.example.com",
    ]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds,
    vec![
      "new.example.com".to_string(),
      "wild-fox.tunnel.example.com".to_string()
    ]
  );

  // One invalid entry rejects the whole list, leaving the override untouched.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_list_req(&["ok.example.com", "bad host"]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  assert_eq!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds
      .len(),
    2
  );

  // An empty list clears it, the same as an empty string in the single form.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
    override_list_req(&[]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds
      .is_empty()
  );
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
  let clients = state.clients.read().await;
  assert!(
    clients
      .get("c1")
      .unwrap()
      .sole()
      .override_hostname_binds
      .is_empty()
  );
}

// ---------------------------------------------------------------------------
// client_enabled_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enabled_toggle_and_unknown() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| h.sole_mut().admin_enabled = true).await;
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
  assert!(
    !state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );

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
  assert!(
    state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );

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
    h.sole_mut().admin_enabled = true;
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
  assert!(
    state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );
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

#[tokio::test]
async fn live_stream_fences_notifications_to_the_subscriber_org() {
  use axum::response::IntoResponse;
  use futures_util::StreamExt;
  use std::time::Duration;
  use tokio::time::timeout;

  let state = Arc::new(test_state());
  // A viewer session scoped to the master org (None).
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;
  let headers = cookie_headers(&token);

  let sse = live_stream_handler(State(state.clone()), headers).await;
  let resp = sse.into_response();
  let mut body = resp.into_body().into_data_stream();

  // Drain the immediate `stats` frame.
  let _ = timeout(Duration::from_secs(2), body.next())
    .await
    .expect("stats frame in time");

  let ev = |event: &str, org: Option<&str>| crate::state::ServerEvent {
    event: event.to_string(),
    timestamp: "2026-08-06T00:00:00+03:00".to_string(),
    data: serde_json::json!({"id": "t1"}),
    org: org.map(str::to_string),
  };
  // Another org's event first: it must not reach this subscriber, so the
  // frame that does arrive is the master one behind it.
  let _ = state.events_tx.send(ev("token_revoked", Some("acme")));
  let _ = state.events_tx.send(ev("client_disconnected", None));

  let mut seen: Option<String> = None;
  for _ in 0..4 {
    let frame = timeout(Duration::from_secs(3), body.next())
      .await
      .expect("frame in time");
    let Some(Ok(bytes)) = frame else { break };
    let text = String::from_utf8_lossy(&bytes).to_string();
    if text.contains("event: notification") {
      seen = Some(text);
      break;
    }
  }
  let seen = seen.expect("the master-org notification streamed");
  assert!(seen.contains("client_disconnected"), "got: {seen}");
  assert!(
    !seen.contains("token_revoked"),
    "another org's event leaked: {seen}"
  );
}

#[tokio::test]
async fn override_refuses_a_hostname_outside_the_org_allowlist() {
  let state = Arc::new(test_state());
  // A fenced org, a client in it, and an admin session that has selected it.
  let org_id = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["*.acme.com".to_string()], None)
    .unwrap()
    .id;
  insert_client(&state, "c1", |h| {
    h.perms.org_id = Some(org_id.clone());
  })
  .await;
  let token = seed_session(&state, Role::Admin, None, Some(org_id.clone())).await;
  let headers = cookie_headers(&token);

  // An overrule is the one bind with no token permission behind it, so the
  // org fence has to hold here too.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers.clone(),
    override_req(Some("evil.example.com"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds
      .is_empty()
  );

  // Inside the fence it applies as before.
  let resp = client_override_handler(
    State(state.clone()),
    Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
    override_req(Some("app.acme.com"), None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    state.clients.write().await["c1"]
      .sole()
      .override_hostname_binds,
    vec!["app.acme.com".to_string()]
  );
}

#[tokio::test]
async fn live_stream_ends_when_the_session_it_opened_with_is_revoked() {
  // The session middleware runs once, when the stream is opened, and the
  // stream then lives for hours: signing out (or "sign out everywhere", or an
  // expiry, or a user being disabled) left it emitting traffic and statistics
  // to a caller with no session at all.
  use axum::response::IntoResponse;
  use futures_util::StreamExt;
  use std::time::Duration;
  use tokio::time::timeout;

  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let token = seed_session(&state, Role::Viewer, Some("v"), None).await;

  let sse = live_stream_handler(State(state.clone()), cookie_headers(&token)).await;
  let mut body = sse.into_response().into_body().into_data_stream();

  let first = timeout(Duration::from_secs(2), body.next())
    .await
    .expect("stats frame in time")
    .expect("some frame")
    .expect("ok bytes");
  assert!(String::from_utf8_lossy(&first).contains("event: stats"));

  // Sign out, then let a tick come round.
  state.sessions.lock().await.remove(&token);
  let mut ended = false;
  for _ in 0..3 {
    match timeout(Duration::from_secs(4), body.next()).await {
      Ok(None) => {
        ended = true;
        break;
      }
      Ok(Some(_)) => continue,
      Err(_) => break,
    }
  }
  assert!(
    ended,
    "the stream must close within a tick of the session going away"
  );
}

#[tokio::test]
async fn client_config_renders_every_optional_knob_and_server_overrule() {
  // One maximal connection, so every branch of the yaml renderer runs: the
  // dashboard overrules, the refused announcements, and each optional line.
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().declared_hostname = Some("app.example.com".to_string());
    h.sole_mut().declared_hostnames = vec!["app.example.com".to_string()];
    h.sole_mut().assigned_hostnames = vec![
      "app.example.com".to_string(),
      "wild-fox.example.com".to_string(),
    ];
    h.sole_mut().random_hostname = Some("wild-fox.example.com".to_string());
    h.sole_mut().override_hostname_binds = vec!["forced.example.com".to_string()];
    h.sole_mut().declared_path = Some("/api".to_string());
    h.sole_mut().override_path_bind = Some("/forced".to_string());
    h.sole_mut().public_denied_warned = true;
    h.sole_mut().visitor_auth_denied_warned = true;
    h.sole_mut().priority = 2;
    h.sole_mut().public = true;
    h.sole_mut().visitor_auth = Some("user:pass".to_string());
    h.sole_mut().allowed_ips = vec!["10.0.0.0/8".to_string(), "203.0.113.7".to_string()];
    h.sole_mut().denied = Some("https://example.com/no".to_string());
    h.sole_mut().cache = false;
    h.sole_mut().resilience = true;
    h.sole_mut().webhook_inbox = true;
    h.sole_mut().max_request_body = Some(1048576);
    h.sole_mut().response_timeout = Some(120);
    h.sole_mut().tcp_enabled = true;
    h.sole_mut().tunnels = vec![crate::protocol::TunnelDecl {
      name: Some("pg".to_string()),
      custom_name: None,
      target: "127.0.0.1:5432".to_string(),
      protocol: "tcp".to_string(),
      encrypt: true,
      idle_timeout: None,
      expose: None,
    }];
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = client_config_handler(State(state.clone()), Path("c1".to_string()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body: serde_json::Value = json_body(resp).await;
  let yaml = body["yaml"].as_str().unwrap();

  for line in [
    "priority: 2",
    "public: true",
    "auth: \"<set by the client>\"",
    "allowed_ips: [\"10.0.0.0/8\", \"203.0.113.7\"]",
    "denied: \"https://example.com/no\"",
    "resilience: true",
    "webhook_inbox: true",
    "max_request_body: 1048576",
    "response_timeout: 120",
    "tcp_target: \"<set by the client>\"",
    "tunnels:",
    "    encrypt: true",
  ] {
    assert!(yaml.contains(line), "missing `{line}` in:\n{yaml}");
  }

  // The three server-side refusals and the two overrules are all notes.
  let notes = body["notes"].as_array().unwrap();
  let fields: Vec<&str> = notes.iter().filter_map(|n| n["field"].as_str()).collect();
  for field in ["public", "auth", "hostname", "path"] {
    assert!(fields.contains(&field), "{fields:?}");
  }
  let hostname_note = notes.iter().find(|n| n["field"] == "hostname").unwrap();
  assert_eq!(hostname_note["effective"], "forced.example.com");
  assert!(
    hostname_note["declared"]
      .as_str()
      .unwrap()
      .contains("wild-fox.example.com"),
    "the assigned hostname is part of what the overrule replaced"
  );
}

#[tokio::test]
async fn enabling_an_unknown_or_foreign_client_is_not_found() {
  // The kill switch answers 404 both for an id that does not exist and for
  // another organization's client: a tenant must not learn which is which.
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |_| {}).await;
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;

  let resp = client_enabled_handler(
    State(state.clone()),
    Path("missing".to_string()),
    axum::extract::ConnectInfo(test_peer()),
    admin_headers(&state).await,
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  let resp = client_enabled_handler(
    State(state.clone()),
    Path("c1".to_string()),
    axum::extract::ConnectInfo(test_peer()),
    cookie_headers(&token),
    Json(ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
  // And the master client is untouched.
  assert!(
    state
      .clients
      .write()
      .await
      .get("c1")
      .unwrap()
      .sole()
      .admin_enabled
  );
}

// --- log filtering (planned_features #31) -----------------------------------

/// A log entry with the fields the filters look at.
fn log_of(
  id: &str,
  method: &str,
  uri: &str,
  status: Option<u16>,
  error: Option<&str>,
) -> RequestLog {
  RequestLog {
    id: id.to_string(),
    timestamp: "2026-08-02T00:00:00Z".to_string(),
    method: method.to_string(),
    uri: uri.to_string(),
    status,
    duration_ms: 5,
    error: error.map(|e| e.to_string()),
    host: Some("svc.example.com".to_string()),
    org_id: None,
  }
}

async fn query(state: &Arc<AppState>, pairs: &[(&str, &str)]) -> Vec<RequestLog> {
  let params: std::collections::HashMap<String, String> = pairs
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
  let headers = admin_headers(state).await;
  logs_handler(State(state.clone()), headers, axum::extract::Query(params))
    .await
    .0
}

async fn seeded_state() -> Arc<AppState> {
  let state = Arc::new(test_state());
  {
    let mut logs = state.recent_logs.lock().await;
    logs.push_back(log_of("a", "GET", "/api/users", Some(200), None));
    logs.push_back(log_of("b", "POST", "/api/login", Some(401), None));
    logs.push_back(log_of("c", "GET", "/api/login", Some(500), None));
    logs.push_back(log_of(
      "d",
      "DELETE",
      "/things/9",
      None,
      Some("client gone"),
    ));
  }
  state
}

#[tokio::test]
async fn status_accepts_an_exact_code_and_a_class() {
  let state = seeded_state().await;
  let exact = query(&state, &[("status", "401")]).await;
  assert_eq!(exact.len(), 1);
  assert_eq!(exact[0].id, "b");

  let class = query(&state, &[("status", "4xx")]).await;
  assert_eq!(class.len(), 1, "401 is the only 4xx");

  // A failed request with no status is a 5xx, as the dashboard buckets it,
  // so it must not vanish from the query an operator runs to find failures.
  let server_errors = query(&state, &[("status", "5xx")]).await;
  let ids: Vec<&str> = server_errors.iter().map(|l| l.id.as_str()).collect();
  assert_eq!(ids, vec!["c", "d"]);
}

#[tokio::test]
async fn method_and_path_filter_and_combine() {
  let state = seeded_state().await;
  assert_eq!(
    query(&state, &[("method", "get")]).await.len(),
    2,
    "case-insensitive"
  );
  assert_eq!(
    query(&state, &[("path", "LOGIN")]).await.len(),
    2,
    "substring, case-insensitive"
  );
  // Predicates are AND, not OR.
  let both = query(&state, &[("method", "GET"), ("path", "/api/login")]).await;
  assert_eq!(both.len(), 1);
  assert_eq!(both[0].id, "c");
}

#[tokio::test]
async fn an_empty_parameter_is_not_a_filter() {
  // A form that submits `?method=` must not mean "no method matches".
  let state = seeded_state().await;
  assert_eq!(query(&state, &[("method", "")]).await.len(), 4);
  assert_eq!(query(&state, &[("status", "  ")]).await.len(), 4);
}

#[tokio::test]
async fn a_limit_returns_the_newest_matches() {
  let state = seeded_state().await;
  let capped = query(&state, &[("limit", "2")]).await;
  let ids: Vec<&str> = capped.iter().map(|l| l.id.as_str()).collect();
  assert_eq!(
    ids,
    vec!["d", "c"],
    "newest first, so a cap keeps the recent ones"
  );
  // Without a limit the order is unchanged, which the live view relies on.
  let all = query(&state, &[]).await;
  assert_eq!(all.first().unwrap().id, "a");
}
