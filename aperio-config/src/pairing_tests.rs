//! What this pins down: a client and a server pair only on the same minor
//! release, in both directions; a different minor names the side to upgrade,
//! and a peer that announces nothing is refused as an older one.

use super::*;

#[test]
fn the_same_minor_pairs_whatever_the_patch() {
  for peer in ["0.13.0", "0.13.4", "v0.13.1", "0.13"] {
    assert!(
      check("0.13.2", Side::Server, Some(peer)).is_none(),
      "{peer} pairs with 0.13.2"
    );
    assert!(
      check("0.13.2", Side::Client, Some(peer)).is_none(),
      "{peer} pairs with 0.13.2"
    );
  }
}

#[test]
fn an_older_client_is_told_to_upgrade_the_client() {
  let refused = check("0.13.0", Side::Server, Some("0.12.3")).expect("refused");
  assert_eq!(refused.upgrade, Side::Client);
  let message = refused.message();
  assert!(message.contains("this client is 0.12.3"), "{message}");
  assert!(message.contains("this server is 0.13.0"), "{message}");
  assert!(message.contains("0.13.x"), "{message}");
  assert!(message.ends_with("Upgrade the client."), "{message}");
}

#[test]
fn a_newer_client_is_told_to_upgrade_the_server() {
  let refused = check("0.13.0", Side::Server, Some("0.14.0")).expect("refused");
  assert_eq!(refused.upgrade, Side::Server);
  assert!(refused.message().ends_with("Upgrade the server."));
}

#[test]
fn the_client_judges_the_server_the_same_way() {
  let refused = check("0.13.1", Side::Client, Some("0.12.9")).expect("refused");
  assert_eq!(refused.upgrade, Side::Server);
  let message = refused.message();
  assert!(message.contains("this server is 0.12.9"), "{message}");
  assert!(message.contains("this client is 0.13.1"), "{message}");

  let refused = check("0.13.1", Side::Client, Some("1.0.0")).expect("refused");
  assert_eq!(refused.upgrade, Side::Client);
}

#[test]
fn silence_and_garbage_are_read_as_an_older_peer() {
  for announced in [None, Some(""), Some("  "), Some("not-a-version")] {
    let refused = check("0.13.0", Side::Server, announced).expect("refused");
    assert_eq!(refused.upgrade, Side::Client, "{announced:?}");
  }
  let message = check("0.13.0", Side::Server, None)
    .expect("refused")
    .message();
  assert!(message.contains("announced no release"), "{message}");
}

#[test]
fn a_major_difference_never_pairs() {
  assert!(check("1.13.0", Side::Server, Some("0.13.0")).is_some());
}

#[test]
fn versions_parse_and_order() {
  let v = Version::parse("v0.13.2-rc1").expect("parses");
  assert_eq!((v.major, v.minor, v.patch), (0, 13, 2));
  assert_eq!(v.line(), "0.13.x");
  assert!(Version::parse("0.13.2.1").is_err());
  assert!(Version::parse("x.y").is_err());
  assert!(Version::parse("0.12.9").unwrap() < Version::parse("0.13.0").unwrap());
}
