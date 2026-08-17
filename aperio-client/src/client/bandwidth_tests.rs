//! What a service announces once the whole file is known: how a budget is
//! split across services and their connections, what a multiplexed group is
//! paced at, the tunnels it declares, and the startup line it prints.

use super::*;
use crate::client::specs::tests::multiplexed_services;
use crate::config::ClientSettings;
use crate::tests::*;
use aperio_config::ServiceEntry;

/// A `services:` entry with just a target, an optional bandwidth request and
/// an optional parallel-connection count.
fn bw_service(name: &str, bandwidth: Option<&str>, connections: u32) -> ServiceEntry {
  ServiceEntry {
    name: Some(name.to_string()),
    target: Some("http://localhost:3000".to_string()),
    bandwidth: bandwidth.map(|s| s.to_string()),
    connections: Some(aperio_config::Connections::Fixed(connections)),
    ..Default::default()
  }
}

/// Maps service name to the rate a single connection of it announces.
fn announced(settings: &ClientSettings) -> Vec<(String, Option<u64>)> {
  build_specs(settings, "id", false)
    .unwrap()
    .into_iter()
    .map(|s| (s.name.clone().unwrap_or_default(), s.bandwidth_bps))
    .collect()
}

#[test]
fn test_config_notes_report_declared_versus_announced() {
  init_tracing();
  // A service whose budget share is divided across its connections announces
  // a rate the operator never wrote, so it reports both sides for the
  // dashboard's config view.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![bw_service("x", Some("10mbit"), 10)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = specs[0]
    .config_notes
    .iter()
    .find(|n| n.field == "bandwidth")
    .expect("a divided rate is reported");
  assert_eq!(note.declared, "10mbit");
  assert_eq!(note.effective, "1mbit");
  assert!(
    note.reason.contains("split across 10 parallel connections"),
    "got: {}",
    note.reason
  );

  // A service that asked for nothing and took a share of the budget reports
  // it too, with an empty `declared` standing for "nothing was configured".
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", None, 1), bw_service("y", None, 1)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = &specs[0].config_notes[0];
  assert_eq!(note.declared, "");
  assert_eq!(note.effective, "1mbit");

  // A rate that fits the budget on its own is announced as written, so there
  // is nothing to report.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("1mbit"), 1)];
  assert!(
    build_specs(&settings, "id", false).unwrap()[0]
      .config_notes
      .is_empty()
  );
}

#[test]
fn test_config_notes_report_invalid_and_clamped_values() {
  init_tracing();
  // An unparseable rate is ignored; the note says so rather than leaving the
  // dashboard to show an unexplained "unlimited".
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("very fast"), 1)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = &specs[0].config_notes[0];
  assert_eq!(note.field, "bandwidth");
  assert_eq!(note.declared, "very fast");
  assert_eq!(note.effective, "unlimited");

  // Past the sanity bound: what was asked for, next to what runs. The
  // server's own ceiling is applied at connect time and reported in the
  // client's log, not here, since this runs before anything has connected.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", None, 100_000)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = &specs[0].config_notes[0];
  assert_eq!(note.field, "connections");
  assert_eq!(note.declared, "100000");
  assert_eq!(note.effective, "256");
}

#[test]
fn test_bandwidth_split_across_parallel_connections() {
  init_tracing();
  // Scenario A: a service's own limit is divided by its connections, since
  // the server shapes each connection with a bucket of its own.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("10mbit"), 10)];
  assert_eq!(announced(&settings), vec![("x".into(), Some(125_000))]);

  // The same holds in single-service mode, where the top-level value is both
  // the budget and the only service's request.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.connections = Some(aperio_config::Connections::Fixed(4));
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].bandwidth_bps, Some(312_500));
}

#[test]
fn test_bandwidth_without_budget_leaves_others_unlimited() {
  init_tracing();
  // Scenarios B and H: with no top-level budget there is nothing to settle
  // requests against, so a service keeps what it asked for and a service that
  // asked for nothing stays unlimited.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("1mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(125_000)), ("y".into(), None)]
  );

  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("3mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(375_000)), ("y".into(), None)]
  );
}

#[test]
fn test_bandwidth_budget_split_equally_then_per_connection() {
  init_tracing();
  // Scenario C: no service named a rate, so the budget is split equally per
  // service (not per connection), then divided within each service.
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", None, 2), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(62_500)), ("y".into(), Some(125_000))]
  );
}

