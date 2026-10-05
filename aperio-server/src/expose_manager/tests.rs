//! Acceptance checks use the manager's bound sockets and durable mutation path.
use super::*;
use crate::{
  protocol::{TunnelDecl, TunnelMessage},
  state::TcpConsumerMsg,
  test_support::{mock_client, test_state},
};
use axum::{body::Bytes, extract::ws::Message};
use base64::prelude::*;
use tokio::{
  io::{AsyncReadExt, AsyncWriteExt},
  net::{TcpStream, UdpSocket},
  sync::mpsc,
};

fn resource(protocol: ExposeProtocol, port: u16) -> ExposeResource {
  ExposeResource {
    id: uuid::Uuid::new_v4().to_string(),
    revision: 1,
    source: ExposeSource::Api,
    spec: ExposeSpec {
      org_id: "master".into(),
      tunnel: "echo".into(),
      listener: ExposeListener {
        address: "127.0.0.1".parse().unwrap(),
        port,
        protocol,
      },
      enabled: true,
      limits: ExposeLimits::defaults(protocol),
      allowed_ips: Vec::new(),
      advertised_host: None,
    },
  }
}
async fn fresh(protocol: ExposeProtocol) -> ExposeResource {
  let port = match protocol {
    ExposeProtocol::Tcp => tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .unwrap()
      .local_addr()
      .unwrap()
      .port(),
    ExposeProtocol::Udp => UdpSocket::bind("127.0.0.1:0")
      .await
      .unwrap()
      .local_addr()
      .unwrap()
      .port(),
  };
  resource(protocol, port)
}
async fn install(state: &Arc<AppState>, rules: Vec<ExposeResource>) -> Result<(), String> {
  let mut manager = state.exposes.lock().await;
  let mut doc = manager.store.document.clone();
  doc.resources = rules;
  manager.replace(state, doc, false).await
}
async fn client(state: &Arc<AppState>, version: u32) -> mpsc::Receiver<Message> {
  let (tx, rx) = mpsc::channel(128);
  let mut c = mock_client(None, None, None, None);
  c.tx = tx;
  c.client_protocol = Some(version);
  c.sole_mut().tunnels = vec![TunnelDecl {
    name: Some("echo".into()),
    custom_name: None,
    target: "127.0.0.1:9000".into(),
    protocol: "tcp/udp".into(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }];
  state.clients.write().await.insert("client".into(), c);
  rx
}
async fn message(rx: &mut mpsc::Receiver<Message>) -> TunnelMessage {
  match tokio::time::timeout(Duration::from_secs(3), rx.recv())
    .await
    .expect("relay deadline")
    .expect("client channel")
  {
    Message::Text(json) => serde_json::from_str(&json).unwrap(),
    other => panic!("unexpected frame {other:?}"),
  }
}
async fn udp_open(rx: &mut mpsc::Receiver<Message>, expected: &[u8]) -> String {
  let id = match message(rx).await {
    TunnelMessage::UdpOpen { stream_id, .. } => stream_id,
    other => panic!("{other:?}"),
  };
  match message(rx).await {
    TunnelMessage::UdpDatagram { stream_id, data } => {
      assert_eq!(stream_id, id);
      assert_eq!(BASE64_STANDARD.decode(data).unwrap(), expected);
    }
    other => panic!("{other:?}"),
  }
  id
}
async fn udp_response(state: &Arc<AppState>, id: &str, payload: &'static [u8]) {
  state
    .udp_streams
    .lock()
    .await
    .get(id)
    .unwrap()
    .tx
    .send(TcpConsumerMsg::Data(Bytes::from_static(payload)))
    .await
    .unwrap();
}
async fn packet(socket: &UdpSocket) -> Vec<u8> {
  let mut buffer = vec![0; 65536];
  let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buffer))
    .await
    .unwrap()
    .unwrap();
  buffer.truncate(n);
  buffer
}
async fn clean(state: &Arc<AppState>) {
  state.exposes.lock().await.shutdown().await;
  tokio::time::timeout(Duration::from_secs(3), async {
    loop {
      if state.udp_streams.lock().await.is_empty() && state.tcp_streams.lock().await.is_empty() {
        break;
      }
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("stream registry cleanup");
}

#[tokio::test]
async fn udp_preserves_peer_identity_empty_datagrams_and_shutdown_releases_port() {
  let state = Arc::new(test_state());
  let mut rx = client(&state, 6).await;
  let rule = fresh(ExposeProtocol::Udp).await;
  let address = SocketAddr::new(rule.spec.listener.address, rule.spec.listener.port);
  install(&state, vec![rule]).await.unwrap();
  let one = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  one.connect(address).await.unwrap();
  let two = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  two.connect(address).await.unwrap();
  one.send(b"one").await.unwrap();
  let first = udp_open(&mut rx, b"one").await;
  two.send(b"").await.unwrap();
  let second = udp_open(&mut rx, b"").await;
  assert_ne!(first, second);
  udp_response(&state, &second, b"two-response").await;
  udp_response(&state, &first, b"").await;
  assert_eq!(packet(&two).await, b"two-response");
  assert!(packet(&one).await.is_empty());
  let sessions = state.exposes.lock().await.sessions(false);
  assert_eq!(sessions.len(), 2);
  assert!(
    sessions
      .iter()
      .all(|s| s.up_packets == 1 && s.down_packets == 1)
  );
  clean(&state).await;
  let rebound = UdpSocket::bind(address)
    .await
    .expect("shutdown released public socket");
  drop(rebound);
}

#[tokio::test]
async fn bind_failure_keeps_committed_rules_and_unrelated_socket() {
  let state = Arc::new(test_state());
  let rule = fresh(ExposeProtocol::Tcp).await;
  install(&state, vec![rule.clone()]).await.unwrap();
  let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let conflict = resource(ExposeProtocol::Tcp, occupied.local_addr().unwrap().port());
  let revision = state.exposes.lock().await.store.document.revision;
  assert!(
    install(&state, vec![rule.clone(), conflict])
      .await
      .unwrap_err()
      .contains("bind")
  );
  let manager = state.exposes.lock().await;
  assert_eq!(manager.store.document.revision, revision);
  assert_eq!(manager.store.document.resources, vec![rule.clone()]);
  assert!(manager.entries[&rule.id].runtime.control.borrow().accepting);
  drop(manager);
  TcpStream::connect(("127.0.0.1", rule.spec.listener.port))
    .await
    .expect("old socket survives");
  clean(&state).await;
}

#[tokio::test]
async fn failed_database_commit_never_activates_staged_socket() {
  let state = Arc::new(test_state());
  let path =
    crate::test_support::test_temp_root().join(format!("readonly-expose-{}", uuid::Uuid::new_v4()));
  let connection = crate::store::open_db(&path.to_string_lossy());
  connection.execute_batch("PRAGMA query_only = ON").unwrap();
  state.exposes.lock().await.store = ExposeStore::from_connection(connection);
  let rule = fresh(ExposeProtocol::Tcp).await;
  assert!(install(&state, vec![rule.clone()]).await.is_err());
  assert!(
    state
      .exposes
      .lock()
      .await
      .store
      .document
      .resources
      .is_empty()
  );
  assert!(state.exposes.lock().await.entries.is_empty());
  tokio::net::TcpListener::bind(("127.0.0.1", rule.spec.listener.port))
    .await
    .expect("staged socket rolled back");
}

#[tokio::test]
async fn tcp_and_udp_can_share_port_but_wildcard_same_transport_conflicts() {
  let state = Arc::new(test_state());
  let tcp = fresh(ExposeProtocol::Tcp).await;
  let udp = resource(ExposeProtocol::Udp, tcp.spec.listener.port);
  install(&state, vec![tcp.clone(), udp.clone()])
    .await
    .unwrap();
  let mut wildcard = resource(ExposeProtocol::Tcp, tcp.spec.listener.port);
  wildcard.spec.listener.address = "0.0.0.0".parse().unwrap();
  assert!(
    install(&state, vec![tcp, udp, wildcard])
      .await
      .unwrap_err()
      .contains("conflict")
  );
  clean(&state).await;
}

#[tokio::test]
async fn tcp_write_eof_uses_negotiated_message_and_keeps_response_direction() {
  let state = Arc::new(test_state());
  let mut rx = client(&state, 10).await;
  let rule = fresh(ExposeProtocol::Tcp).await;
  install(&state, vec![rule.clone()]).await.unwrap();
  let mut visitor = TcpStream::connect(("127.0.0.1", rule.spec.listener.port))
    .await
    .unwrap();
  let id = match message(&mut rx).await {
    TunnelMessage::TcpOpen {
      stream_id, visitor, ..
    } => {
      assert!(visitor.is_some());
      stream_id
    }
    other => panic!("{other:?}"),
  };
  visitor.shutdown().await.unwrap();
  assert!(matches!(message(&mut rx).await, TunnelMessage::TcpEof { stream_id } if stream_id == id));
  let streams = state.tcp_streams.lock().await;
  streams[&id]
    .tx
    .push(TcpConsumerMsg::Data(Bytes::from_static(b"response")))
    .unwrap();
  streams[&id].tx.push(TcpConsumerMsg::Close).unwrap();
  drop(streams);
  let mut response = Vec::new();
  tokio::time::timeout(Duration::from_secs(3), visitor.read_to_end(&mut response))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(response, b"response");
  clean(&state).await;
}

#[tokio::test]
async fn udp_drain_keeps_existing_peer_rejects_new_peer_and_expires() {
  let state = Arc::new(test_state());
  let mut rx = client(&state, 6).await;
  let rule = fresh(ExposeProtocol::Udp).await;
  let address = SocketAddr::new(rule.spec.listener.address, rule.spec.listener.port);
  install(&state, vec![rule.clone()]).await.unwrap();
  let one = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  one.connect(address).await.unwrap();
  one.send(b"one").await.unwrap();
  let id = udp_open(&mut rx, b"one").await;
  state
    .exposes
    .lock()
    .await
    .drain(&rule.id, Duration::from_millis(500))
    .unwrap();
  let two = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  two.send_to(b"new", address).await.unwrap();
  one.send(b"existing").await.unwrap();
  assert!(
    matches!(message(&mut rx).await, TunnelMessage::UdpDatagram { stream_id, .. } if stream_id == id)
  );
  udp_response(&state, &id, b"still-open").await;
  assert_eq!(packet(&one).await, b"still-open");
  tokio::time::timeout(Duration::from_secs(3), async {
    loop {
      if state.exposes.lock().await.sessions(false).is_empty() {
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("bounded drain");
  clean(&state).await;
  UdpSocket::bind(address)
    .await
    .expect("drained socket released");
}

#[tokio::test]
async fn udp_target_edits_only_affect_new_peers() {
  let state = Arc::new(test_state());
  let mut rx = client(&state, 6).await;
  {
    let mut clients = state.clients.write().await;
    let service = clients.get_mut("client").unwrap().sole_mut();
    let mut next = service.tunnels[0].clone();
    next.name = Some("other".into());
    next.target = "127.0.0.1:9001".into();
    service.tunnels.push(next);
  }
  let mut rule = fresh(ExposeProtocol::Udp).await;
  let address = SocketAddr::new(rule.spec.listener.address, rule.spec.listener.port);
  install(&state, vec![rule.clone()]).await.unwrap();
  let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  first.connect(address).await.unwrap();
  first.send(b"initial").await.unwrap();
  let old_id = udp_open(&mut rx, b"initial").await;
  rule.spec.tunnel = "other".into();
  rule.revision += 1;
  install(&state, vec![rule]).await.unwrap();
  first.send(b"pinned").await.unwrap();
  assert!(
    matches!(message(&mut rx).await, TunnelMessage::UdpDatagram { stream_id, .. } if stream_id == old_id)
  );
  let second = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  second.send_to(b"new", address).await.unwrap();
  let new_id = match message(&mut rx).await {
    TunnelMessage::UdpOpen {
      stream_id, target, ..
    } => {
      assert_eq!(target, "127.0.0.1:9001");
      stream_id
    }
    other => panic!("{other:?}"),
  };
  assert!(
    matches!(message(&mut rx).await, TunnelMessage::UdpDatagram { stream_id, .. } if stream_id == new_id)
  );
  let sessions = state.exposes.lock().await.sessions(false);
  assert_eq!(
    sessions.iter().find(|s| s.id == old_id).unwrap().target,
    "127.0.0.1:9000"
  );
  assert_eq!(
    sessions.iter().find(|s| s.id == new_id).unwrap().target,
    "127.0.0.1:9001"
  );
  clean(&state).await;
}

#[tokio::test]
async fn invalid_file_reload_keeps_api_and_file_generations() {
  let state = Arc::new(test_state());
  let api = fresh(ExposeProtocol::Tcp).await;
  let mut file_resource = fresh(ExposeProtocol::Udp).await;
  file_resource.source = ExposeSource::File;
  file_resource.id = format!("file:udp:127.0.0.1:{}", file_resource.spec.listener.port);
  let file = ManagedRule {
    legacy: Some(aperio_config::ExposeEntry {
      protocol: "udp".into(),
      port: file_resource.spec.listener.port,
      tunnel: Some("echo".into()),
      ..Default::default()
    }),
    resource: file_resource,
  };
  let mut manager = state.exposes.lock().await;
  let mut document = manager.store.document.clone();
  document.resources = vec![api.clone()];
  manager
    .apply(&state, document.clone(), vec![file.clone()], true, false)
    .await
    .unwrap();
  let before = manager.store.document.revision;
  let mut conflict = file.clone();
  conflict.resource.spec.listener = api.spec.listener.clone();
  assert!(
    manager
      .apply(&state, document, vec![conflict], false, false)
      .await
      .is_err()
  );
  assert_eq!(manager.store.document.revision, before);
  assert_eq!(
    manager.entries[&file.resource.id].rule.resource,
    file.resource
  );
  assert_eq!(manager.entries[&api.id].rule.resource, api);
  drop(manager);
  clean(&state).await;
}

#[tokio::test]
async fn udp_packet_and_per_ip_limits_bound_admission() {
  let state = Arc::new(test_state());
  let mut rx = client(&state, 6).await;
  let mut rule = fresh(ExposeProtocol::Udp).await;
  if let ExposeLimits::Udp {
    max_sessions_per_ip,
    max_datagram_bytes,
    queue_bytes,
    ..
  } = &mut rule.spec.limits
  {
    *max_sessions_per_ip = 1;
    *max_datagram_bytes = 32;
    *queue_bytes = 64;
  }
  let address = SocketAddr::new(rule.spec.listener.address, rule.spec.listener.port);
  install(&state, vec![rule.clone()]).await.unwrap();
  let one = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  one.connect(address).await.unwrap();
  one.send(b"accepted").await.unwrap();
  let _id = udp_open(&mut rx, b"accepted").await;
  one.send(&[0; 33]).await.unwrap();
  let two = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  two.send_to(b"rejected", address).await.unwrap();
  tokio::time::timeout(Duration::from_secs(3), async {
    loop {
      let manager = state.exposes.lock().await;
      let drops = manager.entries[&rule.id]
        .runtime
        .drops
        .lock()
        .unwrap()
        .clone();
      if drops.get("oversize") == Some(&1) && drops.get("peer_limit") == Some(&1) {
        break;
      }
      drop(manager);
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("limit evidence");
  assert_eq!(state.exposes.lock().await.sessions(false).len(), 1);
  assert!(rx.try_recv().is_err());
  clean(&state).await;
}

#[tokio::test]
async fn udp_flood_with_slow_tunnel_stays_bounded_and_another_listener_progresses() {
  let state = Arc::new(test_state());
  let (blocked_tx, _blocked_rx) = mpsc::channel(1);
  let mut slow = mock_client(None, None, None, None);
  slow.tx = blocked_tx;
  slow.sole_mut().tunnels = vec![TunnelDecl {
    name: Some("slow".into()),
    custom_name: None,
    target: "127.0.0.1:9001".into(),
    protocol: "udp".into(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }];
  state
    .clients
    .write()
    .await
    .insert("slow-client".into(), slow);
  let mut healthy_rx = client(&state, 6).await;
  let mut udp = fresh(ExposeProtocol::Udp).await;
  udp.spec.tunnel = "slow".into();
  if let ExposeLimits::Udp {
    max_sessions,
    max_sessions_per_ip,
    max_datagram_bytes,
    queue_packets,
    queue_bytes,
    ..
  } = &mut udp.spec.limits
  {
    *max_sessions = 4;
    *max_sessions_per_ip = 4;
    *max_datagram_bytes = 32;
    *queue_packets = 2;
    *queue_bytes = 64;
  }
  let tcp = fresh(ExposeProtocol::Tcp).await;
  install(&state, vec![udp.clone(), tcp.clone()])
    .await
    .unwrap();
  let address = SocketAddr::new(udp.spec.listener.address, udp.spec.listener.port);
  let mut peers = Vec::new();
  for _ in 0..12 {
    peers.push(UdpSocket::bind("127.0.0.1:0").await.unwrap());
  }
  for round in 0..200 {
    for peer in &peers {
      peer.send_to(&[round as u8; 32], address).await.unwrap();
    }
    if round % 10 == 0 {
      tokio::task::yield_now().await;
    }
  }
  let mut visitor = TcpStream::connect(("127.0.0.1", tcp.spec.listener.port))
    .await
    .unwrap();
  let id = match message(&mut healthy_rx).await {
    TunnelMessage::TcpOpen { stream_id, .. } => stream_id,
    other => panic!("{other:?}"),
  };
  state.tcp_streams.lock().await[&id]
    .tx
    .push(TcpConsumerMsg::Data(Bytes::from_static(b"progress")))
    .unwrap();
  let mut response = [0; 8];
  tokio::time::timeout(Duration::from_secs(3), visitor.read_exact(&mut response))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&response, b"progress");
  let manager = state.exposes.lock().await;
  let runtime = &manager.entries[&udp.id].runtime;
  assert!(runtime.sessions.lock().unwrap().len() <= 4);
  let drops = runtime.drops.lock().unwrap().clone();
  assert!(
    drops.get("session_limit").copied().unwrap_or(0)
      + drops.get("ingress_queue").copied().unwrap_or(0)
      + drops.get("client_queue").copied().unwrap_or(0)
      > 0
  );
  drop(manager);
  assert!(state.udp_streams.lock().await.len() <= 4);
  // Two directions, at most four peer queues and two 32-byte packets each:
  // the configured payload queues cannot accumulate the 2400 sent packets.
  for stream in state.udp_streams.lock().await.values() {
    assert_eq!(stream.tx.max_capacity(), 2);
  }
  clean(&state).await;
}

#[tokio::test]
async fn durable_rules_survive_reopen_and_corrupt_data_is_preserved() {
  let path =
    crate::test_support::test_temp_root().join(format!("durable-expose-{}", uuid::Uuid::new_v4()));
  let data_dir = path.to_string_lossy();
  let mut store = ExposeStore::load(&data_dir);
  let mut rule = fresh(ExposeProtocol::Tcp).await;
  rule.spec.enabled = false;
  let mut doc = store.document.clone();
  doc.resources.push(rule.clone());
  store.commit(doc).unwrap();
  let revision = store.document.revision;
  drop(store);
  let reopened = ExposeStore::load(&data_dir);
  assert_eq!(reopened.document.resources, vec![rule]);
  assert_eq!(reopened.document.revision, revision);
  drop(reopened);
  let connection = crate::store::open_db(&data_dir);
  connection
    .execute(
      "UPDATE expose_config SET data = 'invalid-json' WHERE id = 'desired'",
      [],
    )
    .unwrap();
  let mut broken = ExposeStore::load(&data_dir);
  assert!(broken.load_error.is_some());
  assert!(broken.commit(ExposeDocument::default()).is_err());
  let raw: String = connection
    .query_row(
      "SELECT data FROM expose_config WHERE id = 'desired'",
      [],
      |row| row.get(0),
    )
    .unwrap();
  assert_eq!(raw, "invalid-json");
}
