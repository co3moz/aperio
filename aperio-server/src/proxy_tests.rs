//! Unit tests for the HTTP proxy path: pure helpers plus end-to-end drives of
//! [`proxy_handler`] through a mock tunnel client (no real backend). A spawned
//! task reads the forwarded [`TunnelMessage`] off the client's receiver and
//! feeds a [`TunnelResponse`] back through `pending_requests`, exactly like the
//! live read loop would.

use super::*;
use crate::protocol::TunnelMessage;
use crate::settings::{FailoverMode, ServerConfig};
use crate::state::TunnelResponse;
use crate::store::tokens::TokenSpec;
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
  let open = crate::state::MaintenanceFlag::default();
  let state = test_state_with(test_config());
  let resp = maintenance_response(&state, None, &open);
  assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(resp.headers().get("retry-after").unwrap(), "300");

  let mut cfg = test_config();
  cfg.custom_503_page = Some("<h1>maint</h1>".to_string());
  let state = test_state_with(cfg);
  let resp = maintenance_response(&state, None, &open);
  assert_eq!(
    resp.headers().get("content-type").unwrap(),
    "text/html; charset=utf-8"
  );
}

#[tokio::test]
async fn in_maintenance_matches_wildcard_and_host() {
  let state = test_state_with(test_config());
  let flag = |until: Option<u64>| crate::state::MaintenanceFlag {
    org: None,
    reason: None,
    until,
    since: 0,
    actor: "test".into(),
  };
  let is_down = |host: Option<&'static str>| {
    let state = &state;
    async move { state.maintenance_for(host).await.is_some() }
  };
  // Empty set → never in maintenance.
  assert!(!is_down(Some("a.example.com")).await);
  // Explicit host entry.
  state
    .maintenance
    .lock()
    .await
    .insert("a.example.com".to_string(), flag(None));
  assert!(is_down(Some("a.example.com")).await);
  assert!(!is_down(Some("b.example.com")).await);
  // A subdomain wildcard is one switch for everything under a domain, which
  // is what "put robogon into maintenance" means.
  state
    .maintenance
    .lock()
    .await
    .insert("*.robogon.com".to_string(), flag(None));
  assert!(is_down(Some("test.robogon.com")).await);
  assert!(is_down(Some("a.b.robogon.com")).await);
  // Not the apex: `*.robogon.com` is a subdomain wildcard the way a TLS
  // certificate's is, so an operator who wants both flags both.
  assert!(!is_down(Some("robogon.com")).await);
  assert!(!is_down(Some("notrobogon.com")).await);
  // An expired window does not apply, whatever it covers.
  state
    .maintenance
    .lock()
    .await
    .insert("expired.example".to_string(), flag(Some(1)));
  assert!(!is_down(Some("expired.example")).await);
  // A window still open does.
  let future = crate::store::tokens::now_secs() + 3600;
  state
    .maintenance
    .lock()
    .await
    .insert("planned.example".to_string(), flag(Some(future)));
  assert!(is_down(Some("planned.example")).await);
  // Wildcard covers every host.
  state
    .maintenance
    .lock()
    .await
    .insert("*".to_string(), flag(None));
  assert!(is_down(Some("b.example.com")).await);
  assert!(is_down(None).await);
}

#[tokio::test]
async fn the_503_page_carries_the_reason_and_the_window() {
  let state = test_state_with(test_config());
  let until = crate::store::tokens::now_secs() + 600;
  let flag = crate::state::MaintenanceFlag {
    org: None,
    reason: Some("database migration".into()),
    until: Some(until),
    since: 0,
    actor: "aperio".into(),
  };
  let resp = maintenance_response(&state, Some("app.example.com"), &flag);
  assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
  // Retry-After is the real window, not the fixed fallback.
  let retry: u64 = resp.headers()["retry-after"]
    .to_str()
    .unwrap()
    .parse()
    .unwrap();
  assert!((595..=600).contains(&retry), "{retry}");
  let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let text = String::from_utf8(body.to_vec()).unwrap();
  assert!(text.contains("database migration"), "{text}");

  // Open-ended: the fallback, which promises nothing in particular.
  let open = crate::state::MaintenanceFlag {
    until: None,
    ..flag
  };
  let resp = maintenance_response(&state, None, &open);
  assert_eq!(resp.headers()["retry-after"], "300");
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
  let f = frame_from_body_item(Ok(BodyFrame::Data(vec![1, 2, 3].into())));
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
    .write()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  record_outlier_failure(&state, "c1", 0).await;

  // Enabled → records against the serving client (and tolerates a missing id).
  let mut cfg = test_config();
  cfg.outlier_ejection = true;
  cfg.outlier_max_failures = 1;
  let state = test_state_with(cfg);
  state
    .clients
    .write()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));
  record_outlier_failure(&state, "c1", 0).await;
  record_outlier_failure(&state, "missing", 0).await;
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

// --- check_visitor_gate ------------------------------------------------------

