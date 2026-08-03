//! What these pin down about the message bus: that a service's parallel
//! connections each hold their own writer, and that losing one of them leaves
//! the others able to publish.

use super::*;

#[tokio::test]
async fn parallel_connections_of_one_service_keep_their_own_writers() {
  // The writers used to be keyed by service label, and a service with
  // `connections: N` shares one label across N connections. So the second
  // connection to come up evicted the first, the bus held exactly one writer
  // however many were live, and whichever connection dropped first took that
  // writer with it: publishing then failed with "no tunnel connection is up"
  // while connections were up and healthy.
  let bus = MessageBus::new(vec![]);
  let (tx_a, mut rx_a) = mpsc::channel::<Message>(4);
  let (tx_b, mut rx_b) = mpsc::channel::<Message>(4);
  bus.attach("client-a", tx_a).await;
  bus.attach("client-a-c2", tx_b).await;

  // The first connection goes away; the second is still there.
  bus.detach("client-a").await;
  bus
    .publish("things/x", b"hi")
    .await
    .expect("still publishable");
  assert!(
    rx_b.try_recv().is_ok(),
    "the surviving connection should have been handed the publish"
  );
  assert!(rx_a.try_recv().is_err(), "the detached one gets nothing");

  // And with the last one gone, the refusal is true rather than premature.
  bus.detach("client-a-c2").await;
  assert!(bus.publish("things/x", b"hi").await.is_err());
}

#[tokio::test]
async fn one_connection_reconnecting_replaces_only_its_own_writer() {
  // Replacing by id is still the right behaviour for a reconnect: the old
  // writer belongs to a socket that is gone.
  let bus = MessageBus::new(vec![]);
  let (tx_old, mut rx_old) = mpsc::channel::<Message>(4);
  let (tx_new, mut rx_new) = mpsc::channel::<Message>(4);
  let (tx_sibling, mut rx_sibling) = mpsc::channel::<Message>(4);
  bus.attach("client-a", tx_old).await;
  bus.attach("client-a-c2", tx_sibling).await;
  bus.attach("client-a", tx_new).await;

  // A publish goes to the first writer that takes it, so drain by detaching
  // the others one at a time and checking who was handed the message.
  bus.detach("client-a-c2").await;
  bus.publish("things/x", b"hi").await.unwrap();
  assert!(rx_new.try_recv().is_ok(), "the reconnected writer is live");
  assert!(rx_old.try_recv().is_err(), "the replaced one is not");
  assert!(rx_sibling.try_recv().is_err(), "the sibling was detached");
}

#[tokio::test]
async fn a_reload_replaces_the_configured_filters_and_keeps_held_ones() {
  // The `subscribe:` list used to be read once at startup, so an edited one
  // needed a restart while the documentation said every setting applies on
  // reload.
  let bus = MessageBus::new(vec!["a/#".to_string(), "b/#".to_string()]);
  // A local subscriber, an SSE connection or an MQTT session, holding a
  // filter the file never mentioned.
  assert!(bus.hold_filter("live/#").await);

  assert!(
    bus
      .set_filters(vec!["b/#".to_string(), "c/#".to_string()])
      .await
  );
  let mut now = bus.filters().await;
  now.sort();
  assert_eq!(now, vec!["b/#", "c/#", "live/#"]);

  // Setting the same list again is not a change, so nothing is resubscribed
  // for it.
  assert!(
    !bus
      .set_filters(vec!["b/#".to_string(), "c/#".to_string()])
      .await
  );

  // The held one is still delivered to, and is still given back when its
  // holder leaves rather than being kept for the life of the process.
  assert!(bus.wants("live/x").await);
  assert!(bus.release_filter("live/#").await);
  assert!(!bus.wants("live/x").await);
}
