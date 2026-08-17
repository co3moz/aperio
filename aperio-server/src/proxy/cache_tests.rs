//! Turning a cached entry back into a visitor response: a fresh hit, a stale
//! one and what marks it as stale, and that a resilient entry still answers
//! when its client has gone and the posture is closed.

use super::super::proxy_tests::*;
use super::*;
use crate::test_support::*;
use std::sync::Arc;

// --- cache_hit_response ------------------------------------------------------

fn hit(
  status: u16,
  headers: Vec<(String, String)>,
  body: &[u8],
  stale: bool,
) -> crate::cache::CacheHit {
  crate::cache::CacheHit {
    status,
    headers,
    body: body.to_vec().into(),
    age_secs: 3,
    stale,
  }
}

#[test]
fn cache_hit_full_body() {
  let (status, bytes, resp) =
    cache_hit_response(hit(200, vec![], b"hello world", false), &HeaderMap::new());
  assert_eq!(status, 200);
  assert_eq!(bytes, 11);
  assert_eq!(resp.headers().get("x-aperio-cache").unwrap(), "hit");
  assert_eq!(resp.headers().get("accept-ranges").unwrap(), "bytes");
}

#[test]
fn cache_hit_stale_marker() {
  let (_, _, resp) = cache_hit_response(hit(200, vec![], b"body", true), &HeaderMap::new());
  assert_eq!(resp.headers().get("x-aperio-stale").unwrap(), "true");
}

#[test]
fn cache_hit_not_modified() {
  // A 304 keeps only cache-metadata headers; entity headers (content-type) are
  // dropped, and a stale content-length is never copied.
  let headers = vec![
    ("etag".to_string(), "\"v1\"".to_string()),
    ("cache-control".to_string(), "max-age=60".to_string()),
    ("content-type".to_string(), "text/plain".to_string()),
    ("content-length".to_string(), "4".to_string()),
  ];
  let mut req = HeaderMap::new();
  req.insert("if-none-match", HeaderValue::from_static("\"v1\""));
  let (status, bytes, resp) = cache_hit_response(hit(200, headers, b"body", false), &req);
  assert_eq!(status, 304);
  assert_eq!(bytes, 0);
  assert!(resp.headers().get("cache-control").is_some());
  assert!(resp.headers().get("content-type").is_none());
}

#[test]
fn cache_hit_skips_stale_content_length() {
  let headers = vec![
    ("content-length".to_string(), "999".to_string()),
    ("x-test".to_string(), "1".to_string()),
  ];
  let (status, _bytes, resp) =
    cache_hit_response(hit(200, headers, b"hello world", false), &HeaderMap::new());
  assert_eq!(status, 200);
  // The stale content-length is not copied verbatim; hyper derives the real one.
  assert_eq!(resp.headers().get("x-test").unwrap(), "1");
}

#[test]
fn cache_hit_range_partial() {
  let mut req = HeaderMap::new();
  req.insert("range", HeaderValue::from_static("bytes=0-4"));
  let (status, bytes, resp) = cache_hit_response(hit(200, vec![], b"hello world", false), &req);
  assert_eq!(status, 206);
  assert_eq!(bytes, 5);
  assert_eq!(resp.headers().get("content-range").unwrap(), "bytes 0-4/11");
}

#[test]
fn cache_hit_range_unsatisfiable() {
  let mut req = HeaderMap::new();
  req.insert("range", HeaderValue::from_static("bytes=100-200"));
  let (status, _bytes, resp) = cache_hit_response(hit(200, vec![], b"hello world", false), &req);
  assert_eq!(status, 416);
  assert_eq!(resp.headers().get("content-range").unwrap(), "bytes */11");
}

#[test]
fn cache_hit_if_range_mismatch_serves_full() {
  // An If-Range validator that no longer matches degrades to the full 200.
  let headers = vec![("etag".to_string(), "\"v2\"".to_string())];
  let mut req = HeaderMap::new();
  req.insert("range", HeaderValue::from_static("bytes=0-4"));
  req.insert("if-range", HeaderValue::from_static("\"stale\""));
  let (status, bytes, _resp) = cache_hit_response(hit(200, headers, b"hello world", false), &req);
  assert_eq!(status, 200);
  assert_eq!(bytes, 11);
}

// --- stale_cache_response ----------------------------------------------------

#[tokio::test]
async fn stale_cache_serves_resilient_entry() {
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  cfg.cache_max_stale = 3600;
  let state = Arc::new(test_state_with(cfg));
  // A resilient entry that has already expired but is within the stale window.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(None, "/x"),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    b"stale-body".to_vec().into(),
    std::time::Duration::from_secs(0),
    64 * 1024 * 1024,
    true, // resilient
    std::time::Duration::from_secs(0),
    Vec::new(),
  );
  let resp = stale_cache_response(
    &state,
    "GET",
    "/x",
    &HeaderMap::new(),
    std::time::Instant::now(),
  )
  .await;
  let resp = resp.expect("resilient stale entry should serve");
  assert_eq!(resp.headers().get("x-aperio-cache").unwrap(), "hit");

  // A non-cacheable method never serves stale.
  assert!(
    stale_cache_response(
      &state,
      "POST",
      "/x",
      &HeaderMap::new(),
      std::time::Instant::now()
    )
    .await
    .is_none()
  );
}

// --- #119: a resilient entry under the closed posture ------------------------

