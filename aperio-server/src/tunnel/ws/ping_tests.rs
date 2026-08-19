//! What a heartbeat is allowed to say, and what happens when it says too much:
//! the binds and limits it declares, token pinning, the self-reported health
//! figures, the ceiling on how many services one connection may carry, and the
//! concurrency limit the dispatcher follows.

use super::super::tests::*;
use super::super::*;
use crate::protocol::TunnelDecl;
use crate::protocol::TunnelMessage;
use crate::state::*;
use crate::test_support::*;
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as TMessage;

// --- Ping handler -----------------------------------------------------------

#[tokio::test]
async fn ping_master_applies_all_binds() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut path_bind,
    ref mut hostname_bind,
    ref mut hostname_binds,
    ref mut max_concurrent,
    ref mut tcp,
    ref mut version,
    ref mut protocol,
    ref mut priority,
    ref mut bandwidth_bps,
    ref mut service,
    ref mut public,
    ref mut visitor_auth,
    ref mut allowed_ips,
    ref mut tunnels,
    ref mut cache,
    ref mut resilience,
    ref mut max_request_body,
    ref mut response_timeout,
    ref mut webhook_inbox,
    ref mut denied,
    ref mut backend_healthy,
    ..
  } = ping
  {
    *path_bind = Some("/api".into());
    *hostname_bind = Some("example.com".into());
    *hostname_binds = vec!["a.example.com".into(), "b.example.com".into()];
    *max_concurrent = Some(4);
    *tcp = true;
    *version = Some("9.9.9".into());
    *protocol = Some(9999);
    *priority = 7;
    *bandwidth_bps = Some(1_000_000);
    *service = Some("svc".into());
    *public = true;
    *visitor_auth = Some("user:pass".into());
    *allowed_ips = vec!["127.0.0.1".into(), "bogus".into()];
    *tunnels = vec![TunnelDecl {
      custom_name: None,
      name: None,
      target: "127.0.0.1:9".into(),
      protocol: "tcp".into(),
      encrypt: false,
      idle_timeout: None,
      expose: None,
    }];
    *cache = true;
    *resilience = true;
    *max_request_body = Some(1000);
    *response_timeout = Some(30);
    *webhook_inbox = true;
    *denied = Some("https://example.com/denied".into());
    *backend_healthy = false;
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;

  {
    let clients = state.clients.read().await;
    let h = clients.get(&cid).unwrap();
    assert_eq!(h.sole().declared_path.as_deref(), Some("/api"));
    assert_eq!(h.sole().declared_hostnames.len(), 2);
    assert_eq!(h.sole().max_concurrent, Some(4));
    assert!(h.sole().tcp_enabled);
    assert!(h.sole().cache);
    assert!(h.sole().resilience);
    assert!(h.sole().webhook_inbox);
    assert!(h.sole().public);
    assert!(h.sole().visitor_auth.is_some());
    assert_eq!(h.sole().allowed_ips, vec!["127.0.0.1".to_string()]);
    assert!(h.sole().denied.is_some());
    assert_eq!(h.sole().response_timeout, Some(30));
    assert_eq!(h.sole().max_request_body, Some(1000));
    assert_eq!(h.sole().priority, 7);
    assert_eq!(h.sole().service_name.as_deref(), Some("svc"));
    assert_eq!(h.reported_instance_id.as_deref(), Some("self"));
    assert!(!h.sole().backend_healthy);
  }

  // A second, identical Ping exercises the "no change" / warn-once branches
  // and the healthy-again transition.
  let mut ping2 = ping.clone();
  if let TunnelMessage::Ping {
    ref mut backend_healthy,
    ..
  } = ping2
  {
    *backend_healthy = true;
  }
  send(&mut ws, &ping2).await;
  read_until_pong(&mut ws).await;
  assert!(
    state
      .clients
      .write()
      .await
      .get(&cid)
      .unwrap()
      .sole()
      .backend_healthy
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn ping_master_invalid_visitor_and_denied() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut visitor_auth,
    ref mut denied,
    ..
  } = ping
  {
    *visitor_auth = Some("no-colon-here".into()); // invalid creds
    *denied = Some("ftp://bad".into()); // not http(s) -> filtered
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let h = clients.get(&cid).unwrap();
  assert!(h.sole().visitor_auth.is_none());
  assert!(h.sole().denied.is_none());
}

#[tokio::test]
async fn ping_dynamic_token_denies_public_and_visitor_auth() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let (secret, _id) = make_dynamic_token(&state, false).await;
  let mut ws = connect(&url, &secret).await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut public,
    ref mut visitor_auth,
    ref mut allowed_ips,
    ..
  } = ping
  {
    *public = true;
    *visitor_auth = Some("user:pass".into());
    *allowed_ips = vec!["10.0.0.0/8".into(), "junk".into()];
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;
  // Second ping to hit the warned-once guards.
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let h = clients.get(&cid).unwrap();
  assert!(!h.sole().public);
  assert!(h.sole().visitor_auth.is_none());
  assert!(h.sole().public_denied_warned);
  assert!(h.sole().visitor_auth_denied_warned);
  assert_eq!(h.sole().allowed_ips, vec!["10.0.0.0/8".to_string()]);
}

