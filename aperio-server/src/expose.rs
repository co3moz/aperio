//! Public TCP expose (the `expose:` section of `aperio-server.yaml`).
//!
//! An expose entry opens a raw public TCP port on the server and relays every
//! accepted connection into a client's declared tunnel: the built-in
//! equivalent of a `--bind-tunnels` peer, with the server itself as the
//! binder.
//!
//! Two ways to say which tunnel. `tunnel:` names it and `org:` says whose it
//! is, so the claim is settled by identity: another organization cannot take
//! the name first and receive the traffic, and `payments@postgres` in the
//! file reads the way the dashboard shows it. `org:` may be written as the
//! `<org>@<name>` prefix of `tunnel:` instead, which is the same thing said
//! once.
//!
//! `token:` was the earlier way to say the same thing and still works. It is
//! the weaker one: a token name is not unique, nothing stops two
//! organizations from each having a token called `ci`, so a rule naming one
//! could match a client of either, and which of them got the port came down
//! to the order of a hash map. An organization is the boundary the rest of
//! the system already enforces, and its name is unique by construction.
//!
//! The older `key:` is a shared secret repeated in the client's declaration;
//! it still works, but it names no owner and cannot be revoked.
//!
//! Deliberate limits: exactly one serving client per connection (the first
//! healthy declarer wins), no load balancing, TCP only (a public UDP port is
//! an amplification surface and a separate decision), and end-to-end
//! encrypted tunnels are excluded, since a raw public socket cannot run the
//! client-side handshake.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::protocol::TunnelMessage;
use crate::state::{AppState, TcpConsumerMsg, TcpStreamHandle};

pub(crate) use aperio_config::ExposeEntry as ExposeRule;

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
  let errors =
    aperio_config::expose::validate_entries(&rules, &[aperio_config::expose::ExposeProtocol::Tcp]);
  if !errors.is_empty() {
    for err in errors {
      error!("invalid public expose: {err}");
    }
    std::process::exit(1);
  }
  for rule in &rules {
    if rule.tunnel.is_some() && rule.token.is_some() {
      warn!("public expose: `token:` is superseded by `org:` or a qualified tunnel name");
    }
  }
  rules
}

