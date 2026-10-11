//! What has to be settled before a connection can serve a service: which
//! visitor gate this server will accept, and the refusals that hold a service
//! back rather than serving it under a gate nobody wrote for it.

use super::*;

// --- negotiate_visitor_gate --------------------------------------------------

/// Parses a policy the way a config file would carry it.
fn policy_of(yaml: &str) -> aperio_config::AuthSetting {
  serde_yaml::from_str(yaml).expect("a valid auth: value")
}

#[test]
fn a_server_that_names_no_method_is_declaring_that_none_may_be_sent() {
  // What a server answers a connection whose token may not control the
  // visitor gate. It is not an old server (it sent the header) and not a
  // method it fails to understand: it is this connection that may declare
  // nothing, so the gate would be dropped and the route served open.
  let gate = policy_of("{method: basic, users: \"admin:s3cret\"}");
  match negotiate_visitor_gate(Some(""), Some(&gate)) {
    GateNegotiation::Unsupported { wanted, accepted } => {
      assert_eq!(wanted, vec!["basic"]);
      assert!(
        accepted.is_empty(),
        "the server named nothing: {accepted:?}"
      );
    }
    other => panic!("a gate cannot be declared where nothing may be: {other:?}"),
  }

  // But `method: none` is not a gate to lose. It says "serve this to anyone",
  // and a server that will not take that instruction keeps whatever gate is
  // already in front of the route, which is narrower than what was asked for
  // rather than wider. Refusing to serve over that would take a site down to
  // protect it from being less open than intended.
  assert_eq!(
    negotiate_visitor_gate(Some(""), Some(&policy_of("{method: none}"))),
    GateNegotiation::Scalar
  );
}

#[test]
fn a_server_that_accepts_the_method_is_told_the_whole_policy() {
  let rich = policy_of("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  match negotiate_visitor_gate(Some("none,basic,bearer,jwt"), Some(&rich)) {
    GateNegotiation::Methods(specs) => {
      assert_eq!(specs.len(), 1);
      assert_eq!(specs[0].method, "bearer");
    }
    other => panic!("expected the policy to travel: {other:?}"),
  }
}

#[test]
fn a_server_that_accepts_some_of_them_still_refuses_the_service() {
  // Announcing the half it understands would leave the other half of the gate
  // unenforced, which is a weaker gate than the one written.
  let mixed = policy_of(
    "[{method: basic, users: \"a:b\"}, {method: jwt, hmac_secret: \"0123456789abcdef-secret\"}]",
  );
  match negotiate_visitor_gate(Some("none,basic,bearer"), Some(&mixed)) {
    GateNegotiation::Unsupported { wanted, .. } => assert_eq!(wanted, vec!["jwt"]),
    other => panic!("expected a refusal naming `jwt`: {other:?}"),
  }
}

#[test]
fn the_announcement_is_read_forgivingly_but_never_widened() {
  let rich = policy_of("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  // Spacing and case are the server's business, not a reason to refuse.
  assert!(matches!(
    negotiate_visitor_gate(Some(" NONE , Basic ,BEARER "), Some(&rich)),
    GateNegotiation::Methods(_)
  ));
  // An empty announcement is not "everything": a header the server sent but
  // could not fill is still a server that named no method.
  assert!(matches!(
    negotiate_visitor_gate(Some("   "), Some(&rich)),
    GateNegotiation::Unsupported { .. }
  ));
}

/// Two named services that would share one connection. Local to this file: the
/// socket-driven version beside `run_service` needs live ports, and these tests
/// only need two specs to look up between.
fn two_multiplexed(ws_url: &str) -> Vec<ServiceRuntime> {
  let mut web = crate::service::tests::test_spec(ws_url, "http://127.0.0.1:9");
  web.name = Some("web".to_string());
  web.multiplex = true;
  let mut api = crate::service::tests::test_spec(ws_url, "http://127.0.0.1:10");
  api.name = Some("api".to_string());
  api.multiplex = true;
  vec![web, api]
    .into_iter()
    .map(|s| {
      let health = BackendHealth::for_spec(&s);
      ServiceRuntime::new(s, health)
    })
    .collect()
}

#[test]
fn a_named_dispatch_finds_its_own_service_and_an_unknown_one_falls_back() {
  let services = two_multiplexed("ws://127.0.0.1:1/");
  let announced = vec![0usize, 1];
  assert_eq!(service_for(&services, &announced, &Some("web".into())), 0);
  assert_eq!(service_for(&services, &announced, &Some("api".into())), 1);
  // A name this connection does not carry, and the pre-v8 spelling: both fall
  // back rather than dropping a request the server has committed to.
  assert_eq!(service_for(&services, &announced, &Some("gone".into())), 0);
  assert_eq!(service_for(&services, &announced, &None), 0);
}

#[test]
fn a_withheld_service_is_never_dispatched_to() {
  // `web` could not be announced (its gate was refused), so a frame naming it,
  // and a frame naming nothing, must land on a service this connection does
  // offer rather than on the backend it deliberately held back.
  let services = two_multiplexed("ws://127.0.0.1:1/");
  let announced = vec![1usize];
  assert_eq!(service_for(&services, &announced, &Some("web".into())), 1);
  assert_eq!(service_for(&services, &announced, &None), 1);
  assert_eq!(service_for(&services, &announced, &Some("api".into())), 1);
}
