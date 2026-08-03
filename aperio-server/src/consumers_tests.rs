//! What these pin down: that repeated dials from one machine stay one edge,
//! and that a dependency does not vanish the moment its connection closes.

use super::*;

const IP: fn(&str) -> IpAddr = |s| s.parse().unwrap();

#[test]
fn repeated_dials_from_one_machine_are_one_edge() {
  let mut c = Consumers::default();
  for _ in 0..5 {
    c.opened(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  }
  let edges = c.live(100);
  // Five connections, one dependency. Counting them as five nodes is the
  // failure mode this key exists to prevent: `--bind-tunnels` opens a
  // connection per accepted socket, so a busy consumer would otherwise fill
  // the graph with copies of itself.
  assert_eq!(edges.len(), 1);
  assert_eq!(edges[0].active, 5);
  assert_eq!(edges[0].total, 5);
}

#[test]
fn a_closed_connection_leaves_the_dependency_in_place() {
  let mut c = Consumers::default();
  c.opened(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  c.closed(IP("10.0.0.7"), "client-a", Some("db"), "ops", 110);
  let edges = c.live(120);
  // Zero connections open, and still a dependency: a database consumer that
  // has just finished a query is idle, not gone.
  assert_eq!(edges.len(), 1);
  assert_eq!(edges[0].active, 0);
  assert_eq!(edges[0].total, 1);
}

#[test]
fn an_idle_edge_expires_and_a_busy_one_never_does() {
  let mut c = Consumers::default();
  c.opened(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  c.closed(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  c.opened(IP("10.0.0.8"), "client-a", Some("db"), "ops", 100);
  // The idle one has been quiet past the TTL; the one still holding a
  // connection stays whatever the clock says, because it is demonstrably
  // there.
  let edges = c.live(100 + EDGE_TTL_SECS + 1);
  assert_eq!(edges.len(), 1);
  assert_eq!(edges[0].from_ip, "10.0.0.8");
}

#[test]
fn different_tunnels_from_one_machine_are_different_edges() {
  let mut c = Consumers::default();
  c.opened(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  c.opened(IP("10.0.0.7"), "client-a", Some("cache"), "ops", 101);
  // One machine depending on two tunnels is two dependencies: losing the
  // declaring client breaks both, and the graph should say so twice.
  assert_eq!(c.live(101).len(), 2);
}

#[test]
fn the_newest_edge_is_listed_first() {
  let mut c = Consumers::default();
  c.opened(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  c.opened(IP("10.0.0.9"), "client-a", Some("db"), "ops", 200);
  let edges = c.live(200);
  assert_eq!(edges[0].from_ip, "10.0.0.9");
}

#[test]
fn an_edge_with_a_live_connection_is_never_swept() {
  let mut c = Consumers::default();
  c.opened(IP("10.0.0.7"), "client-a", Some("db"), "ops", 100);
  // No `closed`. This is correct for a connection that is genuinely still
  // open, and it is why `opened` has to be paired with a `closed` on every
  // path that can reach it: an unmatched one sits here for the life of the
  // process, because an edge holding a connection is never expired.
  assert_eq!(c.live(100 + EDGE_TTL_SECS * 100).len(), 1);
}
