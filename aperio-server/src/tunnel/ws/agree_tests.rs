//! One route, one policy: a service that disagrees with what already serves its
//! route is refused and told why, one that agrees serves beside it, the refusal
//! lifts when the other side leaves, and a site's `access` takes `access` out
//! of what has to agree.

use super::*;
use crate::test_support::*;

fn on(host: &str) -> ClientHandle {
  mock_client(Some(host), None, None, None)
}

fn public(mut c: ClientHandle) -> ClientHandle {
  c.sole_mut().public = true;
  c
}

fn gated(mut c: ClientHandle, creds: &str) -> ClientHandle {
  c.sole_mut().visitor_auth = Some(creds.to_string());
  c
}

#[test]
fn a_second_client_saying_something_else_about_a_route_is_refused_and_told() {
  let state = test_state();
  let mut clients = HashMap::new();
  // `a` is there first, with nobody to agree with.
  clients.insert("a".to_string(), public(on("app.example.com")));
  assert!(settle(&state, &mut clients, "a").is_empty());
  clients.insert("b".to_string(), gated(on("app.example.com"), "u:p"));
  let refusals = settle(&state, &mut clients, "b");
  assert_eq!(refusals.len(), 1);
  assert!(
    refusals[0].message.contains("its `access` differs"),
    "{}",
    refusals[0].message
  );
  assert!(
    refusals[0].message.contains("app.example.com"),
    "{}",
    refusals[0].message
  );
  assert!(clients["b"].sole().policy_conflict.is_some());
  assert!(clients["a"].sole().policy_conflict.is_none());

  // Said once: the next heartbeat with the same disagreement is quiet.
  assert!(settle(&state, &mut clients, "b").is_empty());
}

#[test]
fn a_refused_service_is_out_of_routing() {
  let state = test_state();
  let mut clients = HashMap::new();
  clients.insert("a".to_string(), public(on("app.example.com")));
  clients.insert("b".to_string(), gated(on("app.example.com"), "u:p"));
  settle(&state, &mut clients, "b");
  let (pool, _) = crate::routing::select_client_pool(
    &clients,
    "/",
    Some("app.example.com"),
    false,
    std::time::Duration::from_secs(3600),
  )
  .unwrap();
  let ids: Vec<&str> = pool.iter().map(|r| r.client.as_str()).collect();
  assert_eq!(ids, ["a"]);
}

#[test]
fn clients_that_agree_serve_together() {
  let state = test_state();
  let mut clients = HashMap::new();
  clients.insert("a".to_string(), gated(on("app.example.com"), "u:p"));
  clients.insert("b".to_string(), gated(on("app.example.com"), "u:p"));
  assert!(settle(&state, &mut clients, "a").is_empty());
  assert!(settle(&state, &mut clients, "b").is_empty());
}

#[test]
fn a_different_hostname_or_path_is_a_different_route() {
  let state = test_state();
  let mut clients = HashMap::new();
  clients.insert("a".to_string(), public(on("app.example.com")));
  clients.insert("b".to_string(), gated(on("other.example.com"), "u:p"));
  let mut c = gated(on("app.example.com"), "u:p");
  c.sole_mut().declared_path = Some("/admin".to_string());
  clients.insert("c".to_string(), c);
  assert!(settle(&state, &mut clients, "b").is_empty());
  assert!(settle(&state, &mut clients, "c").is_empty());
}

#[test]
fn the_refusal_lifts_when_the_other_side_leaves() {
  let state = test_state();
  let mut clients = HashMap::new();
  clients.insert("a".to_string(), public(on("app.example.com")));
  clients.insert("b".to_string(), gated(on("app.example.com"), "u:p"));
  settle(&state, &mut clients, "b");
  clients.remove("a");
  assert!(settle(&state, &mut clients, "b").is_empty());
  assert!(clients["b"].sole().policy_conflict.is_none());
}

#[test]
fn a_connection_that_has_not_declared_yet_says_nothing() {
  let state = test_state();
  let mut clients = HashMap::new();
  let mut quiet = on("app.example.com");
  quiet.last_ping_at = None;
  clients.insert("a".to_string(), quiet);
  clients.insert("b".to_string(), public(on("app.example.com")));
  assert!(settle(&state, &mut clients, "b").is_empty());
}

#[test]
fn where_a_site_decides_access_only_the_rest_has_to_agree() {
  let mut cfg = test_config();
  cfg.sites = sites("- hostname: app.example.com\n  access: {method: aperio}\n");
  let state = test_state_with(cfg);
  let mut clients = HashMap::new();
  clients.insert("a".to_string(), public(on("app.example.com")));
  clients.insert("b".to_string(), gated(on("app.example.com"), "u:p"));
  assert!(
    settle(&state, &mut clients, "b").is_empty(),
    "the site decides access"
  );

  let mut c = public(on("app.example.com"));
  c.sole_mut().allowed_ips = vec!["10.0.0.0/8".to_string()];
  clients.insert("c".to_string(), c);
  let refusals = settle(&state, &mut clients, "c");
  assert_eq!(refusals.len(), 1);
  assert!(
    refusals[0].message.contains("`allowed_ips`"),
    "{}",
    refusals[0].message
  );
}
