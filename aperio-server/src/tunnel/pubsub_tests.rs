//! Tests for client-to-client messaging.

use super::*;
use crate::state::ClientPerms;
use crate::test_support::{mock_client, test_state};
use axum::extract::ws::Message;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

/// Registers a subscriber and hands back the channel it will receive on.
///
/// `process` is the instance group: two connections sharing one is one client
/// process running several services, which is the case the delivery rule
/// exists for.
async fn subscriber(
  state: &Arc<crate::state::AppState>,
  connection_id: &str,
  process: Option<&str>,
  org: Option<&str>,
  filters: &[&str],
) -> Receiver<Message> {
  let (tx, rx) = tokio::sync::mpsc::channel::<Message>(8);
  let mut handle = mock_client(None, None, None, None);
  handle.tx = tx;
  handle.instance_group = process.map(str::to_string);
  handle.perms = ClientPerms {
    org_id: org.map(str::to_string),
    ..ClientPerms::master()
  };
  state
    .clients
    .lock()
    .await
    .insert(connection_id.to_string(), handle);
  let refused = set_subscriptions(
    state,
    connection_id,
    filters.iter().map(|s| s.to_string()).collect(),
    true,
  )
  .await;
  assert!(refused.is_empty(), "unexpected refusals: {refused:?}");
  rx
}

/// The topic and payload of one delivered message.
fn delivered(rx: &mut Receiver<Message>) -> Option<(String, String)> {
  let Ok(Message::Text(text)) = rx.try_recv() else {
    return None;
  };
  let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
  assert_eq!(parsed["type"], "Publish");
  use base64::prelude::*;
  let payload = BASE64_STANDARD
    .decode(parsed["payload"].as_str().unwrap())
    .unwrap();
  Some((
    parsed["topic"].as_str().unwrap().to_string(),
    String::from_utf8(payload).unwrap(),
  ))
}

#[tokio::test]
async fn a_message_reaches_the_subscribers_of_a_topic() {
  let state = Arc::new(test_state());
  let mut web = subscriber(&state, "c-web", Some("p-web"), None, &["deploy/web"]).await;
  let mut all = subscriber(&state, "c-all", Some("p-all"), None, &["deploy/#"]).await;
  let mut other = subscriber(&state, "c-other", Some("p-other"), None, &["metrics/+"]).await;

  let out = publish(&state, None, "deploy/web", b"go", Publisher::Server)
    .await
    .unwrap();
  assert_eq!(out.processes, 2);
  assert_eq!(
    delivered(&mut web),
    Some(("deploy/web".to_string(), "go".to_string()))
  );
  assert_eq!(
    delivered(&mut all),
    Some(("deploy/web".to_string(), "go".to_string()))
  );
  assert_eq!(delivered(&mut other), None, "a non-matching filter");
}

#[tokio::test]
async fn one_process_receives_one_copy_however_many_connections_it_holds() {
  // The reason subscriptions key on the instance group. A client with a
  // `services:` list holds one connection per service and subscribes on each;
  // keyed on the connection this would arrive three times, and every
  // subscriber would need a deduplication cache to undo it.
  let state = Arc::new(test_state());
  let mut a = subscriber(&state, "conn-1", Some("one-process"), None, &["fleet/#"]).await;
  let mut b = subscriber(&state, "conn-2", Some("one-process"), None, &["fleet/#"]).await;
  let mut c = subscriber(&state, "conn-3", Some("one-process"), None, &["fleet/#"]).await;

  let out = publish(&state, None, "fleet/drain", b"", Publisher::Server)
    .await
    .unwrap();
  assert_eq!(out.processes, 1, "one process, one delivery");
  let got = [delivered(&mut a), delivered(&mut b), delivered(&mut c)];
  assert_eq!(
    got.iter().filter(|g| g.is_some()).count(),
    1,
    "exactly one connection of the process was written to: {got:?}"
  );
}

#[tokio::test]
async fn a_connection_without_an_instance_group_still_receives() {
  // A client old enough not to send the handshake header has no process
  // identity; falling back to its connection id keeps it working rather than
  // silently excluding it from every message.
  let state = Arc::new(test_state());
  let mut old = subscriber(&state, "legacy", None, None, &["#"]).await;
  let out = publish(&state, None, "anything", b"x", Publisher::Server)
    .await
    .unwrap();
  assert_eq!(out.processes, 1);
  assert!(delivered(&mut old).is_some());
}

