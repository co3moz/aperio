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
    .write()
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

  let out = publish(&state, None, "deploy/web", b"go", Publisher::Server, 0)
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

  let out = publish(&state, None, "fleet/drain", b"", Publisher::Server, 0)
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
  let out = publish(&state, None, "anything", b"x", Publisher::Server, 0)
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

  let out = publish(
    &state,
    Some("acme"),
    "deploy/web",
    b"go",
    Publisher::Server,
    0,
  )
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
    0,
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
    0,
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
    0,
  )
  .await
  .unwrap();
  assert!(delivered(&mut everything).is_none());
  publish(&state, None, "ordinary/topic", b"{}", Publisher::Server, 0)
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
    publish(&state, None, "deploy/#", b"", Publisher::Server, 0)
      .await
      .is_err()
  );
  assert!(
    publish(&state, None, "", b"", Publisher::Server, 0)
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
    publish(&state, None, "bulk", &big, Publisher::Server, 0)
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
  state.clients.write().await.insert("c".to_string(), handle);

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
  publish(&state, None, "deploy/web", b"go", Publisher::Server, 0)
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

  publish(&state, None, "deploy/web", b"", Publisher::Server, 0)
    .await
    .unwrap();
  assert!(delivered(&mut sub).is_none(), "unsubscribed");
  publish(&state, None, "deploy/api", b"", Publisher::Server, 0)
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
  state.clients.write().await.insert("c".to_string(), handle);

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
    .write()
    .await
    .insert("stuck".to_string(), stuck);
  let mut healthy = subscriber(&state, "ok", Some("ok"), None, &["#"]).await;

  let out = publish(&state, None, "deploy/web", b"go", Publisher::Server, 0)
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

/// A non-master token scoped to `topics`.
fn scoped(topics: &[&str]) -> ClientPerms {
  ClientPerms {
    master: false,
    token_id: Some("t".to_string()),
    topics: topics.iter().map(|s| s.to_string()).collect(),
    ..ClientPerms::master()
  }
}

#[test]
fn a_token_may_only_use_the_topics_it_carries() {
  // One rule for both directions: publishing to a topic and subscribing to it
  // are the same access to the same conversation.
  let scope = scoped(&["deploy/#", "metrics/cpu"]);
  assert!(may_use_topic(&scope, "deploy/web"));
  assert!(may_use_topic(&scope, "deploy/web/eu"));
  assert!(may_use_topic(&scope, "deploy/#"));
  assert!(may_use_topic(&scope, "metrics/cpu"));
  assert!(!may_use_topic(&scope, "metrics/memory"));
  assert!(!may_use_topic(&scope, "secrets/rotate"));

  // Subscribing to everything must not be a way around a scope that named
  // one subtree.
  assert!(!may_use_topic(&scope, "#"));
  assert!(!may_use_topic(&scope, "+/web"));
  // A `+` inside the granted subtree is fine: it cannot reach outside it.
  assert!(may_use_topic(&scope, "deploy/+"));

  // Empty means no messaging at all, which is what a token that never asked
  // for the capability carries.
  assert!(!may_use_topic(&scoped(&[]), "anything"));
  // And the master token is unrestricted, as it is everywhere else.
  assert!(may_use_topic(&ClientPerms::master(), "#"));
}

#[test]
fn a_granted_wildcard_covers_what_it_should_and_no_more() {
  let one_level = scoped(&["deploy/+/eu"]);
  assert!(may_use_topic(&one_level, "deploy/web/eu"));
  assert!(may_use_topic(&one_level, "deploy/+/eu"));
  assert!(!may_use_topic(&one_level, "deploy/web/us"));
  // `#` in the asked position would reach past the single level granted.
  assert!(!may_use_topic(&one_level, "deploy/#"));
  assert!(!may_use_topic(&one_level, "deploy/#/eu"));
}

#[tokio::test]
async fn a_subscription_outside_the_token_is_refused_by_name() {
  // Silently dropping the filter would leave the client believing it is
  // subscribed and waiting for messages that never come.
  let state = Arc::new(test_state());
  let mut handle = mock_client(None, None, None, None);
  handle.instance_group = Some("p".to_string());
  handle.perms = scoped(&["deploy/#"]);
  state.clients.write().await.insert("c".to_string(), handle);

  let refused = set_subscriptions(
    &state,
    "c",
    vec!["deploy/web".to_string(), "secrets/#".to_string()],
    true,
  )
  .await;
  assert_eq!(refused.len(), 1, "{refused:?}");
  assert_eq!(refused[0].0, "secrets/#");
  assert!(refused[0].1.contains("token"), "{refused:?}");

  let clients = state.clients.read().await;
  assert_eq!(clients["c"].subscriptions, vec!["deploy/web".to_string()]);
}

/// The frames a subscriber's channel is holding, decoded.
fn drain(rx: &mut Receiver<Message>) -> Vec<(String, Option<String>, u64)> {
  let mut out = Vec::new();
  while let Ok(Message::Text(text)) = rx.try_recv() {
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    out.push((
      parsed["topic"].as_str().unwrap_or_default().to_string(),
      parsed["id"].as_str().map(str::to_string),
      parsed["qos"].as_u64().unwrap_or(0),
    ));
  }
  out
}

#[tokio::test]
async fn a_qos_one_message_is_resent_until_it_is_acknowledged() {
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["deploy/#"]).await;

  publish(&state, None, "deploy/web", b"go", Publisher::Server, 1)
    .await
    .unwrap();
  let first = drain(&mut sub);
  assert_eq!(first.len(), 1);
  assert_eq!(first[0].2, 1, "the delivery carries the qos it was sent at");
  let id = first[0].1.clone().expect("a qos 1 delivery is identified");

  // Nothing is due yet, so a sweep changes nothing.
  assert_eq!(sweep_pending(&state).await, (0, 0));
  assert!(drain(&mut sub).is_empty());

  // Age it past the retry timeout by hand rather than sleeping for it.
  {
    let mut pending = state.pending_messages.lock().await;
    for message in pending.get_mut("p").expect("held for the process") {
      message.last_sent -= ACK_TIMEOUT;
    }
  }
  let (resent, abandoned) = sweep_pending(&state).await;
  assert_eq!((resent, abandoned), (1, 0));
  // Asserted where the metrics endpoint reads it, not only on the return
  // value: the counter was rendered and never incremented, so the endpoint
  // reported no resends at all while resends were happening.
  assert_eq!(
    state
      .message_metrics
      .resent
      .load(std::sync::atomic::Ordering::Relaxed),
    1
  );
  let again = drain(&mut sub);
  assert_eq!(again.len(), 1, "it was sent a second time");
  assert_eq!(again[0].1.as_deref(), Some(id.as_str()), "the same message");

  // The acknowledgement stops it.
  acknowledge(&state, "c", &id).await;
  assert!(
    !state.pending_messages.lock().await.contains_key("p"),
    "nothing is held once it is acknowledged"
  );
  {
    let mut pending = state.pending_messages.lock().await;
    pending.remove("p");
  }
  assert_eq!(sweep_pending(&state).await, (0, 0));
  assert!(drain(&mut sub).is_empty());
}

