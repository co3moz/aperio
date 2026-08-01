//! Unit tests for the experimental public TCP expose module: config parsing,
//! the expose-key -> serving-client match, and an end-to-end relay driven over
//! a loopback socket pair. The listener accept loop in `spawn_listeners` binds a
//! real port and loops forever, so it is exercised only indirectly (its body is
//! `relay_public_tcp`, which is covered directly here).

use super::*;
use crate::protocol::TunnelDecl;
use crate::test_support::test_state;
use std::sync::Arc;
use std::time::Duration;

// --------------------------------------------------------------------------
// ExposeRule / from_config_file
// --------------------------------------------------------------------------

/// Holds the process-wide config lock (shared with the other config-touching
/// test modules) so the global document is not raced.
/// The lock is the point of the struct; holding it is all field 0 does.
struct CfgLock(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
impl CfgLock {
  fn acquire() -> Self {
    CfgLock(crate::test_support::config_lock())
  }
}
impl Drop for CfgLock {
  fn drop(&mut self) {
    unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
    let _ = std::fs::remove_file("aperio-server.yaml");
  }
}

fn load_config(yaml: &str) {
  let file =
    crate::test_support::test_temp_root().join(format!("expose-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(&file, yaml).unwrap();
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", file.to_str().unwrap()) };
  crate::config_file::load();
}

#[test]
fn expose_rule_defaults_protocol_to_tcp() {
  let rule: ExposeRule = serde_yaml::from_str("port: 5000\nkey: longenoughkey\n").unwrap();
  assert_eq!(rule.protocol, "tcp");
  assert_eq!(rule.port, 5000);
  assert_eq!(rule.key.as_deref(), Some("longenoughkey"));
}

#[test]
fn from_config_file_empty_without_section() {
  let _lock = CfgLock::acquire();
  load_config("server_token: 0123456789abcdef\n");
  assert!(from_config_file().is_empty());
}

#[test]
fn from_config_file_parses_valid_rules() {
  let _lock = CfgLock::acquire();
  load_config(concat!(
    "expose:\n",
    "  - port: 5000\n    key: longenoughkey\n",
    "  - port: 5001\n    key: anotherlongkey\n    protocol: tcp\n",
  ));
  let rules = from_config_file();
  assert_eq!(rules.len(), 2);
  assert_eq!(rules[0].port, 5000);
  assert_eq!(rules[1].port, 5001);
}

// --------------------------------------------------------------------------
// find_declarer
// --------------------------------------------------------------------------

/// An `expose:` rule in the deprecated shared-secret form.
fn key_rule(key: &str) -> ExposeRule {
  ExposeRule {
    protocol: "tcp".to_string(),
    port: 5000,
    tunnel: None,
    org: None,
    token: None,
    key: Some(key.to_string()),
  }
}

/// An `expose:` rule in the identity form: a named tunnel owned by a token.
fn named_rule(tunnel: &str, token: Option<&str>) -> ExposeRule {
  ExposeRule {
    protocol: "tcp".to_string(),
    port: 5000,
    tunnel: Some(tunnel.to_string()),
    org: None,
    token: token.map(str::to_string),
    key: None,
  }
}

fn tunnel(key: Option<&str>, protocol: &str, encrypt: bool) -> TunnelDecl {
  TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:9000".to_string(),
    protocol: protocol.to_string(),
    encrypt,
    idle_timeout: None,
    expose: key.map(|k| k.to_string()),
  }
}

/// A mock client declaring the given tunnels, inserted under `cid`.
async fn insert_client(
  state: &Arc<AppState>,
  cid: &str,
  mutate: impl FnOnce(&mut crate::state::ClientHandle),
) {
  let mut c = crate::test_support::mock_client(None, None, None, None);
  mutate(&mut c);
  state.clients.write().await.insert(cid.to_string(), c);
}

#[tokio::test]
async fn find_declarer_matches_healthy_declaring_client() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |c| {
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
  })
  .await;

  let found = find_declarer(&state, &key_rule("mykey12345")).await;
  let (cid, _tx, target, _protocol) = found.expect("declaring client found");
  assert_eq!(cid, "c1");
  assert_eq!(target, "127.0.0.1:9000");
}