// --- Token pinning ----------------------------------------------------------

#[tokio::test]
async fn token_pinning_pins_then_rejects_mismatch() {
  let mut cfg = test_config();
  cfg.token_pinning = true;
  let state = Arc::new(test_state_with(cfg));
  let url = start_server(state.clone()).await;
  let (secret, _id) = make_dynamic_token(&state, false).await;

  // First connection pins the device key.
  let mut ws = connect(&url, &secret).await;
  let _cid = wait_client_id(&state).await;
  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut client_key, ..
  } = ping
  {
    *client_key = Some("device-key-1".into());
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;
  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;

  // Second connection with no key: pinning fails closed and disconnects.
  let mut ws2 = connect(&url, &secret).await;
  wait_client_id(&state).await;
  send(&mut ws2, &base_ping()).await; // no client_key -> Mismatch -> break
  // The server force-closes (an abrupt reset counts as a disconnect); we must
  // never receive a Pong before the connection ends.
  loop {
    let frame = tokio::time::timeout(Duration::from_secs(2), ws2.next())
      .await
      .expect("frame timeout");
    match frame {
      None | Some(Err(_)) | Some(Ok(TMessage::Close(_))) => break,
      Some(Ok(TMessage::Text(t))) => {
        if let Ok(msg) = serde_json::from_str::<TunnelMessage>(&t) {
          assert!(
            !matches!(msg, TunnelMessage::Pong { .. }),
            "unexpected pong after pin mismatch"
          );
        }
      }
      Some(Ok(_)) => {}
    }
  }
  wait_no_clients(&state).await;
}

// --- disconnect cleanup -----------------------------------------------------

#[tokio::test]
async fn disconnect_drains_all_owned_state() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  // Seed owned entries across every map.
  let (preq_tx, _preq_rx) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    "p1".into(),
    PendingRequest {
      tx: preq_tx,
      client_id: cid.clone(),
    },
  );
  let (pup_tx, _pup_rx) = oneshot::channel::<TunnelResponse>();
  state.pending_upgrades.lock().await.insert(
    "u1".into(),
    PendingRequest {
      tx: pup_tx,
      client_id: cid.clone(),
    },
  );
  let (rs_tx, _rs_rx) = mpsc::channel::<Result<BodyFrame, std::io::Error>>(4);
  state.response_streams.lock().await.insert(
    "r1".into(),
    ResponseStreamHandle {
      tx: crate::state::test_pump(rs_tx),
      client_id: cid.clone(),
    },
  );
  let (tcp_tx, mut tcp_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(tcp_tx),
      client_id: cid.clone(),
    },
  );
  let (udp_tx, mut udp_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.udp_streams.lock().await.insert(
    "d1".into(),
    crate::state::UdpStreamHandle {
      tx: udp_tx,
      client_id: cid.clone(),
    },
  );
  let (wss_tx, mut wss_rx) = mpsc::channel::<WsStreamMessage>(4);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx: crate::state::test_pump(wss_tx),
      client_id: cid.clone(),
    },
  );
  // A foreign entry that must survive.
  let (foreign_tx, _foreign_rx) = mpsc::channel::<TcpConsumerMsg>(4);
  state.tcp_streams.lock().await.insert(
    "keep".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(foreign_tx),
      client_id: "foreign".into(),
    },
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;

  // Give cleanup a moment to drain the maps.
  for _ in 0..200 {
    if state.pending_requests.lock().await.is_empty()
      && state.pending_upgrades.lock().await.is_empty()
      && state.response_streams.lock().await.is_empty()
      && state.udp_streams.lock().await.is_empty()
      && state.ws_streams.lock().await.is_empty()
      && !state.tcp_streams.lock().await.contains_key("t1")
    {
      break;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }

  assert!(state.pending_requests.lock().await.is_empty());
  assert!(state.pending_upgrades.lock().await.is_empty());
  assert!(state.response_streams.lock().await.is_empty());
  assert!(!state.tcp_streams.lock().await.contains_key("t1"));
  assert!(state.tcp_streams.lock().await.contains_key("keep")); // foreign kept
  assert!(state.udp_streams.lock().await.is_empty());
  assert!(state.ws_streams.lock().await.is_empty());
  // Consumers were signalled Close.
  assert!(matches!(tcp_rx.recv().await, Some(TcpConsumerMsg::Close)));
  assert!(matches!(udp_rx.recv().await, Some(TcpConsumerMsg::Close)));
  assert!(matches!(wss_rx.recv().await, Some(WsStreamMessage::Close)));
  // Tunnel slot released.
  assert_eq!(
    state
      .active_tunnel_count
      .load(std::sync::atomic::Ordering::SeqCst),
    0
  );
}

