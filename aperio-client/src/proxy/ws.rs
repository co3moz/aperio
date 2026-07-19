//! WebSocket pass-through: upgrades a tunnel stream into a live WebSocket
//! connection to the local backend and relays frames in both directions.

use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::{
  connect_async,
  tungstenite::{
    client::IntoClientRequest,
    http::{HeaderName as TungsteniteHeaderName, HeaderValue},
    protocol::Message,
  },
};
use tracing::{debug, error, info};

use crate::protocol::TunnelMessage;

/// Handle to an active WebSocket proxy stream connected to the local backend.
pub(crate) struct WsStreamHandle {
  /// Sender to forward tunnel WsData frames to the backend WebSocket writer task.
  pub(crate) tx: mpsc::Sender<Message>,
  /// Abort handle to stop the relay tasks.
  pub(crate) abort_tx: mpsc::Sender<()>,
}

/// Handles a WebSocket upgrade request from the server.
/// Connects to the local backend via WebSocket, sends the upgrade response,
/// and spawns relay tasks for bidirectional frame forwarding.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_upgrade_request(
  stream_id: String,
  _method: String,
  uri_str: String,
  headers: Vec<(String, String)>,
  target: &str,
  path_bind: Option<String>,
  trim_bind: bool,
  tunnel_tx: mpsc::Sender<Message>,
  active_streams: Arc<Mutex<HashMap<String, WsStreamHandle>>>,
  client_timeout_secs: u64,
) {
  info!("Handling WebSocket upgrade for stream {}", stream_id);

  // Unix socket targets are HTTP-only: tokio-tungstenite cannot dial a
  // filesystem socket, so the upgrade is refused cleanly instead of failing
  // with a confusing parse error.
  if crate::proxy::unix::is_unix_target(target) {
    error!("WebSocket upgrades are not supported for unix socket targets");
    send_upgrade_error(&stream_id, &tunnel_tx, 502).await;
    return;
  }

  let target_parsed = match url::Url::parse(target) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse local target URL for WS upgrade: {:?}", e);
      send_upgrade_error(&stream_id, &tunnel_tx, 502).await;
      return;
    }
  };

  // Build the WebSocket URL from the target
  let incoming_parsed = match url::Url::parse(&format!("http://localhost{}", uri_str)) {
    Ok(url) => url,
    Err(e) => {
      error!("Failed to parse incoming URI for WS upgrade: {:?}", e);
      send_upgrade_error(&stream_id, &tunnel_tx, 400).await;
      return;
    }
  };

  let mut ws_dest_str = target.to_string();
  // Build the full path for the WebSocket URL
  let target_path = target_parsed.path().trim_end_matches('/');
  let mut incoming_path = incoming_parsed.path().trim_start_matches('/').to_string();

  if trim_bind && let Some(ref bind) = path_bind {
    let bind_trimmed = bind.trim_matches('/');
    if incoming_path.starts_with(bind_trimmed) {
      incoming_path = incoming_path[bind_trimmed.len()..]
        .trim_start_matches('/')
        .to_string();
    }
  }

  // Reconstruct the target as a ws:// URL for tokio-tungstenite
  let ws_scheme = match target_parsed.scheme() {
    "https" => "wss",
    _ => "ws",
  };

  let combined_path = if target_path.is_empty() {
    format!("/{}", incoming_path)
  } else {
    format!("{}/{}", target_path, incoming_path)
  };

  if let Ok(mut parsed) = url::Url::parse(&ws_dest_str) {
    let _ = parsed.set_scheme(ws_scheme);
    parsed.set_path(&combined_path);
    parsed.set_query(incoming_parsed.query());
    ws_dest_str = parsed.to_string();
  }

  // Build the WebSocket request with the original headers (KEEP upgrade headers here)
  let ws_req = match ws_dest_str.clone().into_client_request() {
    Ok(mut req) => {
      // Map relevant headers from the upgrade request (keep Sec-WebSocket-*, etc.)
      for (k, v) in headers.iter() {
        let k_lower = k.to_lowercase();
        // Skip headers that tokio-tungstenite manages or that cause issues
        if k_lower == "host"
          || k_lower == "connection"
          || k_lower == "accept-encoding"
          || k_lower == "content-length"
          || k_lower == "content-type"
        {
          continue;
        }
        // Keep upgrade-related headers for the backend WS handshake
        let is_upgrade_header =
          k_lower == "upgrade" || k_lower.starts_with("sec-websocket-") || k_lower == "origin";

        if is_upgrade_header
          && let (Ok(name), Ok(val)) = (
            TungsteniteHeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
          )
        {
          req.headers_mut().insert(name, val);
        }
      }
      Ok(req)
    }
    Err(e) => Err(format!("Failed to construct WS request: {:?}", e)),
  };

  let ws_req = match ws_req {
    Ok(r) => r,
    Err(e) => {
      error!(
        "WebSocket request building error for stream {}: {}",
        stream_id, e
      );
      send_upgrade_error(&stream_id, &tunnel_tx, 400).await;
      return;
    }
  };

  // Connect with timeout
  let connect_fut = connect_async(ws_req);
  let timeout_fut = tokio::time::sleep(Duration::from_secs(client_timeout_secs));
  tokio::pin!(timeout_fut);

  let (backend_ws, _resp) = tokio::select! {
      _ = &mut timeout_fut => {
          error!("WebSocket connection to backend timed out for stream {}", stream_id);
          send_upgrade_error(&stream_id, &tunnel_tx, 504).await;
          return;
      }
      result = connect_fut => {
          match result {
              Ok(ws) => ws,
              Err(e) => {
                  error!("Failed to connect WebSocket to backend for stream {}: {:?}", stream_id, e);
                  send_upgrade_error(&stream_id, &tunnel_tx, 502).await;
                  return;
              }
          }
      }
  };

  // Send UpgradeResponse (101) to server
  let upgrade_resp = TunnelMessage::UpgradeResponse {
    id: stream_id.clone(),
    status: 101,
    headers: vec![
      ("upgrade".to_string(), "websocket".to_string()),
      ("connection".to_string(), "Upgrade".to_string()),
    ],
  };

  if let Ok(json) = serde_json::to_string(&upgrade_resp)
    && tunnel_tx.send(Message::Text(json)).await.is_err()
  {
    error!("Failed to send UpgradeResponse for stream {}", stream_id);
    return;
  }

  // Split the backend WebSocket
  let (mut backend_sender, mut backend_receiver) = backend_ws.split();

  // Channel to relay tunnel WsData → backend WS
  let (relay_tx, mut relay_rx) = mpsc::channel::<Message>(64);
  // Abort channel
  let (abort_tx, mut abort_rx) = mpsc::channel::<()>(1);

  // Register the stream
  {
    let mut streams = active_streams.lock().await;
    streams.insert(
      stream_id.clone(),
      WsStreamHandle {
        tx: relay_tx,
        abort_tx: abort_tx.clone(),
      },
    );
  }

  let tunnel_tx_clone = tunnel_tx.clone();
  let stream_id_clone = stream_id.clone();

  // Task: read from backend WS → send WsData through tunnel
  let backend_to_tunnel = tokio::spawn(async move {
    while let Some(result) = backend_receiver.next().await {
      match result {
        Ok(msg) => {
          let tunnel_msg = match msg {
            Message::Text(text) => TunnelMessage::WsData {
              stream_id: stream_id_clone.clone(),
              data: text.to_string(),
              is_text: true,
            },
            Message::Binary(data) => TunnelMessage::WsData {
              stream_id: stream_id_clone.clone(),
              data: BASE64_STANDARD.encode(&data),
              is_text: false,
            },
            Message::Close(frame) => TunnelMessage::WsClose {
              stream_id: stream_id_clone.clone(),
              code: frame.as_ref().map(|f| f.code.into()).unwrap_or(1000),
              reason: frame
                .as_ref()
                .map(|f| f.reason.to_string())
                .unwrap_or_default(),
            },
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
              // Auto-handled at transport level
              continue;
            }
          };

          if let Ok(json) = serde_json::to_string(&tunnel_msg)
            && tunnel_tx_clone.send(Message::Text(json)).await.is_err()
          {
            break;
          }
        }
        Err(e) => {
          debug!(
            "Backend WS read error for stream {}: {:?}",
            stream_id_clone, e
          );
          break;
        }
      }
    }

    // Backend WS disconnected — send WsClose to server
    let close_msg = TunnelMessage::WsClose {
      stream_id: stream_id_clone.clone(),
      code: 1000,
      reason: String::new(),
    };
    if let Ok(json) = serde_json::to_string(&close_msg) {
      let _ = tunnel_tx_clone.send(Message::Text(json)).await;
    }
  });

  // Task: read from relay channel (tunnel → backend WS) → write to backend WS
  let stream_id_writer = stream_id.clone();
  let backend_writer = tokio::spawn(async move {
    loop {
      tokio::select! {
          _ = abort_rx.recv() => {
              // Stream is being closed
              let _ = backend_sender.send(Message::Close(None)).await;
              break;
          }
          msg_opt = relay_rx.recv() => {
              match msg_opt {
                  Some(msg) => {
                      if backend_sender.send(msg).await.is_err() {
                          break;
                      }
                  }
                  None => break,
              }
          }
      }
    }
    debug!("Backend WS writer closed for stream {}", stream_id_writer);
  });

  let backend_to_tunnel_abort = backend_to_tunnel.abort_handle();
  let backend_writer_abort = backend_writer.abort_handle();

  // Wait for either task to finish; abort the other
  tokio::select! {
      _ = backend_to_tunnel => {
          backend_writer_abort.abort();
      }
      _ = backend_writer => {
          backend_to_tunnel_abort.abort();
      }
  }

  {
    let mut streams = active_streams.lock().await;
    streams.remove(&stream_id);
  }

  info!("WebSocket stream {} closed", stream_id);
}

/// Sends an error UpgradeResponse to the server.
async fn send_upgrade_error(stream_id: &str, tunnel_tx: &mpsc::Sender<Message>, status: u16) {
  let resp = TunnelMessage::UpgradeResponse {
    id: stream_id.to_string(),
    status,
    headers: vec![("content-type".to_string(), "text/plain".to_string())],
  };
  if let Ok(json) = serde_json::to_string(&resp) {
    let _ = tunnel_tx.send(Message::Text(json)).await;
  }
}