/// A resilient cached answer survives its client under the closed posture.
///
/// This is the deterministic form of `planned_features.md` #119, which spent
/// six passes as an intermittent e2e failure because reproducing it needed the
/// request to arrive in exactly the state below and the suite only sometimes
/// arranged that. Driven directly it is not intermittent at all.
///
/// The state: `default_access: deny`, a route with a resilient entry in the
/// cache, and no client serving it. Nothing declares the route open, so the
/// visitor gate answers `Undeclared`, and that answer is a 504. Returning it
/// makes `resilience: true` do nothing in the one condition it exists for,
/// since a resilient entry is only ever consulted once the client is gone.
///
/// The entry is itself the declaration: `get_for_outage` hands back nothing
/// for a key whose client never asked for serve-stale, so admitting it here
/// cannot disclose a route the posture is meant to hide.
#[tokio::test]
async fn a_resilient_entry_outlives_its_client_under_the_closed_posture() {
  let mut cfg = crate::test_support::test_config();
  cfg.cache_enabled = true;
  cfg.cache_max_stale = 3600;
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(crate::test_support::test_state_with(cfg));

  // Stored as a client with `resilience: true` would have stored it, then
  // expired. No client is connected, which is the whole point.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(Some("resilient.example"), "/data"),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    axum::body::Bytes::from_static(b"cached body"),
    std::time::Duration::from_millis(1),
    64 * 1024 * 1024,
    true,
    std::time::Duration::ZERO,
    Vec::new(),
  );
  tokio::time::sleep(std::time::Duration::from_millis(20)).await;

  let mut req = get("/data");
  req.headers_mut().insert(
    "host",
    axum::http::HeaderValue::from_static("resilient.example"),
  );
  let resp = run(state.clone(), req).await;

  assert_eq!(
    resp.status(),
    axum::http::StatusCode::OK,
    "the stale entry answers instead of the posture's stealth 504"
  );
  assert_eq!(
    resp
      .headers()
      .get("x-aperio-stale")
      .map(|v| v.to_str().unwrap()),
    Some("true"),
    "and it says it is stale"
  );
}

/// The same request without a resilient entry still gets the posture's answer.
///
/// Without this the fix above would read as "the closed posture does not apply
/// to cacheable routes", which is not what it says. A key nothing stored, or
/// one stored by a client that never asked for serve-stale, stays hidden.
#[tokio::test]
async fn a_route_with_no_resilient_entry_still_gets_the_stealth_refusal() {
  let mut cfg = crate::test_support::test_config();
  cfg.cache_enabled = true;
  cfg.cache_max_stale = 3600;
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(crate::test_support::test_state_with(cfg));

  // Present, expired, and *not* resilient: the client never asked.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(Some("plain.example"), "/data"),
    200,
    vec![],
    axum::body::Bytes::from_static(b"cached body"),
    std::time::Duration::from_millis(1),
    64 * 1024 * 1024,
    false,
    std::time::Duration::ZERO,
    Vec::new(),
  );
  tokio::time::sleep(std::time::Duration::from_millis(20)).await;

  let mut req = get("/data");
  req.headers_mut().insert(
    "host",
    axum::http::HeaderValue::from_static("plain.example"),
  );
  let resp = run(state, req).await;
  assert_eq!(resp.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
}

/// A failure is charged to the service that failed, across a reorder.
///
/// The dispatch captures which service it chose, the timeout fires up to
/// thirty seconds later, and any heartbeat in between can rebuild the
/// connection's service list: `match_declarations` is built to survive a
/// client reordering its `services:` block, so indexes move. Charging by
/// index then ejects a healthy neighbour and leaves the failing service
/// serving, which is the worst of both and reports nothing.
#[tokio::test]
async fn an_ejection_follows_the_service_it_belongs_to_not_its_old_index() {
  let mut cfg = crate::test_support::test_config();
  cfg.outlier_ejection = true;
  cfg.outlier_max_failures = 1;
  cfg.outlier_window = std::time::Duration::from_secs(60);
  cfg.outlier_eject = std::time::Duration::from_secs(60);
  let state = Arc::new(crate::test_support::test_state_with(cfg));

  {
    let mut handle = crate::test_support::mock_client(None, None, None, None);
    handle.sole_mut().service_name = Some("api".to_string());
    let mut web = crate::state::ServiceState::newly_declared(Default::default());
    web.service_name = Some("web".to_string());
    handle.services.push(web);
    state.clients.write().await.insert("c1".to_string(), handle);
  }

  // The dispatch chose "web", which was index 1. A heartbeat then reordered
  // the list, exactly as a client editing its config produces.
  {
    let mut clients = state.clients.write().await;
    clients.get_mut("c1").unwrap().services.reverse();
  }

  record_outlier_failure(&state, "c1", Some("web")).await;

  let now = Instant::now();
  let clients = state.clients.read().await;
  let handle = clients.get("c1").unwrap();
  let web = handle
    .services
    .iter()
    .find(|s| s.service_name.as_deref() == Some("web"))
    .unwrap();
  let api = handle
    .services
    .iter()
    .find(|s| s.service_name.as_deref() == Some("api"))
    .unwrap();
  assert!(
    web.is_ejected(now),
    "the service that failed is the one ejected"
  );
  assert!(!api.is_ejected(now), "and its neighbour is left serving");
}
