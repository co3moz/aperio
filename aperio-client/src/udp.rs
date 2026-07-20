//! UDP tunneling for declared `protocol: udp` tunnels: the declaring side
//! relays datagrams between the tunnel and its local UDP target, and the
//! consumer side (`--bind-tunnels`) exposes a local UDP socket whose peers
//! each get their own relay stream through the server.
//!
//! Everything here is best-effort, matching UDP semantics: when a hop is
//! congested datagrams are dropped rather than queued unboundedly, and idle
//! relays expire instead of lingering forever.

use base64::prelude::*;
use futures_util::{sink::SinkExt, stream::StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::{
  connect_async,
  tungstenite::{client::IntoClientRequest, http::HeaderValue, protocol::Message},
};
use tracing::{debug, error, info, warn};

use crate::protocol::TunnelMessage;

/// A relay with no datagrams in either direction for this long is torn down,
/// unless the tunnel declaration overrides it (`idle_timeout`).
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolves a declared `idle_timeout` (seconds) to the effective duration,
/// falling back to [`UDP_IDLE_TIMEOUT`].
pub(crate) fn effective_idle_timeout(declared_secs: Option<u64>) -> Duration {
  declared_secs
    .map(Duration::from_secs)
    .unwrap_or(UDP_IDLE_TIMEOUT)
}
/// Largest datagram relayed (safely above typical MTU-sized payloads).
const UDP_MAX_DATAGRAM: usize = 64 * 1024;

/// Handle to an active UDP relay on the declaring client.
pub(crate) struct UdpStreamHandle {
  /// Datagrams from the tunnel, to be sent to the local target.
  pub(crate) tx: mpsc::Sender<Vec<u8>>,
  /// Aborts the relay tasks (UdpClose from the server).
  pub(crate) abort_tx: mpsc::Sender<()>,
}

/// Declaring-side relay for one UDP stream: binds an ephemeral local socket
/// "connected" to the declared target and shuttles datagrams both ways until
/// the stream is closed or idle for `idle_timeout`.
///
/// Like the TCP path, the handle must already be registered in
/// `active_streams` before this runs; `datagram_rx` buffers datagrams that
/// arrive while the socket is still being set up.
pub(crate) async fn handle_udp_open(
  stream_id: String,
  target_addr: String,
  tunnel_tx: mpsc::Sender<Message>,
  active_streams: Arc<Mutex<HashMap<String, UdpStreamHandle>>>,
  mut datagram_rx: mpsc::Receiver<Vec<u8>>,
  mut abort_rx: mpsc::Receiver<()>,
  idle_timeout: Duration,
) {
  info!("Opening UDP relay {} to {}", stream_id, target_addr);
  let close_stream = |reason: &'static str| {
    let tunnel_tx = tunnel_tx.clone();
    let stream_id = stream_id.clone();
    async move {
      debug!("UDP relay {} closing: {}", stream_id, reason);
      let close = TunnelMessage::UdpClose { stream_id };
      if let Ok(json) = serde_json::to_string(&close) {
        let _ = tunnel_tx.send(Message::Text(json)).await;
      }
    }
  };

  // An unconnected bind + connect() scopes the socket to the target only.
  let socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
    Ok(s) => s,
    Err(e) => {
      error!("UDP bind failed for stream {}: {}", stream_id, e);
      active_streams.lock().await.remove(&stream_id);
      close_stream("local bind failed").await;
      return;
    }
  };
  if let Err(e) = socket.connect(&target_addr).await {
    error!(
      "UDP connect to {} failed for stream {}: {}",
      target_addr, stream_id, e
    );
    active_streams.lock().await.remove(&stream_id);
    close_stream("target unreachable").await;
    return;
  }

  let mut buf = vec![0u8; UDP_MAX_DATAGRAM];
  loop {
    tokio::select! {
      _ = abort_rx.recv() => break,
      _ = tokio::time::sleep(idle_timeout) => {
        debug!("UDP relay {} idle; expiring", stream_id);
        break;
      }
      out = datagram_rx.recv() => match out {
        Some(bytes) => {
          // send() may fail transiently (e.g. ICMP unreachable); UDP is
          // lossy by contract, so log and carry on.
          if let Err(e) = socket.send(&bytes).await {
            debug!("UDP send to {} failed: {}", target_addr, e);
          }
        }
        None => break,
      },
      recv = socket.recv(&mut buf) => match recv {
        Ok(n) => {
          let msg = TunnelMessage::UdpDatagram {
            stream_id: stream_id.clone(),
            data: BASE64_STANDARD.encode(&buf[..n]),
          };
          let Ok(json) = serde_json::to_string(&msg) else { continue };
          // Best-effort: drop the datagram when the tunnel is congested.
          if let Err(mpsc::error::TrySendError::Closed(_)) =
            tunnel_tx.try_send(Message::Text(json))
          {
            break;
          }
        }
        Err(e) => {
          debug!("UDP recv error for stream {}: {}", stream_id, e);
        }
      },
    }
  }

  active_streams.lock().await.remove(&stream_id);
  close_stream("relay ended").await;
  info!("UDP relay {} closed", stream_id);
}

