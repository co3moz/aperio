//! Unit tests for the tunnel-side WebSocket protocol (`ws.rs`).
//!
//! `handle_socket` has no public constructor, so its message-handling and
//! disconnect-cleanup branches are driven through a real in-process axum
//! server and a genuine WebSocket client (`tokio-tungstenite`). Each test
//! connects, discovers the server-assigned client id, seeds `AppState` maps
//! with entries owned by that id (and by a foreign id, to exercise the
//! ownership-gated rejections), sends the relevant tunnel frame, and asserts
//! the effect on the seeded channels/state. `deliver_response_chunk` is also
//! exercised directly.

use super::*;
use crate::protocol::TunnelMessage;
use crate::state::{TcpConsumerMsg, TcpStreamHandle};
use crate::store::tokens::TokenSpec;
use crate::test_support::*;
use axum::Router;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

pub(super) type Client = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// --- harness ----------------------------------------------------------------

/// Spawns an in-process axum server exposing `ws_handler` and returns the
/// `ws://…/ws` URL to connect to.
pub(super) async fn start_server(state: Arc<AppState>) -> String {
  let app = Router::new()
    .route("/ws", get(ws_handler))
    .with_state(state);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(
      listener,
      app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
  });
  format!("ws://{addr}/ws")
}

pub(super) fn client_request(url: &str, token: &str) -> axum::http::Request<()> {
  let uri: axum::http::Uri = url.parse().unwrap();
  let host = uri.authority().unwrap().as_str().to_string();
  axum::http::Request::builder()
    .method("GET")
    .uri(url)
    .header("Host", host)
    .header("Connection", "Upgrade")
    .header("Upgrade", "websocket")
    .header("Sec-WebSocket-Version", "13")
    .header("Sec-WebSocket-Key", generate_key())
    .header("Authorization", format!("Bearer {token}"))
    .body(())
    .unwrap()
}

/// Connects a WebSocket client presenting the given bearer token.
pub(super) async fn connect(url: &str, token: &str) -> Client {
  let (ws, _resp) = tokio_tungstenite::connect_async(client_request(url, token))
    .await
    .unwrap();
  ws
}