#[tokio::test]
async fn find_declarer_none_when_no_client() {
  let state = Arc::new(test_state());
  assert!(
    find_declarer(&state, &key_rule("mykey12345"))
      .await
      .is_none()
  );
}

#[tokio::test]
async fn find_declarer_skips_ineligible_and_mismatched_clients() {
  let state = Arc::new(test_state());
  // Disabled client (skipped by the health/enabled guard).
  insert_client(&state, "disabled", |c| {
    c.admin_enabled = false;
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
  })
  .await;
  // Draining client (also skipped).
  insert_client(&state, "draining", |c| {
    c.draining = true;
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
  })
  .await;
  // Healthy client but the tunnel is encrypted / udp / wrong key.
  insert_client(&state, "wrong", |c| {
    c.tunnels = vec![
      tunnel(Some("mykey12345"), "tcp", true), // encrypted -> excluded
      tunnel(Some("mykey12345"), "udp", false), // wrong protocol
      tunnel(Some("otherkey123"), "tcp", false), // wrong key
    ];
  })
  .await;

  assert!(
    find_declarer(&state, &key_rule("mykey12345"))
      .await
      .is_none()
  );
}

// --------------------------------------------------------------------------
// relay_public_tcp
// --------------------------------------------------------------------------

#[tokio::test]
async fn relay_drops_connection_without_a_declarer() {
  use tokio::net::{TcpListener, TcpStream};
  let state = Arc::new(test_state());

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let _visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  // No client declares this key -> relay audits nothing serving and returns.
  relay_public_tcp(state, server_sock, peer, &key_rule("unknownkey1")).await;
}

#[tokio::test]
async fn relay_end_to_end_pumps_bytes_both_directions() {
  use axum::extract::ws::Message;
  use base64::prelude::*;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::{TcpListener, TcpStream};
  use tokio::sync::mpsc;

  let state = Arc::new(test_state());

  // A client with a live receiver we can observe.
  let (tx, mut client_rx) = mpsc::channel::<Message>(32);
  {
    let mut c = crate::test_support::mock_client(None, None, None, None);
    c.tx = tx;
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
    state.clients.write().await.insert("c1".to_string(), c);
  }

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let mut visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  // Run the relay in the background.
  let relay_state = state.clone();
  let relay = tokio::spawn(async move {
    relay_public_tcp(relay_state, server_sock, peer, &key_rule("mykey12345")).await;
  });

  // First message the client receives must be a TcpOpen for its target.
  let open = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
    .await
    .expect("timely open")
    .expect("open message");
  let stream_id = match open {
    Message::Text(json) => {
      let v: serde_json::Value = serde_json::from_str(&json).unwrap();
      assert_eq!(v["type"], "TcpOpen");
      assert_eq!(v["target"], "127.0.0.1:9000");
      v["stream_id"].as_str().unwrap().to_string()
    }
    other => panic!("expected TcpOpen text, got {other:?}"),
  };

  // Visitor -> tunnel: bytes written to the socket arrive as base64 TcpData.
  visitor.write_all(b"hello").await.unwrap();
  let data = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
    .await
    .expect("timely data")
    .expect("data message");
  match data {
    Message::Text(json) => {
      let v: serde_json::Value = serde_json::from_str(&json).unwrap();
      assert_eq!(v["type"], "TcpData");
      let decoded = BASE64_STANDARD.decode(v["data"].as_str().unwrap()).unwrap();
      assert_eq!(decoded, b"hello");
    }
    other => panic!("expected TcpData text, got {other:?}"),
  }

  // Tunnel -> visitor: push Data through the registered stream handle.
  {
    let streams = state.tcp_streams.lock().await;
    let handle = streams.get(&stream_id).expect("stream registered");
    handle
      .tx
      .push(crate::state::TcpConsumerMsg::Data(b"world".to_vec()))
      .unwrap();
  }
  let mut buf = [0u8; 5];
  tokio::time::timeout(Duration::from_secs(2), visitor.read_exact(&mut buf))
    .await
    .expect("timely read")
    .expect("read bytes");
  assert_eq!(&buf, b"world");

  // Closing the visitor tears the relay down and it cleans up the stream map.
  drop(visitor);
  let _ = tokio::time::timeout(Duration::from_secs(2), relay).await;
  assert!(!state.tcp_streams.lock().await.contains_key(&stream_id));
}

