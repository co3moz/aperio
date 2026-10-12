//! What this pins down: an empty document is the defaults, a document only
//! has to say what it changes, an unknown key anywhere is an error naming
//! it, the minimal form writes back only what differs, and the special-path
//! lists name settings that exist.

use super::*;
use crate::units::{ByteSize, Span};

#[test]
fn an_empty_document_is_the_defaults() {
  assert_eq!(
    ServerSettings::from_yaml("").unwrap(),
    ServerSettings::default()
  );
  assert_eq!(
    ServerSettings::from_yaml("{}").unwrap(),
    ServerSettings::default()
  );
  let d = ServerSettings::default();
  assert_eq!(d.limits.request_body, ByteSize::mb(10));
  assert_eq!(d.gateway.response_timeout, Span::secs(30));
  assert_eq!(d.access.default, DefaultAccess::Deny);
  assert!(d.dashboard.enabled);
  assert!(d.inspector.enabled && d.inspector.redact);
  assert_eq!(d.shutdown.drain, Drain::For(Span::secs(0)));
}

#[test]
fn a_document_only_says_what_it_changes() {
  let s = ServerSettings::from_yaml(
    "limits:\n  request_body: 5MB\ncache:\n  enabled: true\nrouting:\n  lb_strategy: primary-standby\n",
  )
  .unwrap();
  assert_eq!(s.limits.request_body, ByteSize::mb(5));
  assert_eq!(
    s.limits.concurrent_requests, 100,
    "the rest keeps its default"
  );
  assert!(s.cache.enabled);
  assert_eq!(s.cache.size, ByteSize::mb(64));
  assert_eq!(s.routing.lb_strategy, LbStrategy::PrimaryStandby);
}

#[test]
fn an_unknown_key_is_an_error_that_names_it() {
  for doc in [
    "max_body_sise: 5\n",
    "cache:\n  max_byte: 10\n",
    "limits:\n  ip_rate:\n    bursts: 3\n",
  ] {
    let err = ServerSettings::from_yaml(doc).expect_err(doc);
    assert!(err.contains("unknown field"), "{doc}: {err}");
  }
}

#[test]
fn units_are_read_and_a_bare_number_still_works() {
  let s = ServerSettings::from_yaml("gateway:\n  timeout: 2m\n  response_timeout: 45\n").unwrap();
  assert_eq!(s.gateway.timeout, Span::secs(120));
  assert_eq!(s.gateway.response_timeout, Span::secs(45));
  let s = ServerSettings::from_yaml("shutdown:\n  drain: auto\n").unwrap();
  assert_eq!(s.shutdown.drain, Drain::Auto);
  let s = ServerSettings::from_yaml("shutdown:\n  drain: 20s\n").unwrap();
  assert_eq!(s.shutdown.drain, Drain::For(Span::secs(20)));
}

#[test]
fn the_minimal_form_keeps_only_what_differs() {
  assert_eq!(ServerSettings::default().minimal(), serde_json::json!({}));
  let mut s = ServerSettings::default();
  s.cache.enabled = true;
  s.limits.tunnels = 200;
  let m = s.minimal();
  assert_eq!(
    m,
    serde_json::json!({"cache": {"enabled": true}, "limits": {"tunnels": 200}})
  );
  // And it reads back as the same settings.
  let back: ServerSettings = serde_json::from_value(m).unwrap();
  assert_eq!(back, s);
}

#[test]
fn changed_paths_name_fields_and_whole_lists() {
  let a = ServerSettings::default();
  let mut b = a.clone();
  b.proxy.trusted = vec!["10.0.0.0/8".into()];
  b.cache.enabled = true;
  let paths = a.changed_paths(&b);
  assert!(paths.contains(&"proxy.trusted".to_string()), "{paths:?}");
  assert!(paths.contains(&"cache.enabled".to_string()), "{paths:?}");
  assert!(path_in("proxy.trusted", SECURITY_PATHS));
  assert!(!path_in("cache.enabled", SECURITY_PATHS));
}

#[test]
fn every_special_path_is_a_setting_that_exists() {
  let tree = ServerSettings::default().to_value();
  for path in SECURITY_PATHS.iter().chain(RESTART_PATHS) {
    let mut node = &tree;
    for part in path.split('.') {
      // Optional fields are absent from the default tree; their section is
      // what has to exist.
      match node.get(part) {
        Some(next) => node = next,
        None => {
          assert!(node.is_object(), "{path}: `{part}` is under a non-section");
          break;
        }
      }
    }
  }
}

#[test]
fn credentials_read_masked_and_a_mask_written_back_keeps_them() {
  let stored = ServerSettings::from_yaml(
    "oidc:\n  issuer: https://id.example.com\n  client_secret: hunter2\nmetrics:\n  enabled: true\n  token: m-token\notel:\n  headers:\n    authorization: Bearer abc\naccess:\n  gate:\n    - method: basic\n      users: ['dan:pw']\n    - method: bearer\n      secret: 0123456789abcdef\n",
  )
  .unwrap();
  let masked = stored.masked();
  let text = masked.to_string();
  for secret in [
    "hunter2",
    "m-token",
    "Bearer abc",
    "dan:pw",
    "0123456789abcdef",
  ] {
    assert!(!text.contains(secret), "{secret} leaked: {text}");
  }
  // What is not a credential stays readable, and an unset one stays unset.
  assert_eq!(masked["oidc"]["issuer"], "https://id.example.com");
  assert_eq!(masked["access"]["gate"][0]["method"], "basic");
  assert!(masked["edge"]["token"].is_null());

  // Saving the masked document back changes nothing.
  let mut incoming = masked.clone();
  unmask(&mut incoming, &stored).unwrap();
  let back: ServerSettings = serde_json::from_value(incoming).unwrap();
  assert_eq!(back, stored);

  // A credential typed over the mask is the new one.
  let mut incoming = masked.clone();
  incoming["metrics"]["token"] = "fresh".into();
  unmask(&mut incoming, &stored).unwrap();
  let back: ServerSettings = serde_json::from_value(incoming).unwrap();
  assert_eq!(back.metrics.token.as_deref(), Some("fresh"));
  assert_eq!(back.oidc.client_secret.as_deref(), Some("hunter2"));
}

#[test]
fn a_mask_with_nothing_stored_behind_it_is_refused() {
  let mut incoming = serde_json::json!({"edge": {"token": MASKED}});
  let err = unmask(&mut incoming, &ServerSettings::default()).unwrap_err();
  assert!(err.starts_with("edge.token"), "{err}");
}

#[test]
fn every_secret_path_is_also_security_relevant() {
  // A credential nobody may read must not be writable by somebody who could
  // not change the rest of what guards it.
  for path in SECRET_PATHS {
    assert!(
      path_in(path, SECURITY_PATHS),
      "{path} is a credential but not a security path"
    );
  }
}