/// Waits until exactly one client is registered and returns its id.
pub(super) async fn wait_client_id(state: &AppState) -> String {
  for _ in 0..400 {
    {
      let clients = state.clients.read().await;
      if let Some(k) = clients.keys().next() {
        return k.clone();
      }
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  panic!("client never registered");
}

pub(super) async fn wait_no_clients(state: &AppState) {
  for _ in 0..400 {
    if state.clients.write().await.is_empty() {
      return;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  panic!("client never cleaned up");
}

pub(super) async fn send(ws: &mut Client, msg: &TunnelMessage) {
  ws.send(TMessage::Text(serde_json::to_string(msg).unwrap().into()))
    .await
    .unwrap();
}

/// Reads the next frame with a timeout.
pub(super) async fn next_frame(ws: &mut Client) -> Option<TMessage> {
  tokio::time::timeout(Duration::from_secs(2), ws.next())
    .await
    .expect("frame timeout")
    .map(|r| r.expect("ws error"))
}

/// Reads frames until a `Pong` (text or compressed) arrives; returns whether
/// the transport frame was binary (i.e. compression is active).
pub(super) async fn read_until_pong(ws: &mut Client) -> bool {
  loop {
    match next_frame(ws).await.expect("stream ended before pong") {
      TMessage::Text(t) => {
        if let Ok(TunnelMessage::Pong { .. }) = serde_json::from_str::<TunnelMessage>(&t) {
          return false;
        }
      }
      TMessage::Binary(_) => return true,
      _ => {}
    }
  }
}

/// A default Ping with only the connection id set; individual tests mutate the
/// fields they care about.
pub(super) fn base_ping() -> TunnelMessage {
  TunnelMessage::Ping {
    services: None,
    service_custom_name: None,
    client_id: "self".into(),
    timestamp: 1,
    path_bind: None,
    hostname_bind: None,
    hostname_binds: Vec::new(),
    max_concurrent: None,
    tcp: false,
    version: None,
    protocol: None,
    backend_healthy: true,
    backend_probed: true,
    cpu_percent: None,
    rss_bytes: None,
    rtt_ms: None,
    jitter_ms: None,
    reconnects: None,
    priority: 0,
    bandwidth_bps: None,
    service: None,
    public: false,
    visitor_auth: None,
    visitor_auth_methods: None,
    allowed_ips: Vec::new(),
    tunnels: Vec::new(),
    cache: false,
    resilience: false,
    no_capture: false,
    max_request_body: None,
    response_timeout: None,
    client_key: None,
    webhook_inbox: false,
    denied: None,
    scaling: None,
    connections: None,
    connections_min: None,
    connections_max: None,
    config_notes: Vec::new(),
    metrics_labels: Default::default(),
    drain_secs: None,
  }
}

/// Creates a dynamic token in the store and returns its secret and record id.
pub(super) async fn make_dynamic_token(state: &AppState, allow_public: bool) -> (String, String) {
  let mut store = state.token_store.lock().await;
  let (rec, secret) = store
    .create(TokenSpec {
      name: "dyn".into(),
      allow_public,
      ..Default::default()
    })
    .expect("the test store can be written to");
  (secret, rec.id)
}

// --- tunnel slot accounting ------------------------------------------------

#[tokio::test]
pub(super) async fn tunnel_slot_released_when_upgrade_never_runs() {
  use std::sync::atomic::Ordering;
  let state = Arc::new(test_state());
  state.active_tunnel_count.store(1, Ordering::SeqCst);

  // axum drops the on_upgrade callback uncalled when the handshake dies, so
  // handle_socket never runs and the slot has to come back by itself.
  drop(TunnelSlot {
    state: state.clone(),
    armed: true,
  });
  assert_eq!(state.active_tunnel_count.load(Ordering::SeqCst), 0);

  // Once the callback runs, handle_socket owns the slot and the guard must
  // keep its hands off it.
  state.active_tunnel_count.store(1, Ordering::SeqCst);
  TunnelSlot {
    state: state.clone(),
    armed: true,
  }
  .handed_off();
  assert_eq!(state.active_tunnel_count.load(Ordering::SeqCst), 1);
}

// --- ws_handler rejection paths --------------------------------------------

#[tokio::test]
pub(super) async fn ws_handler_rejects_unauthorized() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let res = tokio_tungstenite::connect_async(client_request(&url, "wrong-token")).await;
  assert!(res.is_err(), "bad token must fail the handshake");
}

#[tokio::test]
pub(super) async fn ws_handler_rejects_when_tunnels_full() {
  let mut cfg = test_config();
  cfg.max_tunnels = 0;
  let state = Arc::new(test_state_with(cfg));
  let url = start_server(state.clone()).await;
  let res = tokio_tungstenite::connect_async(client_request(&url, "test")).await;
  assert!(res.is_err(), "full tunnel table must reject the upgrade");
}

// ---------------------------------------------------------------------------
// The writer task's two pure pieces, split out by the #21 decomposition.
// ---------------------------------------------------------------------------

#[test]
pub(super) fn writer_transform_compresses_what_it_should_and_only_that() {
  use crate::protocol::{FRAME_REQUEST_FULL, FRAME_REQUEST_FULL_ZLIB, encode_full_request_frame};

  // Off: everything passes through untouched.
  let text = Message::Text("{\"type\":\"Pong\"}".to_string().into());
  assert!(matches!(writer_transform(text, false), Message::Text(_)));

  // On: a text frame goes out deflated as binary.
  let text = Message::Text("{\"type\":\"Pong\"}".to_string().into());
  let out = writer_transform(text, true);
  let Message::Binary(b) = out else {
    panic!("a compressed text frame is binary");
  };
  assert_eq!(b.first(), Some(&0x78), "a zlib stream, tag byte and all");

  // A v6 full-request frame with a compressible body is re-tagged zlib.
  let body = vec![b'a'; 4096];
  let frame = encode_full_request_frame(FRAME_REQUEST_FULL, "req-1", "{}", &body).unwrap();
  let out = writer_transform(Message::Binary(frame.into()), true);
  let Message::Binary(b) = out else {
    panic!("still binary");
  };
  assert_eq!(b.first(), Some(&FRAME_REQUEST_FULL_ZLIB));
  assert!(b.len() < 4096, "deflate won: {} bytes", b.len());

  // An incompressible body keeps its plain tag: paying for the bytes twice
  // helps nobody.
  let mut x: u32 = 0x9E37_79B9;
  let noise: Vec<u8> = (0..4096)
    .map(|_| {
      // xorshift32: cheap, deterministic, and dense enough that deflate
      // cannot win against its own header overhead.
      x ^= x << 13;
      x ^= x >> 17;
      x ^= x << 5;
      (x >> 24) as u8
    })
    .collect();
  let frame = encode_full_request_frame(FRAME_REQUEST_FULL, "req-2", "{}", &noise).unwrap();
  let out = writer_transform(Message::Binary(frame.clone().into()), true);
  let Message::Binary(b) = out else {
    panic!("still binary");
  };
  assert_eq!(b.first(), Some(&FRAME_REQUEST_FULL));
  assert_eq!(b.len(), frame.len(), "unchanged");

  // A binary chunk frame (any other tag) is never touched.
  let chunk =
    crate::protocol::encode_binary_frame(crate::protocol::FRAME_RESPONSE_CHUNK, "id", b"data")
      .unwrap();
  let out = writer_transform(Message::Binary(chunk.clone().into()), true);
  let Message::Binary(b) = out else {
    panic!("still binary");
  };
  assert_eq!(b.as_ref(), chunk.as_slice());
}

#[test]
pub(super) fn the_pacer_charges_debt_only_past_the_burst() {
  let start = Instant::now();
  let mut pacer = SendPacer::new(start);
  let rate = 1000u64; // bytes per second

  // The first second's burst is free... once earned: a fresh bucket starts
  // empty, so the very first frame already owes its own transmission time.
  let debt = pacer.debt(500, rate, start).expect("an empty bucket owes");
  assert!((debt.as_secs_f64() - 0.5).abs() < 0.001, "{debt:?}");

  // A second later the bucket has refilled to the full burst; a small frame
  // inside it owes nothing.
  let later = start + Duration::from_secs(2);
  assert!(pacer.debt(500, rate, later).is_none());

  // A frame larger than the whole burst drives it negative and pays the
  // remainder as sleep: 2500 bytes against a 500-token bucket at 1000 B/s
  // owes two seconds.
  let debt = pacer.debt(2500, rate, later).expect("past the burst");
  assert!((debt.as_secs_f64() - 2.0).abs() < 0.001, "{debt:?}");
}

/// Sends one raw binary frame (a v7 relay payload) over the tunnel socket.
pub(super) async fn send_frame(ws: &mut Client, tag: u8, stream_id: &str, payload: &[u8]) {
  let frame = crate::protocol::encode_binary_frame(tag, stream_id, payload).expect("encodable");
  ws.send(TMessage::Binary(frame.into())).await.unwrap();
}

#[tokio::test]
pub(super) async fn v7_relay_frames_deliver_and_keep_their_ownership_fence() {
  // The v7 shapes of TcpData/UdpDatagram/WsData(binary): same delivery, same
  // ownership rule. The fence is the point: a client must not be able to
  // write into another client's relay stream by switching frame shape.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tcp_tx, mut tcp_rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "t7".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(tcp_tx),
      client_id: cid.clone(),
    },
  );
  let (udp_tx, mut udp_rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.udp_streams.lock().await.insert(
    "u7".into(),
    crate::state::UdpStreamHandle {
      tx: udp_tx,
      client_id: cid.clone(),
    },
  );
  let (ws_tx, mut ws_rx) = mpsc::channel::<crate::state::WsStreamMessage>(8);
  state.ws_streams.lock().await.insert(
    "w7".into(),
    crate::state::WsStreamHandle {
      tx: crate::state::test_pump(ws_tx),
      client_id: cid.clone(),
    },
  );
  // A stream this connection does not own, one of each kind.
  let (foreign_tcp, mut foreign_rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "foreign".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(foreign_tcp),
      client_id: "somebody-else".into(),
    },
  );

  send_frame(&mut ws, crate::protocol::FRAME_TCP_DATA, "t7", &[4u8, 5]).await;
  match tokio::time::timeout(Duration::from_secs(2), tcp_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d, vec![4, 5]),
    _ => panic!("expected a data frame"),
  }

  send_frame(&mut ws, crate::protocol::FRAME_UDP_DATAGRAM, "u7", b"dgram").await;
  match tokio::time::timeout(Duration::from_secs(2), udp_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d.as_ref(), b"dgram"),
    _ => panic!("expected a datagram"),
  }

  send_frame(&mut ws, crate::protocol::FRAME_WS_DATA_BIN, "w7", &[9u8, 9]).await;
  match tokio::time::timeout(Duration::from_secs(2), ws_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    crate::state::WsStreamMessage::Data(Message::Binary(b)) => assert_eq!(b.as_ref(), &[9, 9]),
    _ => panic!("expected a binary WS frame"),
  }

  // The fence: a v7 frame for a stream owned by another connection delivers
  // nothing, exactly as its JSON shape does not.
  send_frame(&mut ws, crate::protocol::FRAME_TCP_DATA, "foreign", b"x").await;
  // Round-trip a legitimate frame afterwards: if the foreign one had been
  // delivered it would have arrived before this.
  send_frame(&mut ws, crate::protocol::FRAME_TCP_DATA, "t7", b"after").await;
  match tokio::time::timeout(Duration::from_secs(2), tcp_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d.as_ref(), b"after"),
    _ => panic!("expected the owned delivery"),
  }
  assert!(
    foreign_rx.try_recv().is_err(),
    "a frame for another client's stream delivers nothing"
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// ---------------------------------------------------------------------------
// Alternate servers announced on the handshake (planned_features #52)
// ---------------------------------------------------------------------------

#[test]
pub(super) fn alternates_keep_only_what_a_tunnel_can_be_dialed_with() {
  // The list is announced to every client and tried by every client, so a
  // typo reaches further here than most, and dropping what cannot be a tunnel
  // URL is cheaper than every client discovering it one reconnect at a time.
  assert_eq!(
    parse_alternates("wss://eu.example.com/tunnel, https://not-a-tunnel, , ws://b/x"),
    vec![
      "wss://eu.example.com/tunnel".to_string(),
      "ws://b/x".to_string()
    ]
  );
  assert!(parse_alternates("").is_empty());
  assert!(parse_alternates("   ").is_empty());
}

#[test]
pub(super) fn alternates_are_capped() {
  let many = (0..40)
    .map(|i| format!("wss://s{i}.example.com"))
    .collect::<Vec<_>>()
    .join(",");
  // Clients walk this list in rotation; an unbounded one turns every
  // reconnect into a long walk through addresses nobody chose.
  assert_eq!(parse_alternates(&many).len(), MAX_ALTERNATES);
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// The protocol version announced on the handshake (#120)
// ---------------------------------------------------------------------------

#[tokio::test]
pub(super) async fn the_handshake_says_what_protocol_this_server_speaks() {
  // Read by a client before it has declared anything, which is what makes a
  // capability that changes the *first Ping* negotiable at all: multiplexing
  // is decided from this number, and a server too old to send it is a server
  // too old to serve a list.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let (token, _) = make_dynamic_token(&state, true).await;

  let (_ws, resp) = tokio_tungstenite::connect_async(client_request(&url, &token))
    .await
    .expect("the handshake");
  let announced = resp
    .headers()
    .get(PROTOCOL_HEADER)
    .expect("the announcement")
    .to_str()
    .unwrap()
    .parse::<u32>()
    .expect("a number");
  assert_eq!(announced, PROTOCOL_VERSION);
}

// ---------------------------------------------------------------------------
// What a connection may declare, announced on the handshake (#111)
// ---------------------------------------------------------------------------

/// The `x-aperio-visitor-auth-methods` value a handshake with this token gets.
pub(super) async fn announced_methods(url: &str, token: &str) -> String {
  let (_ws, resp) = tokio_tungstenite::connect_async(client_request(url, token))
    .await
    .expect("the handshake");
  resp
    .headers()
    .get(VISITOR_AUTH_METHODS_HEADER)
    .expect("the announcement")
    .to_str()
    .unwrap()
    .to_string()
}

#[tokio::test]
pub(super) async fn a_token_that_may_not_gate_is_told_it_may_declare_nothing() {
  // The announcement is about this connection, not about the build. Declaring
  // a visitor gate needs the same token permission as `public`, and a Ping
  // from a token without it has its policy dropped. Announcing the full list
  // to such a token would be the server contradicting itself one message
  // later: the client would be told its gate was accepted, keep serving, and
  // the route would come up with no gate at all while its own config said
  // otherwise.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;

  let (permitted, _) = make_dynamic_token(&state, true).await;
  assert_eq!(
    announced_methods(&url, &permitted).await,
    CLIENT_DECLARABLE_METHODS.join(","),
    "a token that may control the gate is told what this build accepts"
  );

  let (refused, _) = make_dynamic_token(&state, false).await;
  assert_eq!(
    announced_methods(&url, &refused).await,
    "",
    "a token that may not is told nothing may be declared, and holds the service back"
  );
}

#[tokio::test]
pub(super) async fn a_v8_entry_outranks_the_singular_field_it_disagrees_with() {
  // "Authoritative when present" is the whole reason the list is safe to add:
  // it is what stops a v8 client and a v8 server from half-agreeing, one
  // reading the entry and the other the field beside it. A protocol that only
  // says so in its documentation says nothing, and this is the shape where
  // nobody would notice: both spellings are well-formed, both parse, and the
  // wrong winner is simply a service running with a setting its operator did
  // not write.
  //
  // The two values are deliberately both valid and both plausible, so the
  // assertion cannot pass by one of them being rejected.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services,
    ref mut max_concurrent,
    ref mut response_timeout,
    ..
  } = ping
  {
    *max_concurrent = Some(5);
    *response_timeout = Some(11);
    *services = Some(vec![crate::protocol::ServiceDecl {
      max_concurrent: Some(9),
      response_timeout: Some(22),
      ..Default::default()
    }]);
  }
  send(&mut ws, &ping).await;

  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("the connection is served");
  assert_eq!(
    handle.sole().max_concurrent,
    Some(9),
    "the entry's concurrency wins over the singular field"
  );
  assert_eq!(
    handle.sole().response_timeout,
    Some(22),
    "the entry's response timeout wins over the singular field"
  );
}

#[tokio::test]
pub(super) async fn without_a_list_the_singular_fields_still_decide() {
  // The other half, and the one that matters for every client in the field:
  // absent a list, nothing about the old spelling changed. Without this the
  // test above could be satisfied by an implementation that reads the entry
  // and ignores the fields unconditionally, which would break every client
  // that predates v8.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services,
    ref mut max_concurrent,
    ref mut response_timeout,
    ..
  } = ping
  {
    *services = None;
    *max_concurrent = Some(5);
    *response_timeout = Some(11);
  }
  send(&mut ws, &ping).await;

  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("the connection is served");
  assert_eq!(handle.sole().max_concurrent, Some(5));
  assert_eq!(handle.sole().response_timeout, Some(11));
}

