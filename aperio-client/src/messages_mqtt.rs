//! The MQTT face of client-to-client messaging.
//!
//! An application on this machine connects with the MQTT library it already
//! has, subscribes as usual, and the client translates that into a
//! subscription over the tunnel. MQTT exists only between the application and
//! this process: nothing on the wire to the server speaks it, and the server
//! never learns it was involved.
//!
//! That is the whole reason to carry the protocol at all. On the wire it would
//! buy a dependency and a second connection for something no user would see;
//! at the application boundary it buys a client library in every language and
//! no code to write.
//!
//! **A translator, not a broker.** A broker would fan out locally and bridge
//! upward, and a bridge means loop prevention. Here every publish goes up and
//! every delivery comes down, so a local subscriber sees its own message only
//! if the organization sends it back, which is what a subscriber elsewhere
//! would see too.
//!
//! What an MQTT client gets, answered up front rather than discovered:
//!
//! | Feature | Answer |
//! | --- | --- |
//! | QoS 0 | as asked |
//! | QoS 1, 2 | granted as 0; the tunnel is ordered and reliable, but nothing is stored for an absent subscriber, so promising more would be a lie |
//! | Retained | never stored, never delivered |
//! | Clean session | always; a session does not outlive the connection |
//! | Will | accepted in CONNECT and never published |
//! | Username/password | ignored; the tunnel's token is the credential, and the face is loopback |

use std::sync::Arc;

