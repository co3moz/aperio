//! Which services end up sharing a connection, and what that costs them: the
//! grouping itself, what the default makes of a service that says nothing, and
//! the pool and bandwidth a multiplexed service does not get to keep. Also
//! where `depends_on` and `server_side` may be written.

use super::*;
use crate::service::{Multiplex, ServiceSpec};
use crate::tests::*;

/// A `services:` map whose entries all opt into multiplexing. `n` entries,
/// each pointed at its own port.
pub(crate) fn multiplexed_services(n: usize) -> String {
  let mut yaml = String::from("services:\n");
  for i in 0..n {
    yaml.push_str(&format!(
      "  svc{i}: {{target: 'http://localhost:{}', multiplex: true}}\n",
      3000 + i
    ));
  }
  yaml
}

/// The same map with nothing said about multiplexing, which is what most
/// files with several services look like.
fn plain_services(n: usize) -> String {
  let mut yaml = String::from("services:\n");
  for i in 0..n {
    yaml.push_str(&format!(
      "  svc{i}: {{target: 'http://localhost:{}'}}\n",
      3000 + i
    ));
  }
  yaml
}

#[test]
pub(crate) fn multiplexed_services_share_one_group() {
  let specs = specs(&multiplexed_services(3));
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
  // the ordinary connection it would have had anyway.
  let specs = specs(
    "services:\n  \
       web: {target: 'http://localhost:3000', multiplex: true}\n  \
       api: {target: 'http://localhost:4000', multiplex: false}\n",
  );
  assert_eq!(specs[0].multiplex_group, None);
  assert_eq!(specs[1].multiplex_group, None);
  // What it asked for is still recorded, so nothing downstream has to guess
  // why it is ungrouped.
  assert_eq!(specs[0].multiplex, Multiplex::Asked);
  assert_eq!(specs[1].multiplex, Multiplex::No);
}

#[test]
fn a_default_multiplex_can_be_turned_off_per_entry() {
  let specs = specs(
    "defaults: {multiplex: true}\n\
     services:\n  \
       web: {target: 'http://localhost:3000'}\n  \
       api: {target: 'http://localhost:4000'}\n  \
       bulk: {target: 'http://localhost:5000', multiplex: false}\n",
  );
  assert_eq!(specs[0].multiplex_group, Some(0));
  assert_eq!(specs[1].multiplex_group, Some(0));
  // The entry that opted out keeps a connection of its own, which is the
  // point of being able to say `multiplex: false` when `defaults:` turned it
  // on: one service whose responses are large should not occupy the writer
  // the small ones send through.
  assert_eq!(specs[2].multiplex_group, None);
}

#[test]
fn the_one_service_of_the_command_line_never_multiplexes() {
  // It has nobody to share with, whatever `defaults:` or the environment say.
  let specs = build_specs(&base_with("defaults: {multiplex: true}"), "base-id").unwrap();
  assert_eq!(specs[0].multiplex, Multiplex::No);
  assert_eq!(specs[0].multiplex_group, None);
}

#[test]
fn multiplexing_overrides_a_per_service_connection_pool_and_says_so() {
  let specs = specs(
    "defaults: {multiplex: true}\n\
     services:\n  \
       web: {target: 'http://localhost:3000', connections: 4}\n  \
       api: {target: 'http://localhost:4000', connections: {min: 2, max: 8}}\n",
  );
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
  let yaml = format!(
    "defaults: {{multiplex: true}}\n{}  bulk: {{target: 'http://localhost:9000', multiplex: false, connections: 4}}\n",
    multiplexed_services(2)
  );
  let specs = specs(&yaml);
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
  let err = specs_err(&multiplexed_services(service::MAX_MULTIPLEXED_SERVICES + 1));
  assert!(err.contains("share one connection"), "{err}");

  // Exactly at the ceiling is fine; the bound is a fence, not a limit anybody
  // legitimate is meant to feel.
  let specs = specs(&multiplexed_services(service::MAX_MULTIPLEXED_SERVICES));
  assert_eq!(specs.len(), service::MAX_MULTIPLEXED_SERVICES);
  assert!(specs.iter().all(|s| s.multiplex_group == Some(0)));
}

// ---------------------------------------------------------------------------
// What silence means: sharing for several services, a pool for one
// ---------------------------------------------------------------------------

#[test]
fn several_services_share_one_connection_unless_told_otherwise() {
  let specs = specs(&plain_services(3));
  assert!(specs.iter().all(|s| s.multiplex == Multiplex::ByDefault));
  assert!(specs.iter().all(|s| s.multiplex_group == Some(0)));
  // A shared connection is one connection, and nothing was written that said
  // otherwise, so there is no pool to clamp and nothing to report.
  for spec in &specs {
    assert_eq!((spec.connections_min, spec.connections), (1, 1));
    assert!(spec.config_notes.iter().all(|n| n.field != "connections"));
  }
}

