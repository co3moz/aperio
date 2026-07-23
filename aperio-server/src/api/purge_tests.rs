//! Tests for the right-to-erasure selective purge and response-cache purge
//! dashboard APIs.

use super::*;
use crate::state::{CapturedRequest, RequestLog, RequestTimeline};
use crate::store::users::Role;
use crate::test_support::*;
use std::time::Duration;

/// Builds a traffic-log entry for the given host.
fn log_entry(id: &str, host: Option<&str>) -> RequestLog {
  RequestLog {
    id: id.to_string(),
    timestamp: "now".to_string(),
    method: "GET".to_string(),
    uri: "/".to_string(),
    status: Some(200),
    duration_ms: 1,
    error: None,
    host: host.map(|s| s.to_string()),
    org_id: None,
  }
}

/// Builds an inspector capture carrying the given request headers.
fn capture(id: &str, headers: Vec<(&str, &str)>) -> CapturedRequest {
  CapturedRequest {
    id: id.to_string(),
    timestamp: "now".to_string(),
    method: "GET".to_string(),
    uri: "/".to_string(),
    req_headers: headers
      .into_iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
    req_body: None,
    req_body_truncated: false,
    status: 200,
    resp_headers: Vec::new(),
    resp_body: None,
    resp_body_truncated: false,
    resp_streamed: false,
    duration_ms: 1,
    timeline: None,
    org_id: None,
  }
}

/// A minimal timeline so `StageStats::record` inserts a route entry.
fn timeline() -> RequestTimeline {
  RequestTimeline {
    client_ready_us: None,
    admitted_us: None,
    selected_us: None,
    dispatched_us: 0,
    client_received_us: None,
    backend_sent_us: None,
    backend_first_byte_us: None,
    backend_done_us: None,
    client_responded_us: None,
    response_received_us: 10,
    finished_us: 20,
    estimated_anchor: false,
  }
}

/// Stores a cached entry under `host|uri` with the given surrogate tags.
async fn cache_insert(state: &AppState, key: &str, tags: Vec<&str>) {
  state.response_cache.lock().await.insert(
    key.to_string(),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    b"body".to_vec(),
    Duration::from_secs(60),
    64 * 1024,
    false,
    Duration::ZERO,
    tags.into_iter().map(|s| s.to_string()).collect(),
  );
}

// ---------------------------------------------------------------------------
// cache_stats_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_stats_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = cache_stats_handler(State(state), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cache_stats_reports_occupancy() {
  let state = Arc::new(test_state());
  cache_insert(&state, "a.com|/x", vec![]).await;
  let headers = admin_headers(&state).await;
  let resp = cache_stats_handler(State(state), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["entries"], 1);
}