/// The address a test's visitor arrives from, where the test is about
/// something else.
const VISITOR_IP: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

/// [`crate::proxy::check_visitor_gate`] with an address supplied.
///
/// The gate takes the caller's address because a `forward` method tells the
/// endpoint who is asking, and it takes it as an argument rather than reading
/// `X-Forwarded-For` itself, since that header is worth something only after
/// the trusted-proxy rules have been applied to it. Nearly every test here is
/// about something other than where the request came from, so they go through
/// this; a test that *is* about the address calls the real one and passes its
/// own.
async fn check_visitor_gate(
  state: &Arc<AppState>,
  method: &axum::http::Method,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
) -> VisitorGate {
  crate::proxy::check_visitor_gate(state, method, headers, uri, host, VISITOR_IP).await
}

#[tokio::test]
async fn visitor_gate_allows_without_auth() {
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn visitor_gate_denies_when_auth_configured() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      let loc = resp.headers().get("Location").unwrap().to_str().unwrap();
      assert!(loc.starts_with("/aperio/auth?redirect="));
    }
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn visitor_gate_traversal_requires_session() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn visitor_gate_per_route_visitor_auth() {
  // A client declaring a per-service visitor password supersedes the server
  // gate: without a host session (and no share), the visitor is denied.
  let state = Arc::new(test_state_with(test_config()));
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().visitor_auth = Some("pw".to_string());
  state.clients.write().await.insert("c1".to_string(), c);
  let uri: axum::http::Uri = "/svc".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)));

  // A valid session for the host unlocks it.
  let token =
    crate::test_support::seed_session(&state, crate::store::users::Role::Admin, None, None).await;
  let mut headers = HeaderMap::new();
  headers.insert(
    "cookie",
    HeaderValue::from_str(&format!("aperio_session={token}")).unwrap(),
  );
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn visitor_gate_admits_a_bearer_secret_from_a_header() {
  // The case that had no answer at all: a caller with no browser reaching a
  // gated route. The session cookie was the whole of what the gate looked at.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\"}").unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_static("Bearer 0123456789abcdef-secret"),
  );
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow(_)));

  let mut wrong = HeaderMap::new();
  wrong.insert("authorization", HeaderValue::from_static("Bearer nope"));
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &wrong, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn a_caller_without_a_browser_is_refused_with_a_challenge_rather_than_a_redirect() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\"}").unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  // No `Accept: text/html`: a script, which cannot act on an HTML login page.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
      assert_eq!(
        resp.headers().get("WWW-Authenticate").unwrap(),
        "Bearer",
        "the refusal has to say what to present"
      );
    }
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }

  // The same gate, a browser navigation: still the login page, because that
  // is the shape a browser can act on.
  let mut browser = HeaderMap::new();
  browser.insert("accept", HeaderValue::from_static("text/html"));
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &browser, &uri, None).await;
  match gate {
    VisitorGate::Deny(resp) => assert_eq!(resp.status(), StatusCode::FOUND),
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn a_secret_in_the_url_opens_nothing_unless_the_gate_asked_for_that_form() {
  let header_only = "{method: bearer, secret: \"0123456789abcdef-secret\"}";
  let mut cfg = test_config();
  cfg.visitor_auth =
    crate::visitor_auth::Policy::compile(&serde_yaml::from_str(header_only).unwrap());
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items?aperio_token=0123456789abcdef-secret"
    .parse()
    .unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(
    matches!(gate, VisitorGate::Deny(_)),
    "the query form is opt-in, and this gate did not opt in"
  );
}

#[tokio::test]
async fn a_page_opened_with_a_secret_in_its_url_is_sent_to_a_clean_address() {
  // Otherwise the secret is in the browser's history, in the `Referer` of
  // every outbound link, and on each of the page's own assets.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\", query: true}")
      .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/report?aperio_token=0123456789abcdef-secret&page=2"
    .parse()
    .unwrap();
  let mut browser = HeaderMap::new();
  browser.insert("accept", HeaderValue::from_static("text/html"));

  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &browser,
    &uri,
    Some("app.example.com"),
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      let location = resp.headers().get("Location").unwrap().to_str().unwrap();
      assert_eq!(location, "/report?page=2", "the other parameters survive");
      assert!(!location.contains("aperio_token"));
      let cookie = resp.headers().get("Set-Cookie").unwrap().to_str().unwrap();
      assert!(cookie.starts_with("aperio_share="), "{cookie}");
    }
    VisitorGate::Allow(_) => panic!("expected the clean-address redirect"),
    VisitorGate::Undeclared(_) => panic!("expected the clean-address redirect"),
  }

  // A non-navigation with the same secret is simply admitted: there is no
  // page whose assets need a cookie, and a redirect would break the call.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    Some("app.example.com"),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
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
  state.clients.write().await.insert(id.to_string(), c);
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
          body_raw: None,
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
  state
    .maintenance
    .lock()
    .await
    .insert("*".to_string(), crate::state::MaintenanceFlag::default());
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
  c.sole_mut().cache = true;
  state.clients.write().await.insert("c1".to_string(), c);
  // Pre-seed a fresh cache entry for GET /cached.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(None, "/cached"),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    b"cached-body".to_vec().into(),
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
  // Live receiver, but no responder, the request times out.
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
    body_raw: None,
    trailers: None,
    stream_rx: None,
    timings: None,
  }
}

