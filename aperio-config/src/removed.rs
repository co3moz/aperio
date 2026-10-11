//! Keys a configuration file may still carry from an earlier release, and
//! what to write instead.
//!
//! There is no `version:` in a configuration and no layer that translates an
//! old spelling into a new one. A file that writes a key from this table gets
//! the hint beside it, naming what replaced the key, instead of the generic
//! "not a setting" a typo gets: the message an operator needs after an
//! upgrade is the one that says where the setting went.
//!
//! Add an entry in the same commit that removes or renames a key. A key that
//! is merely new needs nothing here.

/// One removed key and what to do about it.
pub struct Removed {
  /// The key as a file wrote it, at the top level or inside a `services:`
  /// entry.
  pub key: &'static str,
  /// What replaced it, written as the thing to do.
  pub hint: &'static str,
}

/// Client keys (`aperio.yaml`) that no longer exist.
pub const REMOVED_CLIENT_KEYS: &[Removed] = &[Removed {
  key: "version",
  hint: "removed in 0.13.0: a file no longer declares the release it was written for, \
         since a client pairs only with a server of its own minor release. Delete the key.",
}];

/// The hint for a removed client key, if `key` is one.
pub fn client_hint(key: &str) -> Option<&'static str> {
  REMOVED_CLIENT_KEYS
    .iter()
    .find(|r| r.key == key)
    .map(|r| r.hint)
}

#[cfg(test)]
#[path = "removed_tests.rs"]
mod tests;
