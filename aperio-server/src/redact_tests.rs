//! Tests for redaction, which is what stands between a captured request
//! and a secret ending up in the log or the dashboard.

use super::*;

#[test]
fn test_uri_query_redaction() {
  // Sensitive params are masked, ordinary ones and the path are preserved.
  assert_eq!(
    redact_uri("/api/x?page=2&api_key=secret&token=abc&q=hello"),
    format!("/api/x?page=2&api_key={MASK}&token={MASK}&q=hello")
  );
  // Aperio's own signed share token and OAuth code/sig are always masked.
  assert_eq!(
    redact_uri("/p?aperio_share=SIGNED&code=xyz&sig=zzz"),
    format!("/p?aperio_share={MASK}&code={MASK}&sig={MASK}")
  );
  // No query string is untouched.
  assert_eq!(redact_uri("/plain/path"), "/plain/path");
}

#[test]
fn test_header_redaction() {
  let headers = vec![
    ("Host".to_string(), "app.example.com".to_string()),
    (
      "Authorization".to_string(),
      "Bearer sk-live-12345".to_string(),
    ),
    ("Cookie".to_string(), "sid=abc123; theme=dark".to_string()),
    ("X-Api-Key".to_string(), "key-98765".to_string()),
    ("Accept".to_string(), "application/json".to_string()),
  ];
  let out = redact_headers(&headers);
  assert_eq!(out[0].1, "app.example.com");
  assert_eq!(out[1].1, "Bearer [REDACTED]");
  assert_eq!(out[2].1, "sid=[REDACTED]; theme=[REDACTED]");
  assert_eq!(out[3].1, "[REDACTED]");
  assert_eq!(out[4].1, "application/json");
  // Nothing secret survives.
  let all = serde_json::to_string(&out).unwrap();
  assert!(!all.contains("sk-live-12345"));
  assert!(!all.contains("abc123"));
  assert!(!all.contains("key-98765"));
}

#[test]
fn test_json_body_redaction_is_recursive() {
  let body = serde_json::json!({
    "username": "doga",
    "password": "hunter2",
    "nested": { "api_key": "k-1", "note": "keep me" },
    "items": [{ "token": "t-1" }],
  })
  .to_string();
  let b64 = BASE64_STANDARD.encode(&body);
  let out = String::from_utf8(BASE64_STANDARD.decode(redact_body_b64(&b64)).unwrap()).unwrap();
  assert!(out.contains("\"username\":\"doga\""), "got: {out}");
  assert!(out.contains("keep me"));
  assert!(!out.contains("hunter2"));
  assert!(!out.contains("k-1"));
  assert!(!out.contains("t-1"));
  assert!(out.matches("[REDACTED]").count() >= 3);
}

#[test]
fn test_form_and_binary_bodies() {
  let form = BASE64_STANDARD.encode("username=doga&password=hunter2&remember=1");
  let out = String::from_utf8(BASE64_STANDARD.decode(redact_body_b64(&form)).unwrap()).unwrap();
  assert_eq!(out, "username=doga&password=[REDACTED]&remember=1");

  // Binary bodies pass through untouched.
  let binary = BASE64_STANDARD.encode([0u8, 159, 146, 150]);
  assert_eq!(redact_body_b64(&binary), binary);

  // Plain text without secrets passes through.
  let plain = BASE64_STANDARD.encode("hello world");
  assert_eq!(redact_body_b64(&plain), plain);
}