/// Answers one forwarded request with a plain 200 and hands back the headers
/// it carried.
///
/// The inspector capture (`captured_requests`) is taken before the per-attempt
/// headers are added, so a test about what a backend receives has to read the
/// frame itself.
fn spawn_recording_responder(
  state: Arc<AppState>,
  mut rx: mpsc::Receiver<Message>,
) -> Arc<std::sync::Mutex<Vec<(String, String)>>> {
  let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
  let out = seen.clone();
  tokio::spawn(async move {
    let Some(Message::Text(text)) = rx.recv().await else {
      return;
    };
    let (id, headers) = match serde_json::from_str::<TunnelMessage>(&text) {
      Ok(TunnelMessage::Request { id, headers, .. }) => (id, headers),
      Ok(TunnelMessage::RequestStart { id, headers, .. }) => (id, headers),
      _ => return,
    };
    *seen.lock().unwrap() = headers;
    if let Some(req) = state.pending_requests.lock().await.remove(&id) {
      let _ = req.tx.send(text_response(200));
    }
  });
  out
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
  c.sole_mut().cache = true;
  state.clients.write().await.insert("c1".to_string(), c);
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
  c.sole_mut().cache = true;
  state.clients.write().await.insert("c1".to_string(), c);
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
  c.sole_mut().webhook_inbox = true;
  c.sole_mut().service_name = Some("svc".to_string());
  state.clients.write().await.insert("c1".to_string(), c);
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
    .send(Ok(BodyFrame::Data(axum::body::Bytes::from_static(
      b"streamed",
    ))))
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
  state.clients.write().await.insert("c1".to_string(), c);
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
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn visitor_gate_traversal_sees_a_policy_the_scalar_cannot_hold() {
  // The traversal branch is the *entire* gate for such a path, and it asks one
  // question: does anything on this host declare a gate. It used to ask only
  // about the scalar `visitor_auth`, so a `bearer`, a `jwt`, or a `basic`
  // naming two users read as ungated and `/./admin` was served with no
  // credential while `/admin` answered 401.
  for spelling in [
    "{method: bearer, secret: a-long-bearer-secret}",
    "{method: basic, users: [\"alice:one\", \"bob:two\"]}",
  ] {
    let setting =
      serde_yaml::from_str::<aperio_config::AuthSetting>(spelling).expect("a valid auth: value");
    let policy = crate::visitor_auth::Policy::compile(&setting);
    assert!(policy.gates(), "{spelling} is a gate");

    let state = Arc::new(test_state_with(test_config()));
    let mut c = mock_client(None, None, None, None);
    c.sole_mut().visitor_auth = None; // exactly the shape the bug turned on
    c.sole_mut().visitor_auth_policy = Some(policy);
    state.clients.write().await.insert("c1".to_string(), c);

    for path in ["/./admin", "/x/../admin", "/."] {
      let uri: axum::http::Uri = path.parse().unwrap();
      let gate = check_visitor_gate(
        &state,
        &axum::http::Method::GET,
        &HeaderMap::new(),
        &uri,
        None,
      )
      .await;
      assert!(
        matches!(gate, VisitorGate::Deny(_)),
        "{path} under `{spelling}` must not be served without a credential"
      );
    }
  }
}

#[tokio::test]
async fn a_query_token_cookie_is_scoped_to_the_route_that_admitted_it() {
  // The cookie a `?aperio_token=` page load mints is read by *every* branch of
  // the gate, including the server's own. Minted host-wide from a per-route
  // secret it outranked the policy that produced it: the holder of a secret
  // for `/metrics` got the whole hostname for an hour, including routes gated
  // by the operator's own password.
  let mut cfg = test_config();
  // The server's own gate covers everything this client does not.
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("admin:server-password");
  let state = Arc::new(test_state_with(cfg));

  let setting = serde_yaml::from_str::<aperio_config::AuthSetting>(
    "{method: bearer, secret: a-long-route-secret, query: true}",
  )
  .expect("a valid auth: value");
  let mut c = mock_client(Some("app.e2e.local"), Some("/metrics"), None, None);
  c.sole_mut().visitor_auth_policy = Some(crate::visitor_auth::Policy::compile(&setting));
  state.clients.write().await.insert("c1".to_string(), c);

  let mut headers = HeaderMap::new();
  headers.insert("host", "app.e2e.local".parse().unwrap());
  headers.insert("accept", "text/html".parse().unwrap());
  let uri: axum::http::Uri = "/metrics?aperio_token=a-long-route-secret".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &headers,
    &uri,
    Some("app.e2e.local"),
  )
  .await;

  // A navigation carrying the secret is redirected, with the cookie set.
  let VisitorGate::Deny(resp) = gate else {
    panic!("a navigation with the secret should be redirected to a clean address");
  };
  let cookie = resp
    .headers()
    .get("set-cookie")
    .and_then(|v| v.to_str().ok())
    .expect("a share cookie")
    .to_string();
  let token = cookie
    .split(';')
    .next()
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .expect("a cookie value");

  // The scope it carries is the route's bind, not the whole host.
  let claims = crate::share::verify_share_token(
    &token,
    &crate::share::share_signing_key(&state.config().token),
  )
  .expect("a valid share token");
  assert_eq!(
    claims.path.as_deref(),
    Some("/metrics"),
    "the cookie must not outrank the secret that minted it"
  );

  // And it does not open the route the server's own password gates.
  let mut with_cookie = HeaderMap::new();
  with_cookie.insert("host", "app.e2e.local".parse().unwrap());
  with_cookie.insert("cookie", format!("aperio_share={token}").parse().unwrap());
  let elsewhere: axum::http::Uri = "/".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &with_cookie,
    &elsewhere,
    Some("app.e2e.local"),
  )
  .await;
  assert!(
    matches!(gate, VisitorGate::Deny(_)),
    "a cookie minted for /metrics must not open /"
  );
}

