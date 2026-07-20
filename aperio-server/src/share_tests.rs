use super::*;
use crate::test_support::*;
use axum::extract::{ConnectInfo, State};
use axum::http::Uri;

fn key() -> [u8; 32] {
  share_signing_key("test")
}

fn claims(host: &str, path: Option<&str>, exp: Option<u64>) -> ShareClaims {
  ShareClaims {
    host: host.to_string(),
    path: path.map(|p| p.to_string()),
    exp,
    id: "abcd1234".to_string(),
  }
}

fn future() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs()
    + 3600
}

// --- Signing key derivation ---

#[test]
fn signing_key_is_deterministic_and_token_scoped() {
  assert_eq!(share_signing_key("test"), share_signing_key("test"));
  assert_ne!(share_signing_key("test"), share_signing_key("other"));
}

// --- Sign / verify round trip and failure modes ---

#[test]
fn sign_and_verify_round_trip() {
  let c = claims("app.example.com", Some("/docs"), Some(future()));
  let token = sign_share_claims(&c, &key());
  let got = verify_share_token(&token, &key()).unwrap();
  assert_eq!(got.host, "app.example.com");
  assert_eq!(got.path.as_deref(), Some("/docs"));
}

#[test]
fn verify_accepts_never_expiring_and_rejects_expired() {
  // exp = None never expires.
  let never = sign_share_claims(&claims("a.com", None, None), &key());
  assert!(verify_share_token(&never, &key()).is_some());
  // exp in the past is rejected.
  let expired = sign_share_claims(&claims("a.com", None, Some(1)), &key());
  assert!(verify_share_token(&expired, &key()).is_none());
}

#[test]
fn verify_rejects_tampering_and_malformed_tokens() {
  let token = sign_share_claims(&claims("a.com", None, Some(future())), &key());
  // Wrong key (different master token) fails the HMAC check.
  assert!(verify_share_token(&token, &share_signing_key("nope")).is_none());
  // No dot separator.
  assert!(verify_share_token("no-dot-here", &key()).is_none());
  // Signature that is not valid base64.
  let (payload, _) = token.split_once('.').unwrap();
  assert!(verify_share_token(&format!("{payload}.***"), &key()).is_none());
  // Tampered payload → signature mismatch.
  let (_, sig) = token.split_once('.').unwrap();
  assert!(verify_share_token(&format!("AAAA.{sig}"), &key()).is_none());
}

// --- Scope coverage ---

#[test]
fn claims_cover_respects_host_path_and_traversal() {
  let whole = claims("app.example.com", None, None);
  // Whole-site link covers any non-traversal path on the right host.
  assert!(share_claims_cover(
    &whole,
    Some("app.example.com"),
    "/anything"
  ));
  // Wrong host is never covered.
  assert!(!share_claims_cover(&whole, Some("other.com"), "/anything"));
  assert!(!share_claims_cover(&whole, None, "/anything"));
  // A traversal segment can widen scope, so it is refused outright.
  assert!(!share_claims_cover(
    &whole,
    Some("app.example.com"),
    "/a/../b"
  ));

  let scoped = claims("app.example.com", Some("/docs"), None);
  assert!(share_claims_cover(
    &scoped,
    Some("app.example.com"),
    "/docs/page"
  ));
  assert!(!share_claims_cover(
    &scoped,
    Some("app.example.com"),
    "/other"
  ));
}

// --- Cookie extraction ---

#[test]
fn cookie_value_parses_the_cookie_header() {
  let mut h = HeaderMap::new();
  h.insert("cookie", "a=1; aperio_share=tok; b=2".parse().unwrap());
  assert_eq!(cookie_value(&h, "aperio_share").as_deref(), Some("tok"));
  assert_eq!(cookie_value(&h, "a").as_deref(), Some("1"));
  // A name that is not present yields None.
  assert_eq!(cookie_value(&h, "missing"), None);
  // No cookie header at all.
  assert_eq!(cookie_value(&HeaderMap::new(), "aperio_share"), None);
  // A malformed segment (no '=') is skipped without matching.
  let mut h2 = HeaderMap::new();
  h2.insert("cookie", "flagonly; x=9".parse().unwrap());
  assert_eq!(cookie_value(&h2, "flagonly"), None);
  assert_eq!(cookie_value(&h2, "x").as_deref(), Some("9"));
}

// --- check_share_access ---

fn uri(s: &str) -> Uri {
  s.parse().unwrap()
}

#[tokio::test]
async fn check_share_access_query_click_redirects_and_sets_cookie() {
  let state = test_state();
  let token = sign_share_claims(&claims("app.example.com", None, Some(future())), &key());
  let u = uri(&format!("/page?aperio_share={token}&foo=bar"));
  let out = check_share_access(&state, &HeaderMap::new(), &u, Some("app.example.com"));
  let resp = out
    .expect("share credential present")
    .expect("redirect response");
  assert_eq!(resp.status(), StatusCode::FOUND);
  // The clean URL drops the share param but keeps the rest.
  assert_eq!(resp.headers().get("location").unwrap(), "/page?foo=bar");
  let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(cookie.starts_with("aperio_share="));
  assert!(cookie.contains("HttpOnly"));
  // secure_cookies is off in the test config → no Secure attribute.
  assert!(!cookie.contains("Secure"));
}

