//! Tests for the dashboard client/stats API: live stats snapshot, traffic
//! logs, uptime summary, traffic history, the SSE live stream, and the
//! per-client override / enable-disable handlers (including org isolation).

use super::*;
use crate::state::AppState;
use crate::state::RequestLog;
use crate::test_support::{admin_headers, mock_client, test_state};
use axum::extract::State;
use std::sync::Arc;

/// Inserts a client handle under `id`, running `f` to tweak its fields first.
pub(crate) async fn insert_client(
  state: &AppState,
  id: &str,
  f: impl FnOnce(&mut crate::state::ClientHandle),
) {
  let mut handle = mock_client(Some("svc.example.com"), Some("/api"), None, None);
  f(&mut handle);
  state.clients.write().await.insert(id.to_string(), handle);
}

pub(crate) fn log(id: &str, org: Option<&str>) -> RequestLog {
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

// ---------------------------------------------------------------------------
// logs_handler
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// uptime_handler / uptime_pct
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// stats_history_handler
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// client_config_handler
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// client_override_handler
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// client_enabled_handler
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// live_stream_handler (SSE)
// ---------------------------------------------------------------------------

// --- log filtering (planned_features #31) -----------------------------------

/// A connection carrying two services is two rows, each addressable.
///
/// The table is what an operator manages the fleet from. A connection with
/// two services showing as one row does not merely hide the second: it hides
/// that a second exists, so nothing on the page prompts anyone to look. And a
/// row that cannot be addressed on its own is worse than no row, because the
/// enable switch beside it would quietly act on its neighbour.
#[tokio::test]
pub(crate) async fn a_connection_with_two_services_is_two_addressable_rows() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().service_name = Some("api".to_string());
    h.sole_mut().declared_path = Some("/api".to_string());
    let mut second = crate::state::ServiceState::newly_declared(Default::default());
    second.service_name = Some("web".to_string());
    second.declared_path = Some("/web".to_string());
    h.services.push(second);
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = stats_handler(State(state.clone()), headers).await;
  let body = serde_json::to_value(&resp.0).unwrap();
  let rows = body["active_clients"].as_array().unwrap();
  assert_eq!(rows.len(), 2, "one connection, two rows");

  let api = rows.iter().find(|r| r["service"] == "api").unwrap();
  let web = rows.iter().find(|r| r["service"] == "web").unwrap();
  assert_eq!(api["id"], "c1", "both rows carry the connection id");
  assert_eq!(web["id"], "c1");
  assert_eq!(
    api["service_index"], 0,
    "and the pair is what addresses them"
  );
  assert_eq!(web["service_index"], 1);
  assert_eq!(api["path_bind"], "/api", "each row shows its own binds");
  assert_eq!(web["path_bind"], "/web");
}

/// The enable switch acts on the service its row names.
#[tokio::test]
pub(crate) async fn disabling_one_service_leaves_the_other_enabled() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |h| {
    h.sole_mut().service_name = Some("api".to_string());
    let mut second = crate::state::ServiceState::newly_declared(Default::default());
    second.service_name = Some("web".to_string());
    h.services.push(second);
  })
  .await;

  let headers = admin_headers(&state).await;
  let resp = crate::api::clients::client_enabled_handler(
    State(state.clone()),
    axum::extract::Path("c1".to_string()),
    axum::extract::Query(crate::api::clients::ServiceQuery { service: 1 }),
    axum::extract::ConnectInfo("127.0.0.1:9000".parse().unwrap()),
    headers,
    axum::Json(crate::api::clients::ClientEnabledRequest { enabled: false }),
  )
  .await;
  assert_eq!(resp.status(), axum::http::StatusCode::OK);

  let clients = state.clients.read().await;
  let h = clients.get("c1").unwrap();
  assert!(h.services[0].admin_enabled, "the api service is untouched");
  assert!(
    !h.services[1].admin_enabled,
    "the web service is the one switched off"
  );
}
