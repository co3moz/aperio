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

/// Holds the cross-thread config-file lock (shared by path with the other
/// config-touching test modules) so the global document is not raced.
struct CfgLock(std::path::PathBuf);
impl CfgLock {
  fn acquire() -> Self {
    let lock = std::env::temp_dir().join("aperio-cfgfile-test.lock");
    let start = std::time::Instant::now();
    loop {
      match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
      {
        Ok(_) => return CfgLock(lock),
        Err(_) => {
          if let Ok(md) = std::fs::metadata(&lock)
            && md
              .modified()
              .ok()
              .and_then(|m| m.elapsed().ok())
              .is_some_and(|e| e.as_secs() > 30)
          {
            let _ = std::fs::remove_file(&lock);
          }
          assert!(start.elapsed().as_secs() < 120, "cfg lock timeout");
          std::thread::sleep(std::time::Duration::from_millis(5));
        }
      }
    }
  }
}
impl Drop for CfgLock {
  fn drop(&mut self) {
    unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
    let _ = std::fs::remove_file("aperio-server.yaml");
    let _ = std::fs::remove_file(&self.0);
  }
}

fn load_config(yaml: &str) {
  let file = std::env::temp_dir().join(format!("aperio-expose-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(&file, yaml).unwrap();
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", file.to_str().unwrap()) };
  crate::config_file::load();
}

#[test]
fn expose_rule_defaults_protocol_to_tcp() {
  let rule: ExposeRule = serde_yaml::from_str("port: 5000\nkey: longenoughkey\n").unwrap();
  assert_eq!(rule.protocol, "tcp");
  assert_eq!(rule.port, 5000);
  assert_eq!(rule.key, "longenoughkey");
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

fn tunnel(key: Option<&str>, protocol: &str, encrypt: bool) -> TunnelDecl {
  TunnelDecl {
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
  state.clients.lock().await.insert(cid.to_string(), c);
}

#[tokio::test]
async fn find_declarer_matches_healthy_declaring_client() {
  let state = Arc::new(test_state());
  insert_client(&state, "c1", |c| {
    c.tunnels = vec![tunnel(Some("mykey12345"), "tcp", false)];
  })
  .await;

  let found = find_declarer(&state, "mykey12345").await;
  let (cid, _tx, target) = found.expect("declaring client found");
  assert_eq!(cid, "c1");
  assert_eq!(target, "127.0.0.1:9000");
}

#[tokio::test]
async fn find_declarer_none_when_no_client() {
  let state = Arc::new(test_state());
  assert!(find_declarer(&state, "mykey12345").await.is_none());
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

  assert!(find_declarer(&state, "mykey12345").await.is_none());
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
  relay_public_tcp(state, server_sock, peer, "unknownkey1").await;
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
    state.clients.lock().await.insert("c1".to_string(), c);
  }

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let mut visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  // Run the relay in the background.
  let relay_state = state.clone();
  let relay = tokio::spawn(async move {
    relay_public_tcp(relay_state, server_sock, peer, "mykey12345").await;
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
      .send(crate::state::TcpConsumerMsg::Data(b"world".to_vec()))
      .await
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
  relay_public_tcp(state.clone(), server_sock, peer, "mykey12345").await;
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
    state.clients.lock().await.insert("dead".to_string(), c);
  }

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let _visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  relay_public_tcp(state.clone(), server_sock, peer, "mykey12345").await;
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
    state.clients.lock().await.insert("c1".to_string(), c);
  }

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let _visitor = TcpStream::connect(addr).await.unwrap();
  let (server_sock, peer) = listener.accept().await.unwrap();

  let relay_state = state.clone();
  let relay = tokio::spawn(async move {
    relay_public_tcp(relay_state, server_sock, peer, "mykey12345").await;
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
    handle
      .tx
      .send(crate::state::TcpConsumerMsg::Close)
      .await
      .unwrap();
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
      key: "spawnkey123".to_string(),
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
