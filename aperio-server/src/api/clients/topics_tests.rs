//! The stream's topics: every one has a name, a path with a role floor and a
//! cadence; the query is read in both spellings and an unknown name is
//! refused by name; an audit event invalidates the topics it should and no
//! others; and a topic's document is the endpoint's own answer.

use super::*;
use crate::test_support::*;

#[test]
fn every_topic_round_trips_through_its_name_and_has_a_path() {
  for t in Topic::ALL {
    assert_eq!(Topic::parse(t.name()), Some(*t), "{t:?}");
    assert!(t.path().starts_with("/api/"), "{t:?}");
    // The name is what travels in a query string and an SSE event name;
    // both want one word.
    assert!(
      t.name()
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b == b'_'),
      "{t:?}"
    );
  }
  assert_eq!(Topic::parse("nothing"), None);
}

#[test]
fn the_role_floor_is_the_endpoints_own() {
  use crate::store::users::Role;
  assert_eq!(Topic::Tokens.required_role(), Role::Viewer);
  assert_eq!(Topic::Users.required_role(), Role::Admin);
  assert_eq!(Topic::Sessions.required_role(), Role::Admin);
  assert_eq!(Topic::AdminKeys.required_role(), Role::Admin);
  assert_eq!(Topic::Orgs.required_role(), Role::Admin);
  assert!(Topic::Orgs.master_only());
  assert!(!Topic::Tokens.master_only());
}

#[test]
fn the_query_is_read_in_both_spellings_and_an_unknown_name_is_refused() {
  let mut params = std::collections::HashMap::new();
  params.insert("topics".to_string(), "orgs, uptime,session".to_string());
  params.insert("tokens".to_string(), String::new());
  let got = topics_from_query(&params).unwrap();
  for t in [Topic::Orgs, Topic::Uptime, Topic::Session, Topic::Tokens] {
    assert!(got.contains(&t), "{t:?} in {got:?}");
  }
  assert_eq!(got.len(), 4, "listed twice is listed once");

  let mut bad = std::collections::HashMap::new();
  bad.insert("topics".to_string(), "tokens,nonsense".to_string());
  let err = topics_from_query(&bad).unwrap_err();
  assert!(err.contains("nonsense"), "{err}");
  assert!(
    err.contains("stage_stats"),
    "the answer names the topics: {err}"
  );

  assert!(
    topics_from_query(&std::collections::HashMap::new())
      .unwrap()
      .is_empty()
  );
}

#[test]
fn an_audit_event_invalidates_its_topics_and_always_the_audit_ring() {
  assert_eq!(
    Topic::touched_by("token_created"),
    vec![Topic::Audit, Topic::Tokens]
  );
  assert_eq!(
    Topic::touched_by("user_grant_removed"),
    vec![Topic::Audit, Topic::Users]
  );
  assert_eq!(
    Topic::touched_by("session_revoked"),
    vec![Topic::Audit, Topic::Sessions]
  );
  assert_eq!(
    Topic::touched_by("maintenance_on"),
    vec![Topic::Audit, Topic::Maintenance]
  );
  assert_eq!(
    Topic::touched_by("org_created"),
    vec![Topic::Audit, Topic::Orgs]
  );
  assert_eq!(
    Topic::touched_by("client_connected"),
    vec![Topic::Audit],
    "an event that changes no listed store still adds an audit row"
  );
  assert!(Topic::touched_by("import_applied").contains(&Topic::Users));
  assert!(Topic::Orgs.changes_everyone());
  assert!(!Topic::Tokens.changes_everyone());
}

#[test]
fn cadences_send_on_connect_and_on_their_own_beat() {
  // Every topic is sent on connect, whatever its cadence.
  for t in Topic::ALL {
    assert!(t.due_on(0), "{t:?} on connect");
  }
  // A change-driven topic is never sent by the tick afterwards.
  assert!(!Topic::Tokens.due_on(1));
  assert!(!Topic::Tokens.due_on(600));
  // A tick-driven one keeps its beat.
  assert!(Topic::Topology.due_on(2));
  assert!(!Topic::Topology.due_on(3));
  assert!(Topic::Uptime.due_on(7));
  assert!(Topic::Session.due_on(30));
  assert!(!Topic::Session.due_on(29));
}

#[tokio::test]
async fn a_topics_document_is_the_endpoints_own_answer() {
  use crate::store::users::Role;
  let state = std::sync::Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let doc = Topic::Tokens
    .payload(&state, &headers)
    .await
    .expect("a document");
  assert!(doc.is_array(), "the token list, as the endpoint answers it");
  let maintenance = Topic::Maintenance
    .payload(&state, &headers)
    .await
    .expect("a document");
  assert!(maintenance.is_array());

  let session = Topic::Session
    .payload(&state, &headers)
    .await
    .expect("the session");
  assert_eq!(session["username"], "aperio");
  assert!(session["master_admin"].as_bool().unwrap());

  // A refusal is no document, not an error document.
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let viewer = cookie_headers(&token);
  assert!(Topic::Orgs.payload(&state, &viewer).await.is_none());
  assert!(Topic::Tokens.payload(&state, &viewer).await.is_some());
}
