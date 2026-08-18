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

// ---------------------------------------------------------------------------
// The switch, the entry point, and the predicate that decides
// ---------------------------------------------------------------------------

/// Redaction is on unless the operator turns it off, and only the two
/// spellings that mean off turn it off.
///
/// A mutation run left four survivors here, which between them are four ways
/// for redaction to be silently off: the whole function returning `false`,
/// `!=` becoming `==`, the `&&` becoming `||`, and the negation deleted. None
/// of them made a test go red, because nothing asserted the switch at all. It
/// is the one setting in this module whose failure is invisible: everything
/// keeps working, the dashboard keeps rendering, and the secrets are in it.
#[test]
fn redaction_is_on_unless_it_is_explicitly_turned_off() {
  assert!(redaction_setting(None), "on when the operator says nothing");
  assert!(!redaction_setting(Some("0")));
  assert!(!redaction_setting(Some("false")));
  assert!(
    !redaction_setting(Some("FALSE")),
    "the spelling is case-insensitive"
  );
  // Anything that is not one of those two means on. An operator who writes
  // `APERIO_INSPECTOR_REDACT=no` has said something this does not understand,
  // and the safe reading of a misunderstood switch is the protective one.
  assert!(redaction_setting(Some("1")));
  assert!(redaction_setting(Some("true")));
  assert!(
    redaction_setting(Some("no")),
    "unrecognised means on, not off"
  );
  assert!(redaction_setting(Some("")));
  // The live switch is on in a test process, which is what every other test
  // in this file quietly assumes.
  assert!(redaction_enabled());
}

/// A name is sensitive by exact match *or* by containing a needle, and the two
/// lists are independent.
///
/// `||` becoming `&&` survived, and that mutant masks only names that are on
/// both lists at once. `cvv` is on neither needle list and `user_password` is
/// on no exact list, so under the mutant both go to the dashboard in full.
#[test]
fn a_name_is_sensitive_by_either_list_not_both() {
  assert!(field_is_sensitive("cvv"), "exact match, contains no needle");
  assert!(
    field_is_sensitive("user_password"),
    "needle match, is not an exact entry"
  );
  assert!(field_is_sensitive("PASSWORD"), "case-insensitive");
  assert!(!field_is_sensitive("page"));
  assert!(!field_is_sensitive("q"));
}

/// The mask is a placeholder a reader can see, not an empty string.
///
/// `mask()` returning `""` still technically removes the secret, and that is
/// why no test caught it: the value is gone either way. What is lost is the
/// difference between "this field was masked" and "this field was empty", on
/// a screen somebody is reading to find out what the request carried.
#[test]
fn the_mask_is_visible_and_is_not_a_plausible_value() {
  assert!(!mask().is_empty(), "an empty mask reads as an absent field");
  assert_eq!(mask(), MASK);
  assert!(
    !field_is_sensitive(mask()),
    "the mask must not itself look like a secret name"
  );
}

/// A form-shaped body is redacted; a JSON one is not treated as a form.
///
/// The guard is `no '=' at all` **or** `looks structured`, and `||` becoming
/// `&&` survived: under it a JSON body containing an `=` is split on `&` and
/// mangled into pairs, which corrupts what the inspector shows rather than
/// redacting it.
#[test]
fn a_json_body_is_not_mistaken_for_a_form() {
  let form = redact_form("user=dan&password=hunter2").expect("a form is a form");
  assert!(form.contains("user=dan"));
  assert!(form.contains(&format!("password={MASK}")));
  assert!(!form.contains("hunter2"));

  assert!(
    redact_form(r#"{"filter":"a=b","token":"secret"}"#).is_none(),
    "a JSON body is handled as JSON, not split on & into pairs"
  );
  assert!(redact_form("<xml a=\"b\"/>").is_none());
  assert!(redact_form("no equals sign here").is_none());
}

/// The view the dashboard renders is the redacted one, not the original.
///
/// `redacted_view` is the entry point `api/inspector.rs` calls, and deleting
/// the `!` in its guard survived: that mutant returns the untouched capture
/// whenever redaction is *enabled*, which is the exact inversion of the
/// module's purpose and was caught by nothing.
#[test]
fn the_dashboards_view_of_a_capture_has_the_secrets_out_of_it() {
  use base64::prelude::*;
  let captured = CapturedRequest {
    id: "r1".to_string(),
    timestamp: "now".to_string(),
    method: "POST".to_string(),
    uri: "/login?api_key=super-secret&page=2".to_string(),
    req_headers: vec![
      ("authorization".to_string(), "Bearer abc123".to_string()),
      ("accept".to_string(), "application/json".to_string()),
    ],
    req_body: Some(BASE64_STANDARD.encode(r#"{"password":"hunter2","user":"dan"}"#)),
    req_body_truncated: false,
    status: 200,
    resp_headers: vec![("set-cookie".to_string(), "session=zzz".to_string())],
    resp_body: None,
    resp_body_truncated: false,
    resp_streamed: false,
    duration_ms: 1,
    timeline: None,
    client_id: "c1".to_string(),
    client_name: None,
    org_id: None,
  };
  let view = redacted_view(&captured);
  // Serialized, which is both what the dashboard receives and a way to look
  // at every field at once without naming them.
  let flat = serde_json::to_string(&view).expect("the view serializes");
  for secret in ["super-secret", "abc123", "hunter2", "zzz"] {
    assert!(
      !flat.contains(secret),
      "`{secret}` reached the dashboard view: {flat}"
    );
  }
  assert!(view.uri.contains("page=2"), "what is not a secret survives");
  assert_eq!(view.method, "POST", "the shape is unchanged");

  // The original is untouched, because replay re-sends the real bytes.
  assert!(captured.uri.contains("super-secret"));
}