use bytes::BytesMut;
use mqttbytes::QoS;
use mqttbytes::v4::{
  ConnAck, ConnectReturnCode, Packet, PingResp, Publish, SubAck, SubscribeReasonCode, UnsubAck,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

use crate::pubsub::MessageBus;

/// Largest MQTT packet accepted, matching the message ceiling the rest of the
/// path enforces.
const MAX_PACKET_BYTES: usize = 512 * 1024;

/// Starts the MQTT listener. Returns once bound, so a port already in use is
/// a startup failure rather than a surprise on first connect.
pub(crate) async fn serve(addr: &str, bus: Arc<MessageBus>) -> Result<(), String> {
  let listener = TcpListener::bind(addr)
    .await
    .map_err(|e| format!("cannot listen on {addr} for the MQTT face: {e}"))?;
  let local = listener
    .local_addr()
    .map(|a| a.to_string())
    .unwrap_or_else(|_| addr.to_string());
  if listener
    .local_addr()
    .map(|a| !a.ip().is_loopback())
    .unwrap_or(false)
  {
    warn!(
      "The MQTT face is listening on {local}, which is not loopback: it has no authentication of \
       its own, so anything that can reach it can publish as this client"
    );
  }
  info!("MQTT face listening on mqtt://{local} (MQTT 3.1.1, QoS 0)");

  tokio::spawn(async move {
    loop {
      match listener.accept().await {
        Ok((stream, peer)) => {
          let bus = bus.clone();
          tokio::spawn(async move {
            if let Err(e) = session(stream, bus).await {
              debug!("MQTT session with {peer} ended: {e}");
            }
          });
        }
        Err(e) => {
          error!("MQTT face stopped accepting connections: {e}");
          return;
        }
      }
    }
  });
  Ok(())
}

/// One client connection, from CONNECT to disconnect.
async fn session(stream: TcpStream, bus: Arc<MessageBus>) -> Result<(), String> {
  let (mut reader, mut writer) = stream.into_split();
  let mut buf = BytesMut::with_capacity(4096);
  let mut chunk = [0u8; 8192];

  // MQTT says the first packet must be CONNECT; anything else is a protocol
  // error and the connection is closed without a response.
  loop {
    match mqttbytes::v4::read(&mut buf, MAX_PACKET_BYTES) {
      Ok(Packet::Connect(_)) => break,
      Ok(other) => return Err(format!("expected CONNECT, got {other:?}")),
      Err(mqttbytes::Error::InsufficientBytes(_)) => {
        let n = reader.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
          return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
      }
      Err(e) => return Err(format!("malformed CONNECT: {e}")),
    }
  }
  // `session_present: false` always: a session here lives exactly as long as
  // the connection, so claiming otherwise would have the client skip a
  // resubscribe it needs.
  write_packet(&mut writer, |b| {
    ConnAck::new(ConnectReturnCode::Success, false).write(b)
  })
  .await?;
  debug!("MQTT client connected");

  // Filters this connection asked for. Kept per connection so one
  // application's subscriptions do not silently widen another's.
  let mut filters: Vec<String> = Vec::new();
  let mut deliveries = bus.listen();

  loop {
    tokio::select! {
      // Something arrived for this process: forward it if this connection
      // asked for it.
      delivery = deliveries.recv() => {
        match delivery {
          Ok(delivery) => {
            if !filters.iter().any(|f| aperio_config::topic_matches(f, &delivery.topic)) {
              continue;
            }
            let publish = Publish::new(&delivery.topic, QoS::AtMostOnce, delivery.payload);
            write_packet(&mut writer, |b| publish.write(b)).await?;
          }
          Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            warn!("MQTT client fell behind; {n} message(s) were not delivered to it");
          }
          Err(_) => return Ok(()),
        }
      }
      // Something arrived from the application.
      read = reader.read(&mut chunk) => {
        let n = read.map_err(|e| e.to_string())?;
        if n == 0 {
          return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        loop {
          match mqttbytes::v4::read(&mut buf, MAX_PACKET_BYTES) {
            Err(mqttbytes::Error::InsufficientBytes(_)) => break,
            Err(e) => return Err(format!("malformed packet: {e}")),
            Ok(packet) => {
              if handle(packet, &bus, &mut filters, &mut writer).await? {
                return Ok(());
              }
            }
          }
        }
      }
    }
  }
}

/// Acts on one packet. Returns true when the session should end.
async fn handle(
  packet: Packet,
  bus: &Arc<MessageBus>,
  filters: &mut Vec<String>,
  writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<bool, String> {
  match packet {
    Packet::Publish(publish) => {
      // QoS 1 and 2 were granted as 0 at subscribe time and are treated the
      // same on the way up: the message goes out, and no PUBACK is owed
      // because the client was told 0.
      if let Err(why) = bus.publish(&publish.topic, &publish.payload).await {
        // There is no MQTT way to refuse one publish on a QoS 0 connection,
        // so the reason goes to the log rather than nowhere.
        warn!("MQTT publish to '{}' refused: {}", publish.topic, why);
      }
      Ok(false)
    }
    Packet::Subscribe(subscribe) => {
      let mut codes = Vec::with_capacity(subscribe.filters.len());
      for filter in &subscribe.filters {
        match aperio_config::validate_topic_filter(&filter.path) {
          Ok(()) => {
            if !filters.iter().any(|f| f == &filter.path) {
              filters.push(filter.path.clone());
            }
            // Tell the server too, if this process was not already asking.
            if bus.add_filter(&filter.path).await {
              bus.resubscribe_all().await;
            }
            // Granted as QoS 0 whatever was asked for. A library accepts the
            // downgrade — that is what the granted-QoS field is for — and
            // promising 1 would mean promising redelivery this does not do.
            codes.push(SubscribeReasonCode::Success(QoS::AtMostOnce));
          }
          Err(why) => {
            warn!("MQTT subscribe to '{}' refused: {}", filter.path, why);
            codes.push(SubscribeReasonCode::Failure);
          }
        }
      }
      let ack = SubAck::new(subscribe.pkid, codes);
      write_packet(writer, |b| ack.write(b)).await?;
      Ok(false)
    }
    Packet::Unsubscribe(unsubscribe) => {
      // Only this connection stops receiving them. The process may still hold
      // the filter for its config or for another local subscriber, and the
      // server is not told: an unsubscribe here is not authority over what
      // the whole process listens to.
      filters.retain(|f| !unsubscribe.topics.contains(f));
      let ack = UnsubAck::new(unsubscribe.pkid);
      write_packet(writer, |b| ack.write(b)).await?;
      Ok(false)
    }
    Packet::PingReq => {
      write_packet(writer, |b| PingResp.write(b)).await?;
      Ok(false)
    }
    Packet::Disconnect => Ok(true),
    other => {
      debug!("Ignoring MQTT packet this face does not answer: {other:?}");
      Ok(false)
    }
  }
}

/// Encodes a packet and writes it.
async fn write_packet<F>(
  writer: &mut tokio::net::tcp::OwnedWriteHalf,
  encode: F,
) -> Result<(), String>
where
  F: FnOnce(&mut BytesMut) -> Result<usize, mqttbytes::Error>,
{
  let mut out = BytesMut::new();
  encode(&mut out).map_err(|e| format!("cannot encode an MQTT packet: {e}"))?;
  writer.write_all(&out).await.map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "messages_mqtt_tests.rs"]
mod tests;
