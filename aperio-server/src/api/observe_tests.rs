//! Tests for the self-observability endpoints: the master-admin self-health
//! snapshot and the per-org traffic CSV export.

use super::*;
use crate::store::users::Role;
use crate::test_support::{admin_headers, cookie_headers, json_body, seed_session, test_state};
use axum::extract::{Query, State};
use std::collections::HashMap;
use std::sync::Arc;

fn query(pairs: &[(&str, &str)]) -> Query<HashMap<String, String>> {
  Query(
    pairs
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
  )
}

/// Reads the full body of a response as a UTF-8 string.
async fn body_text(resp: Response) -> String {
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  String::from_utf8(bytes.to_vec()).unwrap()
}

// --- csv_field ------------------------------------------------------------

#[test]
fn csv_field_leaves_plain_values_untouched() {
  assert_eq!(csv_field("2026-07-06"), "2026-07-06");
  assert_eq!(csv_field(""), "");
}

#[test]
fn csv_field_quotes_and_escapes_special_characters() {
  // Comma triggers quoting.
  assert_eq!(csv_field("a,b"), "\"a,b\"");
  // Embedded quote is doubled and the field is quoted.
  assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
  // Newlines (LF and CR) trigger quoting.
  assert_eq!(csv_field("a\nb"), "\"a\nb\"");
  assert_eq!(csv_field("a\rb"), "\"a\rb\"");
}

// --- store_bytes ----------------------------------------------------------

