//! Fair-share eviction of the request inspector's buffer: that a noisy
//! organization evicts itself rather than a quiet neighbour, and that repeated
//! eviction converges on an even split instead of oscillating.

use super::*;

// Fair-share capture eviction (planned_features #69)
// ---------------------------------------------------------------------------

/// A capture belonging to `org`, with `id` so eviction order is observable.
fn capture_of(org: Option<&str>, id: &str) -> CapturedRequest {
  CapturedRequest {
    id: id.to_string(),
    timestamp: String::new(),
    method: "GET".to_string(),
    uri: "/".to_string(),
    req_headers: Vec::new(),
    req_body: None,
    req_body_truncated: false,
    status: 200,
    resp_headers: Vec::new(),
    resp_body: None,
    resp_body_truncated: false,
    resp_streamed: false,
    duration_ms: 0,
    timeline: None,
    client_id: "c1".to_string(),
    client_name: None,
    org_id: org.map(str::to_string),
  }
}

fn orgs_in(captured: &VecDeque<CapturedRequest>) -> Vec<Option<String>> {
  captured.iter().map(|c| c.org_id.clone()).collect()
}

#[test]
fn a_noisy_organization_evicts_itself_not_a_quiet_one() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  // The quiet tenant's single capture arrived first, so front-eviction would
  // take it. That is the bug: a tenant investigating one request an hour
  // could never find it.
  captured.push_back(capture_of(Some("quiet"), "q1"));
  for i in 0..9 {
    captured.push_back(capture_of(Some("noisy"), &format!("n{i}")));
  }
  evict_for_fairness(&mut captured);
  assert!(
    orgs_in(&captured).contains(&Some("quiet".to_string())),
    "the quiet org's capture survived"
  );
  assert_eq!(captured.front().unwrap().id, "q1");
  assert_eq!(captured.len(), 9);
}

#[test]
fn eviction_takes_the_oldest_of_the_largest_holder() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  for i in 0..3 {
    captured.push_back(capture_of(Some("big"), &format!("b{i}")));
  }
  captured.push_back(capture_of(Some("small"), "s0"));
  evict_for_fairness(&mut captured);
  // Within the org being trimmed, the oldest goes: the front-eviction rule
  // applied inside the tenant rather than across tenants.
  assert_eq!(
    captured.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    vec!["b1", "b2", "s0"]
  );
}

#[test]
fn repeated_eviction_converges_on_an_even_split() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  for i in 0..10 {
    captured.push_back(capture_of(Some("a"), &format!("a{i}")));
  }
  // `b` arrives late and keeps inserting; each insert trims whoever holds
  // most, so `b` grows at `a`'s expense until they are even, without anyone
  // having chosen a per-org number.
  for i in 0..5 {
    evict_for_fairness(&mut captured);
    captured.push_back(capture_of(Some("b"), &format!("b{i}")));
  }
  let a = captured
    .iter()
    .filter(|c| c.org_id.as_deref() == Some("a"))
    .count();
  let b = captured
    .iter()
    .filter(|c| c.org_id.as_deref() == Some("b"))
    .count();
  assert_eq!((a, b), (5, 5));
}

#[test]
fn one_organization_alone_may_use_the_whole_buffer() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  for i in 0..5 {
    captured.push_back(capture_of(None, &format!("m{i}")));
  }
  evict_for_fairness(&mut captured);
  // A fixed per-org ceiling would have wasted the rest of the buffer here.
  assert_eq!(captured.len(), 4);
  assert_eq!(captured.front().unwrap().id, "m1");
}

#[test]
fn an_empty_buffer_is_left_alone() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  evict_for_fairness(&mut captured);
  assert!(captured.is_empty());
}

#[test]
fn a_tie_is_broken_by_age_and_not_by_hash_order() {
  // Two tenants holding the same number: the one whose oldest capture is
  // oldest gives it up. Taking the maximum out of a HashMap would break this
  // differently on every call, so two equally busy tenants would take turns at
  // random and neither could predict what it kept.
  for _ in 0..20 {
    let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
    captured.push_back(capture_of(Some("first"), "f0"));
    captured.push_back(capture_of(Some("second"), "s0"));
    captured.push_back(capture_of(Some("first"), "f1"));
    captured.push_back(capture_of(Some("second"), "s1"));
    evict_for_fairness(&mut captured);
    assert_eq!(captured.front().unwrap().id, "s0");
  }
}