// --- self-reported client health (planned_features #37) ---------------------

#[tokio::test]
async fn a_v8_ping_declaring_one_service_is_the_shape_that_already_worked() {
  // The list is authoritative when present, and one entry has to mean exactly
  // what the top-level fields have always meant, or the two spellings would
  // half-agree and every later step of #46 would be built on the difference.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![crate::protocol::ServiceDecl {
      hostname_bind: Some("one.e2e.local".into()),
      ..Default::default()
    }]);
  }
  send(&mut ws, &ping).await;

  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;
  assert!(
    state.clients.read().await.contains_key(&id),
    "a one-service declaration is served, not refused"
  );
}

#[tokio::test]
async fn a_ping_carrying_client_health_stores_it_on_the_handle() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut cpu_percent,
    ref mut rss_bytes,
    ref mut rtt_ms,
    ref mut jitter_ms,
    ref mut reconnects,
    ..
  } = ping
  {
    *cpu_percent = Some(12.5);
    *rss_bytes = Some(48 * 1024 * 1024);
    *rtt_ms = Some(23);
    *jitter_ms = Some(4);
    *reconnects = Some(2);
  }
  send(&mut ws, &ping).await;

  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;
  let clients = state.clients.read().await;
  let handle = clients.get(&id).expect("the client is registered");
  assert_eq!(handle.cpu_percent, Some(12.5));
  assert_eq!(handle.rss_bytes, Some(48 * 1024 * 1024));
  assert_eq!(handle.rtt_ms, Some(23));
  assert_eq!(handle.jitter_ms, Some(4));
  assert_eq!(handle.reconnects, Some(2));
}

#[tokio::test]
async fn a_client_that_stops_reporting_shows_nothing_rather_than_a_stale_value() {
  // An older client, or a platform where a figure cannot be read, omits it.
  // Keeping the last value would let a number age silently while looking live.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;

  let mut p = base_ping();
  if let TunnelMessage::Ping {
    ref mut rtt_ms,
    ref mut cpu_percent,
    ..
  } = p
  {
    *rtt_ms = Some(99);
    *cpu_percent = Some(50.0);
  }
  send(&mut ws, &p).await;
  let id = wait_client_id(&state).await;
  let _ = read_until_pong(&mut ws).await;
  assert_eq!(
    state.clients.read().await.get(&id).unwrap().rtt_ms,
    Some(99)
  );

  send(&mut ws, &base_ping()).await;
  let _ = read_until_pong(&mut ws).await;
  let clients = state.clients.read().await;
  let handle = clients.get(&id).unwrap();
  assert_eq!(handle.rtt_ms, None, "the absence is stored, not ignored");
  assert_eq!(handle.cpu_percent, None);
}