#[test]
fn a_service_that_a_shared_socket_would_change_keeps_its_own_by_default() {
  // Each of these says something sharing would quietly undo: a pool it would
  // cut to one, a cap it would turn into the group's, a start it would hand to
  // every sibling, a raw stream it could not address.
  let specs = specs(
    "services:\n  \
       web: {target: 'http://localhost:3000'}\n  \
       api: {target: 'http://localhost:4000'}\n  \
       paced: {target: 'http://localhost:4001', bandwidth: 1mbit}\n  \
       pooled: {target: 'http://localhost:4002', connections: 4}\n  \
       elastic: {target: 'http://localhost:4003', connections: {min: 1, max: 3}}\n  \
       delayed: {target: 'http://localhost:4004', startup_delay: 5s}\n  \
       ordered: {target: 'http://localhost:4005', depends_on: [web]}\n  \
       raw: {tcp_target: '127.0.0.1:5432'}\n",
  );
  assert_eq!(specs[0].multiplex_group, Some(0));
  assert_eq!(specs[1].multiplex_group, Some(0));
  for spec in &specs[2..] {
    assert_eq!(spec.multiplex, Multiplex::No, "{}", spec.label());
    assert_eq!(spec.multiplex_group, None, "{}", spec.label());
  }
  // What they wrote is what they run, with no note about a clamp.
  assert_eq!((specs[3].connections_min, specs[3].connections), (4, 4));
  assert_eq!((specs[4].connections_min, specs[4].connections), (1, 3));
}

#[test]
fn asking_still_shares_what_the_default_would_leave_alone() {
  // The exclusions only decide what silence means.
  let specs = specs(
    "services:\n  \
       web: {target: 'http://localhost:3000'}\n  \
       delayed: {target: 'http://localhost:4004', startup_delay: 5s, multiplex: true}\n",
  );
  assert_eq!(specs[1].multiplex, Multiplex::Asked);
  assert_eq!(specs[0].multiplex_group, Some(0));
  assert_eq!(specs[1].multiplex_group, Some(0));
}

#[test]
fn a_fixed_single_connection_still_shares_by_default() {
  // `connections: 1` is one connection, which a shared one is too. This is
  // also what a client configured with APERIO_CONNECTIONS=1 writes for every
  // service.
  let specs = specs(&format!(
    "defaults: {{connections: 1}}\n{}",
    plain_services(2)
  ));
  assert!(specs.iter().all(|s| s.multiplex_group == Some(0)));
}

#[test]
fn default_sharers_past_what_a_server_accepts_take_another_connection() {
  // A file that started before sharing was the default must not stop starting
  // because of it: the services that only went along with it spread over as
  // many connections as it takes. The last one alone is not a group, so it
  // keeps a connection, and the pool, of its own.
  let max = service::MAX_MULTIPLEXED_SERVICES;
  let specs = specs(&plain_services(max + 1));
  assert!(specs[..max].iter().all(|s| s.multiplex_group == Some(0)));
  assert_eq!(specs[max].multiplex_group, None);
  assert_eq!(specs[max].connections, DEFAULT_CONNECTIONS_MAX);
}

#[test]
fn a_service_on_a_connection_of_its_own_gets_an_elastic_pool_by_default() {
  let lone = build_specs(&base_settings(), "base-id").unwrap();
  assert_eq!(
    (lone[0].connections_min, lone[0].connections),
    (1, DEFAULT_CONNECTIONS_MAX)
  );
  // The same for one of several that declined to share.
  let specs = specs(
    "services:\n  \
       web: {target: 'http://localhost:3000'}\n  \
       bulk: {target: 'http://localhost:5000', multiplex: false}\n",
  );
  assert_eq!(
    (specs[1].connections_min, specs[1].connections),
    (1, DEFAULT_CONNECTIONS_MAX)
  );
}

#[test]
fn a_paced_service_keeps_one_connection_by_default() {
  // The rate is divided across the pool's ceiling and the server paces each
  // connection alone, so a default pool of four would carry one transfer at a
  // quarter of the rate. A budget share paces as surely as a rate of its own.
  let budget = build_specs(&base_with("bandwidth: 8mbit"), "base-id").unwrap();
  assert_eq!((budget[0].connections_min, budget[0].connections), (1, 1));
  assert_eq!(budget[0].bandwidth_bps, Some(1_000_000));

  let own = specs(
    "services:\n  \
       web: {target: 'http://localhost:3000', bandwidth: 8mbit}\n",
  );
  assert_eq!((own[0].connections_min, own[0].connections), (1, 1));
  assert_eq!(own[0].bandwidth_bps, Some(1_000_000));
}

// ---------------------------------------------------------------------------
// depends_on belongs to the service that waits
// ---------------------------------------------------------------------------

