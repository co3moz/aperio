//! Public socket relays, bounded per listener and cancelled with their owner.
//! Session cleanup runs on every exit, including cancellation and panic.

use super::resolve::Target;
use super::*;
use crate::protocol::TunnelMessage;
use crate::state::{TcpConsumerMsg, TcpStreamHandle, UdpStreamHandle};
use axum::body::Bytes;
use axum::extract::ws::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

struct SessionGuard {
  state: Weak<AppState>,
  runtime: Arc<Runtime>,
  target: Target,
  id: String,
  protocol: ExposeProtocol,
  reason: &'static str,
  record: Option<crate::relay_log::RelayRecord>,
}

impl Drop for SessionGuard {
  fn drop(&mut self) {
    let mut ended = None;
    if let Some(mut session) = self
      .runtime
      .sessions
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .remove(&self.id)
    {
      session.view.ended_at = Some(crate::store::tokens::now_secs());
      session.view.reason = Some(self.reason.into());
      let mut history = self
        .runtime
        .history
        .lock()
        .unwrap_or_else(|e| e.into_inner());
      ended = Some(session.view.clone());
      history.push_back(session.view);
      while history.len() > 256 {
        history.pop_front();
      }
    }
    let Some(state) = self.state.upgrade() else {
      return;
    };
    if let Some(record) = self.record.take() {
      record.finish(&state);
    }
    let (id, protocol, tx) = (self.id.clone(), self.protocol, self.target.tx.clone());
    tokio::spawn(async move {
      let close = match protocol {
        ExposeProtocol::Tcp => {
          state.tcp_streams.lock().await.remove(&id);
          TunnelMessage::TcpClose { stream_id: id }
        }
        ExposeProtocol::Udp => {
          state.udp_streams.lock().await.remove(&id);
          TunnelMessage::UdpClose { stream_id: id }
        }
      };
      if let Some(ended) = ended {
        state.exposes.lock().await.store.record_session(&ended);
      }
      let _ = send(&tx, close, Duration::from_secs(2)).await;
    });
  }
}

async fn send(tx: &mpsc::Sender<Message>, message: TunnelMessage, timeout: Duration) -> bool {
  let Ok(json) = serde_json::to_string(&message) else {
    return false;
  };
  matches!(
    tokio::time::timeout(timeout, tx.send(Message::Text(json.into()))).await,
    Ok(Ok(()))
  )
}

fn source_allowed(state: &AppState, spec: &ExposeSpec, peer: IpAddr) -> bool {
  (spec.allowed_ips.is_empty()
    || spec
      .allowed_ips
      .iter()
      .any(|range| aperio_config::expose_policy::network_contains(range, peer)))
    && !state.config().denied_ips.blocks(peer)
}

fn admit(
  runtime: &Arc<Runtime>,
  rule: &ManagedRule,
  peer: SocketAddr,
  target: Target,
  state: &Arc<AppState>,
) -> Option<(SessionGuard, watch::Receiver<bool>)> {
  let mut sessions = runtime.sessions.lock().unwrap_or_else(|e| e.into_inner());
  if sessions.len() >= rule.resource.spec.limits.capacity() as usize {
    runtime.dropped("session_limit");
    return None;
  }
  if let ExposeLimits::Udp {
    max_sessions_per_ip,
    new_sessions_per_second,
    ..
  } = rule.resource.spec.limits
  {
    if sessions
      .values()
      .filter(|s| s.view.peer.ip() == peer.ip())
      .count()
      >= max_sessions_per_ip as usize
    {
      runtime.dropped("peer_limit");
      return None;
    }
    if !runtime
      .budget
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .open(new_sessions_per_second)
    {
      runtime.dropped("admission_rate");
      return None;
    }
  }
  let id = uuid::Uuid::new_v4().to_string();
  let (stop, receiver) = watch::channel(false);
  sessions.insert(
    id.clone(),
    Session {
      touched: Instant::now(),
      stop,
      view: SessionView {
        id: id.clone(),
        expose_id: rule.resource.id.clone(),
        org_id: rule.resource.spec.org_id.clone(),
        revision: rule.resource.revision,
        protocol: rule.resource.spec.listener.protocol,
        peer,
        client_id: target.client_id.clone(),
        target: target.target.clone(),
        started_at: crate::store::tokens::now_secs(),
        up_bytes: 0,
        down_bytes: 0,
        up_packets: 0,
        down_packets: 0,
        idle_seconds: 0,
        ended_at: None,
        reason: None,
      },
    },
  );
  runtime.opened.fetch_add(1, Ordering::Relaxed);
  let record = crate::relay_log::RelayRecord::new(
    rule.resource.spec.listener.protocol.as_str(),
    "expose",
    peer.to_string(),
    target.client_id.clone(),
  )
  .tunnel(Some(rule.resource.spec.tunnel.clone()))
  .port(rule.resource.spec.listener.port);
  Some((
    SessionGuard {
      state: Arc::downgrade(state),
      runtime: runtime.clone(),
      id,
      protocol: rule.resource.spec.listener.protocol,
      target,
      reason: "relay_closed",
      record: Some(record),
    },
    receiver,
  ))
}