#[test]
fn test_bandwidth_requests_starving_others_are_dropped() {
  init_tracing();
  // Scenario D: x claims the whole budget, leaving y nothing. Every named
  // rate is dropped and the budget is split equally instead.
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", Some("2mbit"), 2), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(62_500)), ("y".into(), Some(125_000))]
  );

  // The same rule covers an overshoot with an unspecified service present.
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", Some("4mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(125_000)), ("y".into(), Some(125_000))]
  );
}

#[test]
fn test_bandwidth_remainder_goes_to_unspecified_services() {
  init_tracing();
  // Scenario E: x keeps its 3mbit, y gets the remaining 7mbit.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![bw_service("x", Some("3mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(375_000)), ("y".into(), Some(875_000))]
  );

  // Scenario G: the remainder is shared equally among the services without a
  // request of their own.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![
    bw_service("x", Some("3mbit"), 1),
    bw_service("y", None, 1),
    bw_service("z", None, 1),
  ];
  assert_eq!(
    announced(&settings),
    vec![
      ("x".into(), Some(375_000)),
      ("y".into(), Some(437_500)),
      ("z".into(), Some(437_500)),
    ]
  );
}

#[test]
fn test_bandwidth_over_budget_requests_scale_proportionally() {
  init_tracing();
  // Scenario F: every service named a rate and together they overshoot, so
  // the rates keep their relative weight and are scaled to fit (3+7 over a
  // 5mbit budget becomes 1.5 and 3.5).
  let mut settings = base_settings();
  settings.bandwidth = Some("5mbit".to_string());
  settings.services = vec![
    bw_service("x", Some("3mbit"), 1),
    bw_service("y", Some("7mbit"), 1),
  ];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(187_500)), ("y".into(), Some(437_500))]
  );

  // Under budget, named rates are left alone and the surplus stays unused.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![
    bw_service("x", Some("3mbit"), 1),
    bw_service("y", Some("1mbit"), 1),
  ];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(375_000)), ("y".into(), Some(125_000))]
  );
}

#[test]
fn test_bandwidth_share_never_rounds_to_unlimited() {
  init_tracing();
  // A share small enough to floor to 0 is clamped to 1 byte/s: the server
  // reads an announced 0 as unlimited, the opposite of a tiny share.
  let mut settings = base_settings();
  settings.bandwidth = Some("10".to_string());
  settings.services = vec![bw_service("x", None, 16), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(1)), ("y".into(), Some(5))]
  );
}

#[test]
fn test_log_spec_all_branches() {
  init_tracing();
  // A richly configured named service touches every optional log line.
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("web".to_string()),
    target: Some("http://localhost:3000".to_string()),
    path: Some("/api".to_string()),
    hostname: Some(aperio_config::Hostnames::Many(vec![
      "a.example.com".to_string(),
      "b.example.com".to_string(),
    ])),
    max_concurrent: Some(8),
    priority: Some(5),
    bandwidth: Some("8mbit".to_string()),
    connections: Some(aperio_config::Connections::Fixed(4)),
    tcp_target: Some("127.0.0.1:5432".to_string()),
    public: Some(true),
    auth: Some(aperio_config::AuthSetting::Credentials(
      "user:pass".to_string(),
    )),
    ..Default::default()
  }];
  settings.tunnels = vec![tcp_tunnel("127.0.0.1:6000")];
  // Multiple failover servers so the failover log line runs.
  unsafe { std::env::set_var("APERIO_SERVER_URLS", "https://backup.example.com") };
  let specs = build_specs(&settings, "id", false).unwrap();
  unsafe { std::env::remove_var("APERIO_SERVER_URLS") };
  for spec in &specs {
    log_spec(spec);
  }

  // The single, unnamed, tunnels-only variant: empty target + single hostname.
  let mut settings = base_settings();
  settings.target = None;
  settings.hostnames = vec!["only.example.com".to_string()];
  settings.tunnels = vec![tcp_tunnel("127.0.0.1:6001")];
  let specs = build_specs(&settings, "id", false).unwrap();
  log_spec(&specs[0]);

  // A plain single service with no hostnames at all.
  let mut settings = base_settings();
  settings.hostnames = Vec::new();
  let specs = build_specs(&settings, "id", false).unwrap();
  log_spec(&specs[0]);
}

// ---------------------------------------------------------------------------
// The combined `tcp/udp` declaration: one tunnel, both transports.
// ---------------------------------------------------------------------------

