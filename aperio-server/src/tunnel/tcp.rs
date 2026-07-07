use axum::{
  extract::{
    ConnectInfo, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::auth::authorize_tunnel_token;
use crate::protocol::TunnelMessage;
use crate::routing::extract_client_ip;
use crate::state::{AppState, TcpConsumerMsg, TcpStreamHandle};

/// Experimental TCP tunneling endpoint (`GET /aperio/tcp`, WebSocket).
/// Consumers authenticate with a tunnel token (master or dynamic) and get a
/// raw byte relay to the TCP target configured on a TCP-enabled client.
/// Binary WebSocket frames = raw TCP bytes.
pub(crate) async fn tcp_ws_handler(
  ws: WebSocketUpgrade,
  headers: HeaderMap,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  State(state): State<Arc<AppState>>,
) -> Response {
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
  );
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  if authorize_tunnel_token(&state, &headers, caller_ip)
    .await
    .is_none()
  {
    info!("Unauthorized TCP tunnel attempt blocked.");
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  }

  // Select a TCP-capable, eligible client.
  let client_info = {
    let clients = state.clients.lock().await;
    clients
      .iter()
      .find(|(_, c)| {
        c.tcp_enabled
          && c.admin_enabled
          && !c.draining
          && c.is_healthy(state.config().client_down_threshold)
      })
      .map(|(id, c)| (id.clone(), c.tx.clone()))
  };
  let Some((client_id, client_tx)) = client_info else {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      "No TCP-capable tunnel client connected",
    )
      .into_response();
  };

  state
    .audit(
      "tcp_stream_opened",
      &caller_ip.to_string(),
      &format!("client={}", client_id),
    )
    .await;

  ws.on_upgrade(move |socket| relay_tcp_consumer(state, socket, client_id, client_tx))
}

/// Relays bytes between a public TCP consumer WebSocket and the tunnel.
async fn relay_tcp_consumer(
  state: Arc<AppState>,
  consumer_ws: WebSocket,
  client_id: String,
  client_tx: mpsc::Sender<Message>,
) {
  let stream_id = uuid::Uuid::new_v4().to_string();
  let (relay_tx, mut relay_rx) = mpsc::channel::<TcpConsumerMsg>(64);
  state.tcp_streams.lock().await.insert(
    stream_id.clone(),
    TcpStreamHandle {
      tx: relay_tx,
      client_id: client_id.clone(),
    },
  );

  // Ask the client to open its TCP target.
  let open = TunnelMessage::TcpOpen {
    stream_id: stream_id.clone(),
  };
  if let Ok(json) = serde_json::to_string(&open)
    && client_tx.send(Message::Text(json)).await.is_err()
  {
    state.tcp_streams.lock().await.remove(&stream_id);
    return;
  }

  let (mut ws_sender, mut ws_receiver) = consumer_ws.split();

  // Consumer → tunnel
  let stream_id_up = stream_id.clone();
  let client_tx_up = client_tx.clone();
  let up_task = tokio::spawn(async move {
    use base64::prelude::*;
    while let Some(Ok(msg)) = ws_receiver.next().await {
      let bytes = match msg {
        Message::Binary(b) => b,
        Message::Text(t) => t.into_bytes(),
        Message::Close(_) => break,
        _ => continue,
      };
      let data_msg = TunnelMessage::TcpData {
        stream_id: stream_id_up.clone(),
        data: BASE64_STANDARD.encode(&bytes),
      };
      if let Ok(json) = serde_json::to_string(&data_msg)
        && client_tx_up.send(Message::Text(json)).await.is_err()
      {
        break;
      }
    }
    // Consumer went away → close the client side.
    let close = TunnelMessage::TcpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = client_tx_up.send(Message::Text(json)).await;
    }
  });

  // Tunnel → consumer
  let down_task = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        TcpConsumerMsg::Data(bytes) => {
          if ws_sender.send(Message::Binary(bytes)).await.is_err() {
            break;
          }
        }
        TcpConsumerMsg::Close => {
          let _ = ws_sender.send(Message::Close(None)).await;
          break;
        }
      }
    }
  });

  let up_abort = up_task.abort_handle();
  let down_abort = down_task.abort_handle();
  tokio::select! {
    _ = up_task => down_abort.abort(),
    _ = down_task => up_abort.abort(),
  }

  state.tcp_streams.lock().await.remove(&stream_id);
  debug!("TCP tunnel stream {} closed", stream_id);
}
