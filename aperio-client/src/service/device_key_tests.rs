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
  assert_eq!(resolve_device_key(None, None), None);

  // An explicit value wins and is trimmed.
  assert_eq!(
    resolve_device_key(Some("  explicit-key  ".into()), None).as_deref(),
    Some("explicit-key")
  );

  // A blank explicit value falls through to the file.
  let path = std::env::temp_dir().join(format!("aperio-devkey-{}", uuid::Uuid::new_v4()));
  let path_str = path.to_string_lossy().into_owned();
  // First call: the file does not exist, so a fresh key is generated and
  // persisted.
  let generated =
    resolve_device_key(Some("   ".into()), Some(path_str.clone())).expect("a key is generated");
  assert!(!generated.is_empty());
  assert_eq!(
    std::fs::read_to_string(&path).unwrap().trim(),
    generated,
    "the generated key is persisted"
  );
  // Second call: the existing file's contents are reused verbatim.
  assert_eq!(
    resolve_device_key(None, Some(path_str.clone())).as_deref(),
    Some(generated.as_str())
  );
  // A blank path is treated as unset.
  assert_eq!(resolve_device_key(None, Some("  ".into())), None);

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

  let key = resolve_device_key(None, Some(path.to_string_lossy().into_owned()))
    .expect("an empty file is regenerated");
  assert!(!key.is_empty());
  let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
  assert_eq!(mode, 0o600, "the key file is {mode:o}, not owner-only");

  let _ = std::fs::remove_file(&path);
}