#[test]
fn depends_on_is_read_from_the_entry_that_waits() {
  let specs = specs(
    "services:\n  \
       db: {target: 'http://localhost:5432'}\n  \
       web: {target: 'http://localhost:3000', depends_on: [db]}\n",
  );
  assert!(specs[0].depends_on.is_empty());
  assert_eq!(specs[1].depends_on, vec!["db".to_string()]);
}

#[test]
fn depends_on_is_not_a_default() {
  // A file-wide list meant "everything else waits for these", which needed a
  // rule for the services it named and another for the ones with no name.
  // Written on the service that waits, it needs neither.
  let err = specs_err("defaults: {depends_on: [db]}\nservices:\n  db: {target: '1'}\n");
  assert!(err.contains("`defaults.depends_on`"), "{err}");
}

// ---------------------------------------------------------------------------
// server_side
// ---------------------------------------------------------------------------

/// `server_side: true` on an entry reaches the spec, carrying its target.
///
/// The ask and the address travel together, and this is where they are joined:
/// a spec whose `server_side_target` is None declares nothing, and the server
/// relays it without a word. The e2e phase found exactly that, so it is worth
/// a unit test that fails in this crate rather than three processes away.
#[test]
fn an_entry_that_asks_to_be_served_from_the_server_carries_its_target() {
  let specs = specs(
    "services:\n  ss: {target: 'http://127.0.0.1:9111', hostname: ss.example.com, server_side: true}\n",
  );
  assert_eq!(
    specs[0].server_side_target.as_deref(),
    Some("http://127.0.0.1:9111"),
    "the entry asked to be served from the server and the spec must say so"
  );
}

/// An entry that does not ask keeps its backend address off the wire.
#[test]
fn an_entry_that_does_not_ask_sends_no_target() {
  let specs = specs(
    "services:\n  relayed: {target: 'http://127.0.0.1:9112', hostname: relayed.example.com}\n",
  );
  assert!(specs[0].server_side_target.is_none());
}

/// A default reaches every entry that leaves it out.
///
/// This is the shape a container needs: `APERIO_SERVER_SIDE=1` has no entry to
/// sit on, so it has to reach each service, and an entry that says
/// `server_side: false` keeps its hop on the client.
#[test]
fn a_default_server_side_reaches_every_entry_that_does_not_opt_out() {
  let specs = specs(
    "defaults: {server_side: true}\n\
     services:\n  \
       web: {target: 'http://127.0.0.1:9113'}\n  \
       api: {target: 'http://127.0.0.1:9114', server_side: false}\n",
  );
  assert_eq!(
    specs[0].server_side_target.as_deref(),
    Some("http://127.0.0.1:9113")
  );
  assert!(
    specs[1].server_side_target.is_none(),
    "an entry that opts out keeps its hop on the client"
  );
}

/// The one service of the command line takes it from `defaults:` too.
#[test]
fn the_single_service_takes_server_side_from_the_defaults() {
  let specs = build_specs(&base_with("defaults: {server_side: true}"), "base-id").unwrap();
  assert_eq!(
    specs[0].server_side_target.as_deref(),
    Some("http://localhost:3000")
  );
}

/// `server_side:` moves the last hop to the server, so it cannot coexist with
/// `serve:`, whose files are on this machine.
#[test]
fn server_side_and_serve_are_refused_together() {
  let mut settings = base_with("defaults: {server_side: true}");
  entry(&mut settings).serve = Some("./site".into());
  let err = build_specs(&settings, "base-id").unwrap_err();
  assert!(err.contains("server_side"), "{err}");

  let err = specs_err(
    "services:\n  ss: {target: 'http://127.0.0.1:9115', serve: ./site, server_side: true}\n",
  );
  assert!(err.contains("server_side"), "{err}");
}

#[test]
fn a_resilience_profile_sets_retry_and_the_breaker_and_a_written_value_wins() {
  let specs = specs(
    "services:\n\
     \x20 plain: {target: 'http://localhost:3000'}\n\
     \x20 off: {target: 'http://localhost:3001', resilience: off}\n\
     \x20 hard: {target: 'http://localhost:3002', resilience: aggressive}\n\
     \x20 mixed: {target: 'http://localhost:3003', resilience: aggressive, retry: {attempts: 1}}\n",
  );
  let by = |name: &str| {
    specs
      .iter()
      .find(|s| s.name.as_deref() == Some(name))
      .unwrap()
  };
  assert_eq!(by("plain").retry_attempts, 2, "safe is the default");
  assert_eq!(by("plain").breaker_failures, 0);
  assert_eq!(by("off").retry_attempts, 1);
  assert_eq!(by("hard").retry_attempts, 3);
  assert!(by("hard").breaker_failures > 0);
  assert_eq!(by("mixed").retry_attempts, 1, "what the entry writes wins");
  assert!(
    by("mixed").breaker_failures > 0,
    "and the rest is still the profile's"
  );
}