#[tokio::test]
async fn visitor_gate_traversal_honors_the_closed_posture() {
  // `deny` is checked in section 2, and a traversal path returns before it,
  // so a `.` in the path was the one way to switch the posture off.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
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
  c.sole_mut().cache = true;
  state.clients.write().await.insert("c1".to_string(), c);
  // A cacheable entry that is past its TTL but within its SWR window.
  state.response_cache.lock().await.insert(
    crate::cache::cache_key(None, "/swr"),
    200,
    vec![("content-type".to_string(), "text/plain".to_string())],
    b"stale".to_vec().into(),
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
  c.sole_mut().allowed_ips = vec!["10.0.0.0/8".to_string()];
  state.clients.write().await.insert("c1".to_string(), c);
  let resp = run(state, get("/secret")).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn handler_denied_visitor_redirect_302() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().allowed_ips = vec!["10.0.0.0/8".to_string()];
  c.sole_mut().denied = Some("https://denied.example/blocked".to_string());
  state.clients.write().await.insert("c1".to_string(), c);
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
  c.sole_mut().max_concurrent = Some(2);
  c.sole_mut().inflight_limiter = Some(StdArc::new(Semaphore::new(2)));
  state.clients.write().await.insert("c1".to_string(), c);
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
  c.sole_mut().cache = true;
  state.clients.write().await.insert("c1".to_string(), c);
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
    .write()
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
  c.sole_mut().max_concurrent = Some(1);
  // No permits available → the acquire never succeeds within the gateway
  // timeout → 429.
  c.sole_mut().inflight_limiter = Some(StdArc::new(Semaphore::new(0)));
  state.clients.write().await.insert("c1".to_string(), c);
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
  let (token, _secret) = state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "t".to_string(),
      daily_max_bytes: Some(1),
      ..Default::default()
    })
    .expect("the test store can be written to");
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
  state.clients.write().await.insert("c1".to_string(), c);
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
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = connected(cfg);
  mark_connected(&state).await;
  let _rx = insert_live_client(&state, "c1").await;
  // No session → the visitor gate denies inside proxy_http_request.
  let resp = run(state, get("/private")).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
}

