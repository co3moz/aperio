//! Tests for the messaging half of the admin API: publishing, and the list of
//! who is listening.

use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;

fn body(topic: &str, payload: Option<&str>, b64: Option<&str>) -> Json<PublishRequest> {
  Json(PublishRequest {
    topic: topic.to_string(),
    payload: payload.map(str::to_string),
    payload_base64: b64.map(str::to_string),
    qos: 0,
  })
}

async fn publish(state: &Arc<AppState>, headers: HeaderMap, req: Json<PublishRequest>) -> Response {
  publish_handler(State(state.clone()), ConnectInfo(test_peer()), headers, req).await
}

#[tokio::test]
async fn a_publish_with_no_subscriber_says_so_rather_than_failing() {
  // A publish that reached nobody is not an error, and the count is the only
  // thing that distinguishes it from one that reached everybody.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = publish(&state, headers, body("deploy/finished", Some("v2"), None)).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let out = json_body(resp).await;
  assert_eq!(out["topic"], "deploy/finished");
  assert_eq!(out["clients"], 0);
  assert_eq!(out["connections"], 0);
  assert_eq!(out["qos"], 0);
}

#[tokio::test]
async fn an_empty_message_is_a_signal_and_two_payloads_are_a_mistake() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;

  // The topic is the message: nothing to carry, still a publish.
  let resp = publish(&state, headers.clone(), body("ping", None, None)).await;
  assert_eq!(resp.status(), StatusCode::OK);

  // Both payload forms at once is ambiguous, and guessing is worse than
  // refusing.
  let resp = publish(&state, headers.clone(), body("t", Some("a"), Some("YQ=="))).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Base64 that is not base64 says which field is wrong.
  let resp = publish(
    &state,
    headers.clone(),
    body("t", None, Some("not base64!")),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let text = String::from_utf8(
    axum::body::to_bytes(resp.into_body(), usize::MAX)
      .await
      .unwrap()
      .to_vec(),
  )
  .unwrap();
  assert!(text.contains("payload_base64"), "{text}");

  // And base64 that is base64 goes through.
  let resp = publish(&state, headers, body("t", None, Some("aGVsbG8="))).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_topic_that_is_a_filter_is_refused() {
  // Wildcards are subscribe syntax. Publishing to one would mean "send this
  // to every topic I can think of", which is not a thing to do by accident.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  for topic in ["deploy/#", "deploy/+/done", ""] {
    let resp = publish(&state, headers.clone(), body(topic, Some("x"), None)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "topic {topic:?}");
  }
}

#[tokio::test]
async fn a_publish_is_audited_because_it_reaches_other_machines() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  publish(&state, headers, body("fleet/restart", Some("now"), None)).await;

  let events = state.audit.lock().await.recent();
  let entry = events
    .iter()
    .find(|e| e.event == "message_published")
    .expect("the publish is on the audit trail");
  assert!(
    entry.details.contains("topic=fleet/restart"),
    "{}",
    entry.details
  );
  assert!(entry.details.contains("bytes=3"), "{}", entry.details);
}

#[tokio::test]
async fn subscribers_are_grouped_by_process_and_scoped_to_the_organization() {
  // One client running three services is one subscriber, not three, and a
  // tenant never sees another's listeners.
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;

  let mut a1 = mock_client(Some("a.example.com"), None, None, None);
  a1.instance_group = Some("proc-1".into());
  a1.subscriptions = vec!["deploy/#".into()];
  let mut a2 = mock_client(Some("b.example.com"), None, None, None);
  a2.instance_group = Some("proc-1".into());
  a2.subscriptions = vec!["deploy/#".into(), "alerts/+".into()];
  let mut other = mock_client(Some("c.example.com"), None, None, None);
  other.instance_group = Some("proc-2".into());
  other.subscriptions = vec!["tenant/#".into()];
  other.perms.org_id = Some(org.clone());
  // A connection with no subscription is not a subscriber.
  let quiet = mock_client(Some("d.example.com"), None, None, None);
  {
    let mut clients = state.clients.write().await;
    clients.insert("c1".into(), a1);
    clients.insert("c2".into(), a2);
    clients.insert("c3".into(), other);
    clients.insert("c4".into(), quiet);
  }

  let views = subscribers_handler(State(state.clone()), admin_headers(&state).await)
    .await
    .0;
  assert_eq!(views.len(), 1, "master sees one process, not three clients");
  let view = &views[0];
  assert_eq!(view.instance_group.as_deref(), Some("proc-1"));
  assert_eq!(view.connections, 2);
  assert_eq!(view.topics, vec!["alerts/+", "deploy/#"], "deduped, sorted");

  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let views = subscribers_handler(State(state), cookie_headers(&token))
    .await
    .0;
  assert_eq!(views.len(), 1);
  assert_eq!(views[0].topics, vec!["tenant/#"]);
}
