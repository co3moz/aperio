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

/// Opens a TCP connection to the configured local target and relays bytes
/// bidirectionally between it and the tunnel.
pub(crate) async fn handle_tcp_open(
  stream_id: String,
  target_addr: String,
  tunnel_tx: mpsc::Sender<Message>,
  active_streams: Arc<Mutex<HashMap<String, TcpStreamHandle>>>,
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
  let (bytes_tx, mut bytes_rx) = mpsc::channel::<Vec<u8>>(64);
  let (abort_tx, mut abort_rx) = mpsc::channel::<()>(1);
  active_streams.lock().await.insert(
    stream_id.clone(),
    TcpStreamHandle {
      tx: bytes_tx,
      abort_tx,
    },
  );

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

/// Runs a local TCP bridge: listens on 127.0.0.1:<port> and relays each
/// accepted connection to the server's experimental `/aperio/tcp` endpoint,
/// which tunnels it to the remote client's TCP target.
pub(crate) async fn run_tcp_bridge(local_port: u16, server: &str, token: &str) {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    let (mut sock, peer) = match listener.accept().await {
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
      // Open a fresh tunnel stream for this connection.
      let mut req = match ws_url.clone().into_client_request() {
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
      info!("TCP bridge: connection from {} closed", peer);
    });
  }
}
