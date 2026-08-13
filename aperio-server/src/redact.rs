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
  "otp_code",
  "totp",
  "totp_code",
  "mfa_code",
  "pin",
  "passphrase",
  "pwd",
  "id_token",
  "session_token",
  "auth_token",
  "authtoken",
  "jwt",
  "bearer",
  "cvv",
  "cvc",
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

/// True when a configuration/setting key name suggests it carries a secret and
/// its value must be masked in logs and the audit trail (matches *auth*,
/// *token*, *secret*, *password*, *credential*, case-insensitive).
pub(crate) fn config_key_is_secret(name: &str) -> bool {
  let lower = name.to_ascii_lowercase();
  ["auth", "token", "secret", "password", "credential"]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The placeholder substituted for masked secret values.
pub(crate) fn mask() -> &'static str {
  MASK
}

/// High-signal secret substrings: any field/param/header name *containing* one
/// is masked, so compound names (`aws_secret_access_key`, `user_password`,
/// `x-api-key`, `session-token`) don't slip past the exact-match list.
/// Over-masking a non-secret is safe; leaking a secret to the inspector, HAR
/// download, or copy-as-cURL is not.
const SENSITIVE_NEEDLES: &[&str] = &[
  "password",
  "passwd",
  "secret",
  "token",
  "apikey",
  "api_key",
  "api-key",
  "private_key",
  "passphrase",
  "credential",
];

fn field_is_sensitive(name: &str) -> bool {
  let lower = name.to_ascii_lowercase();
  SENSITIVE_FIELDS.iter().any(|f| lower == *f)
    || SENSITIVE_NEEDLES.iter().any(|n| lower.contains(n))
}

/// Masks the values of sensitive query parameters in a request URI, so secrets
/// carried in the query string (`?api_key=`, `?access_token=`, an OAuth
/// `?code=`, and Aperio's own `?aperio_share=` / `?aperio_token=`) never reach
/// the inspector, the HAR download, or copy-as-cURL. The path and
/// non-sensitive parameters are preserved.
pub(crate) fn redact_uri(uri: &str) -> String {
  let Some((path, query)) = uri.split_once('?') else {
    return uri.to_string();
  };
  let redacted: Vec<String> = query
    .split('&')
    .map(|pair| match pair.split_once('=') {
      Some((k, _))
        if field_is_sensitive(k)
          || k.eq_ignore_ascii_case("aperio_share")
          || k.eq_ignore_ascii_case("aperio_token")
          || k.eq_ignore_ascii_case("code")
          || k.eq_ignore_ascii_case("sig")
          || k.eq_ignore_ascii_case("signature") =>
      {
        format!("{k}={MASK}")
      }
      _ => pair.to_string(),
    })
    .collect();
  format!("{path}?{}", redacted.join("&"))
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
  view.uri = redact_uri(&view.uri);
  view.req_headers = redact_headers(&view.req_headers);
  view.resp_headers = redact_headers(&view.resp_headers);
  view.req_body = view.req_body.as_deref().map(redact_body_b64);
  view.resp_body = view.resp_body.as_deref().map(redact_body_b64);
  view
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
