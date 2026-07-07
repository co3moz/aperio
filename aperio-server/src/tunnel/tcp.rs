use axum::{
  Json,
  extract::{
    ConnectInfo, Query, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::auth::authorize_tunnel_token;
use crate::protocol::TunnelMessage;
use crate::routing::extract_client_ip;
use crate::state::{AppState, ClientPerms, TcpConsumerMsg, TcpStreamHandle};

/// May a consumer authenticated as `consumer` bind the tunnels of a client
/// that connected as `owner`? The master token may bind any client's
/// tunnels; a dynamic token only those of clients using the very same
/// token. Client ids are always required — there is no listing.
fn same_token(consumer: &ClientPerms, owner: &ClientPerms) -> bool {
  consumer.master || (consumer.token_id.is_some() && consumer.token_id == owner.token_id)
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;

/// TCP tunneling endpoint (`GET /aperio/tcp`, WebSocket). Binary WebSocket
/// frames = raw TCP bytes.
///
/// With `?client=<id>&target=<host:port>` the stream is relayed to that
/// specific client's declared tunnel target (`tunnels:` list) — requires
/// the same token the client connected with (master token excepted).
/// Without parameters the legacy behavior applies: any TCP-enabled client's
/// configured `tcp_target`.
pub(crate) async fn tcp_ws_handler(
  ws: WebSocketUpgrade,
  headers: HeaderMap,
  Query(params): Query<HashMap<String, String>>,
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
  let Some(perms) = authorize_tunnel_token(&state, &headers, caller_ip).await else {
    info!("Unauthorized TCP tunnel attempt blocked.");
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  };

  let requested_client = params
    .get("client")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
  let requested_target = params
    .get("target")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

  // Select the serving client and (for declared tunnels) the target.
  let (client_id, client_tx, target) = match requested_client {
    Some(ref id) => {
      let Some(target) = requested_target else {
        return (
          StatusCode::BAD_REQUEST,
          "The client parameter requires a target parameter",
        )
          .into_response();
      };
      let clients = state.clients.lock().await;
      // The id may be the server-side connection id or the client's
      // self-reported instance id (the one shown in the client's own logs).
      let found = clients
        .iter()
        .find(|(cid, c)| *cid == id || c.reported_instance_id.as_deref() == Some(id));
      let Some((cid, c)) = found else {
        return (StatusCode::NOT_FOUND, "No such client connected").into_response();
      };
      if !same_token(&perms, &c.perms) {
        info!(
          "Tunnel bind for client {} rejected: token mismatch (binding requires the same token the client connected with)",
          id
        );
        return (
          StatusCode::FORBIDDEN,
          "Tunnel binding requires the same token the client connected with",
        )
          .into_response();
      }
      if !c.admin_enabled || c.draining || !c.is_healthy(state.config().client_down_threshold) {
        return (StatusCode::SERVICE_UNAVAILABLE, "Client is not available").into_response();
      }
      if !c
        .tunnels
        .iter()
        .any(|d| d.target == target && d.protocol == "tcp")
      {
        return (
          StatusCode::NOT_FOUND,
          "The client does not declare this tunnel target",
        )
          .into_response();
      }
      (cid.clone(), c.tx.clone(), Some(target))
    }
    None => {
      // Legacy mode: any TCP-capable, eligible client.
      let clients = state.clients.lock().await;
      let found = clients
        .iter()
        .find(|(_, c)| {
          c.tcp_enabled
            && c.admin_enabled
            && !c.draining
            && c.is_healthy(state.config().client_down_threshold)
        })
        .map(|(id, c)| (id.clone(), c.tx.clone()));
      let Some((id, tx)) = found else {
        return (
          StatusCode::SERVICE_UNAVAILABLE,
          "No TCP-capable tunnel client connected",
        )
          .into_response();
      };
      (id, tx, None)
    }
  };

  state
    .audit(
      "tcp_stream_opened",
      &caller_ip.to_string(),
      &format!(
        "client={}{}",
        client_id,
        target
          .as_deref()
          .map(|t| format!(" target={}", t))
          .unwrap_or_default()
      ),
    )
    .await;

  ws.on_upgrade(move |socket| relay_tcp_consumer(state, socket, client_id, client_tx, target))
}

/// Tunnel discovery endpoint (`GET /aperio/tunnels/:client_id`): returns the
/// tunnels a connected client declared, for `--bind-tunnels` consumers. Same
/// authorization rule as the stream endpoint: the same token the client
/// connected with (or the master token), and the explicit client id.
pub(crate) async fn tunnels_list_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
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
  let Some(perms) = authorize_tunnel_token(&state, &headers, caller_ip).await else {
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  };

  let id = client_id.trim();
  let clients = state.clients.lock().await;
  let found = clients
    .iter()
    .find(|(cid, c)| *cid == id || c.reported_instance_id.as_deref() == Some(id));
  let Some((_, c)) = found else {
    return (StatusCode::NOT_FOUND, "No such client connected").into_response();
  };
  if !same_token(&perms, &c.perms) {
    return (
      StatusCode::FORBIDDEN,
      "Tunnel binding requires the same token the client connected with",
    )
      .into_response();
  }
  Json(c.tunnels.clone()).into_response()
}

/// Relays bytes between a public TCP consumer WebSocket and the tunnel.
/// `target` names a declared tunnel of the client (None = its legacy
/// `tcp_target`).
async fn relay_tcp_consumer(
  state: Arc<AppState>,
  consumer_ws: WebSocket,
  client_id: String,
  client_tx: mpsc::Sender<Message>,
  target: Option<String>,
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
    target,
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
