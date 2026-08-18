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

// ---------------------------------------------------------------------------
// A file-wide depends_on
// ---------------------------------------------------------------------------

/// `depends_on:` at the top of the file is the default for entries that name
/// none of their own.
///
/// It was in the JSON Schema, so editors completed it and `--check-config`
/// accepted it, and nothing ever read it: only the per-entry key was. A
/// setting an operator can write, that validates, and that does nothing, is
/// worse than one that does not exist.
#[test]
fn a_file_wide_depends_on_reaches_the_services_that_declare_none() {
  let mut settings = base_settings();
  settings.depends_on = Some(vec!["db".to_string()]);
  settings.services = vec![
    ServiceEntry {
      name: Some("db".to_string()),
      target: Some("http://localhost:5432".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      // Its own list wins over the file's, rather than merging with it.
      depends_on: Some(vec!["web".to_string()]),
      ..Default::default()
    },
  ];
  let specs: Vec<ServiceSpec> = build_specs(&settings, "base-id", false).unwrap();
  let of = |name: &str| -> Vec<String> {
    specs
      .iter()
      .find(|s| s.name.as_deref() == Some(name))
      .expect("service is in the list")
      .depends_on
      .clone()
  };
  assert_eq!(of("web"), vec!["db".to_string()], "the file's list applies");
  assert_eq!(of("api"), vec!["web".to_string()], "its own list wins");
  assert!(
    of("db").is_empty(),
    "a service the file-wide list names is one of the things being waited for, \
     not one of the waiters; making it wait for itself refuses to start"
  );
}

/// A file-wide list naming several services in the file is not a cycle.
///
/// This is why the fallback cannot be a plain `or_else`, and why dropping the
/// self-reference alone is not enough either. `depends_on: [a, b]` over
/// services `a` and `b` would leave each waiting for the other, and
/// `validate_depends_on` refuses a cycle by exiting, so an ordinary file
/// would stop the client from starting at all.
#[test]
fn a_file_wide_depends_on_naming_every_service_still_starts() {
  let mut settings = base_settings();
  settings.depends_on = Some(vec!["a".to_string(), "b".to_string()]);
  settings.services = vec![
    ServiceEntry {
      name: Some("a".to_string()),
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("b".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  validate_depends_on(&specs).expect("a file-wide list over every service is not a cycle");
}

/// A file-wide `depends_on:` must not refuse a file that has an unnamed entry.
///
/// `validate_depends_on` rejects a spec that carries a `depends_on` and has no
/// name, because there is nothing for the others to wait *for*. That rule is
/// about an entry declaring its own list. Handing the file-wide default to a
/// nameless entry turns it into the same refusal, so a file that started fine
/// yesterday stops starting today.
#[test]
fn a_file_wide_depends_on_does_not_refuse_a_file_with_an_unnamed_service() {
  let mut settings = base_settings();
  settings.depends_on = Some(vec!["db".to_string()]);
  settings.services = vec![
    ServiceEntry {
      name: Some("db".to_string()),
      target: Some("http://localhost:5432".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      // No name, which is legal for a service nothing depends on.
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  validate_depends_on(&specs)
    .expect("an unnamed entry must not inherit a list it cannot be validated against");
}
