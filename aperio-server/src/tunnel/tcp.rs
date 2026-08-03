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
use crate::state::{AppState, TcpConsumerMsg, TcpStreamHandle};
use crate::tunnel::registry;

/// Turns a resolution failure into the answer the caller gets.
///
/// The three are kept apart on purpose. A binder that is told "not connected"
/// looks for a dead client; one told "not permitted" looks at its token; one
/// told "no path available" waits. Collapsing them, as the previous code did,
/// sent every one of those operators down the first road.
fn reject(rejection: registry::Rejection, what: &str) -> (StatusCode, String) {
  match rejection {
    registry::Rejection::Unknown => (
      StatusCode::NOT_FOUND,
      format!("No connected client declares a tunnel matching '{what}'"),
    ),
    registry::Rejection::Forbidden => {
      info!(
        "Tunnel bind for '{}' rejected: not permitted for this token",
        what
      );
      (
        StatusCode::FORBIDDEN,
        "Binding needs the declaring client's own token, or one in its organization with allow_bind"
          .to_string(),
      )
    }
    registry::Rejection::Unavailable => (
      StatusCode::SERVICE_UNAVAILABLE,
      format!("The client declaring '{what}' is not available"),
    ),
  }
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;

/// TCP tunneling endpoint (`GET /aperio/tcp`, WebSocket). Binary WebSocket
/// frames = raw TCP bytes.
///
/// With `?client=<id>&target=<host:port>` the stream is relayed to that
/// specific client's declared tunnel target (`tunnels:` list), requires
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
    &state.config().trusted_proxies,
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
  let requested_name = params
    .get("tunnel")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

  // Select the serving client and (for declared tunnels) the target.
  let (client_id, client_tx, target) = match (&requested_name, &requested_client) {
    // `?tunnel=<name>`: the organization-wide handle, no client id needed.
    (Some(name), _) => {
      match registry::resolve(&state, &perms, registry::Selector::Name(name)).await {
        Ok(found) if aperio_config::protocol_serves(&found.decl.protocol, "tcp") => {
          let target = found.decl.target.clone();
          (found.client_id, found.tx, Some(target))
        }
        Ok(_) => {
          return (
            StatusCode::BAD_REQUEST,
            "That tunnel does not serve tcp; use the udp endpoint",
          )
            .into_response();
        }
        Err(rejection) => return reject(rejection, name).into_response(),
      }
    }
    // `?client=<id>&target=<host:port>`: the original addressing.
    (None, Some(id)) => {
      let Some(target) = requested_target else {
        return (
          StatusCode::BAD_REQUEST,
          "The client parameter requires a target parameter",
        )
          .into_response();
      };
      let selector = registry::Selector::ClientTarget {
        client: id,
        target: &target,
        protocol: "tcp",
      };
      match registry::resolve(&state, &perms, selector).await {
        Ok(found) => (found.client_id, found.tx, Some(target)),
        Err(rejection) => return reject(rejection, id).into_response(),
      }
    }
    (None, None) => {
      // Legacy mode: any TCP-capable, eligible client this caller may reach.
      //
      // "Any" used to mean any at all. Every other way into this handler is
      // fenced by `may_bind`, and this one was not, so a token of one
      // organization could open a raw socket into another organization's
      // declared target simply by naming nothing.
      let clients = state.clients.read().await;
      let found = clients
        .iter()
        .find(|(_, c)| {
          c.tcp_enabled
            && c.admin_enabled
            && !c.draining
            && c.is_healthy(state.config().client_down_threshold)
            && registry::may_bind(&perms, &c.perms)
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
      "system",
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

  // The dependency this connection represents, recorded before the upgrade so
  // it is on the graph even for a connection that fails immediately: a
  // consumer that cannot get through is exactly the one an operator is
  // looking for.
  let edge = ConsumerEdge {
    from: caller_ip,
    to_client: client_id.clone(),
    tunnel: requested_name.clone(),
    token_name: perms
      .token_name
      .clone()
      .unwrap_or_else(|| "master".to_string()),
  };
  state.consumers.lock().await.opened(
    edge.from,
    &edge.to_client,
    edge.tunnel.as_deref(),
    &edge.token_name,
    crate::store::tokens::now_secs(),
  );

  let peer_addr = std::net::SocketAddr::new(caller_ip, addr.port());
  let log = RelayIdentity::new(peer_addr, requested_name.clone(), &perms);
  ws.on_upgrade(move |socket| async move {
    relay_tcp_consumer(
      state.clone(),
      socket,
      client_id,
      client_tx,
      target,
      Some(peer_addr),
      log,
    )
    .await;
    state.consumers.lock().await.closed(
      edge.from,
      &edge.to_client,
      edge.tunnel.as_deref(),
      &edge.token_name,
      crate::store::tokens::now_secs(),
    );
  })
}

/// One consumer connection's identity, carried into the relay so the edge can
/// be closed with exactly the key it was opened with.
struct ConsumerEdge {
  from: std::net::IpAddr,
  to_client: String,
  tunnel: Option<String>,
  token_name: String,
}

/// UDP tunneling endpoint (`GET /aperio/udp`, WebSocket). Each binary
/// WebSocket frame = one UDP datagram, relayed best-effort to a specific
/// client's declared `protocol: udp` tunnel target. Unlike `/aperio/tcp`
/// there is no legacy parameterless mode: `?client=<id>&target=<host:port>`
/// is required, with the same same-token authorization rule.
pub(crate) async fn udp_ws_handler(
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
    &state.config().trusted_proxies,
  );
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let Some(perms) = authorize_tunnel_token(&state, &headers, caller_ip).await else {
    info!("Unauthorized UDP tunnel attempt blocked.");
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  };

  let requested_name = params
    .get("tunnel")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
  let requested_client = params
    .get("client")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
  let requested_target = params
    .get("target")
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

  let (client_id, client_tx, target) = match (&requested_name, &requested_client, &requested_target)
  {
    (Some(name), _, _) => {
      match registry::resolve(&state, &perms, registry::Selector::Name(name)).await {
        Ok(found) if aperio_config::protocol_serves(&found.decl.protocol, "udp") => {
          let target = found.decl.target.clone();
          (found.client_id, found.tx, target)
        }
        Ok(_) => {
          return (
            StatusCode::BAD_REQUEST,
            "That tunnel does not serve udp; use the tcp endpoint",
          )
            .into_response();
        }
        Err(rejection) => return reject(rejection, name).into_response(),
      }
    }
    (None, Some(id), Some(target)) => {
      let selector = registry::Selector::ClientTarget {
        client: id,
        target,
        protocol: "udp",
      };
      match registry::resolve(&state, &perms, selector).await {
        Ok(found) => (found.client_id, found.tx, target.clone()),
        Err(rejection) => return reject(rejection, id).into_response(),
      }
    }
    _ => {
      return (
        StatusCode::BAD_REQUEST,
        "UDP tunneling requires a tunnel parameter, or client and target parameters",
      )
        .into_response();
    }
  };

  state
    .audit(
      "udp_stream_opened",
      "system",
      &caller_ip.to_string(),
      &format!("client={} target={}", client_id, target),
    )
    .await;

  let peer_addr = std::net::SocketAddr::new(caller_ip, addr.port());
  let log = RelayIdentity::new(peer_addr, requested_name.clone(), &perms);
  ws.on_upgrade(move |socket| relay_udp_consumer(state, socket, client_id, client_tx, target, log))
}

/// What the relay access log needs to know about a connection, gathered at
/// the handler where the token and the query are still in hand.
///
/// A struct rather than three more parameters: these three always travel
/// together and always come from the same place, and threading them
/// separately through two relays made both signatures about their logging.
struct RelayIdentity {
  peer: std::net::SocketAddr,
  tunnel: Option<String>,
  token: Option<String>,
}

impl RelayIdentity {
  fn new(
    peer: std::net::SocketAddr,
    tunnel: Option<String>,
    perms: &crate::state::ClientPerms,
  ) -> Self {
    RelayIdentity {
      peer,
      tunnel,
      token: Some(
        perms
          .token_name
          .clone()
          .unwrap_or_else(|| "master".to_string()),
      ),
    }
  }
}

/// Relays datagrams between a consumer WebSocket (one binary frame = one
/// datagram) and the declaring client's tunnel. Best-effort: a full channel
/// drops datagrams instead of applying backpressure.
async fn relay_udp_consumer(
  state: Arc<AppState>,
  consumer_ws: WebSocket,
  client_id: String,
  client_tx: mpsc::Sender<Message>,
  target: String,
  // For the relay access log.
  log: RelayIdentity,
) {
  let stream_id = uuid::Uuid::new_v4().to_string();
  // Read once per stream: the announcement precedes routability, so the
  // client's protocol cannot change mid-stream in a way that matters.
  let protocol = state.client_protocol(&client_id).await;
  let (relay_tx, mut relay_rx) = mpsc::channel::<TcpConsumerMsg>(64);
  state.udp_streams.lock().await.insert(
    stream_id.clone(),
    crate::state::UdpStreamHandle {
      tx: relay_tx,
      client_id: client_id.clone(),
    },
  );

  let open = TunnelMessage::UdpOpen {
    stream_id: stream_id.clone(),
    target,
  };
  if let Ok(json) = serde_json::to_string(&open)
    && client_tx.send(Message::Text(json.into())).await.is_err()
  {
    state.udp_streams.lock().await.remove(&stream_id);
    return;
  }

  let (mut ws_sender, mut ws_receiver) = consumer_ws.split();

  // Consumer → tunnel (each frame is one datagram)
  let stream_id_up = stream_id.clone();
  let client_tx_up = client_tx.clone();
  let record = crate::relay_log::RelayRecord::new("udp", "tunnel", log.peer.to_string(), client_id)
    .tunnel(log.tunnel)
    .token(log.token);
  let up_bytes = record.up_counter();
  let down_bytes = record.down_counter();

  let up_task = tokio::spawn(async move {
    while let Some(Ok(msg)) = ws_receiver.next().await {
      let bytes = match msg {
        Message::Binary(b) => b,
        Message::Close(_) => break,
        _ => continue,
      };
      // v7 takes the datagram raw; an older client takes base64 in JSON.
      let Some(frame) = crate::protocol::relay_frame(
        protocol,
        crate::protocol::FRAME_UDP_DATAGRAM,
        &stream_id_up,
        &bytes,
        |data| TunnelMessage::UdpDatagram {
          stream_id: stream_id_up.clone(),
          data,
        },
      ) else {
        continue;
      };
      // Best-effort: drop the datagram when the tunnel is congested.
      let moved = bytes.len() as u64;
      if let Err(mpsc::error::TrySendError::Closed(_)) = client_tx_up.try_send(frame) {
        break;
      }
      up_bytes.fetch_add(moved, std::sync::atomic::Ordering::Relaxed);
    }
    let close = TunnelMessage::UdpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = client_tx_up.send(Message::Text(json.into())).await;
    }
  });

  // Tunnel → consumer
  let down_task = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        TcpConsumerMsg::Data(bytes) => {
          let moved = bytes.len() as u64;
          if ws_sender.send(Message::Binary(bytes)).await.is_err() {
            break;
          }
          down_bytes.fetch_add(moved, std::sync::atomic::Ordering::Relaxed);
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

  state.udp_streams.lock().await.remove(&stream_id);
  record.finish(&state);
  debug!("UDP tunnel stream {} closed", stream_id);
}

/// Tunnel discovery (`GET /aperio/tunnels`): every tunnel this caller may
/// bind, across the clients of its organization.
///
/// The listing is what makes a name usable as an address. Without it a binder
/// had to already know a client id to ask anything at all, which meant the
/// only way to find out what you could reach was to be told out of band.
#[utoipa::path(get, path = "/aperio/tunnels", tag = "tunnels",
  description = "Lists the tunnels the presented token may bind.",
  responses((status = 200, description = "Bindable tunnels", body = Vec<registry::TunnelView>)))]
pub(crate) async fn tunnels_discovery_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let Some(perms) = authorize_tunnel_token(&state, &headers, caller_ip).await else {
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  };
  Json(registry::visible(&state, &perms).await).into_response()
}

