//! Raw relay traffic on a tunnel connection: TCP bytes and UDP datagrams in,
//! and the close that ends a stream.

use tracing::{debug, warn};

use super::*;
use crate::protocol::TunnelMessage;

impl ConnCtx {
  /// Handles bytes of a TCP relay stream.
  pub(super) async fn on_tcp_data(&self, msg: TunnelMessage) {
    let TunnelMessage::TcpData { stream_id, data } = msg else {
      return;
    };
    // Base64 fallback path; a v7 client sends FRAME_TCP_DATA binary frames.
    use base64::prelude::*;
    match BASE64_STANDARD.decode(&data) {
      Ok(bytes) => self.deliver_tcp_bytes(&stream_id, bytes.into()).await,
      Err(_) => {
        warn!("Failed to decode Base64 TcpData for stream {}", stream_id);
      }
    }
  }

  /// Delivers one chunk of a TCP relay this client owns, however it arrived
  /// (base64 in JSON, or a v7 binary frame).
  pub(super) async fn deliver_tcp_bytes(&self, stream_id: &str, bytes: axum::body::Bytes) {
    let state = &self.state;
    let client_id = &self.client_id;
    let consumer_tx = {
      let streams = state.tcp_streams.lock().await;
      match streams.get(stream_id) {
        Some(h) if h.client_id == *client_id => Some(h.tx.clone()),
        Some(_) => {
          warn!(
            "TcpData for stream {} rejected: not owned by client {}",
            stream_id, client_id
          );
          None
        }
        None => None,
      }
    };
    if let Some(consumer_tx) = consumer_tx {
      // Non-blocking, like the HTTP chunk path: the stream's pump waits on a
      // slow consumer so this loop does not, and its flow control pauses the
      // producer if need be.
      if let Err(e) = consumer_tx.push(TcpConsumerMsg::Data(bytes)) {
        debug!("Dropping TCP stream {} ({:?})", stream_id, e);
        state.tcp_streams.lock().await.remove(stream_id);
      }
    }
  }

  pub(super) async fn on_tcp_close(&self, msg: TunnelMessage) {
    let TunnelMessage::TcpClose { stream_id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    if let Some(h) =
      take_owned_stream(&state.tcp_streams, &stream_id, client_id, |h| &h.client_id).await
    {
      let _ = h.tx.push(TcpConsumerMsg::Close);
    }
  }

  /// Handles one datagram of a UDP relay.
  pub(super) async fn on_udp_datagram(&self, msg: TunnelMessage) {
    let TunnelMessage::UdpDatagram { stream_id, data } = msg else {
      return;
    };
    // Base64 fallback path; a v7 client sends FRAME_UDP_DATAGRAM frames.
    use base64::prelude::*;
    match BASE64_STANDARD.decode(&data) {
      Ok(bytes) => self.deliver_udp_bytes(&stream_id, bytes.into()).await,
      Err(_) => {
        warn!(
          "Failed to decode Base64 UdpDatagram for stream {}",
          stream_id
        );
      }
    }
  }

  /// Delivers one relayed datagram this client owns, however it arrived.
  pub(super) async fn deliver_udp_bytes(&self, stream_id: &str, bytes: axum::body::Bytes) {
    let state = &self.state;
    let client_id = &self.client_id;
    let consumer_tx = {
      let streams = state.udp_streams.lock().await;
      match streams.get(stream_id) {
        Some(h) if h.client_id == *client_id => Some(h.tx.clone()),
        Some(_) => {
          warn!(
            "UdpDatagram for stream {} rejected: not owned by client {}",
            stream_id, client_id
          );
          None
        }
        None => None,
      }
    };
    if let Some(consumer_tx) = consumer_tx {
      // Best-effort: a congested consumer drops datagrams.
      if let Err(mpsc::error::TrySendError::Closed(_)) =
        consumer_tx.try_send(TcpConsumerMsg::Data(bytes))
      {
        state.udp_streams.lock().await.remove(stream_id);
      }
    }
  }

  pub(super) async fn on_udp_close(&self, msg: TunnelMessage) {
    let TunnelMessage::UdpClose { stream_id } = msg else {
      return;
    };
    let state = &self.state;
    let client_id = &self.client_id;
    let client_ip = &self.client_ip;
    let perms = &self.perms;
    let tx_write = &self.tx_write;
    let server_max_connections = self.server_max_connections;
    let _ = (client_ip, perms, tx_write, server_max_connections);

    if let Some(h) =
      take_owned_stream(&state.udp_streams, &stream_id, client_id, |h| &h.client_id).await
    {
      let _ = h.tx.try_send(TcpConsumerMsg::Close);
    }
  }
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
