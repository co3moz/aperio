//! End-to-end encryption for emergency tunnels (`encrypt: true`).
//!
//! The two *clients* of a bound tunnel — the binder (initiator) and the
//! declaring client (responder) — run an ephemeral X25519 key exchange
//! in-band as the first frame in each direction, then seal every relayed
//! frame with ChaCha20-Poly1305. The aperio server only ever sees the
//! handshake public keys and ciphertext.
//!
//! A passive server (or any relay) learns nothing. An *active* server could
//! man-in-the-middle the plain exchange, so both sides may additionally
//! configure a pre-shared key (`psk`): it is mixed into the HKDF salt, and a
//! MITM without it derives different keys — the first sealed frame fails to
//! open and the stream dies instead of leaking data.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey};

/// Magic prefix of a handshake frame (versioned).
const HANDSHAKE_MAGIC: &[u8; 4] = b"APE1";
/// Full handshake frame length: magic + X25519 public key.
pub(crate) const HANDSHAKE_LEN: usize = 4 + 32;

/// Per-tunnel encryption parameters for a relay endpoint (used as
/// `Option<E2eParams>`: None = plaintext relay).
pub(crate) struct E2eParams {
  /// Optional pre-shared key mixed into the key derivation.
  pub(crate) psk: Option<String>,
}

/// Which side of the exchange this endpoint is; determines key direction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
  /// The binding consumer (sends the first handshake frame).
  Initiator,
  /// The declaring client (responds to the initiator's handshake).
  Responder,
}

/// An in-progress handshake: our ephemeral secret and the frame to send.
pub(crate) struct Handshake {
  secret: EphemeralSecret,
  role: Role,
  psk: Option<String>,
  /// The handshake frame to send to the peer (magic + our public key).
  pub(crate) frame: Vec<u8>,
}

impl Handshake {
  /// Starts a handshake: generates an ephemeral key pair and the frame that
  /// must be sent to the peer.
  pub(crate) fn new(role: Role, psk: Option<String>) -> Self {
    let secret = EphemeralSecret::random_from_rng(rand_core::OsRng);
    let public = PublicKey::from(&secret);
    let mut frame = Vec::with_capacity(HANDSHAKE_LEN);
    frame.extend_from_slice(HANDSHAKE_MAGIC);
    frame.extend_from_slice(public.as_bytes());
    Handshake {
      secret,
      role,
      psk,
      frame,
    }
  }

  /// Completes the handshake with the peer's frame, deriving the two
  /// directional session keys. Returns None on a malformed frame.
  pub(crate) fn complete(self, peer_frame: &[u8]) -> Option<Session> {
    if peer_frame.len() != HANDSHAKE_LEN || &peer_frame[..4] != HANDSHAKE_MAGIC {
      return None;
    }
    let mut peer_pub = [0u8; 32];
    peer_pub.copy_from_slice(&peer_frame[4..]);
    let shared = self.secret.diffie_hellman(&PublicKey::from(peer_pub));
    // The PSK (when configured) becomes the HKDF salt: a MITM re-keying the
    // exchange without it derives different session keys on each leg.
    let salt = self.psk.as_deref().unwrap_or("").as_bytes();
    let hk = Hkdf::<Sha256>::new(Some(salt), shared.as_bytes());
    let mut key_i2r = [0u8; 32];
    let mut key_r2i = [0u8; 32];
    hk.expand(b"aperio-e2e-v1 initiator->responder", &mut key_i2r)
      .ok()?;
    hk.expand(b"aperio-e2e-v1 responder->initiator", &mut key_r2i)
      .ok()?;
    let (send_key, recv_key) = match self.role {
      Role::Initiator => (key_i2r, key_r2i),
      Role::Responder => (key_r2i, key_i2r),
    };
    Some(Session {
      sealer: Sealer {
        cipher: ChaCha20Poly1305::new(&send_key.into()),
        counter: 0,
      },
      opener: Opener {
        cipher: ChaCha20Poly1305::new(&recv_key.into()),
        counter: 0,
      },
    })
  }
}

/// An established end-to-end session: one AEAD cipher and monotonically
/// increasing nonce counter per direction. Frames must be opened in the
/// order they were sealed (the relay preserves frame order per direction).
/// The two halves can be split so each relay direction owns its own state.
pub(crate) struct Session {
  pub(crate) sealer: Sealer,
  pub(crate) opener: Opener,
}

/// The sending half: seals outgoing frames.
pub(crate) struct Sealer {
  cipher: ChaCha20Poly1305,
  counter: u64,
}

/// The receiving half: opens incoming frames.
pub(crate) struct Opener {
  cipher: ChaCha20Poly1305,
  counter: u64,
}

/// 96-bit nonce from a 64-bit per-direction counter.
fn counter_nonce(counter: u64) -> Nonce {
  let mut nonce = [0u8; 12];
  nonce[..8].copy_from_slice(&counter.to_le_bytes());
  Nonce::from(nonce)
}

impl Sealer {
  /// Seals one outgoing frame.
  pub(crate) fn seal(&mut self, plaintext: &[u8]) -> Option<Vec<u8>> {
    let nonce = counter_nonce(self.counter);
    self.counter = self.counter.checked_add(1)?;
    self.cipher.encrypt(&nonce, plaintext).ok()
  }
}

impl Opener {
  /// Opens one incoming frame. A failure is fatal for the stream: it means
  /// tampering, reordering, or a key mismatch (wrong/missing PSK).
  pub(crate) fn open(&mut self, ciphertext: &[u8]) -> Option<Vec<u8>> {
    let nonce = counter_nonce(self.counter);
    self.counter = self.counter.checked_add(1)?;
    self.cipher.decrypt(&nonce, ciphertext).ok()
  }
}

#[cfg(test)]
mod tests {
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
}
