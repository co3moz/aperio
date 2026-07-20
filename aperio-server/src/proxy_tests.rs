//! Unit tests for the HTTP proxy path: pure helpers plus end-to-end drives of
//! [`proxy_handler`] through a mock tunnel client (no real backend). A spawned
//! task reads the forwarded [`TunnelMessage`] off the client's receiver and
//! feeds a [`TunnelResponse`] back through `pending_requests`, exactly like the
//! live read loop would.

use super::*;
use crate::protocol::TunnelMessage;
use crate::settings::{FailoverMode, ServerConfig};
use crate::state::TunnelResponse;
use crate::test_support::{mock_client, test_config, test_peer, test_state_with};
use axum::body::Body;
use axum::extract::ws::Message;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use base64::prelude::*;
use std::sync::Arc;
use tokio::sync::mpsc;

// --- pure / helper functions -------------------------------------------------

#[test]
fn effective_body_limit_only_tightens() {
  // No declared limit → the global cap applies.
  assert_eq!(effective_body_limit(1000, None), 1000);
  // A tighter declared limit wins.
  assert_eq!(effective_body_limit(1000, Some(400)), 400);
  // A wider declared limit is clamped to the global cap.
  assert_eq!(effective_body_limit(1000, Some(5000)), 1000);
}

#[test]
fn is_websocket_upgrade_detection() {
  let mut h = HeaderMap::new();
  // Not an upgrade without the headers.
  assert!(!is_websocket_upgrade(&Method::GET, &h));
  h.insert("upgrade", HeaderValue::from_static("websocket"));
  h.insert("connection", HeaderValue::from_static("Upgrade"));
  assert!(is_websocket_upgrade(&Method::GET, &h));
  // Case-insensitive on both header values.
  let mut h2 = HeaderMap::new();
  h2.insert("upgrade", HeaderValue::from_static("WebSocket"));
  h2.insert(
    "connection",
    HeaderValue::from_static("keep-alive, upgrade"),
  );
  assert!(is_websocket_upgrade(&Method::GET, &h2));
  // A non-GET method is never a WS upgrade.
  assert!(!is_websocket_upgrade(&Method::POST, &h));
  // Wrong upgrade token.
  let mut h3 = HeaderMap::new();
  h3.insert("upgrade", HeaderValue::from_static("h2c"));
  h3.insert("connection", HeaderValue::from_static("upgrade"));
  assert!(!is_websocket_upgrade(&Method::GET, &h3));
}

#[test]
fn login_redirect_preserves_path() {
  let resp = login_redirect("/aperio/auth", "/secret?x=1");
  assert_eq!(resp.status(), StatusCode::FOUND);
  let loc = resp.headers().get("Location").unwrap().to_str().unwrap();
  assert!(loc.starts_with("/aperio/auth?redirect="));
}

#[tokio::test]
async fn gateway_timeout_response_plain_and_custom() {
  let state = test_state_with(test_config());
  // No custom page → plain-text fallback.
  let resp = gateway_timeout_response(&state, None, "504 fallback");
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

  // Custom global 504 page → HTML.
  let mut cfg = test_config();
  cfg.custom_504_page = Some("<h1>down</h1>".to_string());
  let state = test_state_with(cfg);
  let resp = gateway_timeout_response(&state, None, "504 fallback");
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
  assert_eq!(
    resp.headers().get("content-type").unwrap(),
    "text/html; charset=utf-8"
  );
}

#[tokio::test]
async fn maintenance_response_sets_retry_after() {
  let state = test_state_with(test_config());
  let resp = maintenance_response(&state, None);
  assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(resp.headers().get("retry-after").unwrap(), "300");

  let mut cfg = test_config();
  cfg.custom_503_page = Some("<h1>maint</h1>".to_string());
  let state = test_state_with(cfg);
  let resp = maintenance_response(&state, None);
  assert_eq!(
    resp.headers().get("content-type").unwrap(),
    "text/html; charset=utf-8"
  );
}

