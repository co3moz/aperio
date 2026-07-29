//! Tests for the MQTT face.
//!
//! Driven with real MQTT packets rather than a mock, since the point of the
//! face is that an ordinary MQTT library works against it.

use super::*;
use crate::pubsub::{Delivery, MessageBus};
use mqttbytes::v4::{Connect, Subscribe, Unsubscribe};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

async fn start(bus: Arc<MessageBus>) -> String {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap().to_string();
  drop(listener);
  serve(&addr, bus).await.unwrap();
  addr
}

/// A connected client: CONNECT sent, CONNACK read.
async fn connect(addr: &str) -> TcpStream {
  let mut stream = TcpStream::connect(addr).await.unwrap();
  let mut out = BytesMut::new();
  Connect::new("test-client").write(&mut out).unwrap();
  stream.write_all(&out).await.unwrap();
  match read_packet(&mut stream).await {
    Some(Packet::ConnAck(ack)) => {
      assert_eq!(ack.code, ConnectReturnCode::Success);
      assert!(
        !ack.session_present,
        "a session never outlives a connection"
      );
    }
    other => panic!("expected CONNACK, got {other:?}"),
  }
  stream
}

/// Reads one packet, or None if nothing arrives in time.
async fn read_packet(stream: &mut TcpStream) -> Option<Packet> {
  let mut buf = BytesMut::new();
  let mut chunk = [0u8; 4096];
  loop {
    if let Ok(packet) = mqttbytes::v4::read(&mut buf, MAX_PACKET_BYTES) {
      return Some(packet);
    }
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut chunk))
      .await
      .ok()?
      .ok()?;
    if n == 0 {
      return None;
    }
    buf.extend_from_slice(&chunk[..n]);
  }
}

async fn send(stream: &mut TcpStream, encode: impl FnOnce(&mut BytesMut)) {
  let mut out = BytesMut::new();
  encode(&mut out);
  stream.write_all(&out).await.unwrap();
}

#[tokio::test]
async fn an_mqtt_client_subscribes_and_receives() {
  let bus = MessageBus::new(vec![]);
  let addr = start(bus.clone()).await;
  let mut client = connect(&addr).await;

  send(&mut client, |b| {
    Subscribe::new("deploy/#", QoS::AtMostOnce)
      .write(b)
      .unwrap();
  })
  .await;
  match read_packet(&mut client).await {
    Some(Packet::SubAck(ack)) => assert_eq!(
      ack.return_codes,
      vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]
    ),
    other => panic!("expected SUBACK, got {other:?}"),
  }
  // Subscribing over MQTT is enough: the process now asks the server for it.
  assert!(bus.wants("deploy/web").await);

  bus.deliver(Delivery {
    topic: "deploy/web".to_string(),
    payload: b"go".to_vec(),
    id: Some("m-1".to_string()),
  });
  match read_packet(&mut client).await {
    Some(Packet::Publish(p)) => {
      assert_eq!(p.topic, "deploy/web");
      assert_eq!(&p.payload[..], b"go");
      assert_eq!(p.qos, QoS::AtMostOnce);
      assert!(!p.retain, "nothing is ever delivered as retained");
    }
    other => panic!("expected PUBLISH, got {other:?}"),
  }
}

#[tokio::test]
async fn a_higher_qos_is_granted_as_zero_rather_than_refused() {
  // A library accepts the downgrade — that is what the granted-QoS field is
  // for — and promising 1 would mean promising a redelivery this does not do.
  let bus = MessageBus::new(vec![]);
  let addr = start(bus).await;
  let mut client = connect(&addr).await;

  send(&mut client, |b| {
    Subscribe::new("deploy/#", QoS::AtLeastOnce)
      .write(b)
      .unwrap();
  })
  .await;
  match read_packet(&mut client).await {
    Some(Packet::SubAck(ack)) => assert_eq!(
      ack.return_codes,
      vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]
    ),
    other => panic!("expected SUBACK, got {other:?}"),
  }
}

#[tokio::test]
async fn an_unusable_filter_is_refused_in_the_suback() {
  // `deploy/#/eu` matches nothing and looks like it works. Saying Failure for
  // that one filter, and Success for the other, is what SUBACK is for.
  let bus = MessageBus::new(vec![]);
  let addr = start(bus).await;
  let mut client = connect(&addr).await;

  send(&mut client, |b| {
    let mut sub = Subscribe::new("deploy/web", QoS::AtMostOnce);
    sub.add("deploy/#/eu".to_string(), QoS::AtMostOnce);
    sub.write(b).unwrap();
  })
  .await;
  match read_packet(&mut client).await {
    Some(Packet::SubAck(ack)) => assert_eq!(
      ack.return_codes,
      vec![
        SubscribeReasonCode::Success(QoS::AtMostOnce),
        SubscribeReasonCode::Failure
      ]
    ),
    other => panic!("expected SUBACK, got {other:?}"),
  }
}