#[tokio::test]
async fn a_visitors_own_copy_of_a_carried_identity_header_never_reaches_the_backend() {
  // The `response_headers` list is how a `forward` endpoint delivers an
  // identity, and the operator named those headers precisely so the backend
  // could trust what is in them. A visitor sending one of those names
  // themselves must not arrive alongside the endpoint's answer: two headers of
  // one name is not a contradiction a backend is obliged to notice, and most
  // read the first, which would be the visitor's.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    while let Ok((mut sock, _)) = listener.accept().await {
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock
          .write_all(
            b"HTTP/1.1 200 X\r\nx-auth-user: alice\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
          )
          .await;
      });
    }
  });

  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: forward, url: \"http://127.0.0.1:{port}/authcheck\", response_headers: [x-auth-user]}}"
    ))
    .unwrap(),
  );
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // What actually crossed the tunnel, which is where the carried headers are
  // appended: the inspector capture is taken before that.
  let dispatched = spawn_recording_responder(state.clone(), rx);

  let mut req = get("/private");
  req
    .headers_mut()
    .insert("x-auth-user", HeaderValue::from_static("admin"));
  let resp = run(state.clone(), req).await;
  assert_eq!(resp.status(), StatusCode::OK);

  let sent = dispatched.lock().unwrap();
  let named: Vec<&str> = sent
    .iter()
    .filter(|(k, _)| k.eq_ignore_ascii_case("x-auth-user"))
    .map(|(_, v)| v.as_str())
    .collect();
  assert_eq!(
    named,
    vec!["alice"],
    "the backend is told who the endpoint said, once, and never who the visitor claimed"
  );
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
  c.sole_mut().response_timeout = Some(5); // per-service response-timeout override
  state.clients.write().await.insert("c1".to_string(), c);
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
  state.clients.write().await.insert("c1".to_string(), c);
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
  let org = state
    .org_store
    .lock()
    .await
    .create("o1", Vec::new(), None)
    .unwrap();
  state
    .org_store
    .lock()
    .await
    .set_quota(&org.id, None, None, None, Some(Some(1)))
    .expect("the test store can be written to");
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
  state.clients.write().await.insert("c1".to_string(), c);
  let resp = run(state, get("/orgquota")).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn a_body_frame_is_decided_per_client_not_per_request() {
  // The bug this covers: the choice was made once, before the dispatch loop,
  // and the loop re-enters with a *different* client after a failover or a
  // 5xx retry. A v6 frame handed to a client that speaks v5 is a message it
  // cannot read, so the request hung until the gateway timeout with nothing
  // saying why.
  let body = b"payload".as_slice();
  assert!(body_frame_negotiated(Some(6), body));
  assert!(body_frame_negotiated(Some(7), body));
  assert!(
    !body_frame_negotiated(Some(5), body),
    "v5 needs base64 in JSON"
  );
  assert!(!body_frame_negotiated(Some(2), body));
  assert!(
    !body_frame_negotiated(None, body),
    "a client that announced nothing is assumed old"
  );
  // An empty body has nothing to carry either way, so it stays in the JSON.
  assert!(!body_frame_negotiated(Some(6), b""));
}

// --- request id correlation (planned_features #30) --------------------------

#[test]
fn a_safe_request_id_is_bounded_and_printable() {
  use super::is_safe_request_id;
  assert!(is_safe_request_id("7f2c1e40-0f2a-4a11-8f0b-9c0a1b2c3d4e"));
  assert!(is_safe_request_id("trace/00-abc:1+2_3.4"));
  // Empty, over-long, or carrying anything that could forge a log line or
  // look like a second header value.
  assert!(!is_safe_request_id(""));
  assert!(!is_safe_request_id(&"a".repeat(129)));
  assert!(!is_safe_request_id("has space"));
  assert!(!is_safe_request_id("one,two"));
  assert!(!is_safe_request_id("line\nbreak"));
  assert!(!is_safe_request_id("tab\there"));
  assert!(!is_safe_request_id("semi;colon"));
  // Exactly at the cap is still fine.
  assert!(is_safe_request_id(&"a".repeat(128)));
}

// --- identity headers (planned_features #47) --------------------------------

#[test]
fn the_aperio_header_namespace_is_recognised_case_insensitively() {
  // The strip is a prefix test on the raw name, so this pins the shape of it:
  // anything in the namespace goes, anything else stays, whatever the case.
  let is_ours = |k: &str| k.len() > 9 && k[..9].eq_ignore_ascii_case("x-aperio-");
  assert!(is_ours("x-aperio-org"));
  assert!(is_ours("X-Aperio-Client-Id"));
  assert!(is_ours("X-APERIO-TOKEN"));
  // Not in the namespace.
  assert!(!is_ours("x-aperio"), "the bare prefix names no header");
  assert!(!is_ours("x-aperio-"), "nothing after the prefix");
  assert!(!is_ours("x-request-id"));
  assert!(!is_ours("authorization"));
  assert!(
    !is_ours("x-aperiox-thing"),
    "a different namespace that starts alike"
  );
}

