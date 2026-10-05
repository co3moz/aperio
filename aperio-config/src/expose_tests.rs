//! Public listener identity, file validation, socket conflicts and bounded
//! protocol limits; schema preparation must not enable an unimplemented relay.

use super::*;

fn entry(yaml: &str) -> ExposeEntry {
  serde_yaml::from_str(yaml).unwrap()
}

#[test]
fn resolved_file_identities_reject_default_address_aliases_even_when_disabled() {
  let supported = [ExposeProtocol::Tcp, ExposeProtocol::Udp];
  for protocol in ["tcp", "udp"] {
    for address in ["127.0.0.1", "0.0.0.0", "::1", "::"] {
      let first = entry(&format!(
        "port: 23456\ntunnel: echo\nprotocol: {protocol}\n"
      ));
      let mut second = first.clone();
      second.address = Some(address.parse().unwrap());
      second.enabled = false;
      let errors = validate_entries_at_address(
        &[first.clone(), second.clone()],
        &supported,
        address.parse().unwrap(),
      );
      assert!(errors.iter().any(|e| e.field == "expose[1].port"));
      // Address families and transports still have separate socket identities.
      second.protocol = if protocol == "tcp" { "udp" } else { "tcp" }.into();
      assert!(
        validate_entries_at_address(
          &[first.clone(), second.clone()],
          &supported,
          address.parse().unwrap()
        )
        .is_empty()
      );
      second.protocol = protocol.into();
      second.address = Some(
        if address.contains(':') {
          "127.0.0.1"
        } else {
          "::1"
        }
        .parse()
        .unwrap(),
      );
      assert!(
        validate_entries_at_address(&[first, second], &supported, address.parse().unwrap())
          .is_empty()
      );
    }
  }
}

fn tcp_spec() -> ExposeSpec {
  ExposeSpec {
    advertised_host: None,
    org_id: "org-1".into(),
    tunnel: "postgres".into(),
    listener: ExposeListener {
      address: "0.0.0.0".parse().unwrap(),
      port: 5432,
      protocol: ExposeProtocol::Tcp,
    },
    enabled: true,
    allowed_ips: Vec::new(),
    limits: ExposeLimits::Tcp {
      max_connections: 64,
      open_timeout_secs: 10,
      drain_timeout_secs: 30,
      ingress_bytes_per_second: default_bytes_per_second(),
      egress_bytes_per_second: default_bytes_per_second(),
    },
  }
}

fn udp_spec() -> ExposeSpec {
  let mut spec = tcp_spec();
  spec.listener.protocol = ExposeProtocol::Udp;
  spec.limits = ExposeLimits::Udp {
    max_sessions: 256,
    max_sessions_per_ip: 16,
    idle_timeout_secs: 60,
    max_datagram_bytes: 65507,
    queue_packets: 64,
    queue_bytes: 131014,
    new_sessions_per_second: 32,
    ingress_bytes_per_second: 1_000_000,
    egress_bytes_per_second: 1_000_000,
  };
  spec
}

#[test]
fn legacy_claims_keep_their_spelling_and_secret_is_not_in_diagnostics() {
  for (yaml, label) in [
    ("port: 2222\ntunnel: ssh", "tunnel master@ssh"),
    (
      "port: 2222\ntunnel: payments@ssh\norg: Payments",
      "tunnel payments@ssh",
    ),
    (
      "port: 2222\ntunnel: ssh\norg: payments",
      "tunnel payments@ssh",
    ),
    (
      "port: 2222\ntunnel: ssh\ntoken: ci",
      "tunnel ssh (token ci)",
    ),
    ("port: 2222\nkey: secret-secret", "a key-matched tunnel"),
  ] {
    let rule = entry(yaml);
    assert!(rule.validate(&[ExposeProtocol::Tcp]).is_empty());
    assert_eq!(rule.label(), label);
  }
  let errors = entry("port: 2222\nkey: secret").validate(&[ExposeProtocol::Tcp]);
  assert_eq!(errors[0].field, "key");
  assert!(!errors[0].to_string().contains("secret"));
}