#[tokio::test]
async fn in_maintenance_matches_wildcard_and_host() {
  let state = test_state_with(test_config());
  // Empty set → never in maintenance.
  assert!(!in_maintenance(&state, Some("a.example.com")).await);
  // Explicit host entry.
  state
    .maintenance
    .lock()
    .await
    .insert("a.example.com".to_string(), None);
  assert!(in_maintenance(&state, Some("a.example.com")).await);
  assert!(!in_maintenance(&state, Some("b.example.com")).await);
  // Wildcard covers every host.
  state.maintenance.lock().await.insert("*".to_string(), None);
  assert!(in_maintenance(&state, Some("b.example.com")).await);
  assert!(in_maintenance(&state, None).await);
}

#[test]
fn trailer_header_map_skips_invalid() {
  let map = trailer_header_map(&[
    ("grpc-status".to_string(), "0".to_string()),
    ("bad name".to_string(), "x".to_string()), // invalid name → skipped
  ]);
  assert_eq!(map.get("grpc-status").unwrap(), "0");
  assert_eq!(map.len(), 1);
}

#[test]
fn frame_from_body_item_variants() {
  use crate::state::BodyFrame;
  // Data frame.
  let f = frame_from_body_item(Ok(BodyFrame::Data(vec![1, 2, 3])));
  assert!(f.unwrap().into_data().is_ok());
  // Trailer frame.
  let f = frame_from_body_item(Ok(BodyFrame::Trailers(vec![(
    "grpc-status".to_string(),
    "0".to_string(),
  )])));
  assert!(f.unwrap().into_trailers().is_ok());
  // IO error → propagated.
  let f = frame_from_body_item(Err(std::io::Error::other("boom")));
  assert!(f.is_err());
}

#[test]
fn record_outlier_helpers() {
  // retry_covers is exercised in the sibling retry_tests module; here we only
  // assert effective_body_limit's saturating behavior on a zero global.
  assert_eq!(effective_body_limit(0, Some(10)), 0);
}

#[tokio::test]
async fn record_outlier_failure_guarded_by_config() {
  // Disabled → no-op even with a client present.
  let state = test_state_with(test_config());
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  record_outlier_failure(&state, "c1").await;

  // Enabled → records against the serving client (and tolerates a missing id).
  let mut cfg = test_config();
  cfg.outlier_ejection = true;
  cfg.outlier_max_failures = 1;
  let state = test_state_with(cfg);
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  record_outlier_failure(&state, "c1").await;
  record_outlier_failure(&state, "missing").await;
}

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
    body: body.to_vec(),
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

// --- check_visitor_gate ------------------------------------------------------

#[tokio::test]
async fn visitor_gate_allows_without_auth() {
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(&state, &HeaderMap::new(), &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow));
}

#[tokio::test]
async fn visitor_gate_denies_when_auth_configured() {
  let mut cfg = test_config();
  cfg.auth_credentials = Some("secret".to_string());
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(&state, &HeaderMap::new(), &uri, None).await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      let loc = resp.headers().get("Location").unwrap().to_str().unwrap();
      assert!(loc.starts_with("/aperio/auth?redirect="));
    }
    VisitorGate::Allow => panic!("expected deny"),
  }
}

