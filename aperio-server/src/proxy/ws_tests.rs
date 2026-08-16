//! Unit tests for the WebSocket proxy path. These drive [`handle_ws_proxy`]
//! (through [`crate::proxy::proxy_handler`], which detects the upgrade) up to
//! the point of the public-side socket upgrade. The bidirectional relay
//! ([`relay_ws_stream`]) needs a live upgraded socket and is covered only by
//! the e2e suite; every reachable pre-upgrade branch is exercised here.

use crate::protocol::TunnelMessage;
use crate::proxy::proxy_handler;
use crate::state::{AppState, TunnelResponse};
use crate::test_support::{mock_client, test_config, test_peer, test_state_with};
use axum::body::Body;
use axum::extract::ws::Message;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderValue, StatusCode};
use std::sync::Arc;
use tokio::sync::mpsc;

fn connected(config: crate::settings::ServerConfig) -> Arc<AppState> {
  Arc::new(test_state_with(config))
}

async fn mark_connected(state: &AppState) {
  state.connection_state.lock().await.connected = true;
  let _ = state.client_connected.send_replace(true);
}

async fn insert_live_client(state: &AppState, id: &str) -> mpsc::Receiver<Message> {
  let mut c = mock_client(None, None, None, None);
  let (tx, rx) = mpsc::channel::<Message>(64);
  c.tx = tx;
  state.clients.write().await.insert(id.to_string(), c);
  rx
}

/// Answers the forwarded `UpgradeRequest` with a [`TunnelResponse`] carrying
/// `status`, delivered through `pending_upgrades` like the live read loop.
fn spawn_upgrade_responder(state: Arc<AppState>, mut rx: mpsc::Receiver<Message>, status: u16) {
  tokio::spawn(async move {
    let Some(Message::Text(text)) = rx.recv().await else {
      return;
    };
    let Ok(TunnelMessage::UpgradeRequest { id, .. }) = serde_json::from_str::<TunnelMessage>(&text)
    else {
      return;
    };
    if let Some(req) = state.pending_upgrades.lock().await.remove(&id) {
      let _ = req.tx.send(TunnelResponse {
        status,
        headers: Vec::new(),
        body: None,
        body_raw: None,
        trailers: None,
        stream_rx: None,
        timings: None,
      });
    }
  });
}

/// A minimal WebSocket upgrade request. When `valid_key` is set it also carries
/// the `sec-websocket-*` headers a real handshake needs, so the public-side
/// upgrade succeeds instead of being rejected.
fn ws_request(path: &str, valid_key: bool) -> axum::extract::Request<Body> {
  let mut req = axum::extract::Request::new(Body::empty());
  *req.uri_mut() = path.parse().unwrap();
  let h = req.headers_mut();
  h.insert("upgrade", HeaderValue::from_static("websocket"));
  h.insert("connection", HeaderValue::from_static("Upgrade"));
  if valid_key {
    h.insert("sec-websocket-version", HeaderValue::from_static("13"));
    h.insert(
      "sec-websocket-key",
      HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
    );
  }
  req
}

async fn run(state: Arc<AppState>, req: axum::extract::Request<Body>) -> axum::response::Response {
  proxy_handler(State(state), ConnectInfo(test_peer()), req).await
}

