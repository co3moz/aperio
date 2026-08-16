//! What this pins down: the window refuses what it says it refuses, admits
//! everything else, and above all admits silence. A gate that reads "I did not
//! announce" as "I am too old" would refuse the very fleet the promise covers.

use super::*;

#[test]
fn a_peer_below_the_floor_is_refused_and_both_versions_are_named() {
  let refused = check(Some("0.4.2"), "0.9.0", Side::Client).expect("refused");
  assert_eq!(refused.peer, "0.4.2");
  assert_eq!(refused.floor, "0.9.0");
  let message = refused.message();
  assert!(message.contains("0.4.2"), "{message}");
  assert!(message.contains("0.9.0"), "{message}");
  assert!(message.contains("Upgrade the client"), "{message}");

  // The same from the other end, because either side can be the old one.
  let refused = check(Some("0.4.2"), "0.9.0", Side::Server).expect("refused");
  assert!(refused.message().contains("Upgrade the server"));
}

#[test]
fn a_peer_at_or_above_the_floor_is_admitted() {
  for version in ["0.9.0", "0.9.1", "0.10.0", "1.0.0", "v0.9.0"] {
    assert!(
      check(Some(version), "0.9.0", Side::Client).is_none(),
      "{version} meets the floor"
    );
  }
}

#[test]
fn silence_is_admitted_rather_than_read_as_age() {
  // A release old enough to predate the header is inside the documented
  // window anyway. Refusing on silence would take the fleet down on the
  // upgrade that introduced the gate, which is the outage this exists to
  // avoid rather than to cause.
  assert!(check(None, "0.9.0", Side::Client).is_none());
  assert!(check(Some(""), "0.9.0", Side::Client).is_none());
  assert!(check(Some("   "), "0.9.0", Side::Client).is_none());
}

#[test]
fn a_value_that_does_not_parse_is_not_evidence_of_age() {
  for garbled in ["not-a-version", "0.9.x", "..", "1.2.3.4"] {
    assert!(
      check(Some(garbled), "0.9.0", Side::Client).is_none(),
      "{garbled} is garbled, not old"
    );
  }
}

#[test]
fn the_shipped_floors_refuse_nothing_that_the_promise_covers() {
  // The whole point of the value chosen: `docs/upgrade-guide.md` promises
  // every released client from v0.1.0 onward, and this ships as a mechanism,
  // not as a new restriction. If someone narrows a floor, this test is where
  // they are reminded to move the promise with it.
  for released in [
    "0.1.0", "0.2.0", "0.3.0", "0.4.0", "0.4.2", "0.5.0", "0.6.0", "0.7.0", "0.8.0", "0.9.0",
  ] {
    assert!(
      check(Some(released), MIN_SUPPORTED_CLIENT, Side::Client).is_none(),
      "{released} is a released client and the promise covers it"
    );
    assert!(
      check(Some(released), MIN_SUPPORTED_SERVER, Side::Server).is_none(),
      "{released} is a released server and the promise covers it"
    );
  }
}
