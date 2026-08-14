//! Tests for dialling: address family selection, the connect strategy, and
//! the TLS floor an operator can pin under it.

use super::*;

#[test]
fn parse_maps_known_families_and_defaults_to_auto() {
  assert_eq!(IpFamily::parse(Some("ipv4")), IpFamily::V4);
  assert_eq!(IpFamily::parse(Some(" V4 ")), IpFamily::V4);
  assert_eq!(IpFamily::parse(Some("4")), IpFamily::V4);
  assert_eq!(IpFamily::parse(Some("ipv6")), IpFamily::V6);
  assert_eq!(IpFamily::parse(Some("6")), IpFamily::V6);
  assert_eq!(IpFamily::parse(Some("auto")), IpFamily::Auto);
  assert_eq!(IpFamily::parse(Some("nonsense")), IpFamily::Auto);
  assert_eq!(IpFamily::parse(Some("")), IpFamily::Auto);
  assert_eq!(IpFamily::parse(None), IpFamily::Auto);
}

fn v4(n: u8) -> SocketAddr {
  format!("10.0.0.{n}:443").parse().unwrap()
}
fn v6(n: u8) -> SocketAddr {
  format!("[::{n}]:443").parse().unwrap()
}

#[test]
fn interleave_starts_with_ipv4_and_alternates() {
  let out = interleave(vec![v4(1), v4(2)], vec![v6(1), v6(2)]);
  assert_eq!(out, vec![v4(1), v6(1), v4(2), v6(2)]);
}

#[test]
fn interleave_appends_the_longer_families_remainder() {
  let out = interleave(vec![v4(1)], vec![v6(1), v6(2), v6(3)]);
  assert_eq!(out, vec![v4(1), v6(1), v6(2), v6(3)]);
  let out = interleave(vec![v4(1), v4(2), v4(3)], vec![v6(1)]);
  assert_eq!(out, vec![v4(1), v6(1), v4(2), v4(3)]);
}

#[tokio::test]
async fn resolve_ordered_filters_by_family() {
  // Literal addresses resolve without DNS; the target only needs to name
  // both families. lookup_host on an IP echoes it, so we exercise ordering
  // by resolving a hostname is avoided, use loopback-style literals.
  let only_v4 = resolve_ordered("127.0.0.1", 443, IpFamily::V4)
    .await
    .unwrap();
  assert!(only_v4.iter().all(|a| a.is_ipv4()));

  // Asking for a family the target cannot provide is an error, not a hang.
  let none_v6 = resolve_ordered("127.0.0.1", 443, IpFamily::V6).await;
  assert!(none_v6.is_err());
}

#[tokio::test]
async fn a_peer_that_accepts_and_says_nothing_does_not_hold_the_dial_forever() {
  use tokio::net::TcpListener;
  use tokio_tungstenite::tungstenite::client::IntoClientRequest;

  // The case a reconnect loop cannot recover from on its own: the socket
  // opens, so the connect timeout is satisfied, and then the peer never
  // speaks. Only the TCP connect was bounded, so the handshake blocked with
  // no error, the loop never got its turn, and the service stayed down
  // silently instead of retrying or moving on to the next candidate server.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    // Accept and hold. Never read, never write, never close.
    let _held = listener.accept().await;
    std::future::pending::<()>().await;
  });

  let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
  let request = format!("ws://127.0.0.1:{port}/tunnel")
    .into_client_request()
    .unwrap();
  // A small budget stands in for the real one: what is being pinned down is
  // that there is a budget at all, not its value.
  let err = handshake(
    request,
    stream,
    None,
    Duration::from_millis(150),
    "127.0.0.1",
    port,
  )
  .await
  .expect_err("a peer that never speaks cannot produce a connection");
  assert!(
    format!("{err}").contains("handshake") && format!("{err}").contains("timed out"),
    "expected a handshake timeout, got: {err}"
  );
}

/// The floor is spelled several ways in the wild, and all of them mean the
/// same thing to an operator writing it down.
#[test]
fn a_floor_is_read_from_every_spelling_of_it() {
  for spelling in ["1.3", "TLSv1.3", "tls1.3", " 1.3 ", "13"] {
    let policy = TlsPolicy::parse(Some(spelling), None).expect(spelling);
    assert_eq!(policy.min_version, Some(TlsFloor::V13), "{spelling}");
  }
  assert_eq!(
    TlsPolicy::parse(Some("1.2"), None).unwrap().min_version,
    Some(TlsFloor::V12)
  );
}

/// Unset stays unset, which is what leaves the dial exactly as it was.
#[test]
fn nothing_configured_is_not_the_same_as_the_default_configured() {
  let policy = TlsPolicy::parse(None, None).unwrap();
  assert!(
    policy.is_default(),
    "an empty policy leaves the connector alone"
  );
  assert!(TlsPolicy::parse(Some("  "), Some("")).unwrap().is_default());

  let pinned = TlsPolicy::parse(Some("1.3"), None).unwrap();
  assert!(
    !pinned.is_default(),
    "a pinned floor builds a connector of its own"
  );
}

