//! Secret redaction for the request inspector.
//!
//! Captured requests keep their raw form in memory (replay must re-send the
//! original bytes), but everything served to the dashboard passes through
//! here first: credential-bearing headers and secret-looking body fields are
//! masked so tokens and passwords never reach a viewer's browser, the HAR
//! download, or copy-as-cURL. Disable with `APERIO_INSPECTOR_REDACT=0`.

use crate::state::CapturedRequest;
use base64::prelude::*;

const MASK: &str = "[REDACTED]";

/// Header names whose values are masked (case-insensitive).
const SENSITIVE_HEADERS: &[&str] = &[
  "authorization",
  "proxy-authorization",
  "cookie",
  "set-cookie",
  "x-api-key",
  "api-key",
  "x-auth-token",
  "x-access-token",
  "x-amz-security-token",
  "x-aperio-totp",
];

/// Body field names whose values are masked (case-insensitive, JSON keys and
/// form-urlencoded parameter names).
const SENSITIVE_FIELDS: &[&str] = &[
  "password",
  "passwd",
  "secret",
  "token",
  "api_key",
  "apikey",
  "access_key",
  "access_token",
  "refresh_token",
  "client_secret",
  "private_key",
  "credential",
  "credentials",
  "otp",
];

/// True unless the operator opted out with `APERIO_INSPECTOR_REDACT=0`.
pub(crate) fn redaction_enabled() -> bool {
  use std::sync::OnceLock;
  static ENABLED: OnceLock<bool> = OnceLock::new();
  *ENABLED.get_or_init(|| {
    std::env::var("APERIO_INSPECTOR_REDACT")
      .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
      .unwrap_or(true)
  })
}

fn field_is_sensitive(name: &str) -> bool {
  let lower = name.to_ascii_lowercase();
  SENSITIVE_FIELDS.iter().any(|f| lower == *f)
}

/// Masks one header value, preserving harmless structure: cookies keep their
/// names, `Authorization` keeps its scheme, everything else is fully masked.
fn redact_header_value(name: &str, value: &str) -> String {
  let lower = name.to_ascii_lowercase();
  match lower.as_str() {
    "cookie" => value
      .split(';')
      .map(|pair| match pair.split_once('=') {
        Some((k, _)) => format!("{}={}", k.trim(), MASK),
        None => MASK.to_string(),
      })
      .collect::<Vec<_>>()
      .join("; "),
    "set-cookie" => match value.split_once('=') {
      Some((k, _)) => format!("{}={}", k.trim(), MASK),
      None => MASK.to_string(),
    },
    "authorization" | "proxy-authorization" => match value.trim().split_once(' ') {
      Some((scheme, _)) => format!("{scheme} {MASK}"),
      None => MASK.to_string(),
    },
    _ => MASK.to_string(),
  }
}

/// Returns the headers with sensitive values masked.
pub(crate) fn redact_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
  headers
    .iter()
    .map(|(name, value)| {
      let lower = name.to_ascii_lowercase();
      if SENSITIVE_HEADERS.contains(&lower.as_str()) {
        (name.clone(), redact_header_value(name, value))
      } else {
        (name.clone(), value.clone())
      }
    })
    .collect()
}

/// Recursively masks sensitive fields of a JSON value in place.
fn redact_json(value: &mut serde_json::Value) {
  match value {
    serde_json::Value::Object(map) => {
      for (key, val) in map.iter_mut() {
        if field_is_sensitive(key) {
          *val = serde_json::Value::String(MASK.to_string());
        } else {
          redact_json(val);
        }
      }
    }
    serde_json::Value::Array(items) => {
      for item in items {
        redact_json(item);
      }
    }
    _ => {}
  }
}

/// Masks sensitive parameters of a form-urlencoded body; None when the text
/// doesn't look like one.
fn redact_form(text: &str) -> Option<String> {
  if !text.contains('=') || text.contains(['{', '<', '\n']) {
    return None;
  }
  Some(
    text
      .split('&')
      .map(|pair| match pair.split_once('=') {
        Some((k, _)) if field_is_sensitive(k.trim()) => format!("{k}={MASK}"),
        Some((k, v)) => format!("{k}={v}"),
        None => pair.to_string(),
      })
      .collect::<Vec<_>>()
      .join("&"),
  )
}

/// Redacts a captured (base64) body: JSON fields and form parameters with
/// secret-looking names are masked; anything else passes through untouched.
pub(crate) fn redact_body_b64(body_b64: &str) -> String {
  let Ok(bytes) = BASE64_STANDARD.decode(body_b64) else {
    return body_b64.to_string();
  };
  let Ok(text) = std::str::from_utf8(&bytes) else {
    return body_b64.to_string(); // binary bodies carry no parseable secrets
  };
  if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(text) {
    redact_json(&mut json);
    return BASE64_STANDARD.encode(json.to_string());
  }
  if let Some(form) = redact_form(text) {
    return BASE64_STANDARD.encode(form);
  }
  body_b64.to_string()
}

/// The dashboard-facing view of a captured request: same shape, secrets
/// masked. The in-memory original stays intact so replay re-sends the real
/// bytes.
pub(crate) fn redacted_view(captured: &CapturedRequest) -> CapturedRequest {
  if !redaction_enabled() {
    return captured.clone();
  }
  let mut view = captured.clone();
  view.req_headers = redact_headers(&view.req_headers);
  view.resp_headers = redact_headers(&view.resp_headers);
  view.req_body = view.req_body.as_deref().map(redact_body_b64);
  view.resp_body = view.resp_body.as_deref().map(redact_body_b64);
  view
}

#[cfg(test)]
mod tests {
  use super::*;

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
}
