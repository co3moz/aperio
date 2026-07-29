//! Tests for the local message face.

use super::*;
use crate::pubsub::{Delivery, MessageBus};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

/// Starts the face on an ephemeral loopback port and returns its address.
async fn start(bus: Arc<MessageBus>) -> String {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap().to_string();
  drop(listener);
  serve(&addr, bus).await.unwrap();
  addr
}

/// One request/response round trip, returning the whole response.
async fn request(addr: &str, raw: &str) -> String {
  let mut stream = TcpStream::connect(addr).await.unwrap();
  stream.write_all(raw.as_bytes()).await.unwrap();
  let mut out = String::new();
  let _ = tokio::time::timeout(Duration::from_secs(2), async {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    out = String::from_utf8_lossy(&buf).to_string();
  })
  .await;
  out
}

#[tokio::test]
async fn a_subscriber_receives_matching_messages_as_events() {
  let bus = MessageBus::new(vec![]);
  let addr = start(bus.clone()).await;

  let mut stream = TcpStream::connect(&addr).await.unwrap();
  stream
    .write_all(b"GET /subscribe?topic=deploy%2F%23 HTTP/1.1\r\nHost: x\r\n\r\n")
    .await
    .unwrap();
  let mut reader = BufReader::new(stream);
  let mut line = String::new();
  reader.read_line(&mut line).await.unwrap();
  assert!(line.starts_with("HTTP/1.1 200"), "{line}");

  // Asking over HTTP is enough to subscribe: the filter did not have to be in
  // the config file, which is what makes `curl -N` a working subscriber.
  assert!(bus.wants("deploy/web").await);

  // Drain the rest of the headers.
  loop {
    let mut l = String::new();
    reader.read_line(&mut l).await.unwrap();
    if l.trim().is_empty() {
      break;
    }
  }

  bus.deliver(Delivery {
    topic: "deploy/web".to_string(),
    payload: b"go".to_vec(),
    id: Some("m-1".to_string()),
  });
  // A message the filter does not cover must not appear.
  bus.deliver(Delivery {
    topic: "metrics/cpu".to_string(),
    payload: b"99".to_vec(),
    id: Some("m-2".to_string()),
  });
  bus.deliver(Delivery {
    topic: "deploy/api".to_string(),
    payload: b"go2".to_vec(),
    id: Some("m-3".to_string()),
  });

  let mut seen = Vec::new();
  let _ = tokio::time::timeout(Duration::from_secs(2), async {
    while seen.len() < 6 {
      let mut l = String::new();
      if reader.read_line(&mut l).await.unwrap_or(0) == 0 {
        break;
      }
      if !l.trim().is_empty() {
        seen.push(l.trim().to_string());
      }
    }
  })
  .await;

  use base64::prelude::*;
  assert_eq!(seen[0], "id: m-1");
  assert_eq!(seen[1], "event: deploy/web");
  assert_eq!(seen[2], format!("data: {}", BASE64_STANDARD.encode("go")));
  // The non-matching topic was skipped, so the next event is the third publish.
  assert_eq!(seen[3], "id: m-3");
  assert_eq!(seen[4], "event: deploy/api");
}

#[tokio::test]
async fn publishing_without_a_tunnel_says_so_instead_of_dropping_it() {
  // A local application must not believe a message went out when no
  // connection existed to carry it.
  let bus = MessageBus::new(vec![]);
  let addr = start(bus).await;
  let response = request(
    &addr,
    "POST /publish?topic=deploy%2Fweb HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\ngo",
  )
  .await;
  assert!(response.starts_with("HTTP/1.1 400"), "{response}");
  assert!(response.contains("no tunnel connection"), "{response}");
}

#[tokio::test]
async fn a_publish_reaches_the_tunnel_writer() {
  let bus = MessageBus::new(vec![]);
  let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(4);
  bus.attach("svc", tx).await;
  let addr = start(bus).await;

  let response = request(
    &addr,
    "POST /publish?topic=deploy%2Fweb HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\ngo",
  )
  .await;
  assert!(response.starts_with("HTTP/1.1 202"), "{response}");

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
    b"go"
  );
}

#[tokio::test]
async fn the_routes_refuse_what_they_cannot_act_on() {
  let bus = MessageBus::new(vec![]);
  let addr = start(bus).await;

  // No topic at all.
  let r = request(&addr, "GET /subscribe HTTP/1.1\r\nHost: x\r\n\r\n").await;
  assert!(r.starts_with("HTTP/1.1 400"), "{r}");

  // A filter that matches nothing and looks like it works.
  let r = request(
    &addr,
    "GET /subscribe?topic=deploy%2F%23%2Feu HTTP/1.1\r\nHost: x\r\n\r\n",
  )
  .await;
  assert!(r.starts_with("HTTP/1.1 400"), "{r}");

  // Publishing to a filter rather than a topic.
  let r = request(
    &addr,
    "POST /publish?topic=deploy%2F%23 HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
  )
  .await;
  assert!(r.starts_with("HTTP/1.1 400"), "{r}");

  let r = request(&addr, "GET /nope HTTP/1.1\r\nHost: x\r\n\r\n").await;
  assert!(r.starts_with("HTTP/1.1 404"), "{r}");
}

#[test]
fn a_topic_survives_the_query_string() {
  // `#` and `/` both have to be escaped to reach us intact, and a filter that
  // arrives mangled would silently match nothing.
  assert_eq!(percent_decode("deploy%2F%23"), "deploy/#");
  assert_eq!(percent_decode("deploy%2F%2B"), "deploy/+");
  assert_eq!(percent_decode("plain"), "plain");
  // A stray `%` is not an escape and must not eat the rest of the string.
  assert_eq!(percent_decode("100%"), "100%");
}

#[tokio::test]
async fn the_face_refuses_what_the_server_would_drop() {
  // Handing the frame to the tunnel and letting the server reject it would
  // answer 202 to a local application for a message that never went
  // anywhere, with the reason in a log the application cannot read. The e2e
  // caught exactly that.
  let bus = MessageBus::new(vec![]);
  let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(4);
  bus.attach("svc", tx).await;
  let addr = start(bus).await;

  let r = request(
    &addr,
    "POST /publish?topic=%24aperio%2Fforged HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\n\r\nx",
  )
  .await;
  assert!(r.starts_with("HTTP/1.1 400"), "{r}");
  assert!(r.contains("namespace"), "{r}");
  assert!(
    rx.try_recv().is_err(),
    "nothing should have been put on the tunnel"
  );
}

#[tokio::test]
async fn a_redelivery_is_recognized_and_not_handed_out_twice() {
  // The other half of at-least-once. The server resends when an
  // acknowledgement is lost, and acting on a deploy trigger twice is worse
  // than acting on it late.
  let bus = MessageBus::new(vec![]);
  assert!(!bus.is_duplicate("m-1").await, "the first sighting");
  assert!(bus.is_duplicate("m-1").await, "the same message again");
  assert!(!bus.is_duplicate("m-2").await, "a different message");
}
