//! Experimental TCP tunneling: relaying server-initiated streams to the
//! configured local TCP target, and the consumer-side local bridge.

use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::{
  connect_async,
  tungstenite::{client::IntoClientRequest, http::HeaderValue, protocol::Message},
};
use tracing::{error, info};

use crate::config::build_ws_url_with_path;
use crate::protocol::TunnelMessage;

/// Handle to an active raw TCP stream connected to the local TCP target.
pub(crate) struct TcpStreamHandle {
  /// Sender to forward decoded TcpData bytes to the TCP writer task.
  pub(crate) tx: mpsc::Sender<Vec<u8>>,
  /// Abort handle to stop the relay tasks.
  pub(crate) abort_tx: mpsc::Sender<()>,
}

/// Opens a TCP connection to the local target and relays bytes
/// bidirectionally between it and the tunnel.
///
/// The stream handle must already be registered in `active_streams` (the
/// tunnel read loop does so synchronously before spawning this): the server
/// starts relaying consumer bytes right after TcpOpen, and TcpData for an
/// unregistered stream would be dropped silently. `bytes_rx` buffers
/// whatever arrives while the connect is still in flight.
pub(crate) async fn handle_tcp_open(
  stream_id: String,
  target_addr: String,
  tunnel_tx: mpsc::Sender<Message>,
  active_streams: Arc<Mutex<HashMap<String, TcpStreamHandle>>>,
  mut bytes_rx: mpsc::Receiver<Vec<u8>>,
  mut abort_rx: mpsc::Receiver<()>,
  e2e: Option<crate::e2e::E2eParams>,
) {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  info!(
    "Opening TCP stream {} to {}{}",
    stream_id,
    target_addr,
    if e2e.is_some() {
      " (end-to-end encrypted)"
    } else {
      ""
    }
  );
  let send_close = |tx: mpsc::Sender<Message>, id: String| async move {
    let close = TunnelMessage::TcpClose { stream_id: id };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = tx.send(Message::Text(json)).await;
    }
  };

  // End-to-end handshake (encrypt: true): exchange ephemeral X25519 keys
  // with the binder before any payload byte flows. Every relayed frame
  // afterwards is AEAD-sealed; the server only sees ciphertext.
  let (mut sealer, mut opener) = if let Some(params) = e2e {
    let hs = crate::e2e::Handshake::new(crate::e2e::Role::Responder, params.psk);
    let hs_msg = TunnelMessage::TcpData {
      stream_id: stream_id.clone(),
      data: BASE64_STANDARD.encode(&hs.frame),
    };
    let sent = match serde_json::to_string(&hs_msg) {
      Ok(json) => tunnel_tx.send(Message::Text(json)).await.is_ok(),
      Err(_) => false,
    };
    let peer_frame = if sent {
      tokio::time::timeout(Duration::from_secs(10), bytes_rx.recv())
        .await
        .ok()
        .flatten()
    } else {
      None
    };
    match peer_frame.and_then(|frame| hs.complete(&frame)) {
      Some(session) => (Some(session.sealer), Some(session.opener)),
      None => {
        error!(
          "E2E handshake failed for encrypted tunnel stream {}; closing",
          stream_id
        );
        active_streams.lock().await.remove(&stream_id);
        send_close(tunnel_tx.clone(), stream_id).await;
        return;
      }
    }
  } else {
    (None, None)
  };

  let connect = tokio::time::timeout(
    Duration::from_secs(10),
    tokio::net::TcpStream::connect(&target_addr),
  )
  .await;
  let stream = match connect {
    Ok(Ok(s)) => s,
    _ => {
      error!(
        "TCP connect to {} failed for stream {}",
        target_addr, stream_id
      );
      active_streams.lock().await.remove(&stream_id);
      send_close(tunnel_tx.clone(), stream_id).await;
      return;
    }
  };

  let (mut read_half, mut write_half) = stream.into_split();

  // Backend -> tunnel
  let stream_id_up = stream_id.clone();
  let tunnel_tx_up = tunnel_tx.clone();
  let up_task = tokio::spawn(async move {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
      match read_half.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          let payload = match &mut sealer {
            Some(s) => match s.seal(&buf[..n]) {
              Some(sealed) => sealed,
              None => break,
            },
            None => buf[..n].to_vec(),
          };
          let msg = TunnelMessage::TcpData {
            stream_id: stream_id_up.clone(),
            data: BASE64_STANDARD.encode(&payload),
          };
          if let Ok(json) = serde_json::to_string(&msg)
            && tunnel_tx_up.send(Message::Text(json)).await.is_err()
          {
            break;
          }
        }
      }
    }
    let close = TunnelMessage::TcpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = tunnel_tx_up.send(Message::Text(json)).await;
    }
  });

  // Tunnel -> backend
  let stream_id_down = stream_id.clone();
  let down_task = tokio::spawn(async move {
    loop {
      tokio::select! {
        _ = abort_rx.recv() => break,
        chunk = bytes_rx.recv() => match chunk {
          Some(bytes) => {
            let plain = match &mut opener {
              Some(o) => match o.open(&bytes) {
                Some(p) => p,
                None => {
                  // Tampering, reordering, or a PSK mismatch: kill the
                  // stream rather than feed corrupt bytes to the backend.
                  error!("E2E decryption failed on tunnel stream {}; closing", stream_id_down);
                  break;
                }
              },
              None => bytes,
            };
            if write_half.write_all(&plain).await.is_err() {
              break;
            }
          }
          None => break,
        },
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
  active_streams.lock().await.remove(&stream_id);
  info!("TCP stream {} closed", stream_id);
}

/// Exits the process cleanly on SIGINT/SIGTERM. Spawned by the auxiliary
/// modes (`tcp` bridge, `--bind-tunnels`) which otherwise run forever; the
/// clean exit also flushes coverage/profiling data in instrumented builds.
pub(crate) fn spawn_shutdown_watcher() {
  tokio::spawn(async {
    let ctrl_c = async {
      let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
      if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      {
        sig.recv().await;
      } else {
        std::future::pending::<()>().await;
      }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
      _ = ctrl_c => {},
      _ = terminate => {},
    }
    info!("Shutdown signal received; exiting.");
    std::process::exit(0);
  });
}

/// Runs a local TCP bridge: listens on 127.0.0.1:<port> and relays each
/// accepted connection to the server's experimental `/aperio/tcp` endpoint,
/// which tunnels it to the remote client's TCP target.
pub(crate) async fn run_tcp_bridge(local_port: u16, server: &str, token: &str) {
  spawn_shutdown_watcher();
  let ws_url = match build_ws_url_with_path(server, "/aperio/tcp") {
    Ok(u) => u,
    Err(e) => {
      error!("Failed to build TCP bridge URL: {}", e);
      std::process::exit(1);
    }
  };

  let listener = match tokio::net::TcpListener::bind(("127.0.0.1", local_port)).await {
    Ok(l) => l,
    Err(e) => {
      error!("Failed to bind 127.0.0.1:{}: {}", local_port, e);
      std::process::exit(1);
    }
  };
  let bound = listener
    .local_addr()
    .map(|a| a.port())
    .unwrap_or(local_port);
  info!(
    "TCP bridge listening on 127.0.0.1:{} -> {} (remote client's TCP target)",
    bound, ws_url
  );

  loop {
    let (sock, peer) = match listener.accept().await {
      Ok(x) => x,
      Err(e) => {
        error!("TCP bridge accept error: {}", e);
        continue;
      }
    };
    info!("TCP bridge: connection from {}", peer);

    let ws_url = ws_url.clone();
    let token = token.to_string();
    tokio::spawn(async move {
      bridge_connection(sock, &ws_url, &token, false, None).await;
      info!("TCP bridge: connection from {} closed", peer);
    });
  }
}

/// Relays one accepted local TCP connection over a fresh WebSocket stream to
/// the server's `/aperio/tcp` endpoint (query parameters select a specific
/// peer client / declared tunnel target). Returns when either side closes.
///
/// With `encrypt`, an end-to-end X25519 handshake with the declaring client
/// runs first and every relayed frame is AEAD-sealed — the server only sees
/// ciphertext. `psk` (when both sides configure the same one) protects the
/// exchange against an actively hostile server.
pub(crate) async fn bridge_connection(
  mut sock: tokio::net::TcpStream,
  ws_url: &str,
  token: &str,
  encrypt: bool,
  psk: Option<String>,
) {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  // Open a fresh tunnel stream for this connection.
  let mut req = match ws_url.to_string().into_client_request() {
    Ok(r) => r,
    Err(e) => {
      error!("TCP bridge request build error: {:?}", e);
      return;
    }
  };
  match HeaderValue::from_str(&format!("Bearer {}", token)) {
    Ok(val) => {
      req.headers_mut().insert("Authorization", val);
    }
    Err(_) => return,
  }
  let (ws, _) = match connect_async(req).await {
    Ok(x) => x,
    Err(e) => {
      error!("TCP bridge failed to reach server: {:?}", e);
      return;
    }
  };
  let (mut ws_tx, mut ws_rx) = ws.split();

  // End-to-end handshake with the declaring client (encrypted tunnels).
  let (mut sealer, mut opener) = if encrypt {
    let hs = crate::e2e::Handshake::new(crate::e2e::Role::Initiator, psk);
    if ws_tx.send(Message::Binary(hs.frame.clone())).await.is_err() {
      return;
    }
    // The first binary frame from the peer is its handshake.
    let peer_frame = tokio::time::timeout(Duration::from_secs(10), async {
      while let Some(Ok(msg)) = ws_rx.next().await {
        if let Message::Binary(bytes) = msg {
          return Some(bytes);
        }
      }
      None
    })
    .await
    .ok()
    .flatten();
    match peer_frame.and_then(|frame| hs.complete(&frame)) {
      Some(session) => (Some(session.sealer), Some(session.opener)),
      None => {
        error!("E2E handshake with the declaring client failed; closing the tunnel stream");
        let _ = ws_tx.send(Message::Close(None)).await;
        return;
      }
    }
  } else {
    (None, None)
  };

  let (mut tcp_read, mut tcp_write) = sock.split();

  let to_server = async {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
      match tcp_read.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          let payload = match &mut sealer {
            Some(s) => match s.seal(&buf[..n]) {
              Some(sealed) => sealed,
              None => break,
            },
            None => buf[..n].to_vec(),
          };
          if ws_tx.send(Message::Binary(payload)).await.is_err() {
            break;
          }
        }
      }
    }
    let _ = ws_tx.send(Message::Close(None)).await;
  };

  let to_local = async {
    while let Some(Ok(msg)) = ws_rx.next().await {
      match msg {
        Message::Binary(bytes) => {
          let plain = match &mut opener {
            Some(o) => match o.open(&bytes) {
              Some(p) => p,
              None => {
                // Key mismatch (wrong PSK / MITM) or tampering: fail closed.
                error!("E2E decryption failed on the tunnel stream; closing");
                break;
              }
            },
            None => bytes,
          };
          if tcp_write.write_all(&plain).await.is_err() {
            break;
          }
        }
        Message::Close(_) => break,
        _ => {}
      }
    }
    let _ = tcp_write.shutdown().await;
  };

  tokio::join!(to_server, to_local);
}
