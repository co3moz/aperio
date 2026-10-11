//! What this pins down: a removed key is never also a live one, so a hint
//! cannot shadow a setting the schema still reads, and every hint says what
//! to do.

use super::*;

#[test]
fn a_removed_key_is_not_a_live_setting() {
  let (top, service) = crate::authoring::known_keys();
  for removed in REMOVED_CLIENT_KEYS {
    assert!(
      !top.iter().any(|k| k == removed.key) && !service.iter().any(|k| k == removed.key),
      "`{}` is listed as removed but the schema still reads it",
      removed.key
    );
  }
}

#[test]
fn every_hint_names_a_release_and_an_action() {
  for removed in REMOVED_CLIENT_KEYS {
    assert!(
      removed.hint.contains("0."),
      "`{}`: say which release removed it",
      removed.key
    );
    assert!(removed.hint.ends_with('.'), "`{}`", removed.key);
  }
  assert!(client_hint("version").is_some());
  assert!(client_hint("hostname_typo").is_none());
}