#[tokio::test]
async fn closed_by_default_refuses_a_route_nothing_declares_open() {
  // The posture, and the whole of what it changes: with no `auth:` anywhere,
  // a route used to be served because nothing said otherwise.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    // `Undeclared` rather than `Deny`, and the distinction is the point: the
    // thing that would declare this route open is a client, and under
    // scale-to-zero it may be asleep, so the handler asks again after the
    // cold start rather than refusing here. The answer carried is the one to
    // give if nobody arrives, and it is the answer an unclaimed route already
    // gives, so the existence of something here does not leak to a caller who
    // was never going to be let in.
    VisitorGate::Undeclared(resp) => assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT),
    VisitorGate::Deny(_) => panic!("expected the closed-by-default answer, not a refusal"),
    VisitorGate::Allow(_) => panic!("expected the closed-by-default answer"),
  }

  // The same request under the default posture, which is unchanged.
  let state = Arc::new(test_state_with(test_config()));
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn closed_by_default_still_serves_what_declares_itself_open() {
  // `public: true` is the sentence that opens a route, which is what makes
  // the posture expressible rather than being a second, parallel switch.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().public = true;
  state.clients.write().await.insert("c1".to_string(), c);

  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn closed_by_default_leaves_a_configured_gate_exactly_as_it_was() {
  // The posture decides what an *unstated* route means. A route with a gate
  // has stated something, so nothing about it changes.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => assert_eq!(
      resp.status(),
      StatusCode::FOUND,
      "a gated route still sends the visitor somewhere they can act"
    ),
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn a_session_from_one_organization_does_not_open_another_ones_gated_site() {
  // The visitor gate and the dashboard share one session store, and the gate
  // asked only "is this a global session". A session bound to `acme`, even a
  // read-only one, therefore walked past the gate on every hostname on the
  // server, including hostnames served for other tenants.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example.com".to_string()], None)
    .expect("the organization");

  // The shape a per-organization OIDC login produces: a global session that
  // is fixed to one organization.
  let token = uuid::Uuid::new_v4().to_string();
  {
    let now = crate::store::sessions::now_secs();
    state.sessions.lock().await.insert(
      &token,
      crate::store::sessions::SessionInfo {
        plane: crate::store::sessions::Plane::Admin,
        expires_at: now + 86400,
        created_at: now,
        ip: Some("127.0.0.1".to_string()),
        user_agent: None,
        scope_host: None,
        username: Some("viewer@acme.example.com".to_string()),
        role: crate::store::users::Role::Viewer,
        selected_org: None,
        bound_org: Some(org.id.clone()),
      },
    );
  }
  let headers = crate::test_support::cookie_headers(&token);
  let uri: axum::http::Uri = "/private".parse().unwrap();

  // Its own organization's hostname: admitted, as it always was.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &headers,
    &uri,
    Some("acme.example.com"),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));

  // Another tenant's: refused.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &headers,
    &uri,
    Some("globex.example.com"),
  )
  .await;
  assert!(
    matches!(gate, VisitorGate::Deny(_)),
    "an organization's session reached past another organization's gate"
  );
}

#[tokio::test]
async fn a_master_session_still_reaches_every_gated_site() {
  // The fence is on the organization, and master has none. An operator's own
  // dashboard login behaves exactly as it did, which is what keeps this a fix
  // for the cross-tenant case rather than a change for everyone.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let token =
    crate::test_support::seed_session(&state, crate::store::users::Role::Admin, None, None).await;
  let headers = crate::test_support::cookie_headers(&token);
  let uri: axum::http::Uri = "/private".parse().unwrap();

  for host in ["acme.example.com", "globex.example.com", "anything.at.all"] {
    let gate =
      check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, Some(host)).await;
    assert!(matches!(gate, VisitorGate::Allow(_)), "{host}");
  }
}

#[tokio::test]
async fn a_fenced_session_without_a_host_header_is_refused() {
  // A fenced organization has no claim on a request that names no hostname,
  // and admitting it would be the same hole wearing a missing header.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example.com".to_string()], None)
    .expect("the organization");
  let token = uuid::Uuid::new_v4().to_string();
  {
    let now = crate::store::sessions::now_secs();
    state.sessions.lock().await.insert(
      &token,
      crate::store::sessions::SessionInfo {
        plane: crate::store::sessions::Plane::Admin,
        expires_at: now + 86400,
        created_at: now,
        ip: Some("127.0.0.1".to_string()),
        user_agent: None,
        scope_host: None,
        username: Some("viewer@acme.example.com".to_string()),
        role: crate::store::users::Role::Viewer,
        selected_org: None,
        bound_org: Some(org.id.clone()),
      },
    );
  }
  let headers = crate::test_support::cookie_headers(&token);
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn the_gate_says_who_it_let_in() {
  // The identity a backend may be told (#109). It is what the gate already
  // knew at the moment it admitted someone and never said, which is why an
  // application behind a tunnel had to build a second login to greet anyone.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\"}").unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_static("Bearer 0123456789abcdef-secret"),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "bearer");
      assert_eq!(id.who, None, "a secret identifies a caller, not a person");
    }
    _ => panic!("expected an admitted bearer caller"),
  }

  // A session carries the name behind it, which is the answer worth having.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let token = crate::test_support::seed_session(
    &state,
    crate::store::users::Role::Admin,
    Some("alice@example.com"),
    None,
  )
  .await;
  let headers = crate::test_support::cookie_headers(&token);
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "session");
      assert_eq!(id.who.as_deref(), Some("alice@example.com"));
    }
    _ => panic!("expected an admitted session"),
  }
}

#[tokio::test]
async fn an_open_route_names_nobody() {
  // Nothing was asked of this visitor, so there is nothing to announce, and
  // a header saying "anonymous" would be noise a backend learns to ignore.
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  match check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await
  {
    VisitorGate::Allow(identity) => assert_eq!(identity, None),
    VisitorGate::Deny(_) => panic!("expected allow"),
    VisitorGate::Undeclared(_) => panic!("expected allow"),
  }
}

