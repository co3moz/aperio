//! Minimal RFC 6238 TOTP (SHA-1, 6 digits, 30 s steps) plus the base32
//! encoding authenticator apps expect. Implemented directly on the `hmac` /
//! `sha1` crates the server already ships rather than pulling a TOTP
//! dependency; verified against the RFC 6238 test vectors in the tests below.

use hmac::{Hmac, Mac};
use sha1::Sha1;

/// TOTP time step in seconds (the universal authenticator-app default).
const STEP_SECS: u64 = 30;
/// Number of code digits.
const DIGITS: u32 = 6;
/// Steps of clock skew tolerated on either side when verifying.
const SKEW_STEPS: i64 = 1;

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Encodes bytes as unpadded RFC 4648 base32 (what otpauth URLs carry).
pub(crate) fn base32_encode(data: &[u8]) -> String {
  let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
  for chunk in data.chunks(5) {
    let mut buf = [0u8; 5];
    buf[..chunk.len()].copy_from_slice(chunk);
    let bits = u64::from(buf[0]) << 32
      | u64::from(buf[1]) << 24
      | u64::from(buf[2]) << 16
      | u64::from(buf[3]) << 8
      | u64::from(buf[4]);
    let out_chars = (chunk.len() * 8).div_ceil(5);
    for i in 0..out_chars {
      let shift = 35 - 5 * i;
      out.push(BASE32_ALPHABET[((bits >> shift) & 0x1f) as usize] as char);
    }
  }
  out
}

/// Decodes unpadded RFC 4648 base32 (case-insensitive). None on any
/// character outside the alphabet.
pub(crate) fn base32_decode(s: &str) -> Option<Vec<u8>> {
  let mut bits: u64 = 0;
  let mut nbits = 0u32;
  let mut out = Vec::with_capacity(s.len() * 5 / 8);
  for c in s.trim_end_matches('=').bytes() {
    let v = BASE32_ALPHABET
      .iter()
      .position(|a| *a == c.to_ascii_uppercase())? as u64;
    bits = (bits << 5) | v;
    nbits += 5;
    if nbits >= 8 {
      nbits -= 8;
      out.push(((bits >> nbits) & 0xff) as u8);
    }
  }
  Some(out)
}

/// Generates a fresh 160-bit TOTP secret, base32-encoded.
pub(crate) fn generate_secret() -> String {
  let mut bytes = [0u8; 20];
  use argon2::password_hash::rand_core::RngCore;
  argon2::password_hash::rand_core::OsRng.fill_bytes(&mut bytes);
  base32_encode(&bytes)
}

/// The TOTP code for a base32 secret at a specific step counter.
fn code_at(secret: &[u8], counter: u64) -> u32 {
  let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC accepts any key length");
  mac.update(&counter.to_be_bytes());
  let digest = mac.finalize().into_bytes();
  let offset = (digest[19] & 0x0f) as usize;
  let bin = (u32::from(digest[offset]) & 0x7f) << 24
    | u32::from(digest[offset + 1]) << 16
    | u32::from(digest[offset + 2]) << 8
    | u32::from(digest[offset + 3]);
  bin % 10u32.pow(DIGITS)
}

/// Verifies a user-entered 6-digit code against the secret at `now_secs`,
/// tolerating one step of clock skew each way. On success returns the step
/// counter the code matched, so the caller can persist it and reject a later
/// replay of the same (or an older) code within its validity window.
pub(crate) fn verify_step(secret_b32: &str, code: &str, now_secs: u64) -> Option<i64> {
  let code = code.trim();
  if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  let secret = base32_decode(secret_b32)?;
  if secret.is_empty() {
    return None;
  }
  let entered = code.parse::<u32>().ok()?;
  let step = (now_secs / STEP_SECS) as i64;
  (-SKEW_STEPS..=SKEW_STEPS)
    .map(|delta| step + delta)
    .find(|&counter| counter >= 0 && code_at(&secret, counter as u64) == entered)
}

/// True when the code is valid at `now_secs` (ignoring replay). Used where no
/// replay window is tracked (e.g. TOTP enrollment / disable).
pub(crate) fn verify(secret_b32: &str, code: &str, now_secs: u64) -> bool {
  verify_step(secret_b32, code, now_secs).is_some()
}

