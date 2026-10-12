//! The trust-on-first-use device key: where it is read from, that an
//! explicit value wins over a file, and that a key file already on disk is
//! still rewritten owner-only rather than left however it was found.

use super::*;

// ---------------------------------------------------------------------------
// resolve_device_key / device_key
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_device_key_value_and_file() {
  // Nothing configured: nothing announced.
  assert_eq!(resolve_device_key(None, None, false), None);

  // An explicit value wins and is trimmed.
  assert_eq!(
    resolve_device_key(Some("  explicit-key  ".into()), None, false).as_deref(),
    Some("explicit-key")
  );

  // A blank explicit value falls through to the file.
  let path = std::env::temp_dir().join(format!("aperio-devkey-{}", uuid::Uuid::new_v4()));
  let path_str = path.to_string_lossy().into_owned();
  // First call: the file does not exist, so a fresh key is generated and
  // persisted.
  let generated = resolve_device_key(Some("   ".into()), Some(path_str.clone()), false)
    .expect("a key is generated");
  assert!(!generated.is_empty());
  assert_eq!(
    std::fs::read_to_string(&path).unwrap().trim(),
    generated,
    "the generated key is persisted"
  );
  // Second call: the existing file's contents are reused verbatim.
  assert_eq!(
    resolve_device_key(None, Some(path_str.clone()), false).as_deref(),
    Some(generated.as_str())
  );
  // A blank path is treated as unset.
  assert_eq!(resolve_device_key(None, Some("  ".into()), false), None);

  let _ = std::fs::remove_file(&path);

  // device_key() memoizes and returns a stable value.
  let a = device_key();
  let b = device_key();
  assert_eq!(a, b);
}

#[cfg(unix)]
#[test]
fn a_pre_existing_device_key_file_is_still_written_owner_only() {
  use std::os::unix::fs::PermissionsExt;

  // `mode` on the open only applies to a file the call creates. An empty file
  // already at the path, which is exactly what a failed earlier write or an
  // operator's `touch` leaves, kept its 0644 and took the secret anyway: a
  // pinning key readable by every local user, in a file that looks as though
  // it was written owner-only.
  let path = std::env::temp_dir().join(format!("aperio-devkey-perm-{}", uuid::Uuid::new_v4()));
  std::fs::write(&path, "").unwrap();
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

  let key = resolve_device_key(None, Some(path.to_string_lossy().into_owned()), false)
    .expect("an empty file is regenerated");
  assert!(!key.is_empty());
  let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
  assert_eq!(mode, 0o600, "the key file is {mode:o}, not owner-only");

  let _ = std::fs::remove_file(&path);
}

/// The default file is the client's own to create: its directory is made,
/// the key is kept there, and the next start reads the same key back.
#[test]
fn a_default_key_file_is_created_and_kept() {
  let dir = std::env::temp_dir().join(format!("aperio-devkey-default-{}", uuid::Uuid::new_v4()));
  let path = dir.join("clients").join("0123456789abcdef.key");
  let file = Some(path.to_string_lossy().into_owned());
  let first = resolve_device_key(None, file.clone(), true).expect("a key is kept");
  assert!(path.exists(), "the directory is made and the key written");
  assert_eq!(resolve_device_key(None, file, true), Some(first));
  let _ = std::fs::remove_dir_all(&dir);
}

/// A default file that cannot be written announces no key at all, rather
/// than one that changes with every start and would pin the token, on a
/// server enforcing pinning, to a machine that forgets itself.
#[test]
fn a_default_key_file_that_cannot_be_written_announces_nothing() {
  let blocker =
    std::env::temp_dir().join(format!("aperio-devkey-blocked-{}", uuid::Uuid::new_v4()));
  // A file where the directory would have to be: nothing can be made under it.
  std::fs::write(&blocker, "not a directory").unwrap();
  let path = blocker.join("clients").join("0123456789abcdef.key");
  assert_eq!(
    resolve_device_key(None, Some(path.to_string_lossy().into_owned()), true),
    None
  );
  let _ = std::fs::remove_file(&blocker);
}