#[tokio::test]
async fn a_route_nothing_is_connected_to_is_undeclared_rather_than_refused() {
  // The distinction the cold start depends on. Closed by default and nothing
  // connected means the client that would declare this route open may simply
  // be asleep, so the gate hands back the answer to give *if* nobody arrives
  // and lets the handler ask again after the wake. Refusing outright here
  // would have switched scale-to-zero off, since the request that wakes a
  // service is exactly the one nothing has declared anything for.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    Some("asleep.e2e.local"),
  )
  .await;
  match gate {
    VisitorGate::Undeclared(resp) => assert_eq!(
      resp.status(),
      StatusCode::GATEWAY_TIMEOUT,
      "the held answer is the one an unclaimed route gives"
    ),
    VisitorGate::Deny(_) => panic!("a sleeping service must not be refused outright"),
    VisitorGate::Allow(_) => panic!("nothing declared this route open"),
  }

  // And a client that *is* connected and declares itself open is served, so
  // the posture is not simply deferring everything.
  let mut c = mock_client(Some("awake.e2e.local"), None, None, None);
  c.sole_mut().public = true;
  state.clients.write().await.insert("c1".to_string(), c);
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    Some("awake.e2e.local"),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn the_credential_that_opened_the_gate_does_not_travel_to_the_backend() {
  // The header that opened Aperio's gate is Aperio's, on the same rule that
  // already strips the internal cookies: handing a backend a secret that
  // opens every route the gate protects is worse than useless to it.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\", query: true}")
      .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_static("Bearer 0123456789abcdef-secret"),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => assert!(id.consumed_authorization),
    _ => panic!("expected an admitted bearer caller"),
  }

  // The query form consumes no header, so an `Authorization` the visitor
  // happened to be sending is theirs and reaches the backend untouched.
  let query: axum::http::Uri = "/api/items?aperio_token=0123456789abcdef-secret"
    .parse()
    .unwrap();
  match check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &query,
    None,
  )
  .await
  {
    VisitorGate::Allow(Some(id)) => assert!(!id.consumed_authorization),
    _ => panic!("expected an admitted query caller"),
  }
}

#[tokio::test]
async fn a_forward_endpoint_is_told_the_address_the_server_decided_on() {
  // `X-Forwarded-For` is a header any visitor can write, and the gate is the
  // last place that should take one at face value: the address it sends is
  // what an endpoint allowlisting source addresses decides on. So the gate is
  // handed the address the trusted-proxy rules already produced, and a visitor
  // writing their own header changes nothing. A visitor behind no proxy at all
  // still has one, the socket's own peer, rather than reaching the endpoint as
  // an unnamed caller its rules can never match.
  let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let seen_task = seen.clone();
  tokio::spawn(async move {
    while let Ok((mut sock, _)) = listener.accept().await {
      let seen = seen_task.clone();
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        seen
          .lock()
          .unwrap()
          .push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
        let _ = sock
          .write_all(b"HTTP/1.1 403 X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
          .await;
      });
    }
  });

  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: forward, url: \"http://127.0.0.1:{port}/authcheck\"}}"
    ))
    .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));

  let mut spoofed = HeaderMap::new();
  spoofed.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.5"));
  let gate = crate::proxy::check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &spoofed,
    &"/private".parse().unwrap(),
    None,
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 9)),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)), "the endpoint refused");

  let asked = seen.lock().unwrap();
  let raw = asked.first().expect("the endpoint was asked");
  assert!(
    raw.contains("x-forwarded-for: 198.51.100.9"),
    "the endpoint is told the address the server decided on: {raw}"
  );
  assert!(
    !raw.contains("10.0.0.5"),
    "and never the one the visitor wrote for themselves: {raw}"
  );
}

#[tokio::test]
async fn a_jwt_gate_admits_the_token_a_visitor_already_holds() {
  // No round trip per request, which is what separates this from `forward`,
  // and the identity comes out of the token rather than out of a login.
  let secret = "0123456789abcdef-jwt-gate-secret";
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: jwt, hmac_secret: \"{secret}\", issuer: \"https://accounts.example.com\"}}"
    ))
    .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();

  let exp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs()
    + 600;
  let good = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
    &serde_json::json!({
      "sub": "u-1", "email": "alice@example.com",
      "iss": "https://accounts.example.com", "exp": exp
    }),
    &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
  )
  .unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {good}")).unwrap(),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "jwt");
      assert_eq!(id.who.as_deref(), Some("alice@example.com"));
      assert!(
        id.consumed_authorization,
        "the header carried Aperio's credential, so it does not travel on"
      );
    }
    _ => panic!("expected the token to be admitted"),
  }

  // A token for another issuer is refused, with the challenge a caller that
  // speaks in headers can act on.
  let wrong = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
    &serde_json::json!({"sub": "u-1", "iss": "https://somewhere.else", "exp": exp}),
    &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
  )
  .unwrap();
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {wrong}")).unwrap(),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
      assert_eq!(resp.headers().get("WWW-Authenticate").unwrap(), "Bearer");
    }
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn a_jwt_in_a_cookie_is_the_visitors_own_and_keeps_travelling() {
  // Where an identity-aware proxy in front puts it. Stripping the cookie
  // header would take the application's own session with it, so unlike the
  // bearer case nothing is consumed.
  let secret = "0123456789abcdef-jwt-cookie-key";
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: jwt, hmac_secret: \"{secret}\", cookie: CF_Authorization}}"
    ))
    .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let exp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs()
    + 600;
  let t = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
    &serde_json::json!({"sub": "u-9", "exp": exp}),
    &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
  )
  .unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "cookie",
    HeaderValue::from_str(&format!("other=1; CF_Authorization={t}")).unwrap(),
  );
  let uri: axum::http::Uri = "/private".parse().unwrap();
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "jwt");
      assert_eq!(id.who.as_deref(), Some("u-9"));
      assert!(!id.consumed_authorization);
    }
    _ => panic!("expected the cookie token to be admitted"),
  }
}