pub(super) async fn listen(state: Weak<AppState>, runtime: Arc<Runtime>, socket: BoundSocket) {
  let mut control = runtime.control.subscribe();
  let Some(initial) = state.upgrade() else {
    return;
  };
  let mut shutdown = initial.shutdown.subscribe();
  drop(initial);
  let mut tasks = tokio::task::JoinSet::new();
  let mut peers: HashMap<SocketAddr, (String, mpsc::Sender<Bytes>)> = HashMap::new();
  let (finished_tx, mut finished_rx) = mpsc::unbounded_channel::<(SocketAddr, String)>();
  let mut buffer = vec![0u8; 65536];
  let mut tick = tokio::time::interval(Duration::from_secs(1));
  let mut socket_errors = 0;
  loop {
    let current = control.borrow().clone();
    if *shutdown.borrow()
      || current.terminate
      || current.deadline.is_some_and(|d| Instant::now() >= d)
    {
      break;
    }
    if !current.accepting
      && runtime
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
      break;
    }
    tokio::select! {
      _ = shutdown.changed() => {},
      _ = control.changed() => {},
      Some((peer, id)) = finished_rx.recv() => {
        if peers.get(&peer).is_some_and(|(owned, _)| owned == &id) { peers.remove(&peer); }
      },
      Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
      _ = tick.tick() => {
        let Some(state) = state.upgrade() else { break; };
        let org = &current.rule.resource.spec.org_id;
        if org != "master" && !org.starts_with("unresolved:") && state.org_store.lock().await.find(org).is_none() {
          runtime.status(ExposeListenerState::Suspended, Some("organization no longer exists".into()));
          break;
        }
      },
      accepted = accept(&socket), if matches!(socket, BoundSocket::Tcp(_)) && current.accepting => {
        let Ok((stream, peer)) = accepted else {
          runtime.dropped("accept_error"); socket_errors += 1;
          if socket_errors >= 3 { runtime.status(ExposeListenerState::Failed, Some("repeated socket accept failures".into())); break; }
          tokio::time::sleep(Duration::from_millis(100)).await; continue;
        };
        socket_errors = 0;
        let Some(state) = state.upgrade() else { break; };
        let rule = runtime.control.borrow().rule.clone();
        if !source_allowed(&state, &rule.resource.spec, peer.ip()) || !state.check_rate_limit(peer.ip()).await {
          runtime.dropped("source_policy"); continue;
        }
        let Ok(target) = resolve::target(&state, &rule).await else { runtime.dropped("target_unavailable"); continue; };
        if let Some((guard, stop)) = admit(&runtime, &rule, peer, target, &state) {
          tasks.spawn(tcp(state, runtime.clone(), rule, stream, peer, guard, stop));
        }
      },
      received = receive(&socket, &mut buffer), if matches!(socket, BoundSocket::Udp(_)) => {
        let Ok((size, peer)) = received else {
          runtime.dropped("receive_error"); socket_errors += 1;
          if socket_errors >= 3 { runtime.status(ExposeListenerState::Failed, Some("repeated socket receive failures".into())); break; }
          tokio::time::sleep(Duration::from_millis(100)).await; continue;
        };
        socket_errors = 0;
        let Some(state) = state.upgrade() else { break; };
        let rule = runtime.control.borrow().rule.clone();
        let ExposeLimits::Udp { max_datagram_bytes, queue_packets, queue_bytes, .. } = rule.resource.spec.limits else { break; };
        if size > max_datagram_bytes as usize { runtime.dropped("oversize"); continue; }
        if !source_allowed(&state, &rule.resource.spec, peer.ip()) { runtime.dropped("source_policy"); continue; }
        if peers.get(&peer).is_some_and(|(_, tx)| tx.is_closed()) { peers.remove(&peer); }
        if let std::collections::hash_map::Entry::Vacant(e) = peers.entry(peer) {
          if !current.accepting { runtime.dropped("draining"); continue; }
          let Ok(target) = resolve::target(&state, &rule).await else { runtime.dropped("target_unavailable"); continue; };
          let Some((guard, stop)) = admit(&runtime, &rule, peer, target, &state) else { continue; };
          let capacity = queue_packets.min(queue_bytes / max_datagram_bytes).max(1) as usize;
          let (sender, receiver) = mpsc::channel(capacity);
          e.insert((guard.id.clone(), sender));
          let BoundSocket::Udp(socket) = &socket else { unreachable!() };
          tasks.spawn(udp(state, runtime.clone(), rule.clone(), socket.clone(), peer, guard, stop, receiver, finished_tx.clone()));
        }
        if let Some((_, tx)) = peers.get(&peer)
          && tx.try_send(Bytes::copy_from_slice(&buffer[..size])).is_err() { runtime.dropped("ingress_queue"); }
      },
    }
  }
  drop(socket);
  for session in runtime
    .sessions
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .values()
  {
    session.stop.send_replace(true);
  }
  if tokio::time::timeout(Duration::from_secs(5), async {
    while tasks.join_next().await.is_some() {}
  })
  .await
  .is_err()
  {
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
  }
  let status = runtime.status.lock().unwrap_or_else(|e| e.into_inner()).0;
  if !matches!(
    status,
    ExposeListenerState::Suspended | ExposeListenerState::Failed
  ) {
    runtime.status(ExposeListenerState::Disabled, None);
  }
}

