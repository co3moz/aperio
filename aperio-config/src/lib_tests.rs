//! Tests for config parsing helpers and schema generation.

use super::*;

#[test]
fn security_headers_flag_false_is_empty() {
  assert!(SecurityHeaders::Flag(false).headers().is_empty());
}

#[test]
fn security_headers_flag_true_is_the_standard_preset() {
  let h = SecurityHeaders::Flag(true).headers();
  let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
  assert!(names.contains(&"Strict-Transport-Security"));
  assert!(names.contains(&"X-Frame-Options"));
  assert!(names.contains(&"X-Content-Type-Options"));
  assert!(names.contains(&"Referrer-Policy"));
  // HSTS carries the two-year default max-age.
  let hsts = &h
    .iter()
    .find(|(k, _)| k == "Strict-Transport-Security")
    .unwrap()
    .1;
  assert_eq!(hsts, "max-age=63072000");
}

#[test]
fn security_headers_detailed_selects_individually() {
  let opts: SecurityHeaders = serde_json::from_str(
    r#"{"hsts": true, "hsts_max_age": 100, "frame_options": "SAMEORIGIN",
        "nosniff": true, "referrer_policy": "no-referrer", "csp": "default-src 'self'"}"#,
  )
  .unwrap();
  let h = opts.headers();
  assert_eq!(
    h.iter()
      .find(|(k, _)| k == "Strict-Transport-Security")
      .unwrap()
      .1,
    "max-age=100"
  );
  assert_eq!(
    h.iter().find(|(k, _)| k == "X-Frame-Options").unwrap().1,
    "SAMEORIGIN"
  );
  assert!(h.iter().any(|(k, _)| k == "X-Content-Type-Options"));
  assert_eq!(
    h.iter().find(|(k, _)| k == "Referrer-Policy").unwrap().1,
    "no-referrer"
  );
  assert_eq!(
    h.iter()
      .find(|(k, _)| k == "Content-Security-Policy")
      .unwrap()
      .1,
    "default-src 'self'"
  );
}

#[test]
fn security_headers_detailed_max_age_alone_enables_hsts() {
  let opts: SecurityHeaders = serde_json::from_str(r#"{"hsts_max_age": 42}"#).unwrap();
  let h = opts.headers();
  assert_eq!(
    h.iter()
      .find(|(k, _)| k == "Strict-Transport-Security")
      .unwrap()
      .1,
    "max-age=42"
  );
}

#[test]
fn security_headers_detailed_empty_and_blank_values_are_skipped() {
  // All unset → nothing injected.
  let empty: SecurityHeaders = serde_json::from_str("{}").unwrap();
  assert!(empty.headers().is_empty());
  // Blank string values are trimmed away, not injected.
  let blank: SecurityHeaders =
    serde_json::from_str(r#"{"frame_options": "  ", "referrer_policy": "", "csp": " "}"#).unwrap();
  assert!(blank.headers().is_empty());
}

#[test]
fn security_headers_rejects_unknown_fields() {
  // deny_unknown_fields: a typo'd field is an error, not silently ignored.
  assert!(serde_json::from_str::<SecurityHeaders>(r#"{"frame_option": "DENY"}"#).is_err());
}

#[test]
fn hostnames_flatten_trims_and_drops_empties() {
  assert_eq!(
    Hostnames::One("  app.example.com  ".to_string()).into_vec(),
    vec!["app.example.com".to_string()]
  );
  assert_eq!(
    Hostnames::Many(vec![
      " a.com ".to_string(),
      "".to_string(),
      "b.com".to_string()
    ])
    .into_vec(),
    vec!["a.com".to_string(), "b.com".to_string()]
  );
}

#[test]
fn file_config_resolves_server_url_and_token() {
  // Bare-URL form; token from the flat key.
  let c: FileConfig =
    serde_json::from_str(r#"{"server": "https://t.example.com", "token": "flat"}"#).unwrap();
  assert_eq!(c.server_url().as_deref(), Some("https://t.example.com"));
  assert_eq!(c.server_token().as_deref(), Some("flat"));

  // Section form: nested url + token win.
  let c: FileConfig =
    serde_json::from_str(r#"{"server": {"url": "https://s.example.com", "token": "nested"}}"#)
      .unwrap();
  assert_eq!(c.server_url().as_deref(), Some("https://s.example.com"));
  assert_eq!(c.server_token().as_deref(), Some("nested"));

  // Section without token → falls back to the flat token.
  let c: FileConfig =
    serde_json::from_str(r#"{"server": {"url": "https://s.example.com"}, "token": "fallback"}"#)
      .unwrap();
  assert_eq!(c.server_token().as_deref(), Some("fallback"));

  // No server section at all.
  let c: FileConfig = serde_json::from_str("{}").unwrap();
  assert!(c.server_url().is_none());
  assert!(c.server_token().is_none());
}

#[test]
fn schema_json_outputs_are_valid_json() {
  let client = schema_json();
  let server = server_schema_json();
  assert!(!client.is_empty() && !server.is_empty());
  // Both parse back as JSON objects.
  let cv: serde_json::Value = serde_json::from_str(&client).unwrap();
  let sv: serde_json::Value = serde_json::from_str(&server).unwrap();
  assert!(cv.is_object());
  assert!(sv.is_object());
  // The two schemas are different documents (client vs server config).
  assert_ne!(client, server);
}