#[tokio::test]
pub(super) async fn a_named_service_keeps_its_state_across_heartbeats() {
  // The reason declarations are matched by name rather than by position. The
  // state that has to survive is the state the wire does not carry: how many
  // requests this service has served, whether it is ejected, which warnings
  // it has already produced. A second Ping is an update, not a new service.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![crate::protocol::ServiceDecl {
      service: Some("api".into()),
      ..Default::default()
    }]);
  }
  send(&mut ws, &ping).await;
  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;

  // Something only the server knows, which a fresh service would not have.
  {
    let mut clients = state.clients.write().await;
    let handle = clients.get_mut(&id).expect("served");
    assert_eq!(
      handle.services.len(),
      1,
      "the first Ping adopted, not added"
    );
    handle.services[0]
      .request_count
      .store(7, std::sync::atomic::Ordering::SeqCst);
  }

  send(&mut ws, &ping).await;
  let _ = read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("still served");
  assert_eq!(handle.services.len(), 1, "still one service, not two");
  assert_eq!(
    handle.services[0]
      .request_count
      .load(std::sync::atomic::Ordering::SeqCst),
    7,
    "the second heartbeat updated the same service"
  );
}

#[tokio::test]
pub(super) async fn a_ping_declaring_two_services_is_served_as_two() {
  // The milestone this entry existed for, and the first test in the tree
  // where a connection carries more than one service because a *client* said
  // so rather than because a fixture reached in and put it there.
  //
  // Each entry has to land on its own service with its own binds. The failure
  // this rules out is the quiet one: both declarations applied to the same
  // service, leaving one of them serving the other's backend.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![
      crate::protocol::ServiceDecl {
        service: Some("api".into()),
        path_bind: Some("/api".into()),
        // Declared healthy, as a real client does: the entry is authoritative,
        // so an omitted flag is a service announcing itself down.
        backend_healthy: true,
        response_timeout: Some(11),
        ..Default::default()
      },
      crate::protocol::ServiceDecl {
        service: Some("web".into()),
        path_bind: Some("/web".into()),
        // Declared healthy, as a real client does: the entry is authoritative,
        // so an omitted flag is a service announcing itself down.
        backend_healthy: true,
        response_timeout: Some(22),
        ..Default::default()
      },
    ]);
  }
  send(&mut ws, &ping).await;
  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("served, not refused");
  assert_eq!(handle.services.len(), 2, "one connection, two services");

  let api = handle
    .services
    .iter()
    .find(|s| s.service_name.as_deref() == Some("api"))
    .expect("the api service");
  let web = handle
    .services
    .iter()
    .find(|s| s.service_name.as_deref() == Some("web"))
    .expect("the web service");
  assert_eq!(api.declared_path.as_deref(), Some("/api"));
  assert_eq!(web.declared_path.as_deref(), Some("/web"));
  assert_eq!(
    api.response_timeout,
    Some(11),
    "each keeps its own settings"
  );
  assert_eq!(web.response_timeout, Some(22));

  // And routing can tell them apart, which is what makes them two services
  // rather than two rows.
  let (pool, _) = crate::routing::select_client_pool(
    &clients,
    "/web/x",
    None,
    false,
    std::time::Duration::from_secs(3600),
  )
  .expect("routed");
  assert_eq!(pool.len(), 1);
  assert_eq!(
    handle.services[pool[0].index].service_name.as_deref(),
    Some("web"),
    "a request under /web goes to the web service"
  );
}