#[test]
fn store_bytes_sums_present_db_sidecars_and_ignores_missing() {
  let dir = std::env::temp_dir().join(format!("aperio-observe-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  // Empty directory: nothing to count.
  assert_eq!(store_bytes(&dir), 0);
  // Write the main db and one sidecar; the -shm file stays absent.
  std::fs::write(dir.join("aperio.db"), b"1234567890").unwrap();
  std::fs::write(dir.join("aperio.db-wal"), b"abc").unwrap();
  assert_eq!(store_bytes(&dir), 13);
}

// --- process_rss_bytes ----------------------------------------------------

#[test]
fn process_rss_bytes_is_callable() {
  // Linux returns Some(_); other platforms return None. Either way it must not
  // panic and, when present, be a plausible non-zero figure.
  if let Some(v) = process_rss_bytes() {
    assert!(v > 0);
  }
}

// --- self_health_handler --------------------------------------------------

#[tokio::test]
async fn self_health_requires_master_admin() {
  let state = Arc::new(test_state());
  // No credentials at all → 401.
  let resp = self_health_handler(State(state.clone()), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn self_health_forbids_non_master_admin() {
  let state = Arc::new(test_state());
  // A viewer session authenticates but is not a master admin → 403.
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let headers = cookie_headers(&token);
  let resp = self_health_handler(State(state.clone()), headers).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn self_health_returns_snapshot_for_master_admin() {
  let state = Arc::new(test_state());
  // Seed a connected client so `connected_clients` is exercised as non-zero.
  state.clients.lock().await.insert(
    "c1".to_string(),
    crate::test_support::mock_client(None, None, None, None),
  );

  let headers = admin_headers(&state).await;
  let resp = self_health_handler(State(state.clone()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;

  assert_eq!(body["connected_clients"], 1);
  assert!(body["uptime_seconds"].is_u64());
  // On non-Linux platforms rss_bytes is null; on Linux it is a u64.
  assert!(body["rss_bytes"].is_u64() || body["rss_bytes"].is_null());
  assert!(body["store_bytes"].is_u64());
  let cache = &body["cache"];
  assert!(cache["entries"].is_u64());
  assert!(cache["bytes"].is_u64());
  assert!(cache["hits"].is_u64());
  assert!(cache["misses"].is_u64());
  assert!(cache.get("hit_ratio").is_some());
}

// --- traffic_csv_handler --------------------------------------------------

#[tokio::test]
async fn traffic_csv_requires_authentication() {
  let state = Arc::new(test_state());
  let resp = traffic_csv_handler(State(state.clone()), HeaderMap::new(), query(&[])).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn traffic_csv_default_params_emit_empty_day_history() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = traffic_csv_handler(State(state.clone()), headers, query(&[])).await;
  assert_eq!(resp.status(), StatusCode::OK);

  let ct = resp
    .headers()
    .get(header::CONTENT_TYPE)
    .unwrap()
    .to_str()
    .unwrap()
    .to_string();
  assert_eq!(ct, "text/csv; charset=utf-8");
  let cd = resp
    .headers()
    .get(header::CONTENT_DISPOSITION)
    .unwrap()
    .to_str()
    .unwrap()
    .to_string();
  assert!(cd.contains("aperio-traffic-day.csv"));

  let text = body_text(resp).await;
  let mut lines = text.lines();
  assert_eq!(
    lines.next().unwrap(),
    "period,requests,success,failed,bytes_sent,bytes_received,avg_ms"
  );
  // Default count is 30 recent days, all empty → 30 zero rows with avg 0.
  let rows: Vec<&str> = lines.collect();
  assert_eq!(rows.len(), 30);
  assert!(rows.iter().all(|r| r.ends_with(",0,0,0,0,0,0")));
}

#[tokio::test]
async fn traffic_csv_reflects_recorded_master_traffic() {
  let state = Arc::new(test_state());
  // Two requests today for the master org: 1 success, 1 failure.
  {
    let mut stats = state.persistent_stats.lock().await;
    stats.record_request(true, 100, 200, 40, None);
    stats.record_request(false, 10, 20, 60, None);
  }
  let headers = admin_headers(&state).await;
  let resp = traffic_csv_handler(State(state.clone()), headers, query(&[("count", "1")])).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let text = body_text(resp).await;
  let rows: Vec<&str> = text.lines().skip(1).collect();
  assert_eq!(rows.len(), 1);
  // requests=2, success=1, failed=1, bytes_sent=200+20=220 (out),
  // bytes_received=100+10=110 (in), avg=(40+60)/2=50.
  let today = rows[0];
  assert!(
    today.ends_with(",2,1,1,220,110,50"),
    "unexpected row: {today}"
  );
}

#[tokio::test]
async fn traffic_csv_scopes_to_selected_org_for_master_admin() {
  let state = Arc::new(test_state());
  // Record traffic attributed to org "acme".
  {
    let mut stats = state.persistent_stats.lock().await;
    stats.record_request(true, 5, 5, 30, Some("acme"));
  }
  // A master admin whose session has org "acme" selected sees the acme slice.
  let token = seed_session(&state, Role::Admin, None, Some("acme".to_string())).await;
  let headers = cookie_headers(&token);
  let resp = traffic_csv_handler(State(state.clone()), headers, query(&[("count", "1")])).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let text = body_text(resp).await;
  let rows: Vec<&str> = text.lines().skip(1).collect();
  assert_eq!(rows.len(), 1);
  assert!(
    rows[0].ends_with(",1,1,0,5,5,30"),
    "unexpected: {}",
    rows[0]
  );
}

#[tokio::test]
async fn traffic_csv_invalid_unit_falls_back_to_day() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = traffic_csv_handler(
    State(state.clone()),
    headers,
    query(&[("unit", "fortnight"), ("count", "3")]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let cd = resp
    .headers()
    .get(header::CONTENT_DISPOSITION)
    .unwrap()
    .to_str()
    .unwrap()
    .to_string();
  // The invalid unit is ignored and the filename reports the "day" default.
  assert!(cd.contains("aperio-traffic-day.csv"));
  let text = body_text(resp).await;
  assert_eq!(text.lines().skip(1).count(), 3);
}

#[tokio::test]
async fn traffic_csv_honours_each_valid_unit() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  for unit in ["day", "week", "month", "year"] {
    let resp = traffic_csv_handler(
      State(state.clone()),
      headers.clone(),
      query(&[("unit", unit), ("count", "2")]),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let cd = resp
      .headers()
      .get(header::CONTENT_DISPOSITION)
      .unwrap()
      .to_str()
      .unwrap()
      .to_string();
    assert!(cd.contains(&format!("aperio-traffic-{unit}.csv")));
    let text = body_text(resp).await;
    assert_eq!(text.lines().skip(1).count(), 2, "unit {unit}");
  }
}

#[tokio::test]
async fn traffic_csv_count_is_parsed_and_clamped() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  // Non-numeric count → default of 30.
  let resp = traffic_csv_handler(
    State(state.clone()),
    headers.clone(),
    query(&[("count", "abc")]),
  )
  .await;
  assert_eq!(body_text(resp).await.lines().skip(1).count(), 30);

  // Zero clamps up to 1.
  let resp = traffic_csv_handler(
    State(state.clone()),
    headers.clone(),
    query(&[("count", "0")]),
  )
  .await;
  assert_eq!(body_text(resp).await.lines().skip(1).count(), 1);

  // Above the ceiling clamps down to 366.
  let resp = traffic_csv_handler(State(state.clone()), headers, query(&[("count", "9999")])).await;
  assert_eq!(body_text(resp).await.lines().skip(1).count(), 366);
}
