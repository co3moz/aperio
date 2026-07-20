use super::*;

use crate::protocol::TunnelDecl;
use crate::state::ClientHandle;
use crate::test_support::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

fn dynamic_perms(token_id: &str) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: Vec::new(),
    paths: Vec::new(),
    token_name: Some(format!("token-{token_id}")),
    token_id: Some(token_id.to_string()),
    allow_public: false,
    org_id: None,
  }
}

#[test]
fn test_same_token() {
  let master = ClientPerms::master();
  let a = dynamic_perms("a");
  let a2 = dynamic_perms("a");
  let b = dynamic_perms("b");

  // The master token may bind any client's tunnels.
  assert!(same_token(&master, &a));
  assert!(same_token(&master, &master));

  // A dynamic token only matches clients using the very same token.
  assert!(same_token(&a, &a2));
  assert!(!same_token(&a, &b));

  // A dynamic token never matches a master-token client, and a
  // master-token OWNER is only bindable by the master token itself.
  assert!(!same_token(&a, &master));
}

// ---------------------------------------------------------------------------
// Shared helpers. Both the endpoint handlers and the relay loops need a
// genuinely upgraded WebSocket (a synthesized `WebSocketUpgrade` extractor is
// rejected with `ConnectionNotUpgradable` because it lacks the `OnUpgrade`
// extension), so everything is driven through an in-process axum server. The
// pre-upgrade rejection branches surface as the failed-handshake status; the
// relay bodies run once the upgrade succeeds.
// ---------------------------------------------------------------------------

type WsClient = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start_tunnel_server(state: Arc<AppState>) -> String {
  use axum::routing::get;
  let app = axum::Router::new()
    .route("/tcp", get(tcp_ws_handler))
    .route("/udp", get(udp_ws_handler))
    .with_state(state);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(
      listener,
      app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
  });
  format!("ws://{addr}")
}

fn build_request(url: &str, token: Option<&str>) -> axum::http::Request<()> {
  let uri: axum::http::Uri = url.parse().unwrap();
  let host = uri.authority().unwrap().as_str().to_string();
  let mut b = axum::http::Request::builder()
    .method("GET")
    .uri(url)
    .header("Host", host)
    .header("Connection", "Upgrade")
    .header("Upgrade", "websocket")
    .header("Sec-WebSocket-Version", "13")
    .header("Sec-WebSocket-Key", generate_key());
  if let Some(t) = token {
    b = b.header("Authorization", format!("Bearer {t}"));
  }
  b.body(()).unwrap()
}

/// Performs a WebSocket handshake and returns the resulting HTTP status: 101
/// on a successful upgrade, or the rejection status the handler chose. On
/// success the socket is closed immediately (the relay tears down on its own).
async fn handshake_status(url: &str, token: Option<&str>) -> StatusCode {
  match tokio_tungstenite::connect_async(build_request(url, token)).await {
    Ok((mut ws, resp)) => {
      let _ = ws.close(None).await;
      StatusCode::from_u16(resp.status().as_u16()).unwrap()
    }
    Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
      StatusCode::from_u16(resp.status().as_u16()).unwrap()
    }
    Err(e) => panic!("unexpected handshake error: {e:?}"),
  }
}

/// Connects a consumer WebSocket that stays open (for the relay tests).
async fn connect_consumer(url: &str) -> WsClient {
  let (ws, _) = tokio_tungstenite::connect_async(build_request(url, Some("test")))
    .await
    .unwrap();
  ws
}

async fn seed_client(state: &AppState, id: &str, f: impl FnOnce(&mut ClientHandle)) {
  let mut c = mock_client(None, None, None, None);
  f(&mut c);
  state.clients.lock().await.insert(id.to_string(), c);
}