// A declaration is bounded before anything is built from it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_service_list_past_the_ceiling_is_refused_before_it_is_walked() {
  // Everything the Ping does with this list is proportional to its length,
  // some of it quadratic, all of it under the `clients` write lock, and the
  // allocation at the end is one `ServiceState` per entry. A 20 MB frame holds
  // several million `{}` entries, so without a bound one authenticated client
  // could hold that lock through a quadratic pass over them, blocking every
  // other client and every dashboard request, and then ask for the memory.
  //
  // Refused rather than truncated: serving the first 256 of a longer list is a
  // connection that establishes and then serves less than it was told to.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(
      (0..MAX_DECLARED_SERVICES + 1)
        .map(|i| crate::protocol::ServiceDecl {
          service: Some(format!("svc{i}")),
          ..Default::default()
        })
        .collect(),
    );
  }
  send(&mut ws, &ping).await;

  // The connection goes, and the services were never built.
  wait_no_clients(&state).await;
  assert!(state.clients.read().await.get(&cid).is_none());
}

#[tokio::test]
async fn a_list_at_the_ceiling_is_served() {
  // The other half: the bound is a fence around a failure mode, not a limit
  // anybody legitimate is meant to feel.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(
      (0..MAX_DECLARED_SERVICES)
        .map(|i| crate::protocol::ServiceDecl {
          service: Some(format!("svc{i}")),
          ..Default::default()
        })
        .collect(),
    );
  }
  send(&mut ws, &ping).await;
  read_until_pong(&mut ws).await;
  assert_eq!(
    state.clients.read().await.get(&cid).unwrap().services.len(),
    MAX_DECLARED_SERVICES
  );

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// ---------------------------------------------------------------------------
// The concurrency limit follows the client that announces it (#65, #121)
// ---------------------------------------------------------------------------

/// A Ping declaring `n` as this connection's concurrency limit.
fn ping_with_concurrency(n: u32) -> TunnelMessage {
  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut max_concurrent,
    ..
  } = ping
  {
    *max_concurrent = Some(n);
  }
  ping
}

/// (enforced limit, permits the semaphore is handing out) for the sole service.
async fn concurrency_of(state: &AppState, cid: &str) -> (Option<u32>, Option<usize>) {
  let clients = state.clients.write().await;
  let service = clients.get(cid).unwrap().sole();
  (
    service.max_concurrent,
    service
      .inflight_limiter
      .as_ref()
      .map(|l| l.available_permits()),
  )
}