#[tokio::test]
async fn a_publish_goes_out_over_the_tunnel() {
  let bus = MessageBus::new(vec![]);
  let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(4);
  bus.attach("svc", tx).await;
  let addr = start(bus).await;
  let mut client = connect(&addr).await;

  send(&mut client, |b| {
    Publish::new("deploy/web", QoS::AtMostOnce, "v2")
      .write(b)
      .unwrap();
  })
  .await;

  let Message::Text(json) = rx.recv().await.unwrap() else {
    panic!("expected a text frame")
  };
  let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
  assert_eq!(parsed["type"], "Publish");
  assert_eq!(parsed["topic"], "deploy/web");
  use base64::prelude::*;
  assert_eq!(
    BASE64_STANDARD
      .decode(parsed["payload"].as_str().unwrap())
      .unwrap(),
    b"v2"
  );
}

#[tokio::test]
async fn a_delivery_only_reaches_the_connections_that_asked_for_it() {
  // Two applications on one machine: an unsubscribe by one must not take the
  // other's messages away.
  let bus = MessageBus::new(vec![]);
  let addr = start(bus.clone()).await;
  let mut a = connect(&addr).await;
  let mut b = connect(&addr).await;

  send(&mut a, |buf| {
    Subscribe::new("deploy/#", QoS::AtMostOnce)
      .write(buf)
      .unwrap();
  })
  .await;
  read_packet(&mut a).await;
  send(&mut b, |buf| {
    Subscribe::new("metrics/#", QoS::AtMostOnce)
      .write(buf)
      .unwrap();
  })
  .await;
  read_packet(&mut b).await;

  bus.deliver(Delivery {
    topic: "deploy/web".to_string(),
    payload: b"x".to_vec(),
    id: None,
  });
  match read_packet(&mut a).await {
    Some(Packet::Publish(p)) => assert_eq!(p.topic, "deploy/web"),
    other => panic!("the subscriber should have received it, got {other:?}"),
  }

  // `b` asked for something else, so nothing should be waiting for it. Prove
  // it by sending something `b` *did* ask for and seeing that arrive first.
  bus.deliver(Delivery {
    topic: "metrics/cpu".to_string(),
    payload: b"9".to_vec(),
    id: None,
  });
  match read_packet(&mut b).await {
    Some(Packet::Publish(p)) => assert_eq!(p.topic, "metrics/cpu"),
    other => panic!("expected the metrics message first, got {other:?}"),
  }
}

#[tokio::test]
async fn unsubscribing_stops_this_connection_only() {
  let bus = MessageBus::new(vec![]);
  let addr = start(bus.clone()).await;
  let mut client = connect(&addr).await;

  send(&mut client, |b| {
    let mut sub = Subscribe::new("deploy/#", QoS::AtMostOnce);
    sub.add("metrics/#".to_string(), QoS::AtMostOnce);
    sub.write(b).unwrap();
  })
  .await;
  read_packet(&mut client).await;

  send(&mut client, |b| {
    Unsubscribe::new("deploy/#").write(b).unwrap();
  })
  .await;
  match read_packet(&mut client).await {
    Some(Packet::UnsubAck(_)) => {}
    other => panic!("expected UNSUBACK, got {other:?}"),
  }

  bus.deliver(Delivery {
    topic: "deploy/web".to_string(),
    payload: b"x".to_vec(),
    id: None,
  });
  bus.deliver(Delivery {
    topic: "metrics/cpu".to_string(),
    payload: b"9".to_vec(),
    id: None,
  });
  match read_packet(&mut client).await {
    Some(Packet::Publish(p)) => assert_eq!(p.topic, "metrics/cpu", "the unsubscribed one arrived"),
    other => panic!("expected the metrics message, got {other:?}"),
  }
  // The process still holds `deploy/#` for anything else attached to it: an
  // unsubscribe here is not authority over what the whole client listens to.
  assert!(bus.wants("deploy/web").await);
}

#[tokio::test]
async fn ping_is_answered_so_a_client_stays_connected() {
  let bus = MessageBus::new(vec![]);
  let addr = start(bus).await;
  let mut client = connect(&addr).await;
  send(&mut client, |b| {
    mqttbytes::v4::PingReq.write(b).unwrap();
  })
  .await;
  assert!(matches!(
    read_packet(&mut client).await,
    Some(Packet::PingResp)
  ));
}

#[tokio::test]
async fn a_session_that_does_not_start_with_connect_is_closed() {
  let bus = MessageBus::new(vec![]);
  let addr = start(bus).await;
  let mut stream = TcpStream::connect(&addr).await.unwrap();
  send(&mut stream, |b| {
    Publish::new("deploy/web", QoS::AtMostOnce, "x")
      .write(b)
      .unwrap();
  })
  .await;
  assert!(
    read_packet(&mut stream).await.is_none(),
    "the connection should be closed without a response"
  );
}