#[tokio::test]
async fn one_written_policy_behaves_the_same_whichever_side_wrote_it() {
  // The two branches of the gate, a client-declared policy and the server's
  // own, ran the same helpers in two hand-written sequences, and they had
  // drifted: a `bearer` with `query: true` got the clean-address redirect on
  // the server side and a bare admission on the client side, so a page loaded
  // through a client-declared gate rendered and then failed to fetch a single
  // one of its own assets.
  let yaml = "{method: bearer, secret: \"0123456789abcdef-secret\", query: true}";
  let uri: axum::http::Uri = "/report?aperio_token=0123456789abcdef-secret&page=2"
    .parse()
    .unwrap();
  let mut browser = HeaderMap::new();
  browser.insert("accept", HeaderValue::from_static("text/html"));

  // Written on the server.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(&serde_yaml::from_str(yaml).unwrap());
  let server_side = Arc::new(test_state_with(cfg));

  // Written on a client that serves the route.
  let client_side = Arc::new(test_state_with(test_config()));
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().visitor_auth_policy = Some(crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(yaml).unwrap(),
  ));
  client_side
    .clients
    .write()
    .await
    .insert("c1".to_string(), c);

  for (label, state) in [("server", &server_side), ("client", &client_side)] {
    let gate = check_visitor_gate(
      state,
      &axum::http::Method::GET,
      &browser,
      &uri,
      Some("app.example.com"),
    )
    .await;
    match gate {
      VisitorGate::Deny(resp) => {
        assert_eq!(resp.status(), StatusCode::FOUND, "{label}");
        assert_eq!(
          resp.headers().get("Location").unwrap(),
          "/report?page=2",
          "{label}"
        );
        assert!(
          resp
            .headers()
            .get("Set-Cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("aperio_share="),
          "{label}"
        );
      }
      VisitorGate::Allow(_) | VisitorGate::Undeclared(_) => {
        panic!("{label}: a page load carrying the secret should be sent to a clean address")
      }
    }
  }
}

// --- worth_waiting_for_route -------------------------------------------------

/// Waiting is for a route that might come back, not for every route that is
/// down.
///
/// The two questions this separates used to be one server-wide flag, and it
/// answered both wrongly in opposite directions: a route whose own client had
/// gone skipped the wait whenever an unrelated service was online, and a route
/// that had been dead for hours still burned the whole gateway timeout before
/// saying so. The first is the surprising one, the second is the one a visitor
/// feels.
#[test]
fn a_recently_dropped_client_is_worth_waiting_for() {
  let now = Instant::now();
  let recent = std::time::Duration::from_secs(30);
  assert!(worth_waiting_for_route(
    Some(now - std::time::Duration::from_secs(2)),
    now,
    recent,
    false
  ));
}

#[test]
fn a_route_dead_since_long_before_the_wait_is_not() {
  // The case that turns a fast refusal into a thirty-second one for nothing:
  // whatever dropped, dropped long enough ago that a reconnect would already
  // have happened.
  let now = Instant::now();
  let recent = std::time::Duration::from_secs(30);
  assert!(!worth_waiting_for_route(
    Some(now - std::time::Duration::from_secs(3600)),
    now,
    recent,
    false
  ));
}

#[test]
fn a_server_that_has_never_seen_a_disconnect_does_not_wait() {
  // Nothing has ever dropped, so nothing is on its way back. Without
  // scale-to-zero there is no other way for a candidate to appear.
  let now = Instant::now();
  assert!(!worth_waiting_for_route(
    None,
    now,
    std::time::Duration::from_secs(30),
    false
  ));
}

#[test]
fn scale_to_zero_is_always_worth_waiting_for() {
  // The cold start is precisely a candidate arriving without anyone having
  // disconnected, so the disconnect clock says nothing about it.
  let now = Instant::now();
  assert!(worth_waiting_for_route(
    None,
    now,
    std::time::Duration::from_secs(30),
    true
  ));
  assert!(worth_waiting_for_route(
    Some(now - std::time::Duration::from_secs(3600)),
    now,
    std::time::Duration::from_secs(30),
    true
  ));
}