/// Per-client tunnel discovery (`GET /aperio/tunnels/:client_id`): the
/// tunnels one connected client declared. The original endpoint, kept for
/// binders that address a peer by id; `GET /aperio/tunnels` is the listing.
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
    &state.config().trusted_proxies,
  );
  if !state.check_rate_limit(caller_ip).await {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let Some(perms) = authorize_tunnel_token(&state, &headers, caller_ip).await else {
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  };

  let id = client_id.trim();
  let clients = state.clients.read().await;
  // Any connection of the process answers: they all announce the same list.
  // The id may be the connection id, the reported instance id, or the raw
  // `client_id` from the file, which is the one an operator actually has.
  let found = clients.iter().find(|(cid, c)| {
    *cid == id
      || c.reported_instance_id.as_deref() == Some(id)
      || c.instance_group.as_deref() == Some(id)
  });
  let Some((_, c)) = found else {
    return (StatusCode::NOT_FOUND, "No such client connected").into_response();
  };
  if !registry::may_bind(&perms, &c.perms) {
    return reject(registry::Rejection::Forbidden, id).into_response();
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
  // Address of the peer that dialled this tunnel, for `proxy_protocol:`.
  // Here the "visitor" is another client, which is the truthful answer:
  // something at that address is what the backend is really serving.
  visitor: Option<std::net::SocketAddr>,
  // For the relay access log.
  log: RelayIdentity,
) {
  let stream_id = uuid::Uuid::new_v4().to_string();
  // Read once per stream, like the pause support below.
  let protocol = state.client_protocol(&client_id).await;
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

  // Ask the client to open its TCP target.
  let open = TunnelMessage::TcpOpen {
    stream_id: stream_id.clone(),
    target,
    visitor: visitor.map(|a| a.to_string()),
  };
  if let Ok(json) = serde_json::to_string(&open)
    && client_tx.send(Message::Text(json.into())).await.is_err()
  {
    state.tcp_streams.lock().await.remove(&stream_id);
    return;
  }

  let (mut ws_sender, mut ws_receiver) = consumer_ws.split();

  // The record is built here, while what identifies this connection is still
  // in hand, and written once when the relay is over.
  let record = crate::relay_log::RelayRecord::new(
    "tcp",
    "tunnel",
    visitor.map(|a| a.to_string()).unwrap_or_default(),
    client_id.clone(),
  )
  .tunnel(log.tunnel)
  .token(log.token);
  let up_bytes = record.up_counter();
  let down_bytes = record.down_counter();

  // Consumer → tunnel
  let stream_id_up = stream_id.clone();
  let client_tx_up = client_tx.clone();
  let up_task = tokio::spawn(async move {
    while let Some(Ok(msg)) = ws_receiver.next().await {
      let bytes = match msg {
        Message::Binary(b) => b,
        Message::Text(t) => t.as_bytes().to_vec().into(),
        Message::Close(_) => break,
        _ => continue,
      };
      // v7 takes the bytes raw; an older client takes base64 in JSON.
      let Some(frame) = crate::protocol::relay_frame(
        protocol,
        crate::protocol::FRAME_TCP_DATA,
        &stream_id_up,
        &bytes,
        |data| TunnelMessage::TcpData {
          stream_id: stream_id_up.clone(),
          data,
        },
      ) else {
        break;
      };
      let moved = bytes.len() as u64;
      if client_tx_up.send(frame).await.is_err() {
        break;
      }
      up_bytes.fetch_add(moved, std::sync::atomic::Ordering::Relaxed);
    }
    // Consumer went away → close the client side.
    let close = TunnelMessage::TcpClose {
      stream_id: stream_id_up.clone(),
    };
    if let Ok(json) = serde_json::to_string(&close) {
      let _ = client_tx_up.send(Message::Text(json.into())).await;
    }
  });

  // Tunnel → consumer
  let down_task = tokio::spawn(async move {
    while let Some(msg) = relay_rx.recv().await {
      match msg {
        TcpConsumerMsg::Data(bytes) => {
          let moved = bytes.len() as u64;
          if ws_sender.send(Message::Binary(bytes)).await.is_err() {
            break;
          }
          down_bytes.fetch_add(moved, std::sync::atomic::Ordering::Relaxed);
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
  record.finish(&state);
  debug!("TCP tunnel stream {} closed", stream_id);
}