#[test]
fn validation_reports_all_errors_with_indexed_fields() {
  let rules = vec![
    entry("port: 0\nprotocol: udp\ntunnel: Bad-Name"),
    entry("port: 22\ntunnel: acme@ssh\norg: beta"),
    entry("port: 22"),
  ];
  let fields: Vec<_> = validate_entries(&rules, &[ExposeProtocol::Tcp])
    .into_iter()
    .map(|e| e.field)
    .collect();
  assert_eq!(
    fields,
    [
      "expose[0].protocol",
      "expose[0].port",
      "expose[0].tunnel",
      "expose[1].org",
      "expose[2].tunnel",
      "expose[2].port"
    ]
  );
}

#[test]
fn transport_support_is_explicit_and_port_uniqueness_includes_protocol() {
  let tcp = entry("port: 53\ntunnel: dns");
  let udp = entry("port: 53\ntunnel: dns\nprotocol: udp");
  assert_eq!(
    validate_entries(&[tcp.clone(), udp.clone()], &[ExposeProtocol::Tcp]).len(),
    1
  );
  assert!(
    validate_entries(
      &[tcp.clone(), udp],
      &[ExposeProtocol::Tcp, ExposeProtocol::Udp]
    )
    .is_empty()
  );
  assert_eq!(
    validate_entries(&[tcp.clone(), tcp], &[ExposeProtocol::Tcp])[0].field,
    "expose[1].port"
  );
}

#[test]
fn schema_and_runtime_parser_reject_unknown_file_fields() {
  assert!(serde_yaml::from_str::<ExposeEntry>("port: 22\ntunnel: ssh\ntunel: ssh").is_err());
}

#[test]
fn listener_conflicts_respect_transport_family_and_wildcards() {
  let base = tcp_spec().listener;
  let mut other = base.clone();
  other.address = "127.0.0.1".parse().unwrap();
  assert!(base.conflicts_with(&other));
  assert!(other.conflicts_with(&base));
  let mut specific = other.clone();
  specific.address = "127.0.0.2".parse().unwrap();
  assert!(!specific.conflicts_with(&other));
  other.protocol = ExposeProtocol::Udp;
  assert!(!base.conflicts_with(&other));
  other.protocol = ExposeProtocol::Tcp;
  other.address = "::".parse().unwrap();
  assert!(!base.conflicts_with(&other));
  specific.address = "::1".parse().unwrap();
  assert!(other.conflicts_with(&specific));
  specific.port += 1;
  assert!(!other.conflicts_with(&specific));
}

#[test]
fn resource_round_trip_preserves_identity_and_has_no_observed_state() {
  for spec in [tcp_spec(), udp_spec()] {
    assert!(spec.validate().is_empty());
    let rule = ExposeResource {
      id: "expose-1".into(),
      revision: 3,
      source: ExposeSource::Api,
      spec,
    };
    let json = serde_json::to_value(&rule).unwrap();
    assert!(json.get("status").is_none());
    assert_eq!(
      serde_json::from_value::<ExposeResource>(json).unwrap(),
      rule
    );
  }
}

#[test]
fn managed_spec_rejects_ambiguous_identity_and_mapped_ipv6() {
  let mut spec = tcp_spec();
  spec.org_id = "*".into();
  spec.tunnel = "other@postgres".into();
  spec.listener.port = 0;
  spec.listener.address = "::ffff:127.0.0.1".parse().unwrap();
  let fields: Vec<_> = spec.validate().into_iter().map(|e| e.field).collect();
  assert_eq!(
    fields,
    ["org_id", "tunnel", "listener.port", "listener.address"]
  );
}

#[test]
fn protocol_limits_cannot_be_unbounded_or_mismatched() {
  let mut spec = udp_spec();
  spec.listener.protocol = ExposeProtocol::Tcp;
  if let ExposeLimits::Udp {
    idle_timeout_secs,
    max_sessions_per_ip,
    queue_bytes,
    max_datagram_bytes,
    egress_bytes_per_second,
    ..
  } = &mut spec.limits
  {
    *idle_timeout_secs = 0;
    *max_sessions_per_ip = 257;
    *queue_bytes = 100;
    *max_datagram_bytes = 65508;
    *egress_bytes_per_second = 0;
  }
  let fields: Vec<_> = spec.validate().into_iter().map(|e| e.field).collect();
  for expected in [
    "limits.idle_timeout_secs",
    "limits.max_sessions_per_ip",
    "limits.queue_bytes",
    "limits.max_datagram_bytes",
    "limits.egress_bytes_per_second",
    "limits.protocol",
  ] {
    assert!(fields.iter().any(|f| f == expected), "missing {expected}");
  }
}