#[tokio::test]
async fn a_message_never_crosses_an_organization() {
  let state = Arc::new(test_state());
  let mut acme = subscriber(&state, "c-acme", Some("p-acme"), Some("acme"), &["#"]).await;
  let mut globex = subscriber(&state, "c-globex", Some("p-globex"), Some("globex"), &["#"]).await;
  let mut master = subscriber(&state, "c-master", Some("p-master"), None, &["#"]).await;

  let out = publish(&state, Some("acme"), "deploy/web", b"go", Publisher::Server)
    .await
    .unwrap();
  assert_eq!(out.processes, 1);
  assert!(delivered(&mut acme).is_some());
  assert!(delivered(&mut globex).is_none(), "another organization");
  assert!(
    delivered(&mut master).is_none(),
    "master is not a superset of its children"
  );
}

#[tokio::test]
async fn the_reserved_namespace_is_the_servers_alone() {
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["$aperio/#"]).await;

  // A client cannot forge an infrastructure event.
  let refused = publish(
    &state,
    None,
    "$aperio/client/connected",
    b"{}",
    Publisher::Client("c"),
  )
  .await;
  assert!(refused.is_err(), "a client published into $aperio/");
  assert!(delivered(&mut sub).is_none());

  // The server can, and a subscriber that asked for it by name receives it.
  publish(
    &state,
    None,
    "$aperio/client/connected",
    b"{}",
    Publisher::Server,
  )
  .await
  .unwrap();
  assert!(delivered(&mut sub).is_some());
}

#[tokio::test]
async fn a_bare_wildcard_subscriber_does_not_receive_server_events() {
  // Subscribing to everything is a common thing to do while debugging; it
  // must not enroll the client in infrastructure events it never asked to
  // parse.
  let state = Arc::new(test_state());
  let mut everything = subscriber(&state, "c", Some("p"), None, &["#"]).await;
  publish(
    &state,
    None,
    "$aperio/client/connected",
    b"{}",
    Publisher::Server,
  )
  .await
  .unwrap();
  assert!(delivered(&mut everything).is_none());
  publish(&state, None, "ordinary/topic", b"{}", Publisher::Server)
    .await
    .unwrap();
  assert!(delivered(&mut everything).is_some());
}

#[tokio::test]
async fn publishing_to_a_filter_is_refused() {
  // `deploy/#` looks like a broadcast and reaches nobody, so it is an error
  // rather than a silent no-op.
  let state = Arc::new(test_state());
  let _sub = subscriber(&state, "c", Some("p"), None, &["deploy/web"]).await;
  assert!(
    publish(&state, None, "deploy/#", b"", Publisher::Server)
      .await
      .is_err()
  );
  assert!(
    publish(&state, None, "", b"", Publisher::Server)
      .await
      .is_err()
  );
}

#[tokio::test]
async fn an_oversized_payload_is_refused_rather_than_relayed() {
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["#"]).await;
  let big = vec![b'x'; MAX_PAYLOAD_BYTES + 1];
  assert!(
    publish(&state, None, "bulk", &big, Publisher::Server)
      .await
      .is_err()
  );
  assert!(delivered(&mut sub).is_none());
}

#[tokio::test]
async fn unusable_filters_are_reported_and_the_rest_still_apply() {
  // One bad filter must not throw away the others: they are what the operator
  // asked for, and a whole subscription silently failing is worse than a
  // named refusal.
  let state = Arc::new(test_state());
  let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(8);
  let mut handle = mock_client(None, None, None, None);
  handle.tx = tx;
  handle.instance_group = Some("p".to_string());
  state.clients.lock().await.insert("c".to_string(), handle);

  let refused = set_subscriptions(
    &state,
    "c",
    vec![
      "deploy/web".to_string(),
      "deploy/#/eu".to_string(), // `#` is only legal last
      "".to_string(),
    ],
    true,
  )
  .await;
  assert_eq!(refused.len(), 2, "{refused:?}");
  publish(&state, None, "deploy/web", b"go", Publisher::Server)
    .await
    .unwrap();
  assert!(
    delivered(&mut rx).is_some(),
    "the good filter still applies"
  );
}