async fn accept(socket: &BoundSocket) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
  match socket {
    BoundSocket::Tcp(s) => s.accept().await,
    _ => std::future::pending().await,
  }
}
async fn receive(socket: &BoundSocket, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
  match socket {
    BoundSocket::Udp(s) => s.recv_from(buffer).await,
    _ => std::future::pending().await,
  }
}

async fn bandwidth(runtime: &Runtime, down: bool, size: usize, limit: u64) {
  loop {
    if runtime
      .budget
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .bytes(down, size, limit)
    {
      return;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
}

async fn tcp(
  state: Arc<AppState>,
  runtime: Arc<Runtime>,
  rule: Arc<ManagedRule>,
  socket: tokio::net::TcpStream,
  peer: SocketAddr,
  mut guard: SessionGuard,
  mut stop: watch::Receiver<bool>,
) {
  let ExposeLimits::Tcp {
    open_timeout_secs,
    drain_timeout_secs,
    ingress_bytes_per_second,
    egress_bytes_per_second,
    ..
  } = rule.resource.spec.limits
  else {
    return;
  };
  let (sender, mut receiver) = mpsc::channel(64);
  let flow = crate::state::StreamFlow::new(
    guard.id.clone(),
    guard.target.tx.clone(),
    state.client_supports_pause(&guard.target.client_id).await,
    state.stream_limits(),
  );
  let sender =
    crate::state::spawn_consumer_pump(sender, state.config().gateway_response_timeout, flow);
  state.tcp_streams.lock().await.insert(
    guard.id.clone(),
    TcpStreamHandle {
      tx: sender,
      client_id: guard.target.client_id.clone(),
    },
  );
  let opened = send(
    &guard.target.tx,
    TunnelMessage::TcpOpen {
      stream_id: guard.id.clone(),
      target: Some(guard.target.target.clone()),
      service: guard.target.service.clone(),
      visitor: Some(peer.to_string()),
    },
    Duration::from_secs(open_timeout_secs.into()),
  );
  let ok = tokio::select! { result = opened => result, _ = stop.changed() => false };
  if !ok {
    guard.reason = "open_failed";
    runtime.dropped("open_failed");
    return;
  }
  let record = guard.record.as_ref().expect("live record");
  let (up_count, down_count) = (record.up_counter(), record.down_counter());
  let (mut reader, mut writer) = socket.into_split();
  let upstream = async {
    let mut buffer = vec![0u8; (16 * 1024).min(ingress_bytes_per_second as usize).max(1)];
    loop {
      let size = match reader.read(&mut buffer).await {
        Ok(0) => return "visitor_eof",
        Ok(n) => n,
        Err(_) => return "visitor_error",
      };
      bandwidth(&runtime, false, size, ingress_bytes_per_second).await;
      let Some(frame) = crate::protocol::relay_frame(
        guard.target.protocol,
        crate::protocol::FRAME_TCP_DATA,
        &guard.id,
        &buffer[..size],
        |data| TunnelMessage::TcpData {
          stream_id: guard.id.clone(),
          data,
        },
      ) else {
        return "frame_error";
      };
      if !matches!(
        tokio::time::timeout(
          Duration::from_secs(open_timeout_secs.into()),
          guard.target.tx.send(frame)
        )
        .await,
        Ok(Ok(()))
      ) {
        return "client_send_failed";
      }
      up_count.fetch_add(size as u64, Ordering::Relaxed);
      runtime.transfer(&guard.id, false, size);
    }
  };
  let downstream = async {
    while let Some(message) = receiver.recv().await {
      match message {
        TcpConsumerMsg::Close => break,
        TcpConsumerMsg::Data(bytes) => {
          // Split only the TCP byte stream; a rate smaller than one frame must
          // still make progress. UDP retains its whole-datagram contract.
          for part in bytes.chunks((16 * 1024).min(egress_bytes_per_second as usize).max(1)) {
            bandwidth(&runtime, true, part.len(), egress_bytes_per_second).await;
            if writer.write_all(part).await.is_err() {
              return "visitor_error";
            }
            down_count.fetch_add(part.len() as u64, Ordering::Relaxed);
            runtime.transfer(&guard.id, true, part.len());
          }
        }
      }
    }
    let _ = writer.shutdown().await;
    "backend_closed"
  };
  let target_health = async {
    loop {
      tokio::time::sleep(Duration::from_secs(1)).await;
      if !resolve::still_serving(&state, &guard.target).await {
        return "target_unavailable";
      }
    }
  };
  tokio::pin!(upstream, downstream, target_health);
  let reason = tokio::select! {
    reason = &mut target_health => reason,
    _ = stop.changed() => "administrator_closed",
    reason = &mut downstream => reason,
    reason = &mut upstream => {
      let eof = if reason == "visitor_eof" && guard.target.protocol >= 10 {
        TunnelMessage::TcpEof { stream_id: guard.id.clone() }
      } else { TunnelMessage::TcpClose { stream_id: guard.id.clone() } };
      let _ = send(&guard.target.tx, eof, Duration::from_secs(2)).await;
      tokio::select! {
        _ = stop.changed() => "administrator_closed",
        reason = &mut target_health => reason,
        result = tokio::time::timeout(Duration::from_secs(drain_timeout_secs.into()), &mut downstream) => {
          match result { Ok(_) => reason, Err(_) => "drain_timeout" }
        },
      }
    },
  };
  guard.reason = reason;
}

#[allow(clippy::too_many_arguments)]
async fn udp(
  state: Arc<AppState>,
  runtime: Arc<Runtime>,
  rule: Arc<ManagedRule>,
  socket: Arc<tokio::net::UdpSocket>,
  peer: SocketAddr,
  mut guard: SessionGuard,
  mut stop: watch::Receiver<bool>,
  mut ingress: mpsc::Receiver<Bytes>,
  finished: mpsc::UnboundedSender<(SocketAddr, String)>,
) {
  let ExposeLimits::Udp {
    idle_timeout_secs,
    max_datagram_bytes,
    queue_packets,
    queue_bytes,
    ingress_bytes_per_second,
    egress_bytes_per_second,
    ..
  } = rule.resource.spec.limits
  else {
    return;
  };
  let capacity = queue_packets.min(queue_bytes / max_datagram_bytes).max(1) as usize;
  let (sender, mut receiver) = mpsc::channel(capacity);
  state.udp_streams.lock().await.insert(
    guard.id.clone(),
    UdpStreamHandle {
      tx: sender,
      client_id: guard.target.client_id.clone(),
      public: Some((max_datagram_bytes as usize, Arc::downgrade(&runtime))),
    },
  );
  let opened = send(
    &guard.target.tx,
    TunnelMessage::UdpOpen {
      stream_id: guard.id.clone(),
      target: guard.target.target.clone(),
      service: guard.target.service.clone(),
    },
    Duration::from_secs(10),
  );
  let ok = tokio::select! { result = opened => result, _ = stop.changed() => false };
  if !ok {
    guard.reason = "open_failed";
    runtime.dropped("open_failed");
    let _ = finished.send((peer, guard.id.clone()));
    return;
  }
  let record = guard.record.as_ref().expect("live record");
  let (up_count, down_count) = (record.up_counter(), record.down_counter());
  let mut idle = Instant::now();
  let mut health = tokio::time::interval(Duration::from_secs(1));
  loop {
    tokio::select! {
      _ = stop.changed() => { guard.reason = "administrator_closed"; break; },
      _ = tokio::time::sleep_until((idle + Duration::from_secs(idle_timeout_secs.into())).into()) => { guard.reason = "idle_timeout"; break; },
      _ = health.tick() => {
        if !resolve::still_serving(&state, &guard.target).await { guard.reason = "target_unavailable"; break; }
      },
      datagram = ingress.recv() => {
        let Some(bytes) = datagram else { break; };
        if !runtime.budget.lock().unwrap_or_else(|e| e.into_inner()).bytes(false, bytes.len(), ingress_bytes_per_second) { runtime.dropped("ingress_rate"); continue; }
        let Some(frame) = crate::protocol::relay_frame(guard.target.protocol, crate::protocol::FRAME_UDP_DATAGRAM, &guard.id,
          &bytes, |data| TunnelMessage::UdpDatagram { stream_id: guard.id.clone(), data }) else { runtime.dropped("frame_error"); continue; };
        match guard.target.tx.try_send(frame) {
          Ok(()) => { idle = Instant::now(); up_count.fetch_add(bytes.len() as u64, Ordering::Relaxed); runtime.transfer(&guard.id, false, bytes.len()); },
          Err(mpsc::error::TrySendError::Full(_)) => runtime.dropped("client_queue"),
          Err(mpsc::error::TrySendError::Closed(_)) => { guard.reason = "client_disconnected"; break; },
        }
      },
      response = receiver.recv() => {
        let Some(TcpConsumerMsg::Data(bytes)) = response else { guard.reason = "backend_closed"; break; };
        if bytes.len() > max_datagram_bytes as usize { runtime.dropped("oversize"); continue; }
        if !runtime.budget.lock().unwrap_or_else(|e| e.into_inner()).bytes(true, bytes.len(), egress_bytes_per_second) { runtime.dropped("egress_rate"); continue; }
        match socket.try_send_to(&bytes, peer) {
          Ok(n) if n == bytes.len() => { idle = Instant::now(); down_count.fetch_add(n as u64, Ordering::Relaxed); runtime.transfer(&guard.id, true, n); },
          _ => runtime.dropped("socket_send"),
        }
      },
    }
  }
  let _ = finished.send((peer, guard.id.clone()));
}