/// Spawns one listener task per expose rule. Called once at startup.
pub(crate) fn spawn_listeners(state: Arc<AppState>, host: &str, rules: Vec<ExposeRule>) {
  for rule in rules {
    let state = state.clone();
    let addr = format!("{}:{}", host, rule.port);
    crate::supervise::spawn_critical("expose-listener", async move {
      let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(err) => {
          error!("public expose: cannot bind {addr}: {err}");
          return;
        }
      };
      info!(
        "public expose: listening on {addr} (tcp) for {}",
        rule.label()
      );
      loop {
        match listener.accept().await {
          Ok((socket, peer)) => {
            let state = state.clone();
            let rule = rule.clone();
            tokio::spawn(async move {
              relay_public_tcp(state, socket, peer, &rule).await;
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
  rule: &ExposeRule,
) -> Option<(
  String,
  mpsc::Sender<axum::extract::ws::Message>,
  String,
  u32,
)> {
  // Which organization may claim this port, resolved before the client map is
  // locked. An unknown name matches nothing at all: a typo in a server file
  // must not widen a port to whoever answers first.
  //
  // Only when the rule actually names one. A file written before `org:`
  // existed says `token:` alone, and that has always meant "whoever holds
  // this token, wherever they are"; changing what those files match would be
  // a silent outage on upgrade, so the older rule is left exactly as it was.
  let org_id = match rule.explicit_org() {
    Some(name) => match crate::tunnel::registry::org_id_for_name(state, name).await {
      Ok(id) => Some(id),
      Err(why) => {
        warn!(
          "public expose: {} names an organization that does not exist ({why})",
          rule.label()
        );
        return None;
      }
    },
    None => None,
  };
  let clients = state.clients.read().await;
  for (cid, c) in clients.iter() {
    if !c.serves_process_scoped(state.config().client_down_threshold) {
      continue;
    }
    // A public socket cannot run the client-side encryption handshake, so an
    // encrypted tunnel is never eligible however it is addressed.
    let matched = c.tunnels().iter().find(|d| {
      if !aperio_config::protocol_serves(&d.protocol, "tcp") || d.encrypt {
        return false;
      }
      match &rule.tunnel {
        // Identity: the right name, declared by a client of the named
        // organization. A `token:` still narrows it further, so a file
        // written before `org:` existed keeps meaning what it meant.
        Some(name) => {
          let (_, bare) = crate::tunnel::registry::split_qualified(name);
          if crate::tunnel::registry::name_of(d) != bare {
            return false;
          }
          match (&org_id, rule.token.as_deref().map(str::trim)) {
            // The organization is the claim; a `token:` alongside it narrows
            // the claim further rather than replacing it.
            (Some(org), Some(token)) => {
              c.perms.org_id == *org && c.perms.token_name.as_deref() == Some(token)
            }
            (Some(org), None) => c.perms.org_id == *org,
            // The older rule, unchanged: the named token, or the master token
            // when the rule names none.
            (None, token) => c.perms.token_name.as_deref() == token,
          }
        }
        // The deprecated shared secret.
        None => d.expose.as_deref() == rule.key.as_deref(),
      }
    });
    if let Some(decl) = matched {
      return Some((
        cid.clone(),
        c.tx.clone(),
        decl.target.clone(),
        c.client_protocol.unwrap_or(1),
      ));
    }
  }
  None
}

/// Relays bytes between a public TCP socket and the declaring client's
/// tunnel, the raw-socket sibling of `relay_tcp_consumer`.
async fn relay_public_tcp(
  state: Arc<AppState>,
  socket: tokio::net::TcpStream,
  peer: std::net::SocketAddr,
  rule: &ExposeRule,
) {
  use axum::extract::ws::Message;

  if !state.check_rate_limit(peer.ip()).await {
    return;
  }
  let Some((client_id, client_tx, target, protocol)) = find_declarer(&state, rule).await else {
    debug!(
      "public expose on port {}: no connected client serves {}; dropping {peer}",
      rule.port,
      rule.label()
    );
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
  // The tunnel read loop feeds a pump rather than this channel: a consumer
  // that stops reading must not stall the other streams on that tunnel;
  // past the byte watermark the client is asked to pause producing.
  let flow = crate::state::StreamFlow::new(
    stream_id.clone(),
    client_tx.clone(),
    state.client_supports_pause(&client_id).await,
    state.stream_limits(),
  );
  let relay_tx =
    crate::state::spawn_consumer_pump(relay_tx, state.config().gateway_response_timeout, flow);
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
    // No selector yet: the TCP/UDP bind registry keys on the connection, so
    // which service owns a raw tunnel is not resolved here. A multiplexing
    // client with `tunnels:` is the case this leaves open, and it is named in
    // planned_features #46 rather than guessed at.
    service: None,
    // The address the server actually accepted the connection from, so a
    // tunnel with `proxy_protocol: true` can name the visitor to its backend.
    visitor: Some(peer.to_string()),
  };
  if let Ok(json) = serde_json::to_string(&open)
    && client_tx.send(Message::Text(json.into())).await.is_err()
  {
    state.tcp_streams.lock().await.remove(&stream_id);
    return;
  }

  let (mut read_half, mut write_half) = socket.into_split();

  // A public port authorizes nobody, so there is no token to name: the whole
  // point of `expose:` is that a visitor reaches it with no credential, and
  // the log should say so by leaving the field out rather than inventing one.
  let record = crate::relay_log::RelayRecord::new("tcp", "expose", peer.to_string(), client_id)
    .tunnel(rule.tunnel.clone())
    .port(rule.port);
  let up_bytes = record.up_counter();
  let down_bytes = record.down_counter();

  // Visitor socket → tunnel
  let stream_id_up = stream_id.clone();
  let client_tx_up = client_tx.clone();
  let up_task = tokio::spawn(async move {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
      match read_half.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          // v7 takes the bytes raw; an older client takes base64 in JSON.
          let Some(frame) = crate::protocol::relay_frame(
            protocol,
            crate::protocol::FRAME_TCP_DATA,
            &stream_id_up,
            &buf[..n],
            |data| TunnelMessage::TcpData {
              stream_id: stream_id_up.clone(),
              data,
            },
          ) else {
            break;
          };
          if client_tx_up.send(frame).await.is_err() {
            break;
          }
          up_bytes.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
      }
    }
    // Visitor went away → close the client side.
    let close = TunnelMessage::TcpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = client_tx_up.send(Message::Text(json.into())).await;
    }
  });

  // Tunnel → visitor socket
  let down_task = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        TcpConsumerMsg::Data(bytes) => {
          let moved = bytes.len() as u64;
          if write_half.write_all(&bytes).await.is_err() {
            break;
          }
          down_bytes.fetch_add(moved, std::sync::atomic::Ordering::Relaxed);
        }
        TcpConsumerMsg::Close => break,
      }
    }
    let _ = write_half.shutdown().await;
  });

  let (mut up_task, mut down_task) = (up_task, down_task);
  tokio::select! {
    _ = &mut up_task => {
      // The visitor stopped sending, which for a TCP client usually means it
      // finished its request and is now waiting for the answer, not that it
      // is done with the connection. Aborting the download here discarded
      // whatever the backend had already sent. Let it drain instead, bounded
      // so a client that never closes its side cannot hold the stream open.
      let _ = tokio::time::timeout(
        state.config().gateway_response_timeout,
        &mut down_task,
      )
      .await;
      down_task.abort();
    }
    _ = &mut down_task => up_task.abort(),
  }

  state.tcp_streams.lock().await.remove(&stream_id);
  record.finish(&state);
  debug!("public expose stream {} closed", stream_id);
}

#[cfg(test)]
#[path = "expose_tests.rs"]
mod tests;
