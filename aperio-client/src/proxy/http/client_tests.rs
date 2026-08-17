//! Building the forwarding client, and the answer a visitor gets when the
//! backend could not be reached: the TLS floor spellings accepted and refused,
//! and the error response itself.

use super::*;

#[tokio::test]
async fn test_make_error_response() {
  let response = make_error_response("req-123".to_string(), 502);
  if let TunnelMessage::Response {
    id,
    status,
    headers,
    body,
    ..
  } = response
  {
    assert_eq!(id, "req-123");
    assert_eq!(status, 502);
    let ct = headers
      .iter()
      .find(|(k, _)| k == "content-type")
      .map(|(_, v)| v)
      .unwrap();
    assert_eq!(ct, "text/plain");
    let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
    let decoded_str = String::from_utf8(decoded).unwrap();
    assert!(decoded_str.contains("502 Bad Gateway"));
  } else {
    panic!("Expected Response variant");
  }
}

#[test]
fn tls_floor_reads_the_spellings_people_write() {
  use reqwest::tls::Version;
  assert_eq!(tls_floor(None).unwrap(), None);
  assert_eq!(tls_floor(Some("  ")).unwrap(), None);
  assert_eq!(tls_floor(Some("1.2")).unwrap(), Some(Version::TLS_1_2));
  assert_eq!(tls_floor(Some("1.3")).unwrap(), Some(Version::TLS_1_3));
  // The spellings a compliance document uses, since that is where this
  // setting's value is written down before it reaches the config file.
  assert_eq!(tls_floor(Some("TLSv1.3")).unwrap(), Some(Version::TLS_1_3));
  assert_eq!(tls_floor(Some("tls1.2")).unwrap(), Some(Version::TLS_1_2));
}

#[test]
fn tls_floor_refuses_a_value_it_does_not_understand() {
  // Not a silent fallback: the setting exists to raise a floor, and a typo
  // that quietly leaves it where it was is what makes a security setting
  // worse than none.
  assert!(tls_floor(Some("1.1")).is_err());
  assert!(tls_floor(Some("best")).is_err());
  assert!(tls_floor(Some("1.3.1")).is_err());
}