#[tokio::test]
pub(super) async fn a_service_the_client_stops_declaring_leaves_routing() {
  // The other half of reconcile. A service that is no longer declared has to
  // go, or a client that removes one from its config keeps serving it until
  // it reconnects, which is the kind of thing an operator finds out about
  // from traffic they thought they had turned off.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let two = |list: Vec<crate::protocol::ServiceDecl>| {
    let mut ping = base_ping();
    if let TunnelMessage::Ping {
      ref mut services, ..
    } = ping
    {
      *services = Some(list);
    }
    ping
  };

  send(
    &mut ws,
    &two(vec![
      crate::protocol::ServiceDecl {
        service: Some("api".into()),
        path_bind: Some("/api".into()),
        // Declared healthy, as a real client does: the entry is authoritative,
        // so an omitted flag is a service announcing itself down.
        backend_healthy: true,
        ..Default::default()
      },
      crate::protocol::ServiceDecl {
        service: Some("web".into()),
        path_bind: Some("/web".into()),
        // Declared healthy, as a real client does: the entry is authoritative,
        // so an omitted flag is a service announcing itself down.
        backend_healthy: true,
        ..Default::default()
      },
    ]),
  )
  .await;
  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;
  assert_eq!(state.clients.read().await[&id].services.len(), 2);

  send(
    &mut ws,
    &two(vec![crate::protocol::ServiceDecl {
      service: Some("api".into()),
      path_bind: Some("/api".into()),
      // Declared healthy, as a real client does: the entry is authoritative,
      // so an omitted flag is a service announcing itself down.
      backend_healthy: true,
      ..Default::default()
    }]),
  )
  .await;
  let _ = read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("still served");
  assert_eq!(handle.services.len(), 1, "the withdrawn service is gone");
  assert_eq!(handle.services[0].service_name.as_deref(), Some("api"));
  assert!(
    crate::routing::select_client_pool(
      &clients,
      "/web/x",
      None,
      false,
      std::time::Duration::from_secs(3600),
    )
    .is_none(),
    "and nothing routes to it any more"
  );
}

