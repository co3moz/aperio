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
) {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  info!("Opening TCP stream {} to {}", stream_id, target_addr);
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
      let close = TunnelMessage::TcpClose {
        stream_id: stream_id.clone(),
      };
      if let Ok(json) = serde_json::to_string(&close) {
        let _ = tunnel_tx.send(Message::Text(json)).await;
      }
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
          let msg = TunnelMessage::TcpData {
            stream_id: stream_id_up.clone(),
            data: BASE64_STANDARD.encode(&buf[..n]),
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
  let down_task = tokio::spawn(async move {
    loop {
      tokio::select! {
        _ = abort_rx.recv() => break,
        chunk = bytes_rx.recv() => match chunk {
          Some(bytes) => {
            if write_half.write_all(&bytes).await.is_err() {
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
      bridge_connection(sock, &ws_url, &token).await;
      info!("TCP bridge: connection from {} closed", peer);
    });
  }
}

/// Relays one accepted local TCP connection over a fresh WebSocket stream to
/// the server's `/aperio/tcp` endpoint (query parameters select a specific
/// peer client / declared tunnel target). Returns when either side closes.
pub(crate) async fn bridge_connection(mut sock: tokio::net::TcpStream, ws_url: &str, token: &str) {
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
  let (mut tcp_read, mut tcp_write) = sock.split();

  let to_server = async {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
      match tcp_read.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          if ws_tx
            .send(Message::Binary(buf[..n].to_vec()))
            .await
            .is_err()
          {
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
          if tcp_write.write_all(&bytes).await.is_err() {
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