/// **The point of the entry.** A floor that is misspelled must not quietly
/// become the default: the operator asked for a stricter dial than they would
/// then be getting, and the control would exist only on paper.
#[test]
fn a_version_this_client_cannot_offer_is_refused_by_name() {
  let err = TlsPolicy::parse(Some("1.4"), None).unwrap_err();
  assert!(
    err.contains("1.4"),
    "the refusal quotes what was written: {err}"
  );
  assert!(
    err.contains("1.2") && err.contains("1.3"),
    "and says what is on offer: {err}"
  );

  assert!(
    TlsPolicy::parse(Some("1.1"), None).is_err(),
    "a floor below what rustls has"
  );
  assert!(TlsPolicy::parse(Some("yes"), None).is_err());
}

/// Same reasoning one level down: a suite this build does not have is a
/// refusal, and the message lists the ones it does have, because the names are
/// long and nobody remembers them exactly.
#[test]
fn an_unknown_cipher_suite_is_refused_and_the_known_ones_listed() {
  let err = TlsPolicy::parse(None, Some("TLS_MADE_UP_SUITE")).unwrap_err();
  assert!(err.contains("TLS_MADE_UP_SUITE"), "{err}");
  for known in all_cipher_suites() {
    assert!(err.contains(&known), "the refusal lists {known}: {err}");
  }
}

/// The names this build actually has are accepted, in either case and
/// separated by commas or spaces, since a list pasted out of a policy document
/// arrives in every shape.
#[test]
fn the_suites_this_build_has_are_accepted_as_written() {
  let known = all_cipher_suites();
  let first = known.first().expect("this build offers at least one suite");

  let policy = TlsPolicy::parse(None, Some(first)).unwrap();
  assert_eq!(policy.cipher_suites, vec![first.clone()]);
  assert!(TlsPolicy::parse(None, Some(&first.to_lowercase())).is_ok());

  if known.len() >= 2 {
    let pair = format!("{}, {}", known[0], known[1]);
    assert_eq!(
      TlsPolicy::parse(None, Some(&pair))
        .unwrap()
        .cipher_suites
        .len(),
      2
    );
  }
}

/// A pinned floor really does build a connector rather than being recorded and
/// forgotten.
#[test]
fn a_pinned_floor_builds_a_connector() {
  let policy = TlsPolicy::parse(Some("1.3"), None).unwrap();
  assert!(build_connector(&policy).is_ok(), "a 1.3 floor alone");

  let with_suite = TlsPolicy::parse(Some("1.3"), Some("TLS13_AES_256_GCM_SHA384")).unwrap();
  assert!(
    build_connector(&with_suite).is_ok(),
    "a 1.3 floor and a 1.3 suite"
  );
}

/// The one contradiction parsing cannot see: each half is valid and together
/// they describe a dial that can offer nothing. rustls says so, and the
/// message has to survive to the operator, since "no cipher suites" is not a
/// self-explaining failure at 3am.
#[test]
fn a_floor_with_no_suite_that_can_meet_it_says_so() {
  let contradictory = TlsPolicy {
    min_version: Some(TlsFloor::V13),
    cipher_suites: vec!["TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256".into()],
  };
  let err = match build_connector(&contradictory) {
    // `Connector` has no `Debug`, so this cannot go through `unwrap_err`.
    Ok(_) => panic!("a 1.3 floor with only a 1.2 suite must not build a connector"),
    Err(e) => e,
  };
  assert!(
    err.contains("1.3"),
    "the message names the floor that cannot be met: {err}"
  );
}

/// And that contradiction is caught by `parse`, so it is a startup refusal
/// with the file in front of the operator rather than a dial failure later.
#[test]
fn a_contradictory_pair_is_refused_when_the_file_is_read() {
  let err = TlsPolicy::parse(Some("1.3"), Some("TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256"))
    .expect_err("a 1.3 floor with only a 1.2 suite describes a dial that offers nothing");
  assert!(err.contains("1.3"), "{err}");
}

/// The static wiring: what an operator configured is what the dial reaches
/// for. Without this the policy could parse, validate, and never be consulted.
///
/// The only test that touches these process-wide statics, since they are set
/// once per process.
#[test]
fn the_configured_policy_reaches_the_dial() {
  assert!(
    tls_connector()
      .expect("nothing configured is not an error")
      .is_none(),
    "an unset policy leaves tokio-tungstenite's own connector in place"
  );

  set_tls_policy(TlsPolicy::parse(Some("1.3"), None).unwrap());
  // `tls_connector` memoizes, and it was already called above, so this asserts
  // the recorded policy rather than a second build. That is the honest limit
  // of what a unit test can see here; the memo is deliberate, since a
  // reconnect must not re-parse every webpki root.
  assert_eq!(TLS_POLICY.get().unwrap().min_version, Some(TlsFloor::V13));
  assert!(build_connector(TLS_POLICY.get().unwrap()).is_ok());
}
