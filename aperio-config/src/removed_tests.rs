//! What this pins down: a removed key or variable is never also a live one,
//! so a hint cannot shadow a setting the schema still reads, and every hint
//! says what to do.

use super::*;

/// The keys a live client file accepts at the top level, and in a service.
fn live_keys() -> (Vec<String>, Vec<String>) {
  let schema = crate::layers::client_schema();
  let names = |node: &serde_json::Value| -> Vec<String> {
    node["properties"]
      .as_object()
      .map(|o| o.keys().cloned().collect())
      .unwrap_or_default()
  };
  (names(schema), names(&schema["$defs"]["ServiceEntry"]))
}

#[test]
fn a_removed_key_is_not_a_live_setting() {
  let (top, service) = live_keys();
  for removed in REMOVED_CLIENT_KEYS {
    let live = match removed.scope {
      KeyScope::Top => &top,
      KeyScope::Service => &service,
    };
    assert!(
      !live.iter().any(|k| k == removed.key),
      "`{}` is listed as removed but the schema still reads it",
      removed.key
    );
  }
}

#[test]
fn a_removed_variable_is_not_a_live_one() {
  for removed in REMOVED_CLIENT_ENV {
    assert!(
      !crate::layers::client_env()
        .iter()
        .any(|s| s.var == removed.key),
      "{} is listed as removed but a setting still reads it",
      removed.key
    );
  }
}

#[test]
fn every_hint_names_a_release_and_an_action() {
  let hints = REMOVED_CLIENT_KEYS
    .iter()
    .map(|r| (r.key, r.hint))
    .chain(REMOVED_CLIENT_ENV.iter().map(|r| (r.key, r.hint)));
  for (key, hint) in hints {
    assert!(hint.contains("0."), "`{key}`: say which release removed it");
    assert!(hint.ends_with('.'), "`{key}`");
  }
  assert!(client_hint(KeyScope::Top, "version").is_some());
  assert!(client_hint(KeyScope::Service, "public").is_some());
  assert!(client_hint(KeyScope::Top, "hostname_typo").is_none());
}
