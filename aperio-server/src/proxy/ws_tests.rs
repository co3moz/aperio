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
  state.clients.lock().await.insert(id.to_string(), c);
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
  c.allowed_ips = vec!["10.0.0.0/8".to_string()];
  c.denied = Some("https://denied.example/ws".to_string());
  state.clients.lock().await.insert("c1".to_string(), c);
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
  c.allowed_ips = vec!["10.0.0.0/8".to_string()];
  state.clients.lock().await.insert("c1".to_string(), c);
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
