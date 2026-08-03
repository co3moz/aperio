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