#[test]
fn test_validate_tunnels_accepts_the_combined_protocol() {
  let decl = |protocol: &str| protocol::TunnelDecl {
    custom_name: None,
    name: Some("dns".to_string()),
    target: "192.168.3.100:53".to_string(),
    protocol: protocol.to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: Some(30),
    expose: None,
  };

  let out = validate_tunnels(&[decl("tcp/udp")]).expect("tcp/udp is accepted");
  assert_eq!(out[0].protocol, "tcp/udp");
  // The idle timeout belongs to the datagram half, so a combined tunnel keeps
  // it rather than being told it is a tcp-only setting.
  assert_eq!(out[0].idle_timeout, Some(30));

  // Written the other way round it means the same thing, and is normalized so
  // everything downstream compares against one spelling.
  let out = validate_tunnels(&[decl("UDP/TCP")]).expect("udp/tcp is the same declaration");
  assert_eq!(out[0].protocol, "tcp/udp");
}

#[test]
fn test_validate_tunnels_refuses_encrypt_on_a_combined_tunnel() {
  // Encryption is the tcp-only handshake; accepting it here would leave the
  // udp half in the clear under a flag that says otherwise.
  let err = validate_tunnels(&[protocol::TunnelDecl {
    custom_name: None,
    name: Some("dns".to_string()),
    target: "192.168.3.100:53".to_string(),
    protocol: "tcp/udp".to_string(),
    encrypt: true,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  }])
  .unwrap_err();
  assert!(err.contains("only supported for tcp tunnels"), "got: {err}");
}

#[test]
fn test_validate_tunnels_allows_expose_on_a_combined_tunnel() {
  // A public port relays TCP; the tunnel's tcp half qualifies.
  let out = validate_tunnels(&[protocol::TunnelDecl {
    custom_name: None,
    name: Some("dns".to_string()),
    target: "192.168.3.100:53".to_string(),
    protocol: "tcp/udp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: Some("a-long-shared-secret".to_string()),
  }])
  .expect("expose is accepted on the tcp half");
  assert_eq!(out.len(), 1);
}

// ---------------------------------------------------------------------------
// depends_on validation (planned_features #62)
// ---------------------------------------------------------------------------

/// Two service entries with the given names and dependencies.

#[test]
fn a_multiplexed_group_announces_the_budget_it_actually_gets_paced_at() {
  // The server shapes the socket, not the service: every service on a
  // connection announces into one token bucket and the last one wins. A share
  // per service is right when each has a connection of its own and wrong when
  // they share one, and the wrongness is silent and large: four services
  // splitting an 8mbit budget announced 2mbit each, the cell held 2mbit, and a
  // link sized at 8 ran at 2. At forty services it is a fortieth.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.bandwidth = Some("8mbit".to_string());
  settings.services = multiplexed_services(4);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  let budget = parse_bandwidth("8mbit").unwrap();
  for spec in &specs {
    assert_eq!(spec.bandwidth_bps, Some(budget));
  }
  // Said out loud, since what a service announces is no longer its own share.
  let note = specs[0]
    .config_notes
    .iter()
    .find(|n| n.field == "bandwidth")
    .expect("a note about bandwidth");
  assert!(
    note.reason.contains("share one shaped connection"),
    "{note:?}"
  );
}

#[test]
fn one_uncapped_service_uncaps_the_connection_it_shares_and_says_so() {
  // The server reads an absent limit as zero and zero as unlimited, so a
  // member without one wipes the cell whatever its neighbours declared. The
  // cap was already not being enforced; the only question was whether anything
  // said so. Capping the socket at the declared ones instead would throttle a
  // service the file never limited.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = multiplexed_services(3);
  settings.services[0].bandwidth = Some("4mbit".to_string());
  let specs = build_specs(&settings, "base-id", false).unwrap();
  for spec in &specs {
    assert_eq!(spec.bandwidth_bps, None);
  }
  let note = specs[0]
    .config_notes
    .iter()
    .find(|n| n.field == "bandwidth")
    .expect("a note about bandwidth");
  assert_eq!(note.effective, "unlimited");
  assert!(note.reason.contains("declares no limit"), "{note:?}");
}

#[test]
fn a_service_on_its_own_connection_still_splits_its_bandwidth_per_connection() {
  // The fix is the group's, not the flag's: an ordinary service keeps the
  // per-connection division, which is right because the server shapes each of
  // its connections separately.
  let mut settings = base_settings();
  settings.bandwidth = Some("8mbit".to_string());
  settings.connections = Some(aperio_config::Connections::Fixed(4));
  let specs = build_specs(&settings, "base-id", false).unwrap();
  let budget = parse_bandwidth("8mbit").unwrap();
  assert_eq!(specs[0].bandwidth_bps, Some(budget / 4));
}
