//! Tests for the end-to-end encryption layer: key agreement, the sealed
//! frame format, and what happens to a frame that has been tampered with.

use super::*;

fn pair(psk_a: Option<&str>, psk_b: Option<&str>) -> (Session, Session) {
  let a = Handshake::new(Role::Initiator, psk_a.map(String::from));
  let b = Handshake::new(Role::Responder, psk_b.map(String::from));
  let frame_a = a.frame.clone();
  let frame_b = b.frame.clone();
  (
    a.complete(&frame_b).expect("initiator"),
    b.complete(&frame_a).expect("responder"),
  )
}

#[test]
fn test_roundtrip_both_directions() {
  let (mut i, mut r) = pair(None, None);
  let c1 = i.sealer.seal(b"hello").unwrap();
  assert_ne!(c1, b"hello");
  assert_eq!(r.opener.open(&c1).unwrap(), b"hello");
  let c2 = r.sealer.seal(b"world").unwrap();
  assert_eq!(i.opener.open(&c2).unwrap(), b"world");
  // Counters advance: the same plaintext seals differently.
  let c3 = i.sealer.seal(b"hello").unwrap();
  assert_ne!(c1, c3);
  assert_eq!(r.opener.open(&c3).unwrap(), b"hello");
}

#[test]
fn test_psk_mismatch_fails_to_open() {
  let (mut i, mut r) = pair(Some("right"), Some("wrong"));
  let sealed = i.sealer.seal(b"secret").unwrap();
  assert!(
    r.opener.open(&sealed).is_none(),
    "PSK mismatch must not decrypt"
  );
  // Matching PSKs work.
  let (mut i2, mut r2) = pair(Some("same"), Some("same"));
  let sealed = i2.sealer.seal(b"secret").unwrap();
  assert_eq!(r2.opener.open(&sealed).unwrap(), b"secret");
}

#[test]
fn test_tampering_and_reordering_fail() {
  let (mut i, mut r) = pair(None, None);
  let mut sealed = i.sealer.seal(b"data").unwrap();
  sealed[0] ^= 1;
  assert!(
    r.opener.open(&sealed).is_none(),
    "tampered frame must not open"
  );

  // A dropped/reordered frame desynchronizes the counter and fails.
  let (mut i, mut r) = pair(None, None);
  let _skipped = i.sealer.seal(b"one").unwrap();
  let second = i.sealer.seal(b"two").unwrap();
  assert!(
    r.opener.open(&second).is_none(),
    "out-of-order frame must not open"
  );
}

#[test]
fn test_handshake_rejects_malformed_frames() {
  let h = Handshake::new(Role::Initiator, None);
  assert!(
    Handshake::new(Role::Initiator, None)
      .complete(&h.frame[..10])
      .is_none()
  );
  let mut bad_magic = h.frame.clone();
  bad_magic[0] = b'X';
  assert!(
    Handshake::new(Role::Initiator, None)
      .complete(&bad_magic)
      .is_none()
  );
}
