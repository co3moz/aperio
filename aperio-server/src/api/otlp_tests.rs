//! What these pin down: that the bridge answers honestly when it is off or
//! misconfigured, and that identity comes from the token rather than the
//! payload.

use super::*;

#[test]
fn only_the_three_otlp_signals_are_forwarded() {
  assert_eq!(signal_path("traces"), Some("v1/traces"));
  assert_eq!(signal_path("metrics"), Some("v1/metrics"));
  assert_eq!(signal_path("logs"), Some("v1/logs"));
  // A signal we do not know is a 404 here rather than a forward: the
  // collector would answer the same way, later and less clearly.
  assert_eq!(signal_path("profiles"), None);
  assert_eq!(signal_path(""), None);
}

#[test]
fn identity_names_the_token_and_the_organization() {
  let mut perms = ClientPerms::master();
  perms.master = false;
  perms.token_name = Some("edge-01".to_string());
  let attrs = identity(&perms);
  assert!(attrs.contains(&("aperio.token".to_string(), "edge-01".to_string())));
  // No org: a master-organization client should not carry an empty one.
  assert!(!attrs.iter().any(|(k, _)| k == "aperio.org"));

  perms.org_id = Some("acme".to_string());
  let attrs = identity(&perms);
  assert!(attrs.contains(&("aperio.org".to_string(), "acme".to_string())));
}

#[test]
fn the_master_token_is_named_rather_than_left_blank() {
  // Telemetry attributed to nothing is telemetry nobody can filter.
  let perms = ClientPerms::master();
  assert_eq!(
    identity(&perms),
    vec![("aperio.token".to_string(), "master".to_string())]
  );
}

#[test]
fn the_bridge_needs_the_permission_on_the_token() {
  // The master token may, as it may everything else.
  assert!(may_bridge(&ClientPerms::master()));

  let mut perms = ClientPerms::master();
  perms.master = false;
  perms.allow_otel = false;
  // Off by default, for the same reason `topics` is: a capability that
  // switches itself on for every token that predates it is how a permission
  // model quietly stops meaning anything.
  assert!(!may_bridge(&perms));

  perms.allow_otel = true;
  assert!(may_bridge(&perms));
}

#[tokio::test]
async fn an_ip_fenced_token_is_judged_on_the_real_peer() {
  // The bug this pins down: the handler used a placeholder address, and the
  // token's `allowed_ips` fence is evaluated against exactly that value. A
  // token allow-listing loopback would have been accepted from anywhere on
  // the internet, and one fenced to a private range refused from the host it
  // was issued for.
  let mut config = crate::test_support::test_config();
  config.otel_bridge = true;
  let state = std::sync::Arc::new(crate::test_support::test_state_with(config));
  let (_record, secret) = state.token_store.lock().await.create(
    "edge".into(),
    Vec::new(),
    Vec::new(),
    // Fenced to loopback only.
    vec!["127.0.0.1".to_string()],
    None,
    None,
    None,
    false,
    false,
    false,
    None,
    Vec::new(),
    None,
    true,
  );

  let mut headers = axum::http::HeaderMap::new();
  headers.insert("authorization", format!("Bearer {secret}").parse().unwrap());

  // From somewhere else entirely: the fence must refuse it.
  let outside: std::net::SocketAddr = "203.0.113.7:40000".parse().unwrap();
  let resp = otlp_handler(
    axum::extract::State(state.clone()),
    axum::extract::ConnectInfo(outside),
    axum::extract::Path("traces".to_string()),
    headers.clone(),
    axum::body::Bytes::new(),
  )
  .await;
  assert_eq!(
    resp.status(),
    StatusCode::UNAUTHORIZED,
    "a token fenced to loopback must not be usable from a public address"
  );
}