#[tokio::test]
async fn relay_rejected_by_rate_limit_returns_early() {
  use tokio::net::{TcpListener, TcpStream};
  // A config that grants zero tokens rejects every connection.
  let mut cfg = crate::test_support::test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let state = Arc::new(crate::test_support::test_state_with(cfg));

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let _visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  // check_rate_limit -> false -> relay returns before touching the stream map.
  relay_public_tcp(state.clone(), server_sock, peer, &key_rule("mykey12345")).await;
  assert!(state.tcp_streams.lock().await.is_empty());
}

#[tokio::test]
async fn relay_bails_when_the_client_channel_is_closed() {
  use tokio::net::{TcpListener, TcpStream};
  let state = Arc::new(test_state());

  // A declaring client whose receiver has already been dropped: sending the
  // TcpOpen fails, so the relay removes the just-registered stream and returns.
  {
    let mut c = crate::test_support::mock_client(None, None, None, None); // rx already dropped
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
    state.clients.write().await.insert("dead".to_string(), c);
  }

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let _visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  relay_public_tcp(state.clone(), server_sock, peer, &key_rule("mykey12345")).await;
  assert!(state.tcp_streams.lock().await.is_empty());
}

#[tokio::test]
async fn relay_closes_when_tunnel_signals_close() {
  use axum::extract::ws::Message;
  use tokio::net::{TcpListener, TcpStream};
  use tokio::sync::mpsc;

  let state = Arc::new(test_state());
  let (tx, mut client_rx) = mpsc::channel::<Message>(32);
  {
    let mut c = crate::test_support::mock_client(None, None, None, None);
    c.tx = tx;
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
    state.clients.write().await.insert("c1".to_string(), c);
  }

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let _visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  let relay_state = state.clone();
  let relay = tokio::spawn(async move {
    relay_public_tcp(relay_state, server_sock, peer, &key_rule("mykey12345")).await;
  });

  // Consume the TcpOpen and grab the stream id.
  let stream_id = match tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    Message::Text(json) => {
      let v: serde_json::Value = serde_json::from_str(&json).unwrap();
      v["stream_id"].as_str().unwrap().to_string()
    }
    other => panic!("expected TcpOpen, got {other:?}"),
  };

  // Signal Close from the tunnel side: the down task shuts the socket and the
  // relay tears down (the down-completes-first select arm).
  {
    let streams = state.tcp_streams.lock().await;
    let handle = streams.get(&stream_id).expect("stream registered");
    handle.tx.push(crate::state::TcpConsumerMsg::Close).unwrap();
  }
  let _ = tokio::time::timeout(Duration::from_secs(2), relay).await;
  assert!(!state.tcp_streams.lock().await.contains_key(&stream_id));
}

#[tokio::test]
async fn spawn_listeners_accepts_and_relays_a_connection() {
  use tokio::net::{TcpListener, TcpStream};
  let state = Arc::new(test_state());

  // Grab a free port, release it, then hand it to spawn_listeners.
  let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = probe.local_addr().unwrap().port();
  drop(probe);

  spawn_listeners(
    state.clone(),
    "127.0.0.1",
    vec![ExposeRule {
      protocol: "tcp".to_string(),
      port,
      tunnel: None,
      org: None,
      token: None,
      key: Some("spawnkey123".to_string()),
    }],
  );

  // Give the listener a moment to bind, then drive one connection through the
  // accept loop (no declarer -> relay drops it, but the accept branch runs).
  let mut connected = false;
  for _ in 0..50 {
    tokio::time::sleep(Duration::from_millis(20)).await;
    if let Ok(sock) = TcpStream::connect(("127.0.0.1", port)).await {
      drop(sock);
      connected = true;
      break;
    }
  }
  assert!(connected, "listener should accept a connection");
  // Let the accepted connection be relayed before the test ends.
  tokio::time::sleep(Duration::from_millis(50)).await;
}

