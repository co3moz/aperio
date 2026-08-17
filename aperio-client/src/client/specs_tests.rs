//! Which services end up sharing a connection, and what that costs them: the
//! grouping itself, the name each member has to have, and the pool and
//! bandwidth a multiplexed service does not get to keep.

use super::*;
use crate::service::ServiceSpec;
use crate::tests::*;
use aperio_config::ServiceEntry;

/// below. `n` entries, each named and pointed at its own port.
/// A `services:` list whose entries all opt into multiplexing, for the tests
/// below. `n` entries, each named and pointed at its own port.
pub(crate) fn multiplexed_services(n: usize) -> Vec<ServiceEntry> {
  (0..n)
    .map(|i| ServiceEntry {
      name: Some(format!("svc{i}")),
      target: Some(format!("http://localhost:{}", 3000 + i)),
      multiplex: Some(true),
      ..Default::default()
    })
    .collect()
}

#[test]
pub(crate) fn multiplexed_services_share_one_group() {
  let mut settings = base_settings();
  settings.services = multiplexed_services(3);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  // One group, and every service in it: the ids are what `spawn_services`
  // groups by, so two groups here would be two connections.
  assert_eq!(
    specs.iter().map(|s| s.multiplex_group).collect::<Vec<_>>(),
    vec![Some(0), Some(0), Some(0)]
  );
}

#[test]
fn a_service_that_asks_to_multiplex_alone_keeps_its_own_connection() {
  // Nobody to share with is not an error and not a group: a group of one is
  // the ordinary connection it would have had anyway, and announcing a
  // one-entry `services` list instead would only narrow which servers can
  // read the Ping.
  let mut settings = base_settings();
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      multiplex: Some(true),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].multiplex_group, None);
  assert_eq!(specs[1].multiplex_group, None);
  // What it asked for is still recorded, so nothing downstream has to guess
  // why it is ungrouped.
  assert!(specs[0].multiplex);
  assert!(!specs[1].multiplex);
}

#[test]
fn a_file_wide_multiplex_can_be_turned_off_per_entry() {
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("bulk".to_string()),
      target: Some("http://localhost:5000".to_string()),
      multiplex: Some(false),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].multiplex_group, Some(0));
  assert_eq!(specs[1].multiplex_group, Some(0));
  // The entry that opted out keeps a connection of its own, which is the
  // point of being able to say `multiplex: false` in a file that turned it on
  // for everything: one service whose responses are large should not occupy
  // the writer the small ones send through.
  assert_eq!(specs[2].multiplex_group, None);
}

#[test]
fn a_multiplexed_service_must_be_named() {
  // Two unnamed services on one connection are told apart only by their
  // position in a list, and a name is what the server keeps routing, ejection
  // and statistics under. Refused at config time, where it is one line to fix.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = vec![
    ServiceEntry {
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
  ];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("multiplexed service needs a name"), "{err}");
}

#[test]
fn multiplexing_overrides_a_per_service_connection_pool_and_says_so() {
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      connections: Some(aperio_config::Connections::Fixed(4)),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      connections: Some(aperio_config::Connections::Range(
        aperio_config::ConnectionRange {
          min: Some(2),
          max: Some(8),
        },
      )),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  // One connection is what multiplexing means, so the pool is not something
  // these services can also have.
  for spec in &specs {
    assert_eq!(spec.connections, 1);
    assert_eq!(spec.connections_min, 1);
  }
  // Reported rather than silently dropped: the dashboard's config view is
  // where a value that did not survive its config is supposed to show up.
  let note = |spec: &ServiceSpec| {
    spec
      .config_notes
      .iter()
      .find(|n| n.field == "connections")
      .cloned()
      .unwrap_or_else(|| panic!("a note about connections"))
  };
  assert_eq!(note(&specs[0]).declared, "4");
  assert_eq!(note(&specs[0]).effective, "1");
  assert_eq!(note(&specs[1]).declared, "2-8");
  assert!(note(&specs[1]).reason.contains("share one connection"));
}

#[test]
fn a_service_left_on_its_own_connection_keeps_its_pool() {
  // The clamp is the group's, not the flag's: an entry that opted out is
  // untouched even in a file that multiplexes everything else.
  let mut settings = base_settings();
  settings.multiplex = true;
  let mut services = multiplexed_services(2);
  services.push(ServiceEntry {
    name: Some("bulk".to_string()),
    target: Some("http://localhost:9000".to_string()),
    multiplex: Some(false),
    connections: Some(aperio_config::Connections::Fixed(4)),
    ..Default::default()
  });
  settings.services = services;
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[2].connections, 4);
  assert!(
    specs[2]
      .config_notes
      .iter()
      .all(|n| n.field != "connections")
  );
}

#[test]
fn more_multiplexed_services_than_a_server_accepts_is_a_config_error() {
  // The server answers a longer list by dropping the connection, so refusing
  // here is what lets the message name the file: otherwise the operator sees a
  // client that connects and disconnects with the reason in somebody else's
  // log.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = multiplexed_services(service::MAX_MULTIPLEXED_SERVICES + 1);
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("share one connection"), "{err}");

  // Exactly at the ceiling is fine; the bound is a fence, not a limit anybody
  // legitimate is meant to feel.
  settings.services = multiplexed_services(service::MAX_MULTIPLEXED_SERVICES);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs.len(), service::MAX_MULTIPLEXED_SERVICES);
  assert!(specs.iter().all(|s| s.multiplex_group == Some(0)));
}