/// The otpauth:// provisioning URL an authenticator app enrolls from.
pub(crate) fn otpauth_url(username: &str, secret_b32: &str) -> String {
  // Label and issuer are percent-encoded conservatively (space and reserved
  // URL characters), enough for usernames the user store accepts.
  let label: String = username
    .bytes()
    .flat_map(|b| {
      if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
        vec![b as char]
      } else {
        format!("%{:02X}", b).chars().collect()
      }
    })
    .collect();
  format!(
    "otpauth://totp/Aperio:{label}?secret={secret_b32}&issuer=Aperio&algorithm=SHA1&digits=6&period=30"
  )
}

/// Hex SHA-256 of a recovery code, the form stored in the user row.
pub(crate) fn hash_recovery_code(code: &str) -> String {
  use sha2::{Digest, Sha256};
  let mut hasher = Sha256::new();
  hasher.update(code.trim().as_bytes());
  hasher
    .finalize()
    .iter()
    .map(|b| format!("{:02x}", b))
    .collect()
}

/// Generates `n` single-use recovery codes (shown once) and their hashes
/// (persisted). Codes are 10 base32 characters: ~50 bits of entropy.
pub(crate) fn generate_recovery_codes(n: usize) -> (Vec<String>, Vec<String>) {
  use argon2::password_hash::rand_core::RngCore;
  let mut codes = Vec::with_capacity(n);
  let mut hashes = Vec::with_capacity(n);
  for _ in 0..n {
    let mut bytes = [0u8; 7];
    argon2::password_hash::rand_core::OsRng.fill_bytes(&mut bytes);
    let code = base32_encode(&bytes).to_lowercase()[..10].to_string();
    hashes.push(hash_recovery_code(&code));
    codes.push(code);
  }
  (codes, hashes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_base32_roundtrip() {
    for data in [
      b"".to_vec(),
      b"f".to_vec(),
      b"fo".to_vec(),
      b"foo".to_vec(),
      b"foob".to_vec(),
      b"fooba".to_vec(),
      b"foobar".to_vec(),
      (0u8..=255).collect::<Vec<u8>>(),
    ] {
      let enc = base32_encode(&data);
      assert_eq!(base32_decode(&enc).unwrap(), data, "roundtrip of {enc}");
    }
    // RFC 4648 vectors (unpadded).
    assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    assert_eq!(base32_decode("mzxw6ytboi").unwrap(), b"foobar");
    assert!(base32_decode("not base32!").is_none());
  }

  #[test]
  fn test_rfc6238_vectors() {
    // RFC 6238 Appendix B, SHA-1, secret "12345678901234567890". The RFC
    // lists 8-digit codes; ours are the low 6 digits.
    let secret = base32_encode(b"12345678901234567890");
    for (t, code8) in [
      (59u64, 94287082u32),
      (1111111109, 7081804),
      (1234567890, 89005924),
      (2000000000, 69279037),
    ] {
      let expected = format!("{:06}", code8 % 1_000_000);
      assert!(
        verify(&secret, &expected, t),
        "t={t} expected code {expected}"
      );
    }
    // A wrong code never verifies.
    assert!(!verify(&secret, "000000", 59));
    assert!(!verify(&secret, "94287082", 59)); // 8 digits rejected
    assert!(!verify(&secret, "9428x2", 59));
  }

  #[test]
  fn test_skew_window() {
    let secret = generate_secret();
    let now = 1_700_000_000u64;
    let decoded = base32_decode(&secret).unwrap();
    let current = format!("{:06}", code_at(&decoded, now / 30));
    // The current code works within ±1 step and fails beyond it.
    assert!(verify(&secret, &current, now));
    assert!(verify(&secret, &current, now + 30));
    assert!(verify(&secret, &current, now - 30));
    assert!(!verify(&secret, &current, now + 120));
  }

  #[test]
  fn test_recovery_codes() {
    let (codes, hashes) = generate_recovery_codes(8);
    assert_eq!(codes.len(), 8);
    assert_eq!(hashes.len(), 8);
    for (code, hash) in codes.iter().zip(&hashes) {
      assert_eq!(&hash_recovery_code(code), hash);
      assert_eq!(code.len(), 10);
    }
    // Codes are unique in practice.
    let unique: std::collections::HashSet<_> = codes.iter().collect();
    assert_eq!(unique.len(), 8);
  }

  #[test]
  fn test_otpauth_url() {
    let url = otpauth_url("ops user", "ABC234");
    assert_eq!(
      url,
      "otpauth://totp/Aperio:ops%20user?secret=ABC234&issuer=Aperio&algorithm=SHA1&digits=6&period=30"
    );
  }
}
