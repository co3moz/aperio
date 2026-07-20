//! Experimental public TCP expose (the `expose:` section of
//! `aperio-server.yaml`).
//!
//! An expose entry opens a raw public TCP port on the server and relays every
//! accepted connection to the declared tunnel of a connected client whose
//! `tunnels:` entry carries the matching `expose: <key>` — the built-in
//! equivalent of a `--bind-tunnels` peer, with the server itself as the
//! binder. The key is a shared secret between the server file and the client
//! config; it travels only inside the (TLS-protected) tunnel handshake and is
//! never re-serialized to binders.
//!
//! Experimental semantics, on purpose: exactly one serving client per
//! connection (the first healthy declarer wins — like client-id binding),
//! no load balancing, TCP only, and end-to-end encrypted tunnels are
//! excluded (a raw public socket cannot run the client-side handshake).

use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::protocol::TunnelMessage;
use crate::state::{AppState, TcpConsumerMsg, TcpStreamHandle};

/// One public expose port from aperio-server.yaml.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct ExposeRule {
  /// Transport of the exposed port; only `tcp` is supported while
  /// experimental.
  #[serde(default = "default_tcp")]
  pub(crate) protocol: String,
  /// Public port the server listens on.
  pub(crate) port: u16,
  /// Shared secret a client's tunnel declaration must present
  /// (`tunnels: [{target: ..., expose: <key>}]`).
  pub(crate) key: String,
}

fn default_tcp() -> String {
  "tcp".to_string()
}

/// Reads and validates the `expose:` section of `aperio-server.yaml`.
/// Like the other structured sections, a malformed one is a startup error.
pub(crate) fn from_config_file() -> Vec<ExposeRule> {
  let Some(section) = crate::config_file::structured("expose") else {
    return Vec::new();
  };
  let rules: Vec<ExposeRule> = match serde_yaml::from_value(section) {
    Ok(rules) => rules,
    Err(err) => {
      error!("invalid `expose:` section in aperio-server.yaml: {err}");
      std::process::exit(1);
    }
  };
  let mut ports = std::collections::HashSet::new();
  for (i, rule) in rules.iter().enumerate() {
    if rule.protocol != "tcp" {
      error!(
        "expose entry #{}: protocol `{}` is not supported (experimental public expose is TCP only)",
        i + 1,
        rule.protocol
      );
      std::process::exit(1);
    }
    if rule.key.trim().len() < 8 {
      error!(
        "expose entry #{}: the key must be at least 8 characters (it is the only thing gating the port)",
        i + 1
      );
      std::process::exit(1);
    }
    if !ports.insert(rule.port) {
      error!(
        "expose entry #{}: port {} is declared twice",
        i + 1,
        rule.port
      );
      std::process::exit(1);
    }
  }
  rules
}

/// Spawns one listener task per expose rule. Called once at startup.
pub(crate) fn spawn_listeners(state: Arc<AppState>, host: &str, rules: Vec<ExposeRule>) {
  for rule in rules {
    let state = state.clone();
    let addr = format!("{}:{}", host, rule.port);
    tokio::spawn(async move {
      let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(err) => {
          error!("public expose: cannot bind {addr}: {err}");
          return;
        }
      };
      info!("public expose (experimental): listening on {addr} (tcp)");
      loop {
        match listener.accept().await {
          Ok((socket, peer)) => {
            let state = state.clone();
            let key = rule.key.clone();
            tokio::spawn(async move {
              relay_public_tcp(state, socket, peer, &key).await;
            });
          }
          Err(err) => {
            warn!("public expose {addr}: accept failed: {err}");
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
          }
        }
      }
    });
  }
}

/// Finds the serving client for an expose key: the first healthy, enabled,
/// non-draining client declaring a plain (non-encrypted) TCP tunnel with
/// this key. Returns (client id, sender, declared target).
async fn find_declarer(
  state: &Arc<AppState>,
  key: &str,
) -> Option<(String, mpsc::Sender<axum::extract::ws::Message>, String)> {
  let clients = state.clients.lock().await;
  for (cid, c) in clients.iter() {
    if !c.admin_enabled || c.draining || !c.is_healthy(state.config().client_down_threshold) {
      continue;
    }
    if let Some(decl) = c
      .tunnels
      .iter()
      .find(|d| d.protocol == "tcp" && !d.encrypt && d.expose.as_deref() == Some(key))
    {
      return Some((cid.clone(), c.tx.clone(), decl.target.clone()));
    }
  }
  None
}

/// Relays bytes between a public TCP socket and the declaring client's
/// tunnel — the raw-socket sibling of `relay_tcp_consumer`.
async fn relay_public_tcp(
  state: Arc<AppState>,
  socket: tokio::net::TcpStream,
  peer: std::net::SocketAddr,
  key: &str,
) {
  use axum::extract::ws::Message;
  use base64::prelude::*;

  if !state.check_rate_limit(peer.ip()).await {
    return;
  }
  let Some((client_id, client_tx, target)) = find_declarer(&state, key).await else {
    debug!("public expose: no connected client declares this key; dropping {peer}");
    return;
  };

  state
    .audit(
      "expose_stream_opened",
      "system",
      &peer.ip().to_string(),
      &format!("client={} target={}", client_id, target),
    )
    .await;

  let stream_id = uuid::Uuid::new_v4().to_string();
  let (relay_tx, mut relay_rx) = mpsc::channel::<TcpConsumerMsg>(64);
  state.tcp_streams.lock().await.insert(
    stream_id.clone(),
    TcpStreamHandle {
      tx: relay_tx,
      client_id: client_id.clone(),
    },
  );

  // Ask the client to open its declared target.
  let open = TunnelMessage::TcpOpen {
    stream_id: stream_id.clone(),
    target: Some(target),
  };
  if let Ok(json) = serde_json::to_string(&open)
    && client_tx.send(Message::Text(json)).await.is_err()
  {
    state.tcp_streams.lock().await.remove(&stream_id);
    return;
  }

  let (mut read_half, mut write_half) = socket.into_split();

  // Visitor socket → tunnel
  let stream_id_up = stream_id.clone();
  let client_tx_up = client_tx.clone();
  let up_task = tokio::spawn(async move {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
      match read_half.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          let data_msg = TunnelMessage::TcpData {
            stream_id: stream_id_up.clone(),
            data: BASE64_STANDARD.encode(&buf[..n]),
          };
          if let Ok(json) = serde_json::to_string(&data_msg)
            && client_tx_up.send(Message::Text(json)).await.is_err()
          {
            break;
          }
        }
      }
    }
    // Visitor went away → close the client side.
    let close = TunnelMessage::TcpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = client_tx_up.send(Message::Text(json)).await;
    }
  });

  // Tunnel → visitor socket
  let down_task = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        TcpConsumerMsg::Data(bytes) => {
          if write_half.write_all(&bytes).await.is_err() {
            break;
          }
        }
        TcpConsumerMsg::Close => break,
      }
    }
    let _ = write_half.shutdown().await;
  });

  let up_abort = up_task.abort_handle();
  let down_abort = down_task.abort_handle();
  tokio::select! {
    _ = up_task => down_abort.abort(),
    _ = down_task => up_abort.abort(),
  }

  state.tcp_streams.lock().await.remove(&stream_id);
  debug!("public expose stream {} closed", stream_id);
}

#[cfg(test)]
#[path = "expose_tests.rs"]
mod tests;