/// Consumer-side UDP binder: listens on 127.0.0.1:`port` and gives every
/// distinct local peer its own relay stream to the server's `/aperio/udp`
/// endpoint (one WebSocket per peer, one binary frame per datagram), so
/// responses find their way back to the right peer. Sessions expire after
/// `idle_timeout` without traffic.
pub(crate) async fn run_udp_bind(port: u16, ws_url: String, token: String, idle_timeout: Duration) {
  let socket = match tokio::net::UdpSocket::bind(("127.0.0.1", port)).await {
    Ok(s) => Arc::new(s),
    Err(e) => {
      error!("Failed to bind UDP 127.0.0.1:{}: {} — not binding", port, e);
      return;
    }
  };

  // peer address → sender feeding that peer's relay stream.
  let sessions: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>> =
    Arc::new(Mutex::new(HashMap::new()));

  let mut buf = vec![0u8; UDP_MAX_DATAGRAM];
  loop {
    let (n, peer) = match socket.recv_from(&mut buf).await {
      Ok(x) => x,
      Err(e) => {
        error!("UDP accept error on port {}: {}", port, e);
        tokio::time::sleep(Duration::from_secs(1)).await;
        continue;
      }
    };
    let datagram = buf[..n].to_vec();

    let tx = {
      let mut map = sessions.lock().await;
      match map.get(&peer) {
        Some(tx) => tx.clone(),
        None => {
          let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
          map.insert(peer, tx.clone());
          info!("UDP session from {} -> {}", peer, ws_url);
          let (socket, sessions) = (socket.clone(), sessions.clone());
          let (ws_url, token) = (ws_url.clone(), token.clone());
          tokio::spawn(async move {
            bridge_udp_session(&ws_url, &token, socket, peer, rx, idle_timeout).await;
            sessions.lock().await.remove(&peer);
            debug!("UDP session from {} ended", peer);
          });
          tx
        }
      }
    };
    // Best-effort: drop when the session's relay is congested. A closed
    // channel means the session just ended; the datagram is lost, and the
    // peer's next datagram opens a fresh session.
    let _ = tx.try_send(datagram);
  }
}

/// Relays one consumer UDP session over a fresh WebSocket stream to the
/// server's `/aperio/udp` endpoint. Returns when the stream closes or the
/// session idles out.
async fn bridge_udp_session(
  ws_url: &str,
  token: &str,
  socket: Arc<tokio::net::UdpSocket>,
  peer: SocketAddr,
  mut rx: mpsc::Receiver<Vec<u8>>,
  idle_timeout: Duration,
) {
  let mut req = match ws_url.to_string().into_client_request() {
    Ok(r) => r,
    Err(e) => {
      error!("UDP bridge request build error: {:?}", e);
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
      warn!("UDP bridge failed to reach server: {:?}", e);
      return;
    }
  };
  let (mut ws_tx, mut ws_rx) = ws.split();

  loop {
    tokio::select! {
      _ = tokio::time::sleep(idle_timeout) => break,
      out = rx.recv() => match out {
        Some(bytes) => {
          if ws_tx.send(Message::Binary(bytes)).await.is_err() {
            break;
          }
        }
        None => break,
      },
      msg = ws_rx.next() => match msg {
        Some(Ok(Message::Binary(bytes))) => {
          if let Err(e) = socket.send_to(&bytes, peer).await {
            debug!("UDP send_to {} failed: {}", peer, e);
          }
        }
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
        _ => {}
      },
    }
  }
  let _ = ws_tx.send(Message::Close(None)).await;
}

#[cfg(test)]
#[path = "udp_tests.rs"]
mod tests;
