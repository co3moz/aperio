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
//! | Publishing at QoS 0 or 1 | as asked. A QoS 1 PUBLISH is answered with PUBACK once the message is on its way, and travels at-least-once to the other clients |
//! | Publishing at QoS 2 | treated as 1; there is no store-and-forward here to build exactly-once on |
//! | Subscribing | granted QoS 0. Delivery to a local application over loopback does not need MQTT's own retry on top of the tunnel's |
//! | Retained | never stored, never delivered |
//! | Clean session | always; a session does not outlive the connection |
//! | Will | accepted in CONNECT and never published |
//! | Username/password | ignored; the tunnel's token is the credential, and the face is loopback |

use std::sync::Arc;

use bytes::BytesMut;
use mqttbytes::QoS;
use mqttbytes::v4::{
  ConnAck, ConnectReturnCode, Packet, PingResp, PubAck, Publish, SubAck, SubscribeReasonCode,
  UnsubAck,
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
  // application's subscriptions do not silently widen another's, and given
  // back to the bus when the session ends.
  let mut filters: Vec<String> = Vec::new();
  let outcome = converse(&mut reader, &mut writer, &mut buf, &bus, &mut filters).await;
  for filter in &filters {
    if bus.release_filter(filter).await {
      bus.unsubscribe_everywhere(filter).await;
    }
  }
  outcome
}

/// The session proper, from CONNACK to disconnect. Split out so a session that
/// ends any of the ways it can still gives its filters back.
async fn converse(
  reader: &mut tokio::net::tcp::OwnedReadHalf,
  writer: &mut tokio::net::tcp::OwnedWriteHalf,
  buf: &mut BytesMut,
  bus: &Arc<MessageBus>,
  filters: &mut Vec<String>,
) -> Result<(), String> {
  let mut chunk = [0u8; 8192];
  let mut deliveries = bus.listen();

  // Whatever arrived in the same read as the CONNECT is already in the buffer
  // and would otherwise wait for the next packet to arrive before being
  // looked at. A client that writes CONNECT and SUBSCRIBE together — one
  // write, or two the kernel coalesced — would sit unsubscribed until its
  // keep-alive ping woke the parser up.
  if drain_buffered(buf, bus, filters, writer).await? {
    return Ok(());
  }

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
            write_packet(writer, |b| publish.write(b)).await?;
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
        if drain_buffered(buf, bus, filters, writer).await? {
          return Ok(());
        }
      }
    }
  }
}

/// Acts on every complete packet sitting in `buf`. Returns true when the
/// session should end.
async fn drain_buffered(
  buf: &mut BytesMut,
  bus: &Arc<MessageBus>,
  filters: &mut Vec<String>,
  writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<bool, String> {
  loop {
    match mqttbytes::v4::read(buf, MAX_PACKET_BYTES) {
      Err(mqttbytes::Error::InsufficientBytes(_)) => return Ok(false),
      Err(e) => return Err(format!("malformed packet: {e}")),
      Ok(packet) => {
        if handle(packet, bus, filters, writer).await? {
          return Ok(true);
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
      // The QoS the application asked for travels with the message: a QoS 1
      // publish is held by the server until every subscriber acknowledges it.
      let qos = match publish.qos {
        QoS::AtMostOnce => 0,
        _ => 1,
      };
      let outcome = bus.publish_at(&publish.topic, &publish.payload, qos).await;
      if let Err(why) = &outcome {
        warn!("MQTT publish to '{}' refused: {}", publish.topic, why);
      }
      // PUBACK is owed for QoS 1 and is not optional: without it a
      // well-behaved library holds the message and retries it forever, which
      // is what this face did before it understood QoS at all.
      //
      // Sent even when the publish was refused. MQTT 3.1.1 has no way to say
      // "no" to one publish, and never answering would leave the application
      // retrying a message that will be refused every time; the reason is in
      // the log, where a refusal can actually be read.
      if publish.qos == QoS::AtLeastOnce {
        let ack = PubAck::new(publish.pkid);
        write_packet(writer, |b| ack.write(b)).await?;
      }
      Ok(false)
    }
    Packet::Subscribe(subscribe) => {
      let mut codes = Vec::with_capacity(subscribe.filters.len());
      for filter in &subscribe.filters {
        match aperio_config::validate_topic_filter(&filter.path) {
          Ok(()) => {
            // One hold per connection per filter, so a client that
            // subscribes to the same thing twice does not leave a hold behind
            // when it goes.
            if !filters.iter().any(|f| f == &filter.path) {
              filters.push(filter.path.clone());
              // Tell the server too, if this process was not already asking.
              if bus.hold_filter(&filter.path).await {
                bus.resubscribe_all().await;
              }
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
      // Only this connection stops receiving them. The process may still want
      // the filter for its config or for another local subscriber, in which
      // case the server is not told: an unsubscribe here is not authority
      // over what the whole process listens to. When nothing else holds it,
      // it is given back, so the filter limit is not spent on a subscription
      // no one has any more.
      let dropped: Vec<String> = filters
        .iter()
        .filter(|f| unsubscribe.topics.contains(f))
        .cloned()
        .collect();
      filters.retain(|f| !unsubscribe.topics.contains(f));
      for filter in dropped {
        if bus.release_filter(&filter).await {
          bus.unsubscribe_everywhere(&filter).await;
        }
      }
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