/// Mints a dynamic token in the store and returns its secret.
async fn make_token(state: &AppState) -> String {
  let mut store = state.token_store.lock().await;
  let (_rec, secret) = store.create(
    "caller".into(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    None,
    None,
    None,
    false,
    false,
    None,
  );
  secret
}

fn tcp_tunnel(target: &str) -> TunnelDecl {
  TunnelDecl {
    target: target.into(),
    protocol: "tcp".into(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }
}

fn udp_tunnel(target: &str) -> TunnelDecl {
  TunnelDecl {
    target: target.into(),
    protocol: "udp".into(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }
}

fn no_budget_config() -> crate::settings::ServerConfig {
  let mut c = test_config();
  c.ip_limit_max = 0.0;
  c.ip_limit_refill = 0.0;
  c
}

// --- tcp_ws_handler ---------------------------------------------------------

#[tokio::test]
async fn tcp_handler_rate_limited() {
  let state = Arc::new(test_state_with(no_budget_config()));
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/tcp"), Some("test")).await;
  assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn tcp_handler_unauthorized() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/tcp"), None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tcp_handler_client_without_target() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/tcp?client=c1"), Some("test")).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn tcp_handler_no_such_client() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/tcp?client=nope&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tcp_handler_token_mismatch() {
  let state = Arc::new(test_state());
  let caller = make_token(&state).await;
  seed_client(&state, "c1", |c| {
    c.perms = dynamic_perms("owner-token");
    c.tunnels = vec![tcp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/tcp?client=c1&target=127.0.0.1:9"),
    Some(&caller),
  )
  .await;
  assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tcp_handler_client_unavailable() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.draining = true;
    c.tunnels = vec![tcp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/tcp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn tcp_handler_tunnel_not_declared() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |_| {}).await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/tcp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tcp_handler_declared_tunnel_success() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.tunnels = vec![tcp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/tcp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
}

#[tokio::test]
async fn tcp_handler_legacy_no_capable_client() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/tcp"), Some("test")).await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn tcp_handler_legacy_success() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.tcp_enabled = true;
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/tcp"), Some("test")).await;
  assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
}

// --- udp_ws_handler ---------------------------------------------------------

#[tokio::test]
async fn udp_handler_rate_limited() {
  let state = Arc::new(test_state_with(no_budget_config()));
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/udp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn udp_handler_unauthorized() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/udp?client=c1&target=127.0.0.1:9"), None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn udp_handler_missing_params() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(&format!("{url}/udp?client=c1"), Some("test")).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn udp_handler_no_such_client() {
  let state = Arc::new(test_state());
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/udp?client=nope&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn udp_handler_token_mismatch() {
  let state = Arc::new(test_state());
  let caller = make_token(&state).await;
  seed_client(&state, "c1", |c| {
    c.perms = dynamic_perms("owner-token");
    c.tunnels = vec![udp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/udp?client=c1&target=127.0.0.1:9"),
    Some(&caller),
  )
  .await;
  assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn udp_handler_client_unavailable() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.admin_enabled = false;
    c.tunnels = vec![udp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/udp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn udp_handler_tunnel_not_declared() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.tunnels = vec![tcp_tunnel("127.0.0.1:9")]; // tcp, not udp
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/udp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn udp_handler_success() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.tunnels = vec![udp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state).await;
  let status = handshake_status(
    &format!("{url}/udp?client=c1&target=127.0.0.1:9"),
    Some("test"),
  )
  .await;
  assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
}

// --- tunnels_list_handler (plain HTTP, callable directly) -------------------

#[tokio::test]
async fn tunnels_list_rate_limited() {
  let state = Arc::new(test_state_with(no_budget_config()));
  let resp = tunnels_list_handler(
    State(state.clone()),
    axum::extract::Path("c1".to_string()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn tunnels_list_unauthorized() {
  let state = Arc::new(test_state());
  let resp = tunnels_list_handler(
    State(state.clone()),
    axum::extract::Path("c1".to_string()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tunnels_list_no_such_client() {
  let state = Arc::new(test_state());
  let resp = tunnels_list_handler(
    State(state.clone()),
    axum::extract::Path("nope".to_string()),
    ConnectInfo(test_peer()),
    master_token_headers(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tunnels_list_token_mismatch() {
  let state = Arc::new(test_state());
  let caller = make_token(&state).await;
  seed_client(&state, "c1", |c| {
    c.perms = dynamic_perms("owner-token");
  })
  .await;
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    axum::http::HeaderValue::from_str(&format!("Bearer {caller}")).unwrap(),
  );
  let resp = tunnels_list_handler(
    State(state.clone()),
    axum::extract::Path("c1".to_string()),
    ConnectInfo(test_peer()),
    headers,
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tunnels_list_success() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.tunnels = vec![tcp_tunnel("127.0.0.1:9"), udp_tunnel("127.0.0.1:53")];
  })
  .await;
  let resp = tunnels_list_handler(
    State(state.clone()),
    axum::extract::Path("c1".to_string()),
    ConnectInfo(test_peer()),
    master_token_headers(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Live relay tests: drive `relay_tcp_consumer` / `relay_udp_consumer` through
// a genuinely upgraded consumer WebSocket so both directions and the teardown
// execute.
// ---------------------------------------------------------------------------

async fn recv_msg(rx: &mut mpsc::Receiver<Message>) -> Message {
  tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
    .await
    .expect("client frame timeout")
    .expect("client channel closed")
}

async fn wait_for_stream(state: &AppState, udp: bool) -> mpsc::Sender<TcpConsumerMsg> {
  for _ in 0..200 {
    {
      let map = if udp {
        state.udp_streams.lock().await
      } else {
        state.tcp_streams.lock().await
      };
      if let Some(h) = map.values().next() {
        return h.tx.clone();
      }
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
  }
  panic!("relay stream never registered");
}

async fn wait_streams_empty(state: &AppState, udp: bool) {
  for _ in 0..200 {
    let empty = if udp {
      state.udp_streams.lock().await.is_empty()
    } else {
      state.tcp_streams.lock().await.is_empty()
    };
    if empty {
      return;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
  }
  panic!("relay stream never cleaned up");
}

#[tokio::test]
async fn tcp_relay_full_roundtrip() {
  let state = Arc::new(test_state());
  // A TCP-capable client whose tunnel-side channel we keep, to observe the
  // frames the relay pushes toward the client.
  let (ctx, mut crx) = mpsc::channel::<Message>(16);
  {
    let mut c = mock_client(None, None, None, None);
    c.tx = ctx;
    c.tcp_enabled = true;
    state.clients.lock().await.insert("c1".into(), c);
  }
  let url = start_tunnel_server(state.clone()).await;
  let mut consumer = connect_consumer(&format!("{url}/tcp")).await;

  // Relay opens the client-side TCP target.
  match recv_msg(&mut crx).await {
    Message::Text(t) => assert!(t.contains("TcpOpen")),
    other => panic!("expected TcpOpen, got {other:?}"),
  }

  // Consumer -> tunnel.
  consumer
    .send(TMessage::Binary(vec![10, 20, 30]))
    .await
    .unwrap();
  match recv_msg(&mut crx).await {
    Message::Text(t) => assert!(t.contains("TcpData")),
    other => panic!("expected TcpData, got {other:?}"),
  }

  // Tunnel -> consumer via the registered relay channel.
  let relay_tx = wait_for_stream(&state, false).await;
  relay_tx
    .send(TcpConsumerMsg::Data(vec![1, 2, 3]))
    .await
    .unwrap();
  let got = tokio::time::timeout(std::time::Duration::from_secs(2), consumer.next())
    .await
    .expect("consumer frame timeout")
    .unwrap()
    .unwrap();
  assert_eq!(got, TMessage::Binary(vec![1, 2, 3]));

  // Close from the tunnel side ends the relay and drops the stream.
  relay_tx.send(TcpConsumerMsg::Close).await.unwrap();
  wait_streams_empty(&state, false).await;
}

#[tokio::test]
async fn tcp_relay_client_channel_closed_aborts() {
  let state = Arc::new(test_state());
  // mock_client drops its receiver, so the first send (TcpOpen) fails and the
  // relay tears down immediately.
  seed_client(&state, "c1", |c| {
    c.tcp_enabled = true;
  })
  .await;
  let url = start_tunnel_server(state.clone()).await;
  let _consumer = connect_consumer(&format!("{url}/tcp")).await;
  wait_streams_empty(&state, false).await;
}

#[tokio::test]
async fn udp_relay_full_roundtrip() {
  let state = Arc::new(test_state());
  let (ctx, mut crx) = mpsc::channel::<Message>(16);
  {
    let mut c = mock_client(None, None, None, None);
    c.tx = ctx;
    c.tunnels = vec![udp_tunnel("127.0.0.1:9")];
    state.clients.lock().await.insert("c1".into(), c);
  }
  let url = start_tunnel_server(state.clone()).await;
  let mut consumer = connect_consumer(&format!("{url}/udp?client=c1&target=127.0.0.1:9")).await;

  match recv_msg(&mut crx).await {
    Message::Text(t) => assert!(t.contains("UdpOpen")),
    other => panic!("expected UdpOpen, got {other:?}"),
  }

  consumer.send(TMessage::Binary(vec![7, 7])).await.unwrap();
  match recv_msg(&mut crx).await {
    Message::Text(t) => assert!(t.contains("UdpDatagram")),
    other => panic!("expected UdpDatagram, got {other:?}"),
  }

  let relay_tx = wait_for_stream(&state, true).await;
  relay_tx
    .send(TcpConsumerMsg::Data(vec![9, 8]))
    .await
    .unwrap();
  let got = tokio::time::timeout(std::time::Duration::from_secs(2), consumer.next())
    .await
    .expect("consumer frame timeout")
    .unwrap()
    .unwrap();
  assert_eq!(got, TMessage::Binary(vec![9, 8]));

  relay_tx.send(TcpConsumerMsg::Close).await.unwrap();
  wait_streams_empty(&state, true).await;
}

#[tokio::test]
async fn udp_relay_client_channel_closed_aborts() {
  let state = Arc::new(test_state());
  seed_client(&state, "c1", |c| {
    c.tunnels = vec![udp_tunnel("127.0.0.1:9")];
  })
  .await;
  let url = start_tunnel_server(state.clone()).await;
  let _consumer = connect_consumer(&format!("{url}/udp?client=c1&target=127.0.0.1:9")).await;
  wait_streams_empty(&state, true).await;
}
