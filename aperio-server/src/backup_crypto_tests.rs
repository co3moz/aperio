//! Tests for encrypted snapshots: that a backup round-trips, and that the
//! three ways a chunked format goes wrong are noticed rather than absorbed.

use super::*;

fn dir() -> std::path::PathBuf {
  let d = crate::test_support::test_temp_root().join(format!("bkcrypt-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&d).unwrap();
  d
}

fn key() -> BackupKey {
  BackupKey::parse(&"a1".repeat(32)).expect("64 hex characters")
}

fn plaintext(path: &std::path::Path, bytes: usize) -> Vec<u8> {
  // Not zeros: a bug that writes the buffer instead of the plaintext would
  // pass against a file of zeros.
  let data: Vec<u8> = (0..bytes).map(|i| (i % 251) as u8).collect();
  std::fs::write(path, &data).unwrap();
  data
}

/// A key is 32 bytes, however it was written down, and anything else is
/// refused with a message saying what was wrong.
#[test]
fn a_key_is_read_from_hex_or_base64_and_nothing_else() {
  assert!(
    BackupKey::parse(&"ab".repeat(32)).is_ok(),
    "64 hex characters"
  );
  use base64::Engine;
  let b64 = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
  assert!(BackupKey::parse(&b64).is_ok(), "base64 of 32 bytes");
  assert!(BackupKey::parse(&format!("  {b64}  ")).is_ok(), "trimmed");

  let short = base64::engine::general_purpose::STANDARD.encode([7u8; 16]);
  let err = BackupKey::parse(&short).unwrap_err();
  assert!(err.contains("32 bytes"), "{err}");
  assert!(BackupKey::parse("").is_err());
  assert!(BackupKey::parse("not a key at all !!").is_err());
}

/// **The refusal the entry is about.** A key inside the directory it protects
/// is not a key: whoever has the backups has it too.
#[test]
fn a_key_file_inside_the_backup_directory_is_refused() {
  let backups = dir();
  let inside = backups.join("backup.key");
  std::fs::write(&inside, "ab".repeat(32)).unwrap();

  let err = load_key(None, Some(inside.to_str().unwrap()), &backups).unwrap_err();
  assert!(err.contains("inside the backup directory"), "{err}");

  // The same key one directory up is fine, which is what makes the refusal
  // about the arrangement rather than about key files.
  let outside = backups.parent().unwrap().join("elsewhere.key");
  std::fs::write(&outside, "ab".repeat(32)).unwrap();
  assert!(
    load_key(None, Some(outside.to_str().unwrap()), &backups)
      .unwrap()
      .is_some()
  );
}

/// Two key sources is a question about which one is in force, and the answer
/// must not be "whichever the code checks first".
#[test]
fn a_key_and_a_key_file_together_are_refused() {
  let backups = dir();
  let err = load_key(Some(&"ab".repeat(32)), Some("/tmp/whatever.key"), &backups).unwrap_err();
  assert!(err.contains("exactly one"), "{err}");
  assert!(
    load_key(None, None, &backups).unwrap().is_none(),
    "unset is not an error"
  );
}

/// The whole point: what comes back is what went in, across a size that spans
/// several chunks and does not end on a chunk boundary.
#[test]
fn a_snapshot_round_trips_across_chunk_boundaries() {
  let d = dir();
  for size in [0usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, CHUNK * 2 + 37] {
    let plain = d.join(format!("plain-{size}"));
    let enc = d.join(format!("enc-{size}"));
    let back = d.join(format!("back-{size}"));
    let original = plaintext(&plain, size);

    encrypt_file(&key(), &plain, &enc).unwrap();
    decrypt_file(&key(), &enc, &back).unwrap();
    assert_eq!(std::fs::read(&back).unwrap(), original, "size {size}");

    let ciphertext = std::fs::read(&enc).unwrap();
    assert!(ciphertext.starts_with(MAGIC), "size {size}");
    if size > 64 {
      assert!(
        !ciphertext.windows(64).any(|w| w == &original[..64]),
        "size {size}: the plaintext is not in the file"
      );
    }
  }
}

/// Another key is another answer, and the answer is a failure rather than
/// rubbish written to the output.
#[test]
fn the_wrong_key_does_not_decrypt() {
  let d = dir();
  let plain = d.join("plain");
  let enc = d.join("enc");
  plaintext(&plain, CHUNK + 5);
  encrypt_file(&key(), &plain, &enc).unwrap();

  let other = BackupKey::parse(&"bc".repeat(32)).unwrap();
  let err = decrypt_file(&other, &enc, &d.join("back")).unwrap_err();
  assert!(err.contains("failed to decrypt"), "{err}");
}

/// **Truncation is the one a chunked format absorbs silently** unless it is
/// built not to: without the end marker, a backup cut short decrypts to a
/// shorter database and restores as if it were whole.
#[test]
fn a_truncated_backup_is_refused_rather_than_shortened() {
  let d = dir();
  let plain = d.join("plain");
  let enc = d.join("enc");
  let original = plaintext(&plain, CHUNK * 2 + 11);
  encrypt_file(&key(), &plain, &enc).unwrap();

  // Cut the end marker off, leaving whole, valid, correctly ordered chunks.
  let full = std::fs::read(&enc).unwrap();
  let cut = d.join("cut");
  std::fs::write(&cut, &full[..full.len() - TAG_LEN]).unwrap();

  let back = d.join("back");
  let err = decrypt_file(&key(), &cut, &back).unwrap_err();
  assert!(err.contains("truncated"), "{err}");
  assert!(
    !back.exists(),
    "and nothing was left at the output path to be restored by mistake"
  );
  let _ = original;
}

/// Swapping two chunks keeps every tag valid for *some* position, which is
/// exactly why the position is authenticated.
#[test]
fn reordered_chunks_are_refused() {
  let d = dir();
  let plain = d.join("plain");
  let enc = d.join("enc");
  plaintext(&plain, CHUNK * 2);
  encrypt_file(&key(), &plain, &enc).unwrap();

  // Swap the first two frames, keeping every frame whole and every tag valid
  // for the position it was written at. Anything cruder would fail on the
  // framing rather than on the order, and would pass while proving nothing.
  let full = std::fs::read(&enc).unwrap();
  let header = 8 + 1 + 4 + NONCE_PREFIX_LEN;
  let mut frames: Vec<Vec<u8>> = Vec::new();
  let mut at = header;
  while at < full.len() {
    let len = u32::from_be_bytes(full[at..at + 4].try_into().unwrap()) as usize;
    frames.push(full[at..at + 4 + len].to_vec());
    at += 4 + len;
  }
  assert!(frames.len() >= 3, "two data chunks and the end marker");
  frames.swap(0, 1);

  let mut swapped = full[..header].to_vec();
  for f in &frames {
    swapped.extend_from_slice(f);
  }
  assert_eq!(swapped.len(), full.len(), "only the order changed");
  let path = d.join("swapped");
  std::fs::write(&path, &swapped).unwrap();

  assert!(decrypt_file(&key(), &path, &d.join("back")).is_err());
  assert!(
    !d.join("back").exists(),
    "and no half-decrypted database was left behind to be restored by mistake"
  );
}

/// A file that is not one of ours says so, rather than failing somewhere
/// deeper with a cipher error.
#[test]
fn something_that_is_not_a_snapshot_is_named_as_such() {
  let d = dir();
  let path = d.join("random.db.enc");
  std::fs::write(&path, b"SQLite format 3\0and so on").unwrap();

  let err = decrypt_file(&key(), &path, &d.join("back")).unwrap_err();
  assert!(err.contains("not an encrypted Aperio snapshot"), "{err}");

  let tiny = d.join("tiny");
  std::fs::write(&tiny, b"abc").unwrap();
  assert!(
    decrypt_file(&key(), &tiny, &d.join("back2"))
      .unwrap_err()
      .contains("too short")
  );
}