// ---------------------------------------------------------------------------
// cache_purge_handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_purge_requires_master_admin() {
  let state = Arc::new(test_state());
  let resp = cache_purge_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(CachePurgeRequest {
      hostname: None,
      path_prefix: None,
      surrogate_key: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cache_purge_by_surrogate() {
  let state = Arc::new(test_state());
  cache_insert(&state, "a.com|/x", vec!["prod-1"]).await;
  cache_insert(&state, "a.com|/y", vec!["prod-1"]).await;
  cache_insert(&state, "a.com|/z", vec!["prod-2"]).await;
  let headers = admin_headers(&state).await;
  let resp = cache_purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(CachePurgeRequest {
      hostname: None,
      path_prefix: None,
      // Surrogate wins even alongside a (here empty-after-trim) host.
      surrogate_key: Some("  prod-1  ".to_string()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["status"], "ok");
  assert_eq!(body["removed"], 2);
  // The prod-2 entry survives.
  assert_eq!(state.response_cache.lock().await.stats().entries, 1);
}

#[tokio::test]
async fn cache_purge_by_host_and_prefix() {
  let state = Arc::new(test_state());
  cache_insert(&state, "a.com|/assets/1", vec![]).await;
  cache_insert(&state, "a.com|/page", vec![]).await;
  cache_insert(&state, "b.com|/assets/1", vec![]).await;
  let headers = admin_headers(&state).await;
  let resp = cache_purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(CachePurgeRequest {
      hostname: Some("A.COM".to_string()),
      path_prefix: Some("/assets/".to_string()),
      surrogate_key: Some("   ".to_string()), // whitespace-only → ignored
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["removed"], 1);
  assert_eq!(state.response_cache.lock().await.stats().entries, 2);
}

#[tokio::test]
async fn cache_purge_empty_body_clears_everything() {
  let state = Arc::new(test_state());
  cache_insert(&state, "a.com|/x", vec![]).await;
  cache_insert(&state, "b.com|/y", vec![]).await;
  let headers = admin_headers(&state).await;
  let resp = cache_purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(CachePurgeRequest {
      hostname: Some("".to_string()), // empty → filtered to None
      path_prefix: None,
      surrogate_key: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(json_body(resp).await["removed"], 2);
  assert_eq!(state.response_cache.lock().await.stats().entries, 0);
}

// ---------------------------------------------------------------------------
// purge_handler: auth + validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn purge_requires_session() {
  let state = Arc::new(test_state());
  let resp = purge_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PurgeRequest {
      hostname: Some("a.com".to_string()),
      token: None,
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn purge_forbidden_for_non_master() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let headers = cookie_headers(&token);
  let resp = purge_handler(
    State(state),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: Some("a.com".to_string()),
      token: None,
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn purge_rejects_no_selector() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  // All selectors blank/whitespace collapse to None.
  let resp = purge_handler(
    State(state),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: Some("  ".to_string()),
      token: Some("".to_string()),
      ip: Some("   ".to_string()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// purge_handler: selectors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn purge_by_hostname_clears_all_surfaces() {
  let state = Arc::new(test_state());

  // Traffic log: two entries for a.com (mixed case), one for b.com.
  {
    let mut logs = state.recent_logs.lock().await;
    logs.push_back(log_entry("1", Some("A.com")));
    logs.push_back(log_entry("2", Some("a.com")));
    logs.push_back(log_entry("3", Some("b.com")));
    logs.push_back(log_entry("4", None));
  }
  // Inspector captures: one with a matching Host header, one for another host.
  {
    let mut caps = state.captured_requests.lock().await;
    caps.push_back(capture("c1", vec![("Host", "a.com:8080")]));
    caps.push_back(capture("c2", vec![("Host", "other.com")]));
  }
  // Persistent stats + stage window + cache entries keyed on a.com.
  state.persistent_stats.lock().await.record_request_labeled(
    true,
    1,
    2,
    3,
    None,
    Some("a.com"),
    None,
  );
  state
    .stage_stats
    .lock()
    .await
    .record(Some("a.com"), None, &timeline());
  cache_insert(&state, "a.com|/x", vec![]).await;
  cache_insert(&state, "a.com|/y", vec![]).await;
  cache_insert(&state, "b.com|/z", vec![]).await;

  let headers = admin_headers(&state).await;
  let resp = purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: Some("A.COM".to_string()),
      token: None,
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["status"], "ok");
  assert_eq!(body["removed"]["traffic_log"], 2);
  assert_eq!(body["removed"]["inspector_captures"], 1);
  // Recorded in both the global aggregate and the master-org slice.
  assert_eq!(body["removed"]["stats_rows"], 2);
  assert_eq!(body["removed"]["stage_windows"], 1);
  assert_eq!(body["removed"]["cache_entries"], 2);
  // No access log configured → null.
  assert_eq!(body["removed"]["access_log_lines"], serde_json::Value::Null);

  // Surviving records for the other hosts remain.
  assert_eq!(state.recent_logs.lock().await.len(), 2);
  assert_eq!(state.captured_requests.lock().await.len(), 1);
  assert_eq!(state.response_cache.lock().await.stats().entries, 1);
}

#[tokio::test]
async fn purge_by_token_clears_stats_only() {
  let state = Arc::new(test_state());
  state.persistent_stats.lock().await.record_request_labeled(
    true,
    1,
    2,
    3,
    Some("tok-a"),
    None,
    None,
  );
  let headers = admin_headers(&state).await;
  let resp = purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: None,
      token: Some("TOK-A".to_string()),
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  // Recorded in both the global aggregate and the master-org slice.
  assert_eq!(body["removed"]["stats_rows"], 2);
  assert_eq!(body["removed"]["traffic_log"], 0);
  assert_eq!(body["removed"]["cache_entries"], 0);
}

#[tokio::test]
async fn purge_by_ip_matches_forwarded_headers() {
  let state = Arc::new(test_state());
  {
    let mut caps = state.captured_requests.lock().await;
    caps.push_back(capture("x1", vec![("X-Real-IP", "9.9.9.9")]));
    caps.push_back(capture("x2", vec![("CF-Connecting-IP", "9.9.9.9")]));
    caps.push_back(capture(
      "x3",
      vec![("X-Forwarded-For", "1.1.1.1, 9.9.9.9, 2.2.2.2")],
    ));
    // Non-matching: different IP and an unrelated header.
    caps.push_back(capture("x4", vec![("X-Real-IP", "8.8.8.8")]));
    caps.push_back(capture("x5", vec![("User-Agent", "curl")]));
  }
  let headers = admin_headers(&state).await;
  let resp = purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: None,
      token: None,
      ip: Some("9.9.9.9".to_string()),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["removed"]["inspector_captures"], 3);
  assert_eq!(state.captured_requests.lock().await.len(), 2);
}

#[tokio::test]
async fn purge_unknown_hostname_is_noop() {
  let state = Arc::new(test_state());
  {
    let mut logs = state.recent_logs.lock().await;
    logs.push_back(log_entry("1", Some("a.com")));
  }
  cache_insert(&state, "a.com|/x", vec![]).await;
  let headers = admin_headers(&state).await;
  let resp = purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: Some("nope.com".to_string()),
      token: None,
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["removed"]["traffic_log"], 0);
  assert_eq!(body["removed"]["cache_entries"], 0);
  assert_eq!(body["removed"]["stage_windows"], 0);
  // Nothing was dropped.
  assert_eq!(state.recent_logs.lock().await.len(), 1);
}

// ---------------------------------------------------------------------------
// rewrite_access_log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn purge_rewrites_access_log() {
  let mut state = test_state();
  // Point the access log at a temp file with mixed matching/non-matching and
  // malformed lines.
  let path = std::env::temp_dir().join(format!("aperio-purge-log-{}.jsonl", uuid::Uuid::new_v4()));
  let contents = concat!(
    "{\"host\":\"a.com\",\"token\":\"t1\"}\n",
    "{\"host\":\"b.com\",\"token\":\"t2\"}\n",
    "not-json-at-all\n",
    "{\"token\":\"secret\"}\n",
    "{\"other\":\"x\"}\n"
  );
  std::fs::write(&path, contents).unwrap();
  state.access_log_path = Some(path.to_string_lossy().into_owned());
  state.access_log = Some(std::sync::Mutex::new(
    std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(&path)
      .unwrap(),
  ));
  let state = Arc::new(state);

  let headers = admin_headers(&state).await;
  let resp = purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: Some("a.com".to_string()),
      token: Some("secret".to_string()),
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  // The a.com line and the secret-token line are dropped (2 removed).
  assert_eq!(body["removed"]["access_log_lines"], 2);

  let after = std::fs::read_to_string(&path).unwrap();
  assert!(!after.contains("a.com"));
  assert!(!after.contains("secret"));
  assert!(after.contains("b.com"));
  assert!(after.contains("not-json-at-all"));
  let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn purge_access_log_no_match_leaves_file_intact() {
  let mut state = test_state();
  let path = std::env::temp_dir().join(format!("aperio-purge-log-{}.jsonl", uuid::Uuid::new_v4()));
  let contents = "{\"host\":\"b.com\",\"token\":\"t2\"}\n";
  std::fs::write(&path, contents).unwrap();
  state.access_log_path = Some(path.to_string_lossy().into_owned());
  state.access_log = Some(std::sync::Mutex::new(
    std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(&path)
      .unwrap(),
  ));
  let state = Arc::new(state);

  let headers = admin_headers(&state).await;
  let resp = purge_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PurgeRequest {
      hostname: Some("nomatch.com".to_string()),
      token: None,
      ip: None,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  // No matching lines → 0 removed, file untouched.
  assert_eq!(body["removed"]["access_log_lines"], 0);
  assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
  let _ = std::fs::remove_file(&path);
}