#[tokio::test]
async fn unsubscribing_stops_delivery() {
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["deploy/web", "deploy/api"]).await;
  set_subscriptions(&state, "c", vec!["deploy/web".to_string()], false).await;

  publish(&state, None, "deploy/web", b"", Publisher::Server)
    .await
    .unwrap();
  assert!(delivered(&mut sub).is_none(), "unsubscribed");
  publish(&state, None, "deploy/api", b"", Publisher::Server)
    .await
    .unwrap();
  assert!(delivered(&mut sub).is_some(), "the other filter remains");
}

#[tokio::test]
async fn a_client_at_the_filter_limit_is_told_rather_than_silently_capped() {
  let state = Arc::new(test_state());
  let filters: Vec<String> = (0..MAX_FILTERS_PER_CLIENT)
    .map(|i| format!("topic/{i}"))
    .collect();
  let mut handle = mock_client(None, None, None, None);
  handle.instance_group = Some("p".to_string());
  state.clients.lock().await.insert("c".to_string(), handle);

  let refused = set_subscriptions(&state, "c", filters, true).await;
  assert!(refused.is_empty());
  let refused = set_subscriptions(&state, "c", vec!["one/too/many".to_string()], true).await;
  assert_eq!(refused.len(), 1);
  assert!(refused[0].1.contains("limit"), "{refused:?}");
}

#[tokio::test]
async fn a_slow_subscriber_does_not_hold_up_the_others() {
  // A full channel means a client that is not keeping up. The publish drops
  // its copy and carries on rather than blocking, so one stuck subscriber
  // cannot stall the fan-out.
  let state = Arc::new(test_state());
  let (full_tx, _full_rx) = tokio::sync::mpsc::channel::<Message>(1);
  full_tx.try_send(Message::Text("occupied".into())).unwrap();
  let mut stuck = mock_client(None, None, None, None);
  stuck.tx = full_tx;
  stuck.instance_group = Some("stuck".to_string());
  stuck.subscriptions = vec!["#".to_string()];
  state
    .clients
    .lock()
    .await
    .insert("stuck".to_string(), stuck);
  let mut healthy = subscriber(&state, "ok", Some("ok"), None, &["#"]).await;

  let out = publish(&state, None, "deploy/web", b"go", Publisher::Server)
    .await
    .unwrap();
  assert_eq!(out.processes, 2, "both matched");
  assert_eq!(out.connections, 1, "only one could be written to");
  assert!(delivered(&mut healthy).is_some());
}

#[tokio::test]
async fn a_server_event_reaches_a_client_that_asked_for_it() {
  // The reason this namespace exists. The events already fed webhooks; on a
  // topic they reach a client without it standing up an HTTP receiver, which
  // makes the feature a way into an existing system rather than a new one.
  let state = Arc::new(test_state());
  let mut watcher = subscriber(&state, "c", Some("p"), None, &["$aperio/client/#"]).await;

  state
    .emit_event_in(
      "client_draining",
      serde_json::json!({"client_id": "abc"}),
      None,
    )
    .await;

  let (topic, payload) = delivered(&mut watcher).expect("the event arrived");
  // An event name is snake_case; a topic has levels.
  assert_eq!(topic, "$aperio/client/draining");
  let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
  assert_eq!(parsed["client_id"], "abc");
}

#[tokio::test]
async fn a_server_event_stays_inside_its_organization() {
  let state = Arc::new(test_state());
  let mut acme = subscriber(&state, "c-acme", Some("p1"), Some("acme"), &["$aperio/#"]).await;
  let mut other = subscriber(
    &state,
    "c-other",
    Some("p2"),
    Some("globex"),
    &["$aperio/#"],
  )
  .await;

  state
    .emit_event_in(
      "client_draining",
      serde_json::json!({}),
      Some("acme".to_string()),
    )
    .await;

  assert!(delivered(&mut acme).is_some());
  assert!(delivered(&mut other).is_none());
}
