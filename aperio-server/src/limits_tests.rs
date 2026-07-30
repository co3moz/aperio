//! Tests for the refusal that names itself.

use super::*;
use axum::body::to_bytes;

/// Every variant, so a new limit cannot be added without a header for it.
const ALL: &[Limit] = &[
  Limit::Ip,
  Limit::ServerConcurrency,
  Limit::Route,
  Limit::ClientConcurrency,
  Limit::TokenRate,
  Limit::TokenQuota,
  Limit::OrgQuota,
];

#[tokio::test]
async fn every_limit_names_itself_and_its_setting() {
  for limit in ALL {
    let response = too_many_requests(*limit);
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let header = response
      .headers()
      .get(LIMIT_HEADER)
      .unwrap_or_else(|| panic!("{limit:?} has no {LIMIT_HEADER}"))
      .to_str()
      .unwrap()
      .to_string();
    assert_eq!(
      header,
      format!("{}; setting={}", limit.kind(), limit.setting())
    );

    // The body says it too: a header is for a script, a body is what a
    // browser puts on the screen.
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains(limit.explain()), "{limit:?}: {text}");
  }
}

#[test]
fn the_kinds_are_distinct() {
  // The header is the thing scripts key on, so two limits sharing a kind
  // would be a silent merge of two different causes.
  let mut kinds: Vec<&str> = ALL.iter().map(|l| l.kind()).collect();
  kinds.sort_unstable();
  let count = kinds.len();
  kinds.dedup();
  assert_eq!(kinds.len(), count, "two limits share a kind");
}

#[tokio::test]
async fn only_the_limits_that_refill_promise_a_retry() {
  // A monthly quota with `Retry-After: 1` invites a client to hammer a door
  // that opens next month.
  for limit in [
    Limit::Ip,
    Limit::ServerConcurrency,
    Limit::Route,
    Limit::TokenRate,
  ] {
    let response = too_many_requests(limit);
    assert!(
      response
        .headers()
        .contains_key(axum::http::header::RETRY_AFTER),
      "{limit:?} should say when to come back"
    );
  }
  for limit in [Limit::TokenQuota, Limit::OrgQuota] {
    let response = too_many_requests(limit);
    assert!(
      !response
        .headers()
        .contains_key(axum::http::header::RETRY_AFTER),
      "{limit:?} has no honest retry time"
    );
  }
}