#[tokio::test]
pub(super) async fn a_service_added_later_announces_into_the_cell_the_writer_reads() {
  // The bandwidth cap is applied by the connection's writer task, which holds
  // one cell for the socket. A service the client adds mid-connection used to
  // be given a fresh cell of its own, so its announcement went somewhere
  // nothing reads: `/aperio/api/stats` reported the service as throttled
  // while the wire ran at whatever the first service had asked for. Nothing
  // errors in that state, which is why it is worth a test rather than a
  // glance.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let decl = |name: &str, bw: Option<u64>| crate::protocol::ServiceDecl {
    service: Some(name.into()),
    backend_healthy: true,
    bandwidth_bps: bw,
    ..Default::default()
  };
  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![decl("api", None)]);
  }
  send(&mut ws, &ping).await;
  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;

  // The cell the writer was handed when the connection opened.
  let writers_cell = {
    let clients = state.clients.read().await;
    clients.get(&id).expect("served").services[0]
      .bandwidth_bps
      .clone()
  };

  let mut ping2 = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping2
  {
    *services = Some(vec![decl("api", None), decl("web", Some(125_000))]);
  }
  send(&mut ws, &ping2).await;
  let _ = read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("still served");
  assert_eq!(handle.services.len(), 2, "the second service was added");
  assert_eq!(
    writers_cell.load(std::sync::atomic::Ordering::SeqCst),
    125_000,
    "the announcement reached the cell the writer actually paces from"
  );
}
