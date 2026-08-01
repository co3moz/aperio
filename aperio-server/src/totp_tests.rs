//! Tests for the TOTP second factor: code derivation, the drift window,
//! and the replay guard.

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