#[tokio::test]
async fn check_share_access_query_only_param_and_secure_and_never_expiring() {
  let mut config = test_config();
  config.secure_cookies = true;
  let state = test_state_with(config);
  // Never-expiring link (exp = None) exercises the 10-year Max-Age branch.
  let token = sign_share_claims(&claims("app.example.com", None, None), &key());
  let u = uri(&format!("/only?aperio_share={token}"));
  let resp = check_share_access(&state, &HeaderMap::new(), &u, Some("app.example.com"))
    .unwrap()
    .unwrap();
  // No leftover params → clean URL is just the path.
  assert_eq!(resp.headers().get("location").unwrap(), "/only");
  let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(cookie.contains("Secure"));
  assert!(cookie.contains(&format!("Max-Age={}", SHARE_MAX_TTL_SECS)));
}

#[tokio::test]
async fn check_share_access_invalid_query_token_is_ignored() {
  let state = test_state();
  let u = uri("/page?aperio_share=garbage&keep=1");
  assert!(check_share_access(&state, &HeaderMap::new(), &u, Some("app.example.com")).is_none());
}

#[tokio::test]
async fn check_share_access_cookie_grants_access() {
  let state = test_state();
  let token = sign_share_claims(
    &claims("app.example.com", Some("/docs"), Some(future())),
    &key(),
  );
  let mut h = HeaderMap::new();
  h.insert("cookie", format!("aperio_share={token}").parse().unwrap());
  // Valid cookie covering the path → proceed (Some(None)).
  let out = check_share_access(&state, &h, &uri("/docs/x"), Some("app.example.com"));
  assert!(matches!(out, Some(None)));
  // A cookie that does not cover this path → no credential.
  assert!(check_share_access(&state, &h, &uri("/other"), Some("app.example.com")).is_none());
}

#[tokio::test]
async fn check_share_access_without_any_credential_is_none() {
  let state = test_state();
  assert!(
    check_share_access(
      &state,
      &HeaderMap::new(),
      &uri("/page"),
      Some("app.example.com")
    )
    .is_none()
  );
  // A malformed cookie token is ignored.
  let mut h = HeaderMap::new();
  h.insert("cookie", "aperio_share=bad.token".parse().unwrap());
  assert!(check_share_access(&state, &h, &uri("/page"), Some("app.example.com")).is_none());
}

// --- share_create_handler ---

async fn state_owning(host: &str) -> std::sync::Arc<AppState> {
  let state = test_state();
  let client = mock_client(Some(host), None, None, None);
  state.clients.lock().await.insert("c1".to_string(), client);
  std::sync::Arc::new(state)
}

#[tokio::test]
async fn share_create_mints_a_link_for_an_owned_host() {
  let state = state_owning("app.example.com").await;
  let payload = ShareCreateRequest {
    hostname: "app.example.com".to_string(),
    path: Some("/docs".to_string()),
    ttl_seconds: Some(3600),
  };
  let resp = share_create_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(payload),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let url = body["url"].as_str().unwrap();
  assert!(url.starts_with("https://app.example.com/docs?aperio_share="));
  assert!(body["token"].is_string());
  assert!(body["expires_at"].is_u64());
}

#[tokio::test]
async fn share_create_never_expiring_link() {
  let state = state_owning("app.example.com").await;
  let payload = ShareCreateRequest {
    hostname: "app.example.com".to_string(),
    path: None,
    ttl_seconds: Some(0),
  };
  let resp = share_create_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(payload),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  // ttl 0 → the whole site, never expiring.
  assert!(
    body["url"]
      .as_str()
      .unwrap()
      .starts_with("https://app.example.com/?aperio_share=")
  );
  assert!(body["expires_at"].is_null());
}

#[tokio::test]
async fn share_create_rejects_invalid_hostname() {
  let state = std::sync::Arc::new(test_state());
  let payload = ShareCreateRequest {
    hostname: "bad host!!".to_string(),
    path: None,
    ttl_seconds: None,
  };
  let resp = share_create_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(payload),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn share_create_rejects_unowned_host() {
  let state = std::sync::Arc::new(test_state());
  let payload = ShareCreateRequest {
    hostname: "app.example.com".to_string(),
    path: None,
    ttl_seconds: None,
  };
  let resp = share_create_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(payload),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn share_create_rejects_invalid_path() {
  let state = state_owning("app.example.com").await;
  let payload = ShareCreateRequest {
    hostname: "app.example.com".to_string(),
    path: Some("/bad path!!".to_string()),
    ttl_seconds: None,
  };
  let resp = share_create_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(payload),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn share_create_rejects_excessive_ttl() {
  let state = state_owning("app.example.com").await;
  let payload = ShareCreateRequest {
    hostname: "app.example.com".to_string(),
    path: None,
    ttl_seconds: Some(SHARE_MAX_TTL_SECS + 1),
  };
  let resp = share_create_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(payload),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
