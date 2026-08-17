//! Pulling a token out of a request in both spellings, and what a tunnel
//! token is allowed to connect with: the store lookup, the IP gate, and the
//! alerts a refused one raises.

use super::super::tests::*;
use super::*;
use crate::store::tokens::TokenSpec;
use crate::test_support::*;

// --- token extraction -------------------------------------------------------

#[test]
fn extract_token_from_bearer_and_x_auth() {
  let mut bearer = HeaderMap::new();
  bearer.insert("authorization", "Bearer secret123".parse().unwrap());
  assert_eq!(extract_token(&bearer), Some("secret123".to_string()));

  let mut xauth = HeaderMap::new();
  xauth.insert("x-auth-token", "tok".parse().unwrap());
  assert_eq!(extract_token(&xauth), Some("tok".to_string()));

  // Non-Bearer authorization schemes are ignored (no x-auth-token fallback hit).
  let mut basic = HeaderMap::new();
  basic.insert("authorization", "Basic abc".parse().unwrap());
  assert_eq!(extract_token(&basic), None);

  assert_eq!(extract_token(&HeaderMap::new()), None);
}

#[test]
fn extract_and_verify_token_matches_constant_time() {
  let mut h = HeaderMap::new();
  h.insert("authorization", "Bearer right".parse().unwrap());
  assert!(extract_and_verify_token(&h, "right"));
  assert!(!extract_and_verify_token(&h, "wrong"));
  assert!(!extract_and_verify_token(&HeaderMap::new(), "right"));
}

// --- authorize_tunnel_token -------------------------------------------------

#[tokio::test]
async fn authorize_tunnel_master_and_missing() {
  let state = test_state();
  // No token at all -> None.
  assert!(
    authorize_tunnel_token(&state, &HeaderMap::new(), ip("127.0.0.1"))
      .await
      .is_none()
  );
  // Master bearer token -> master perms.
  let perms = authorize_tunnel_token(&state, &master_token_headers(), ip("127.0.0.1"))
    .await
    .unwrap();
  assert!(perms.master);
}

#[tokio::test]
async fn authorize_tunnel_store_token_ip_and_alerts() {
  let state = test_state();
  let (_t, secret) = state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "svc".to_string(),
      hostnames: vec!["site.test".to_string()],
      allowed_ips: vec!["10.0.0.0/8".to_string()],
      org_id: Some("org-7".to_string()),
      ..Default::default()
    })
    .expect("the test store can be written to");
  let mut h = HeaderMap::new();
  h.insert("authorization", format!("Bearer {secret}").parse().unwrap());

  // Source IP outside the token's allowlist -> rejected.
  assert!(
    authorize_tunnel_token(&state, &h, ip("192.168.0.1"))
      .await
      .is_none()
  );
  // First allowed IP establishes the baseline silently.
  let perms = authorize_tunnel_token(&state, &h, ip("10.1.2.3"))
    .await
    .unwrap();
  assert!(!perms.master);
  assert_eq!(perms.org_id.as_deref(), Some("org-7"));
  // A new source IP trips the new-IP alert branch.
  assert!(
    authorize_tunnel_token(&state, &h, ip("10.9.9.9"))
      .await
      .is_some()
  );
  // An unknown secret is rejected.
  let mut bad = HeaderMap::new();
  bad.insert("authorization", "Bearer apr_deadbeef".parse().unwrap());
  assert!(
    authorize_tunnel_token(&state, &bad, ip("10.1.2.3"))
      .await
      .is_none()
  );
}

#[tokio::test]
async fn authorize_tunnel_canary_trips_alert() {
  let state = test_state();
  let (_t, secret) = state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "decoy".to_string(),
      canary: true,
      ..Default::default()
    })
    .expect("the test store can be written to");
  let mut h = HeaderMap::new();
  h.insert("authorization", format!("Bearer {secret}").parse().unwrap());
  // Using a canary token authenticates but trips the breach alert path.
  assert!(
    authorize_tunnel_token(&state, &h, ip("203.0.113.1"))
      .await
      .is_some()
  );
}