#[tokio::test]
async fn a_qos_zero_message_is_never_held() {
  // The default costs nothing: no bookkeeping, no resend, no memory.
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["deploy/#"]).await;
  publish(&state, None, "deploy/web", b"go", Publisher::Server, 0)
    .await
    .unwrap();
  assert_eq!(drain(&mut sub).len(), 1);
  assert!(state.pending_messages.lock().await.is_empty());
}

#[tokio::test]
async fn an_unacknowledged_message_is_given_up_on_rather_than_kept() {
  // The window is the whole of the promise: it covers a connection that died
  // between the write and the acknowledgement, not a subscriber that is away.
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["deploy/#"]).await;
  publish(&state, None, "deploy/web", b"go", Publisher::Server, 1)
    .await
    .unwrap();
  drain(&mut sub);

  {
    let mut pending = state.pending_messages.lock().await;
    for message in pending.get_mut("p").unwrap() {
      message.first_sent -= MAX_ACK_WAIT;
      message.last_sent -= MAX_ACK_WAIT;
    }
  }
  let (_, abandoned) = sweep_pending(&state).await;
  assert_eq!(abandoned, 1);
  assert!(
    state.pending_messages.lock().await.is_empty(),
    "the queue is emptied rather than growing forever"
  );
}

#[tokio::test]
async fn a_client_that_stops_acknowledging_costs_a_bounded_amount() {
  let state = Arc::new(test_state());
  // A generous channel so the sends themselves are not what limits this.
  let (tx, _rx) = tokio::sync::mpsc::channel::<Message>(MAX_PENDING_PER_PROCESS * 2);
  let mut handle = mock_client(None, None, None, None);
  handle.tx = tx;
  handle.instance_group = Some("p".to_string());
  handle.subscriptions = vec!["#".to_string()];
  state.clients.write().await.insert("c".to_string(), handle);

  for i in 0..(MAX_PENDING_PER_PROCESS + 50) {
    publish(
      &state,
      None,
      &format!("deploy/{i}"),
      b"x",
      Publisher::Server,
      1,
    )
    .await
    .unwrap();
  }
  let held = state.pending_messages.lock().await["p"].len();
  assert_eq!(held, MAX_PENDING_PER_PROCESS, "capped, not unbounded");
}

#[tokio::test]
async fn an_acknowledgement_counts_from_any_connection_of_the_process() {
  // The delivery goes out on one connection of a multi-service client and the
  // acknowledgement may come back on another. It is the same subscriber.
  let state = Arc::new(test_state());
  let mut first = subscriber(&state, "conn-1", Some("shared"), None, &["deploy/#"]).await;
  let mut second = subscriber(&state, "conn-2", Some("shared"), None, &["deploy/#"]).await;

  publish(&state, None, "deploy/web", b"go", Publisher::Server, 1)
    .await
    .unwrap();
  // Which of the process's connections carried it is the server's choice and
  // is not fixed: the client map is a HashMap, so asserting on one of them
  // would pass or fail by iteration order.
  let mut sent = drain(&mut first);
  sent.extend(drain(&mut second));
  assert_eq!(sent.len(), 1, "one process, one delivery");
  let id = sent[0].1.clone().expect("a qos 1 delivery is identified");

  acknowledge(&state, "conn-2", &id).await;
  assert!(
    !state.pending_messages.lock().await.contains_key("shared"),
    "an acknowledgement on a sibling connection counts"
  );
}

#[tokio::test]
async fn a_qos_above_one_is_delivered_as_one_rather_than_promised() {
  let state = Arc::new(test_state());
  let mut sub = subscriber(&state, "c", Some("p"), None, &["deploy/#"]).await;
  publish(&state, None, "deploy/web", b"go", Publisher::Server, 2)
    .await
    .unwrap();
  assert_eq!(drain(&mut sub)[0].2, 1);
}
