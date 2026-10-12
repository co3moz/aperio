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

#[test]
fn a_moved_settings_section_is_not_a_live_one() {
  let schema = serde_json::to_value(schemars::schema_for!(
    crate::server_settings::ServerSettings
  ))
  .unwrap();
  let live: Vec<&String> = schema["properties"]
    .as_object()
    .map(|o| o.keys().collect())
    .unwrap_or_default();
  for removed in REMOVED_SETTINGS {
    assert!(
      !live.iter().any(|k| *k == removed.key),
      "`{}` is listed as moved but the settings still have it",
      removed.key
    );
    assert!(removed.hint.contains("0.13.0"), "`{}`", removed.key);
  }
}

#[test]
fn a_document_in_the_old_shape_is_told_where_the_section_went() {
  let err = crate::server_settings::ServerSettings::from_yaml("rate_limits: []\n").unwrap_err();
  assert!(err.contains("unknown field `rate_limits`"), "{err}");
  assert!(err.contains("a site's `rate_limit"), "{err}");
  // Anything else keeps the plain refusal.
  let err = crate::server_settings::ServerSettings::from_yaml("limitz: {}\n").unwrap_err();
  assert!(!err.contains("moved into"), "{err}");
}

#[test]
fn a_0_12_server_file_pasted_under_settings_is_told_key_by_key() {
  // The likeliest first try at the upgrade: the old flat keys as they were.
  for (old, says) in [
    ("cache_max_bytes: 64MB", "`cache.size`"),
    ("trust_proxy: true", "`proxy.trust`"),
    ("oidc_issuer: https://id.example.com", "`oidc.issuer`"),
    ("stream_pause_bytes: 2MB", "`advanced.stream.pause_at`"),
    ("port: 8080", "PORT"),
    ("version: 0.12.0", "delete it"),
    ("login_lockout: {threshold: 5}", "`security.login_lockout`"),
  ] {
    let err = crate::server_settings::ServerSettings::from_yaml(&format!("{old}\n")).unwrap_err();
    assert!(err.contains(says), "{old}: {err}");
  }
  // The same word inside a section is a typo there, not the old key.
  let err = crate::server_settings::ServerSettings::from_yaml("tls: {version: 1}\n").unwrap_err();
  assert!(err.contains("unknown field `version`"), "{err}");
  assert!(!err.contains("delete it"), "{err}");
}

#[test]
fn every_key_of_the_0_12_server_file_is_answered() {
  // A sample across the flat keys the 0.12 server read from its file, each
  // of which a 0.13 document refuses: none is left with the bare refusal.
  for key in [
    "access_log_sample_rate",
    "alert_rules",
    "backup_dir",
    "dashboard_hostname",
    "expose",
    "ip_limit_refill",
    "max_body_size",
    "outlier_ejection",
    "server",
    "trusted_proxies",
    "webhook_retry_schedule",
  ] {
    assert!(
      REMOVED_SETTINGS.iter().any(|r| r.key == key),
      "`{key}` has no hint"
    );
  }
}

/// Every path the client and server schemas accept, with `*` for a map key.
fn schema_paths(root: &serde_json::Value) -> Vec<String> {
  fn visit(
    root: &serde_json::Value,
    node: &serde_json::Value,
    prefix: &str,
    depth: usize,
    out: &mut Vec<String>,
  ) {
    if depth > 12 {
      return;
    }
    let node = match node.get("$ref").and_then(|r| r.as_str()) {
      Some(r) => &root["$defs"][r.rsplit('/').next().unwrap_or_default()],
      None => node,
    };
    for key in ["anyOf", "allOf", "oneOf"] {
      for sub in node[key].as_array().into_iter().flatten() {
        visit(root, sub, prefix, depth + 1, out);
      }
    }
    if let Some(items) = node.get("items") {
      visit(root, items, prefix, depth + 1, out);
    }
    let join = |name: &str| {
      if prefix.is_empty() {
        name.to_string()
      } else {
        format!("{prefix}.{name}")
      }
    };
    for (name, sub) in node["properties"].as_object().into_iter().flatten() {
      out.push(join(name));
      visit(root, sub, &join(name), depth + 1, out);
    }
    if let Some(map) = node.get("additionalProperties").filter(|v| v.is_object()) {
      out.push(join("*"));
      visit(root, map, &join("*"), depth + 1, out);
    }
  }
  let mut out = Vec::new();
  visit(root, root, "", 0, &mut out);
  out
}

/// Whether `path` is a setting today: exactly, through a map key (`*`), or
/// as the key of one entry, which a hint writes without what holds it: a
/// client's service key without `defaults.`, a site's without `sites.`.
fn is_a_setting(path: &str, known: &[String]) -> bool {
  let matches = |pattern: &str, path: &str| {
    let (p, q): (Vec<&str>, Vec<&str>) = (pattern.split('.').collect(), path.split('.').collect());
    p.len() == q.len() && p.iter().zip(&q).all(|(a, b)| *a == "*" || a == b)
  };
  let in_an_entry = [format!("defaults.{path}"), format!("sites.{path}")];
  known
    .iter()
    .any(|k| matches(k, path) || in_an_entry.iter().any(|e| matches(k, e)))
}

/// A hint is read by someone who has just been refused, and acts on it
/// word for word. Every setting it points at has to exist: two named
/// `pages.unavailable` and one `routing.require_hostname_bind` after both
/// had moved again in the same release, so the refusal sent its reader to a
/// key that was refused in turn.
#[test]
fn every_setting_a_hint_points_at_exists() {
  let client = schema_paths(crate::layers::client_schema());
  let server_root: serde_json::Value = serde_json::from_str(&crate::server_schema_json()).unwrap();
  let mut known = schema_paths(&server_root);
  known.extend(client);

  let hints = REMOVED_CLIENT_KEYS
    .iter()
    .map(|r| (r.key, r.hint))
    .chain(REMOVED_CLIENT_ENV.iter().map(|r| (r.key, r.hint)))
    .chain(REMOVED_SERVER_ENV.iter().map(|r| (r.key, r.hint)))
    .chain(REMOVED_SETTINGS.iter().map(|r| (r.key, r.hint)));
  let mut wrong = Vec::new();
  for (key, hint) in hints {
    let root = key.split('.').next().unwrap_or(key);
    for (i, quoted) in hint.split('`').enumerate() {
      let looks_like_a_path = i % 2 == 1
        && quoted.starts_with(|c: char| c.is_ascii_lowercase())
        && quoted.contains('.')
        && quoted
          .chars()
          .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.*".contains(c))
        && !quoted.ends_with(".yaml");
      // The removed key itself, and what was under it, are the old names.
      let the_old_name = quoted == key || quoted.starts_with(&format!("{root}."));
      if looks_like_a_path && !the_old_name && !is_a_setting(quoted, &known) {
        wrong.push(format!("{key}: `{quoted}`"));
      }
    }
  }
  assert!(
    wrong.is_empty(),
    "hints pointing at settings that do not exist:\n  {}",
    wrong.join("\n  ")
  );
}