// --------------------------------------------------------------------------
// The identity form: `tunnel:` + `token:` instead of a shared secret.
// --------------------------------------------------------------------------

/// A named tunnel declaration.
fn named_tunnel(name: &str) -> TunnelDecl {
  TunnelDecl {
    custom_name: None,
    name: Some(name.to_string()),
    target: "127.0.0.1:9000".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }
}

#[tokio::test]
async fn a_named_rule_matches_the_tunnel_of_the_named_token() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |c| {
    c.tunnels = vec![named_tunnel("ssh_bastion")];
    c.perms.token_name = Some("bastion-host".to_string());
  })
  .await;

  let found = find_declarer(&state, &named_rule("ssh_bastion", Some("bastion-host"))).await;
  let (cid, _tx, target, _protocol) = found.expect("the declaring client is found by name");
  assert_eq!(cid, "c1");
  assert_eq!(target, "127.0.0.1:9000");
}

#[tokio::test]
async fn a_named_rule_does_not_match_another_token_claiming_the_name() {
  // The point of pinning the token: a second client in the same organization
  // must not be able to take the name and receive the public port's traffic.
  let state = Arc::new(test_state());
  insert_client(&state, "impostor", |c| {
    c.tunnels = vec![named_tunnel("ssh_bastion")];
    c.perms.token_name = Some("some-other-token".to_string());
  })
  .await;

  assert!(
    find_declarer(&state, &named_rule("ssh_bastion", Some("bastion-host")))
      .await
      .is_none()
  );
}

#[tokio::test]
async fn a_named_rule_without_a_token_accepts_only_the_master_token() {
  let state = Arc::new(test_state());
  insert_client(&state, "master-client", |c| {
    c.tunnels = vec![named_tunnel("ssh_bastion")];
    // A master-token client reports no token name.
    c.perms.token_name = None;
  })
  .await;
  assert!(
    find_declarer(&state, &named_rule("ssh_bastion", None))
      .await
      .is_some()
  );

  let state2 = Arc::new(test_state());
  insert_client(&state2, "named-client", |c| {
    c.tunnels = vec![named_tunnel("ssh_bastion")];
    c.perms.token_name = Some("ops".to_string());
  })
  .await;
  assert!(
    find_declarer(&state2, &named_rule("ssh_bastion", None))
      .await
      .is_none(),
    "a named token's tunnel needs the rule to name that token"
  );
}

/// A rule that claims the port for an organization, by name.
fn org_rule(tunnel: &str, org: Option<&str>) -> ExposeRule {
  ExposeRule {
    protocol: "tcp".to_string(),
    port: 5000,
    tunnel: Some(tunnel.to_string()),
    org: org.map(str::to_string),
    token: None,
    key: None,
  }
}

/// Creates an organization and returns its id.
async fn make_org(state: &Arc<AppState>, name: &str) -> String {
  state
    .org_store
    .lock()
    .await
    .create(name, Vec::new(), None)
    .expect("the organization is created")
    .id
}

#[tokio::test]
async fn a_named_rule_matches_the_organization_that_owns_the_tunnel() {
  // Two organizations, the same tunnel name, and, the case that made this
  // necessary, the same token name in both. Matching on the token name alone
  // made the winner a question of hash map order.
  let state = Arc::new(test_state());
  let payments = make_org(&state, "payments").await;
  let billing = make_org(&state, "billing").await;
  insert_client(&state, "payments-client", |c| {
    c.tunnels = vec![named_tunnel("postgres")];
    c.perms.token_name = Some("ci".to_string());
    c.perms.org_id = Some(payments.clone());
  })
  .await;
  insert_client(&state, "billing-client", |c| {
    c.tunnels = vec![named_tunnel("postgres")];
    c.perms.token_name = Some("ci".to_string());
    c.perms.org_id = Some(billing);
  })
  .await;

  for rule in [
    org_rule("postgres", Some("payments")),
    // The same claim written as the prefix instead of the key.
    org_rule("payments@postgres", None),
  ] {
    let (cid, _tx, _target, _protocol) = find_declarer(&state, &rule)
      .await
      .expect("the owning organization's client is found");
    assert_eq!(cid, "payments-client", "{}", rule.qualified_name());
  }
}