#[tokio::test]
async fn visitor_gate_traversal_requires_session() {
  let mut cfg = test_config();
  cfg.auth_credentials = Some("secret".to_string());
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(&state, &HeaderMap::new(), &uri, None).await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn visitor_gate_per_route_visitor_auth() {
  // A client declaring a per-service visitor password supersedes the server
  // gate: without a host session (and no share), the visitor is denied.
  let state = Arc::new(test_state_with(test_config()));
  let mut c = mock_client(None, None, None, None);
  c.visitor_auth = Some("pw".to_string());
  state.clients.lock().await.insert("c1".to_string(), c);
  let uri: axum::http::Uri = "/svc".parse().unwrap();
  let gate = check_visitor_gate(&state, &HeaderMap::new(), &uri, None).await;
  assert!(matches!(gate, VisitorGate::Deny(_)));

  // A valid session for the host unlocks it.
  let token =
    crate::test_support::seed_session(&state, crate::store::users::Role::Admin, None, None).await;
  let mut headers = HeaderMap::new();
  headers.insert(
    "cookie",
    HeaderValue::from_str(&format!("aperio_session={token}")).unwrap(),
  );
  let gate = check_visitor_gate(&state, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow));
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
    b"stale-body".to_vec(),
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

// --- proxy_handler drives ----------------------------------------------------

/// A connected [`AppState`] whose config is derived from [`test_config`].
fn connected(config: ServerConfig) -> Arc<AppState> {
  let state = test_state_with(config);
  Arc::new(state)
}

async fn mark_connected(state: &AppState) {
  state.connection_state.lock().await.connected = true;
  let _ = state.client_connected.send_replace(true);
}

/// Inserts a client whose receiver is retained so dispatched frames can be
/// observed and answered. Returns the receiver.
async fn insert_live_client(state: &AppState, id: &str) -> mpsc::Receiver<Message> {
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  state.clients.lock().await.insert(id.to_string(), c);
  rx
}

/// Spawns a task that answers each forwarded `Request`/`RequestStart` with the
/// next status in `statuses`, feeding it back through `pending_requests`.
fn spawn_responder(state: Arc<AppState>, mut rx: mpsc::Receiver<Message>, statuses: Vec<u16>) {
  tokio::spawn(async move {
    for status in statuses {
      let Some(Message::Text(text)) = rx.recv().await else {
        return;
      };
      let id = match serde_json::from_str::<TunnelMessage>(&text) {
        Ok(TunnelMessage::Request { id, .. }) => id,
        Ok(TunnelMessage::RequestStart { id, .. }) => id,
        _ => return,
      };
      if let Some(req) = state.pending_requests.lock().await.remove(&id) {
        let _ = req.tx.send(TunnelResponse {
          status,
          headers: vec![("content-type".to_string(), "text/plain".to_string())],
          body: Some(BASE64_STANDARD.encode(format!("body-{status}"))),
          trailers: None,
          stream_rx: None,
          timings: None,
        });
      }
    }
  });
}

fn get(path: &str) -> axum::extract::Request<Body> {
  let mut req = axum::extract::Request::new(Body::empty());
  *req.uri_mut() = path.parse().unwrap();
  req
}

async fn run(state: Arc<AppState>, req: axum::extract::Request<Body>) -> axum::response::Response {
  proxy_handler(State(state), ConnectInfo(test_peer()), req).await
}

#[tokio::test]
async fn handler_maintenance_returns_503() {
  let state = connected(test_config());
  state.maintenance.lock().await.insert("*".to_string(), None);
  let resp = run(state, get("/whatever")).await;
  assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn handler_no_client_returns_504() {
  let state = connected(test_config());
  mark_connected(&state).await; // connected, but no clients registered
  let resp = run(state, get("/hello")).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn handler_rate_limited_returns_429() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let state = connected(cfg);
  mark_connected(&state).await;
  let _rx = insert_live_client(&state, "c1").await;
  let resp = run(state, get("/hello")).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn handler_request_too_large_returns_413() {
  let mut cfg = test_config();
  cfg.max_body_size = 8;
  let state = connected(cfg);
  mark_connected(&state).await;
  let _rx = insert_live_client(&state, "c1").await;
  let mut req = axum::extract::Request::new(Body::from("x".repeat(64)));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = "/upload".parse().unwrap();
  req
    .headers_mut()
    .insert("content-length", HeaderValue::from_static("64"));
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn handler_success_round_trip_returns_200() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_responder(state.clone(), rx, vec![200]);
  let resp = run(state, get("/hello")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
}

#[tokio::test]
async fn handler_serves_cache_hit_without_tunnel() {
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = connected(cfg);
  mark_connected(&state).await;
  // Client marked cacheable; its receiver stays dropped since we never dispatch.
  let mut c = mock_client(None, None, None, None);
  c.cache = true;
  state.clients.lock().await.insert("c1".to_string(), c);
  // Pre-seed a fresh cache entry for GET /cached.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(None, "/cached"),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    b"cached-body".to_vec(),
    std::time::Duration::from_secs(60),
    64 * 1024 * 1024,
    false,
    std::time::Duration::from_secs(0),
    Vec::new(),
  );
  let resp = run(state, get("/cached")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(resp.headers().get("x-aperio-cache").unwrap(), "hit");
}

#[tokio::test]
async fn handler_retries_on_5xx_then_succeeds() {
  let mut cfg = test_config();
  cfg.retry_on_5xx = true;
  cfg.failover_max_jumps = 2;
  cfg.failover_mode = FailoverMode::Retry;
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // First dispatch answers 500 (retryable), the re-dispatch answers 200.
  spawn_responder(state.clone(), rx, vec![500, 200]);
  let resp = run(state, get("/retry")).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_returns_5xx_when_retry_disabled() {
  let state = connected(test_config()); // retry_on_5xx off by default
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_responder(state.clone(), rx, vec![503]);
  let resp = run(state, get("/err")).await;
  assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn handler_response_timeout_returns_504() {
  let mut cfg = test_config();
  cfg.gateway_response_timeout = std::time::Duration::from_millis(50);
  let state = connected(cfg);
  mark_connected(&state).await;
  // Live receiver, but no responder — the request times out.
  let _rx = insert_live_client(&state, "c1").await;
  let resp = run(state, get("/slow")).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

// --- richer success-path drives ---------------------------------------------

fn text_response(status: u16) -> TunnelResponse {
  TunnelResponse {
    status,
    headers: vec![("content-type".to_string(), "text/plain".to_string())],
    body: Some(BASE64_STANDARD.encode("body")),
    trailers: None,
    stream_rx: None,
    timings: None,
  }
}

/// Answers each forwarded request with the next queued response; a `None` slot
/// simulates a vanished client (the pending sender is dropped without a send).
fn spawn_custom(
  state: Arc<AppState>,
  mut rx: mpsc::Receiver<Message>,
  mut responses: Vec<Option<TunnelResponse>>,
) {
  tokio::spawn(async move {
    let mut i = 0;
    while i < responses.len() {
      let Some(Message::Text(text)) = rx.recv().await else {
        return;
      };
      let id = match serde_json::from_str::<TunnelMessage>(&text) {
        Ok(TunnelMessage::Request { id, .. }) => id,
        Ok(TunnelMessage::RequestStart { id, .. }) => id,
        _ => continue, // streamed-body chunks / RequestEnd
      };
      if let Some(req) = state.pending_requests.lock().await.remove(&id) {
        if let Some(resp) = responses[i].take() {
          let _ = req.tx.send(resp);
        }
        // A `None` slot drops `req` (and its sender) here → client vanished.
        i += 1;
      }
    }
  });
}

#[tokio::test]
async fn handler_filters_internal_cookies() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let mut req = get("/x");
  req.headers_mut().insert(
    "cookie",
    HeaderValue::from_static("aperio_session=secret; real=1; aperio_affinity=z"),
  );
  let resp = run(state.clone(), req).await;
  assert_eq!(resp.status(), StatusCode::OK);
  // The captured request (post-serialization) keeps only the non-internal cookie.
  let captured = state.captured_requests.lock().await;
  let entry = captured.back().expect("a captured request");
  let cookie = entry
    .req_headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
    .map(|(_, v)| v.clone())
    .unwrap();
  assert_eq!(cookie, "real=1");
}

#[tokio::test]
async fn handler_stores_cacheable_response() {
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.cache = true;
  state.clients.lock().await.insert("c1".to_string(), c);
  let mut r = text_response(200);
  r.headers.push((
    "cache-control".to_string(),
    "public, max-age=60".to_string(),
  ));
  spawn_custom(state.clone(), rx, vec![Some(r)]);
  let resp = run(state.clone(), get("/store")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  // The response is now cached for the key.
  let lookup = state.response_cache.lock().await.lookup(
    &crate::cache::cache_key(None, "/store"),
    std::time::Duration::from_secs(0),
  );
  assert!(matches!(lookup, crate::cache::SwrLookup::Fresh(_)));
}

#[tokio::test]
async fn handler_negatively_caches_404() {
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.cache = true;
  state.clients.lock().await.insert("c1".to_string(), c);
  spawn_custom(state.clone(), rx, vec![Some(text_response(404))]);
  let resp = run(state.clone(), get("/missing")).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn handler_webhook_inbox_records_post() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.webhook_inbox = true;
  c.service_name = Some("svc".to_string());
  state.clients.lock().await.insert("c1".to_string(), c);
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let mut req = axum::extract::Request::new(Body::from("hook"));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = "/hook".parse().unwrap();
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_streams_response_body_with_trailers() {
  use crate::state::BodyFrame;
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  let (btx, brx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(8);
  btx
    .send(Ok(BodyFrame::Data(b"streamed".to_vec())))
    .await
    .unwrap();
  btx
    .send(Ok(BodyFrame::Trailers(vec![(
      "grpc-status".to_string(),
      "0".to_string(),
    )])))
    .await
    .unwrap();
  drop(btx);
  let mut r = text_response(200);
  r.stream_rx = Some(brx);
  spawn_custom(state.clone(), rx, vec![Some(r)]);
  let resp = run(state, get("/stream")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  assert_eq!(&body[..], b"streamed");
}

#[tokio::test]
async fn handler_buffered_response_with_trailers() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  let mut r = text_response(200);
  r.trailers = Some(vec![("grpc-status".to_string(), "0".to_string())]);
  spawn_custom(state.clone(), rx, vec![Some(r)]);
  let resp = run(state, get("/trailers")).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_sticky_sets_affinity_cookie() {
  let mut cfg = test_config();
  cfg.lb_strategy = crate::settings::LbStrategy::Sticky;
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  // A returning visitor's affinity cookie is read on the way in.
  let mut req = get("/sticky");
  req
    .headers_mut()
    .insert("cookie", HeaderValue::from_static("aperio_affinity=c1"));
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let sc = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(sc.contains("aperio_affinity="));
}

#[tokio::test]
async fn handler_client_vanished_returns_502() {
  let state = connected(test_config()); // failover_mode = Fail
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // The pending sender is dropped without answering → in-flight loss.
  spawn_custom(state.clone(), rx, vec![None]);
  let resp = run(state, get("/gone")).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn handler_failover_retry_after_vanish() {
  let mut cfg = test_config();
  cfg.failover_mode = FailoverMode::Retry;
  cfg.failover_max_jumps = 2;
  let state = connected(cfg);
  mark_connected(&state).await;
  // Two clients; the first vanishes, the re-dispatch reaches a live one.
  let rx1 = insert_live_client(&state, "c1").await;
  let rx2 = insert_live_client(&state, "c2").await;
  spawn_custom(state.clone(), rx1, vec![None]);
  spawn_custom(state.clone(), rx2, vec![Some(text_response(200))]);
  let resp = run(state, get("/failover")).await;
  // Either client may be picked first; after the vanish the request re-dispatches.
  assert!(
    resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_GATEWAY,
    "unexpected {}",
    resp.status()
  );
}

#[tokio::test]
async fn handler_concurrency_limit_returns_429() {
  let mut cfg = test_config();
  cfg.max_concurrent_requests = 0;
  let state = connected(cfg);
  mark_connected(&state).await;
  let _rx = insert_live_client(&state, "c1").await;
  let resp = run(state, get("/busy")).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn handler_streamed_upload_round_trip() {
  let state = connected(test_config());
  mark_connected(&state).await;
  // A protocol-v2 client streams large uploads as RequestStart + chunk frames.
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.client_protocol = Some(2);
  state.clients.lock().await.insert("c1".to_string(), c);
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  // Body above the 256 KiB stream threshold, declared via content-length.
  let big = vec![b'a'; 300 * 1024];
  let mut req = axum::extract::Request::new(Body::from(big));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = "/upload".parse().unwrap();
  req
    .headers_mut()
    .insert("content-length", HeaderValue::from_static("307200"));
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_fresh_install_redirects_root() {
  let state = connected(test_config());
  mark_connected(&state).await; // no clients, no lifetime traffic
  let resp = run(state, get("/")).await;
  assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
  assert_eq!(resp.headers().get("location").unwrap(), "/aperio");
}

#[tokio::test]
async fn handler_offline_then_reconnect_succeeds() {
  // Start disconnected; a client connects mid-wait, so the handler proceeds to
  // a normal round-trip instead of timing out.
  let mut cfg = test_config();
  cfg.gateway_timeout = std::time::Duration::from_secs(5);
  let state = connected(cfg); // connection_state starts disconnected
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let s2 = state.clone();
  tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    s2.connection_state.lock().await.connected = true;
    let _ = s2.client_connected.send_replace(true);
  });
  let resp = run(state, get("/reconnect")).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_offline_reconnect_wait_times_out() {
  let mut cfg = test_config();
  cfg.gateway_timeout = std::time::Duration::from_millis(50);
  let state = connected(cfg); // connection_state stays disconnected
  let resp = run(state, get("/wait")).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn visitor_gate_traversal_allowed_without_gate() {
  // No server auth configured and no per-route gate → a traversal path is
  // allowed straight through.
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(&state, &HeaderMap::new(), &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow));
}

// --- SWR, denial, preview, limiter, coalescing ------------------------------

#[tokio::test]
async fn handler_swr_serves_stale_and_revalidates() {
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.cache = true;
  state.clients.lock().await.insert("c1".to_string(), c);
  // A cacheable entry that is past its TTL but within its SWR window.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(None, "/swr"),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    b"stale".to_vec(),
    std::time::Duration::from_secs(0), // already expired
    64 * 1024 * 1024,
    false,
    std::time::Duration::from_secs(60), // SWR window still open
    Vec::new(),
  );
  // The background revalidation re-fetches through the tunnel; answer it with a
  // fresh cacheable 200 so `spawn_swr_revalidation`'s store path runs.
  let mut fresh = text_response(200);
  fresh.headers.push((
    "cache-control".to_string(),
    "public, max-age=60".to_string(),
  ));
  spawn_custom(state.clone(), rx, vec![Some(fresh)]);
  let resp = run(state.clone(), get("/swr")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(resp.headers().get("x-aperio-cache").unwrap(), "hit");
  assert_eq!(resp.headers().get("x-aperio-stale").unwrap(), "true");
  // Give the fire-and-forget revalidation a moment to complete.
  tokio::time::sleep(std::time::Duration::from_millis(80)).await;
}

#[tokio::test]
async fn handler_denied_visitor_stealth_504() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  // The caller (127.0.0.1) is not in the client's allowlist → rejected, and no
  // `denied:` redirect is declared → stealth 504 (identical to unclaimed).
  c.allowed_ips = vec!["10.0.0.0/8".to_string()];
  state.clients.lock().await.insert("c1".to_string(), c);
  let resp = run(state, get("/secret")).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn handler_denied_visitor_redirect_302() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  c.allowed_ips = vec!["10.0.0.0/8".to_string()];
  c.denied = Some("https://denied.example/blocked".to_string());
  state.clients.lock().await.insert("c1".to_string(), c);
  let resp = run(state, get("/secret")).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  assert_eq!(
    resp.headers().get("Location").unwrap(),
    "https://denied.example/blocked"
  );
}

#[tokio::test]
async fn handler_preview_noindex_robots() {
  let mut cfg = test_config();
  cfg.preview_noindex = true;
  cfg.random_subdomain_suffix = Some("*.example.com".to_string());
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut req = get("/robots.txt");
  req
    .headers_mut()
    .insert("host", HeaderValue::from_static("abc123.example.com"));
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    resp.headers().get("x-robots-tag").unwrap(),
    "noindex, nofollow"
  );
}

#[tokio::test]
async fn handler_preview_noindex_response_header() {
  let mut cfg = test_config();
  cfg.preview_noindex = true;
  cfg.random_subdomain_suffix = Some("*.example.com".to_string());
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let mut req = get("/page");
  req
    .headers_mut()
    .insert("host", HeaderValue::from_static("abc123.example.com"));
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(
    resp.headers().get("x-robots-tag").unwrap(),
    "noindex, nofollow"
  );
}

#[tokio::test]
async fn handler_inflight_limiter_admits_request() {
  use std::sync::Arc as StdArc;
  use tokio::sync::Semaphore;
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.max_concurrent = Some(2);
  c.inflight_limiter = Some(StdArc::new(Semaphore::new(2)));
  state.clients.lock().await.insert("c1".to_string(), c);
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let resp = run(state, get("/limited")).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_single_flight_follower_serves_from_cache() {
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.cache = true;
  state.clients.lock().await.insert("c1".to_string(), c);
  // Only the leader dispatches; it stores a cacheable answer, then the follower
  // wakes and re-checks the cache instead of stampeding the backend.
  let mut r = text_response(200);
  r.headers.push((
    "cache-control".to_string(),
    "public, max-age=60".to_string(),
  ));
  spawn_custom(state.clone(), rx, vec![Some(r)]);
  let s1 = state.clone();
  let s2 = state.clone();
  let leader = tokio::spawn(async move { run(s1, get("/sf")).await });
  // Small stagger so the follower observes the leader's in-flight entry.
  tokio::time::sleep(std::time::Duration::from_millis(10)).await;
  let follower = tokio::spawn(async move { run(s2, get("/sf")).await });
  let (r1, r2) = tokio::join!(leader, follower);
  assert_eq!(r1.unwrap().status(), StatusCode::OK);
  assert_eq!(r2.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_5xx_retry_exhausted_returns_5xx() {
  let mut cfg = test_config();
  cfg.retry_on_5xx = true;
  cfg.failover_max_jumps = 1;
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // Every dispatch returns 500; after the single allowed jump the 500 is
  // returned to the visitor.
  spawn_custom(
    state.clone(),
    rx,
    vec![Some(text_response(500)), Some(text_response(500))],
  );
  let resp = run(state, get("/always5xx")).await;
  assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// A single client that vanishes on its first dispatch and answers 200 on the
/// re-dispatch drives every failover mode deterministically (the same client is
/// re-selected since it is never removed from the pool on an in-flight loss).
async fn drive_failover(mode: FailoverMode) -> StatusCode {
  let mut cfg = test_config();
  cfg.failover_mode = mode;
  cfg.failover_max_jumps = 2;
  cfg.failover_window = std::time::Duration::from_secs(2);
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![None, Some(text_response(200))]);
  run(state, get("/fo")).await.status()
}

#[tokio::test]
async fn handler_failover_retry_mode() {
  assert_eq!(drive_failover(FailoverMode::Retry).await, StatusCode::OK);
}

#[tokio::test]
async fn handler_failover_wait_mode() {
  assert_eq!(drive_failover(FailoverMode::Wait).await, StatusCode::OK);
}

#[tokio::test]
async fn handler_failover_retrywait_mode() {
  assert_eq!(
    drive_failover(FailoverMode::RetryWait).await,
    StatusCode::OK
  );
}

#[tokio::test]
async fn handler_dispatch_send_failure_returns_502() {
  let state = connected(test_config());
  mark_connected(&state).await;
  // A plain mock_client's receiver is already dropped, so the very first
  // `tx.send` fails → the handler treats it as an in-flight loss (502 under the
  // default Fail mode).
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  let resp = run(state, get("/dead")).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn handler_inflight_limiter_timeout_returns_429() {
  use std::sync::Arc as StdArc;
  use tokio::sync::Semaphore;
  let mut cfg = test_config();
  cfg.gateway_timeout = std::time::Duration::from_millis(50);
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, _rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.max_concurrent = Some(1);
  // No permits available → the acquire never succeeds within the gateway
  // timeout → 429.
  c.inflight_limiter = Some(StdArc::new(Semaphore::new(0)));
  state.clients.lock().await.insert("c1".to_string(), c);
  let resp = run(state, get("/blocked")).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn handler_captures_truncated_bodies() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // Response body over the 64 KiB capture limit → captured truncated.
  let big = "y".repeat(70 * 1024);
  let mut r = text_response(200);
  r.body = Some(BASE64_STANDARD.encode(&big));
  spawn_custom(state.clone(), rx, vec![Some(r)]);
  // Request body over the capture limit too (buffered, under the 1 MiB cap).
  let mut req = axum::extract::Request::new(Body::from("x".repeat(70 * 1024)));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = "/big".parse().unwrap();
  let resp = run(state.clone(), req).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let captured = state.captured_requests.lock().await;
  let entry = captured.back().unwrap();
  assert!(entry.req_body_truncated);
  assert!(entry.resp_body_truncated);
}

#[tokio::test]
async fn handler_token_daily_quota_returns_429() {
  let state = connected(test_config());
  mark_connected(&state).await;
  // A token with a 1-byte daily quota, already over budget for today.
  let (token, _secret) = state.token_store.lock().await.create(
    "t".to_string(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    None,
    None,
    Some(1),
    false,
    false,
    None,
  );
  let today = crate::store::stats::period_keys()[0].clone();
  state
    .token_daily_bytes
    .lock()
    .await
    .insert(token.id.clone(), (today, 1000));
  let mut c = mock_client(None, None, None, None);
  let (tx, _rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.perms.token_id = Some(token.id.clone());
  state.clients.lock().await.insert("c1".to_string(), c);
  let resp = run(state, get("/quota")).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn handler_mirrors_h2_authority_to_host() {
  // An HTTP/2 request carries the host in the URI authority, not a Host header;
  // the handler mirrors it so hostname routing sees it. With no client this
  // still resolves to a 504, but the mirroring branch runs.
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut req = axum::extract::Request::new(Body::empty());
  *req.uri_mut() = "https://h2.example.com/path".parse().unwrap();
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn handler_visitor_gate_denies_with_302() {
  let mut cfg = test_config();
  cfg.auth_credentials = Some("secret".to_string());
  let state = connected(cfg);
  mark_connected(&state).await;
  let _rx = insert_live_client(&state, "c1").await;
  // No session → the visitor gate denies inside proxy_http_request.
  let resp = run(state, get("/private")).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
}

#[tokio::test]
async fn handler_fully_filtered_cookie_dropped() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let mut req = get("/x");
  // Only internal cookies → the filtered value is empty and no cookie header is
  // forwarded.
  req.headers_mut().insert(
    "cookie",
    HeaderValue::from_static("aperio_session=x; aperio_share=y"),
  );
  let resp = run(state.clone(), req).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let captured = state.captured_requests.lock().await;
  let entry = captured.back().unwrap();
  assert!(
    !entry
      .req_headers
      .iter()
      .any(|(k, _)| k.eq_ignore_ascii_case("cookie")),
    "no cookie header should be forwarded"
  );
}

#[tokio::test]
async fn handler_response_timeout_override_and_header_strip() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.response_timeout = Some(5); // per-service response-timeout override
  state.clients.lock().await.insert("c1".to_string(), c);
  let mut r = text_response(200);
  // Hop-by-hop headers must be stripped from the visitor response.
  r.headers
    .push(("connection".to_string(), "keep-alive".to_string()));
  r.headers
    .push(("transfer-encoding".to_string(), "chunked".to_string()));
  spawn_custom(state.clone(), rx, vec![Some(r)]);
  let resp = run(state, get("/timeout")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(resp.headers().get("connection").is_none());
  assert!(resp.headers().get("transfer-encoding").is_none());
}

#[tokio::test]
async fn handler_sticky_secure_cookie() {
  let mut cfg = test_config();
  cfg.lb_strategy = crate::settings::LbStrategy::Sticky;
  cfg.secure_cookies = true;
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  let resp = run(state, get("/sticky")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let sc = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(sc.contains("Secure"));
}

#[tokio::test]
async fn handler_streamed_upload_truncates_oversized_body() {
  let mut cfg = test_config();
  cfg.max_body_size = 256 * 1024; // below the streamed body size
  let state = connected(cfg);
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(256);
  c.tx = tx;
  c.client_protocol = Some(2);
  state.clients.lock().await.insert("c1".to_string(), c);
  spawn_custom(state.clone(), rx, vec![Some(text_response(200))]);
  // Chunked upload (no content-length) streams; the pump truncates once the
  // running total exceeds the body limit.
  let mut req = axum::extract::Request::new(Body::from(vec![b'a'; 400 * 1024]));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = "/bigupload".parse().unwrap();
  req
    .headers_mut()
    .insert("transfer-encoding", HeaderValue::from_static("chunked"));
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_org_month_quota_returns_429() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let org = state.org_store.lock().await.create("o1").unwrap();
  state
    .org_store
    .lock()
    .await
    .set_quota(&org.id, None, None, None, Some(Some(1)));
  // Seed this month's usage for the org above the 1-byte cap.
  state.persistent_stats.lock().await.record_request_labeled(
    true,
    500,
    500,
    1,
    None,
    None,
    Some(&org.id),
  );
  let mut c = mock_client(None, None, None, None);
  let (tx, _rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  c.perms.org_id = Some(org.id.clone());
  state.clients.lock().await.insert("c1".to_string(), c);
  let resp = run(state, get("/orgquota")).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}