#[tokio::test]
async fn ws_rate_limited_returns_429() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let state = connected(cfg);
  mark_connected(&state).await;
  let resp = run(state, ws_request("/ws", false)).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn ws_no_client_returns_504() {
  let state = connected(test_config());
  mark_connected(&state).await; // connected, no clients
  let resp = run(state, ws_request("/ws", false)).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn ws_offline_reconnect_wait_times_out() {
  let mut cfg = test_config();
  cfg.gateway_timeout = std::time::Duration::from_millis(50);
  let state = connected(cfg); // connection_state stays disconnected
  let resp = run(state, ws_request("/ws", false)).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn ws_backend_non_101_propagates_error() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // Backend declines the upgrade (non-101) → handler returns that status.
  spawn_upgrade_responder(state.clone(), rx, 502);
  let resp = run(state, ws_request("/ws", false)).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn ws_upgrade_rejected_when_key_missing() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // Backend accepts (101), but the public request lacks a valid WS key, so the
  // server-side upgrade is rejected (exercises the WsClose teardown branch).
  spawn_upgrade_responder(state.clone(), rx, 101);
  let resp = run(state, ws_request("/ws", false)).await;
  assert!(
    resp.status().is_client_error() || resp.status().is_server_error(),
    "expected a rejection status, got {}",
    resp.status()
  );
}

#[tokio::test]
async fn ws_denied_visitor_redirect_302() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  c.service.allowed_ips = vec!["10.0.0.0/8".to_string()];
  c.service.denied = Some("https://denied.example/ws".to_string());
  state.clients.write().await.insert("c1".to_string(), c);
  let resp = run(state, ws_request("/ws", false)).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  assert_eq!(
    resp.headers().get("Location").unwrap(),
    "https://denied.example/ws"
  );
}

#[tokio::test]
async fn ws_denied_visitor_stealth_504() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let mut c = mock_client(None, None, None, None);
  c.service.allowed_ips = vec!["10.0.0.0/8".to_string()];
  state.clients.write().await.insert("c1".to_string(), c);
  let resp = run(state, ws_request("/ws", false)).await;
  assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn ws_filters_cookies_and_sticky_affinity() {
  let mut cfg = test_config();
  cfg.lb_strategy = crate::settings::LbStrategy::Sticky;
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // Backend declines (non-101) so the handler returns before the socket
  // upgrade, but the request still exercises the sticky affinity read and the
  // cookie-filtering header serialization.
  spawn_upgrade_responder(state.clone(), rx, 502);
  let mut req = ws_request("/ws", false);
  req.headers_mut().insert(
    "cookie",
    HeaderValue::from_static("aperio_session=x; aperio_affinity=c1; real=1"),
  );
  let resp = run(state, req).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn ws_upgrade_accepted_reaches_upgrade() {
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  // Backend accepts (101) and the public request is a valid handshake, so the
  // handler takes the `Ok(ws)` arm and registers the relay via `on_upgrade`.
  // A synthetic request carries no live hyper upgrade, so axum answers 426
  // UPGRADE_REQUIRED at response time (a real socket yields 101; the relay body
  // itself is covered only by the e2e suite).
  spawn_upgrade_responder(state.clone(), rx, 101);
  let resp = run(state, ws_request("/ws", true)).await;
  assert_eq!(resp.status(), StatusCode::UPGRADE_REQUIRED);
}

/// Answers the forwarded `UpgradeRequest` with a non-101 so the handler
/// returns before the socket upgrade, and hands back the request line and
/// headers the backend was sent.
#[allow(clippy::type_complexity)]
fn spawn_recording_upgrade_responder(
  state: Arc<AppState>,
  mut rx: mpsc::Receiver<Message>,
) -> Arc<std::sync::Mutex<Option<(String, Vec<(String, String)>)>>> {
  let seen = Arc::new(std::sync::Mutex::new(None));
  let out = seen.clone();
  tokio::spawn(async move {
    let Some(Message::Text(text)) = rx.recv().await else {
      return;
    };
    let Ok(TunnelMessage::UpgradeRequest {
      id, uri, headers, ..
    }) = serde_json::from_str::<TunnelMessage>(&text)
    else {
      return;
    };
    *seen.lock().unwrap() = Some((uri, headers));
    if let Some(req) = state.pending_upgrades.lock().await.remove(&id) {
      let _ = req.tx.send(TunnelResponse {
        status: 502,
        headers: Vec::new(),
        body: None,
        body_raw: None,
        trailers: None,
        stream_rx: None,
        timings: None,
      });
    }
  });
  out
}

#[tokio::test]
async fn a_gated_upgrade_does_not_carry_aperios_own_credential_to_the_backend() {
  // The credential that opened the gate is Aperio's, and the HTTP path has
  // always taken it back off the request before forwarding. The upgrade path
  // did not, on either of the two forms it can arrive in, and the query form
  // is the one that matters most here: a browser cannot put a header on a
  // `WebSocket`, so `?aperio_token=` is how a gated socket is opened at all.
  // The secret is shared by every visitor of the route, so a backend that logs
  // its request line was publishing the key to the whole site.
  let secret = "0123456789abcdef-secret";
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: bearer, secret: \"{secret}\", query: true}}"
    ))
    .unwrap(),
  );
  let state = connected(cfg);
  mark_connected(&state).await;

  // In the query, which is the form a browser has to use.
  let rx = insert_live_client(&state, "c1").await;
  let seen = spawn_recording_upgrade_responder(state.clone(), rx);
  let resp = run(
    state.clone(),
    ws_request(&format!("/ws?aperio_token={secret}&keep=1"), false),
  )
  .await;
  assert_eq!(
    resp.status(),
    StatusCode::BAD_GATEWAY,
    "the gate admitted it"
  );
  let (uri, _) = seen.lock().unwrap().clone().expect("an upgrade was sent");
  assert_eq!(
    uri, "/ws?keep=1",
    "the gate's own parameter is gone and the visitor's own is untouched"
  );
}