#[tokio::test]
async fn a_named_rule_with_no_organization_is_the_master_one() {
  let state = Arc::new(test_state());
  let payments = make_org(&state, "payments").await;
  insert_client(&state, "payments-client", |c| {
    c.tunnels = vec![named_tunnel("postgres")];
    c.perms.org_id = Some(payments);
  })
  .await;
  assert!(
    find_declarer(&state, &org_rule("postgres", Some("master")))
      .await
      .is_none(),
    "a child organization's tunnel is not the master organization's"
  );

  insert_client(&state, "master-client", |c| {
    c.tunnels = vec![named_tunnel("postgres")];
    c.perms.org_id = None;
  })
  .await;
  let (cid, _tx, _target, _protocol) = find_declarer(&state, &org_rule("postgres", Some("master")))
    .await
    .expect("the master organization's client is found");
  assert_eq!(cid, "master-client");
}

#[tokio::test]
async fn a_rule_naming_an_organization_that_does_not_exist_matches_nothing() {
  // Not "match whoever answers first": a typo in a server file must not open
  // a public port to another organization's tunnel of the same name.
  let state = Arc::new(test_state());
  let payments = make_org(&state, "payments").await;
  insert_client(&state, "payments-client", |c| {
    c.tunnels = vec![named_tunnel("postgres")];
    c.perms.org_id = Some(payments);
  })
  .await;
  assert!(
    find_declarer(&state, &org_rule("postgres", Some("paymnets")))
      .await
      .is_none()
  );
}

#[tokio::test]
async fn a_token_rule_written_before_organizations_still_matches_as_it_did() {
  // The compatibility that makes the change safe to upgrade into: `token:`
  // with no organization has always meant "whoever holds this token", and a
  // file that says it keeps matching the client it always matched.
  let state = Arc::new(test_state());
  let payments = make_org(&state, "payments").await;
  insert_client(&state, "payments-client", |c| {
    c.tunnels = vec![named_tunnel("postgres")];
    c.perms.token_name = Some("bastion-host".to_string());
    c.perms.org_id = Some(payments);
  })
  .await;
  let (cid, _tx, _target, _protocol) =
    find_declarer(&state, &named_rule("postgres", Some("bastion-host")))
      .await
      .expect("the token rule still matches across organizations");
  assert_eq!(cid, "payments-client");
}

#[tokio::test]
async fn a_named_rule_still_refuses_an_encrypted_tunnel() {
  // A raw public socket cannot run the client-side handshake, whichever way
  // the rule addresses the tunnel.
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |c| {
    let mut decl = named_tunnel("ssh_bastion");
    decl.encrypt = true;
    c.tunnels = vec![decl];
    c.perms.token_name = Some("bastion-host".to_string());
  })
  .await;
  assert!(
    find_declarer(&state, &named_rule("ssh_bastion", Some("bastion-host")))
      .await
      .is_none()
  );
}

#[test]
fn an_unnamed_tunnel_is_addressable_by_its_derived_name() {
  // A rule may name a tunnel the client never named, since the derivation is
  // shared between both sides.
  let decl = TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:22".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  };
  assert_eq!(crate::tunnel::registry::name_of(&decl), "127_0_0_1_22_tcp");
}

#[test]
fn a_rule_labels_itself_the_way_the_file_spelled_it() {
  // Every spelling of ownership, rendered for logs and the audit trail.
  let mut rule = named_rule("payments@pg", None);
  assert_eq!(rule.label(), "tunnel payments@pg");
  assert_eq!(rule.qualified_name(), "payments@pg");

  rule = named_rule("pg", None);
  rule.org = Some("payments".to_string());
  assert_eq!(rule.qualified_name(), "payments@pg");

  rule = named_rule("pg", Some("ci"));
  assert_eq!(rule.qualified_name(), "pg (token ci)");

  rule = named_rule("pg", None);
  assert_eq!(rule.qualified_name(), "master@pg");

  let keyed = key_rule("shared-secret");
  assert_eq!(keyed.label(), "a key-matched tunnel");
  let mut bare = key_rule("x");
  bare.key = None;
  assert_eq!(bare.label(), "nothing");
}