#[tokio::test]
async fn a_lowered_concurrency_limit_reaches_the_dispatcher() {
  // `adaptive_concurrency` exists so a client whose backend has fallen behind
  // stops being sent work it cannot do, and the three answers that buys, the
  // server holding the request, handing it to a healthier client, or asking
  // for capacity, are all the server's to make. It could make none of them:
  // the limiter was built on the first Ping that named a number and never
  // moved again, so a client announcing 8 and then 4 was still dispatched 8.
  // The excess queued on the struggling client instead, which is the one place
  // the feature's own documentation says is the wrong place for it.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  send(&mut ws, &ping_with_concurrency(8)).await;
  read_until_pong(&mut ws).await;
  assert_eq!(concurrency_of(&state, &cid).await, (Some(8), Some(8)));

  send(&mut ws, &ping_with_concurrency(4)).await;
  read_until_pong(&mut ws).await;
  assert_eq!(concurrency_of(&state, &cid).await, (Some(4), Some(4)));

  // And back up when the backend recovers, one step at a time, the way the
  // client's own additive increase moves it.
  send(&mut ws, &ping_with_concurrency(5)).await;
  read_until_pong(&mut ws).await;
  assert_eq!(concurrency_of(&state, &cid).await, (Some(5), Some(5)));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn a_client_cannot_climb_above_the_limit_it_first_announced() {
  // The band is the operator's: this lowers a ceiling under pressure, it does
  // not raise one. Without the clamp a peer could announce an ever-growing
  // number and talk its way into more concurrency than its config asked for,
  // and a legitimate raise already arrives the right way, a config reload
  // respawns the connection and the new number becomes the new ceiling.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  send(&mut ws, &ping_with_concurrency(4)).await;
  read_until_pong(&mut ws).await;
  send(&mut ws, &ping_with_concurrency(9999)).await;
  read_until_pong(&mut ws).await;
  assert_eq!(concurrency_of(&state, &cid).await, (Some(4), Some(4)));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn a_shrink_under_load_reports_what_it_could_actually_take() {
  // Forgetting permits takes at most what is free, so a shrink while requests
  // are in flight takes fewer than it asked for. What matters is that the
  // number on screen is the one the semaphore is enforcing: the alternative
  // puts a client at 1 in the dashboard while the dispatcher still sends it 4,
  // and the autoscaler reads that same pair to decide whether to add capacity.
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  send(&mut ws, &ping_with_concurrency(4)).await;
  read_until_pong(&mut ws).await;

  // Three of the four permits are out with in-flight requests.
  let limiter = {
    let clients = state.clients.write().await;
    clients
      .get(&cid)
      .unwrap()
      .sole()
      .inflight_limiter
      .clone()
      .unwrap()
  };
  let held = limiter.clone().acquire_many_owned(3).await.unwrap();

  // The client asks to drop to 1, so it wants three permits gone; only one is
  // free to take.
  send(&mut ws, &ping_with_concurrency(1)).await;
  read_until_pong(&mut ws).await;
  assert_eq!(concurrency_of(&state, &cid).await, (Some(3), Some(0)));

  // The requests finish, and the next heartbeat takes the rest.
  drop(held);
  send(&mut ws, &ping_with_concurrency(1)).await;
  read_until_pong(&mut ws).await;
  assert_eq!(concurrency_of(&state, &cid).await, (Some(1), Some(1)));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// --- server-side serving ----------------------------------------------------

/// Builds a state whose operator has named `targets` as reachable directly.
fn state_allowing(targets: &str) -> Arc<AppState> {
  let mut cfg = test_config();
  cfg.server_side_targets = crate::outbound::parse_patterns(targets).expect("valid patterns");
  Arc::new(test_state_with(cfg))
}

/// A token that may ask for it, and a target the operator named: the service
/// is served from the server.
#[tokio::test]
async fn a_permitted_target_is_served_from_the_server() {
  let state = state_allowing("10.0.0.0/8");
  let url = start_server(state.clone()).await;
  let secret = {
    let mut store = state.token_store.lock().await;
    let (_rec, secret) = store
      .create(crate::store::tokens::TokenSpec {
        name: "ss".into(),
        allow_server_side: true,
        ..Default::default()
      })
      .expect("the test store can be written to");
    secret
  };
  let mut ws = connect(&url, &secret).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![crate::protocol::ServiceDecl {
      server_side_target: Some("http://10.1.2.3:8080".into()),
      ..Default::default()
    }]);
  }
  send(&mut ws, &ping).await;
  let cid = wait_client_id(&state).await;
  read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let h = clients.get(&cid).unwrap();
  assert_eq!(
    h.sole().server_side_target.as_deref(),
    Some("http://10.1.2.3:8080"),
    "the target the operator permitted should be what the server dials"
  );
  assert!(h.sole().server_side_refused.is_none());
}

/// The token gates the asking, and a token that may not ask is refused even
/// when the operator's list would have allowed the address.
#[tokio::test]
async fn a_token_without_the_permission_is_refused_and_not_relayed() {
  let state = state_allowing("10.0.0.0/8");
  let url = start_server(state.clone()).await;
  let (secret, _id) = make_dynamic_token(&state, false).await;
  let mut ws = connect(&url, &secret).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![crate::protocol::ServiceDecl {
      server_side_target: Some("http://10.1.2.3:8080".into()),
      ..Default::default()
    }]);
  }
  send(&mut ws, &ping).await;
  let cid = wait_client_id(&state).await;
  read_until_pong(&mut ws).await;

  let clients = state.clients.read().await;
  let h = clients.get(&cid).unwrap();
  assert!(h.sole().server_side_target.is_none());
  let why = h
    .sole()
    .server_side_refused
    .as_deref()
    .expect("a refusal, not a silent relay");
  assert!(
    why.contains("allow_server_side"),
    "the refusal should name the permission that is missing: {why}"
  );
}