#[tokio::test]
async fn a_gated_upgrade_does_not_carry_the_bearer_header_either() {
  // The other form the same credential arrives in, for a caller that can set
  // a header. The HTTP path drops it once it has opened the gate; the upgrade
  // path passed it through to the backend.
  let secret = "0123456789abcdef-secret";
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!("{{method: bearer, secret: \"{secret}\"}}")).unwrap(),
  );
  let state = connected(cfg);
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  let seen = spawn_recording_upgrade_responder(state.clone(), rx);

  let mut req = ws_request("/ws", false);
  req.headers_mut().insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
  );
  let resp = run(state.clone(), req).await;
  assert_eq!(
    resp.status(),
    StatusCode::BAD_GATEWAY,
    "the gate admitted it"
  );

  let (_, headers) = seen.lock().unwrap().clone().expect("an upgrade was sent");
  assert!(
    !headers
      .iter()
      .any(|(k, _)| k.eq_ignore_ascii_case("authorization")),
    "the header that opened the gate does not travel on: {headers:?}"
  );
}

#[tokio::test]
async fn an_upgrade_keeps_the_same_headers_from_a_visitor_the_http_path_does() {
  // One rule, both paths. The `x-aperio-` namespace is the server's own, and
  // the session cookie in its `__Host-` spelling is as internal as the plain
  // one; the upgrade path filtered neither.
  let state = connected(test_config());
  mark_connected(&state).await;
  let rx = insert_live_client(&state, "c1").await;
  let seen = spawn_recording_upgrade_responder(state.clone(), rx);

  let mut req = ws_request("/ws", false);
  let h = req.headers_mut();
  h.insert("x-aperio-org", HeaderValue::from_static("someone-elses"));
  h.insert(
    "cookie",
    HeaderValue::from_static("__Host-aperio_session=x; real=1"),
  );
  let resp = run(state.clone(), req).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

  let (_, headers) = seen.lock().unwrap().clone().expect("an upgrade was sent");
  assert!(
    !headers
      .iter()
      .any(|(k, _)| k.eq_ignore_ascii_case("x-aperio-org")),
    "a visitor's copy of the server's own namespace: {headers:?}"
  );
  let cookie = headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
    .map(|(_, v)| v.as_str());
  assert_eq!(
    cookie,
    Some("real=1"),
    "the session cookie is gone in either spelling, the visitor's own stays"
  );
}
