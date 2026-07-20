//! Tests for the top-level dashboard/static handlers in `api.rs`.

use super::*;
use crate::test_support::{json_body, mock_client, test_state};
use axum::extract::State;

#[tokio::test]
async fn health_reports_status_and_counts() {
  let state = Arc::new(test_state());
  state.clients.lock().await.insert(
    "c1".to_string(),
    mock_client(Some("app.example.com"), None, None, None),
  );

  let resp = health_handler(State(state.clone())).await.into_response();
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["status"], "healthy");
  assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
  assert_eq!(body["connected_clients"], 1);
  assert_eq!(body["ui_language"], "en");
  assert!(body["uptime_seconds"].is_number());
  assert!(body["total_requests"].is_number());
}

#[test]
fn serve_embedded_serves_index_with_no_cache() {
  let resp = serve_embedded("index.html", false);
  assert_eq!(resp.status(), StatusCode::OK);
  let cc = resp
    .headers()
    .get(axum::http::header::CACHE_CONTROL)
    .unwrap()
    .to_str()
    .unwrap();
  assert_eq!(cc, "no-cache");
  // Security headers are attached.
  assert_eq!(
    resp
      .headers()
      .get(axum::http::header::X_FRAME_OPTIONS)
      .unwrap(),
    "DENY"
  );
  assert!(
    resp
      .headers()
      .contains_key(axum::http::header::CONTENT_SECURITY_POLICY)
  );
}

#[test]
fn serve_embedded_marks_immutable_assets() {
  let resp = serve_embedded("index.html", true);
  assert_eq!(resp.status(), StatusCode::OK);
  let cc = resp
    .headers()
    .get(axum::http::header::CACHE_CONTROL)
    .unwrap()
    .to_str()
    .unwrap();
  assert!(cc.contains("immutable"), "got: {cc}");
}

#[test]
fn serve_embedded_missing_file_is_404() {
  let resp = serve_embedded("does-not-exist-xyz.bin", false);
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dashboard_handler_serves_spa() {
  let resp = dashboard_handler().await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn dashboard_asset_handler_missing_is_404() {
  let resp = dashboard_asset_handler(axum::extract::Path("nope-123.js".to_string())).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