/// A target the operator never named is refused, and the refusal says so.
///
/// The service is then excluded from routing rather than relayed, which
/// `select.rs` enforces: relaying looks kind and is not, since a client asking
/// for this usually cannot reach the target itself.
#[tokio::test]
async fn a_target_outside_the_operators_list_is_refused() {
  let state = state_allowing("10.0.0.0/8");
  let url = start_server(state.clone()).await;
  let secret = {
    let mut store = state.token_store.lock().await;
    let (_rec, secret) = store
      .create(crate::store::tokens::TokenSpec {
        name: "ss".into(),
        allow_server_side: true,
        ..Default::default()
      })
      .expect("the test store can be written to");
    secret
  };
  let mut ws = connect(&url, &secret).await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    ref mut services, ..
  } = ping
  {
    *services = Some(vec![crate::protocol::ServiceDecl {
      hostname_bind: Some("ss.e2e.local".into()),
      server_side_target: Some("http://192.168.9.9:8080".into()),
      ..Default::default()
    }]);
  }
  send(&mut ws, &ping).await;
  let cid = wait_client_id(&state).await;
  read_until_pong(&mut ws).await;

  {
    let clients = state.clients.read().await;
    let h = clients.get(&cid).unwrap();
    assert!(h.sole().server_side_target.is_none());
    let why = h.sole().server_side_refused.as_deref().expect("refused");
    assert!(why.contains("not on server_side_targets"), "{why}");
  }

  // And it is not routed: a refused service answers as an unclaimed route
  // does, rather than quietly falling back to the tunnel.
  let picked = crate::routing::select::pick_proxy_client(
    &state,
    "/",
    Some("ss.e2e.local"),
    None,
    None,
    Some("127.0.0.1".parse().unwrap()),
    None,
  )
  .await;
  assert!(
    matches!(picked, crate::routing::select::PickOutcome::NoRoute),
    "a refused service must not be routed to"
  );
}

// --- what a client calls itself -------------------------------------------

/// Sends a Ping declaring `name` and returns the handle's stored value.
async fn name_after_ping(name: Option<&str>) -> (Arc<AppState>, String, Option<String>) {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    name: ref mut declared,
    ..
  } = ping
  {
    *declared = name.map(str::to_string);
  }
  send(&mut ws, &ping).await;
  let cid = wait_client_id(&state).await;
  read_until_pong(&mut ws).await;
  let stored = state
    .clients
    .read()
    .await
    .get(&cid)
    .and_then(|h| h.declared_name.clone());
  (state, cid, stored)
}

#[tokio::test]
async fn a_client_that_names_itself_is_shown_by_that_name() {
  let (_s, _cid, stored) = name_after_ping(Some("eu_server_1")).await;
  assert_eq!(stored.as_deref(), Some("eu_server_1"));
}

/// A name that is not one is dropped rather than shown.
///
/// It arrives on every heartbeat from a party the server trusts for nothing
/// else, and it reaches the dashboard, the logs and an operator's eye. A name
/// is only worth having if it is the name, so an unusable one leaves the
/// client showing its id, which is what it did before.
#[tokio::test]
async fn a_name_that_does_not_validate_is_dropped_rather_than_shown() {
  for bad in ["Has Spaces", "UPPER", "with-hyphen", "", "  "] {
    let (_s, _cid, stored) = name_after_ping(Some(bad)).await;
    assert_eq!(stored, None, "{bad:?} should not have been kept");
  }
}

#[tokio::test]
async fn a_client_that_sends_no_name_keeps_showing_its_id() {
  let (_s, _cid, stored) = name_after_ping(None).await;
  assert_eq!(stored, None);
}

/// The name is a label, and nothing addresses the client by it.
///
/// This is the property the whole design rests on: `client_id` stays the
/// identity because failover and `bind_tunnels:` need one value per process,
/// and a name is shared by replicas on purpose. Two connections calling
/// themselves the same thing must therefore stay two connections.
#[tokio::test]
async fn two_clients_may_share_a_name_and_stay_two_clients() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut first = connect(&url, "test").await;
  let mut second = connect(&url, "test").await;

  let mut ping = base_ping();
  if let TunnelMessage::Ping {
    name: ref mut declared,
    ..
  } = ping
  {
    *declared = Some("eu_server".to_string());
  }
  send(&mut first, &ping).await;
  read_until_pong(&mut first).await;
  send(&mut second, &ping).await;
  read_until_pong(&mut second).await;

  let clients = state.clients.read().await;
  let named: Vec<&String> = clients
    .iter()
    .filter(|(_, h)| h.declared_name.as_deref() == Some("eu_server"))
    .map(|(id, _)| id)
    .collect();
  assert_eq!(
    named.len(),
    2,
    "a shared name must not merge two connections into one"
  );
}
