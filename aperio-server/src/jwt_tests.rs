//! What the `jwt` method accepts and, mostly, what it refuses.
//!
//! Signed here with `HS256` so the tests need no key server: the claim rules,
//! the expiry and the signature check are the same code path whichever
//! algorithm produced the signature, and the parts that differ (fetching a key
//! set, picking one by `kid`) are covered separately below.

use super::*;
use crate::test_support::test_state;

/// A secret long enough for the file to accept it.
const SECRET: &str = "0123456789abcdef-jwt-test-secret";

/// Mints a token with the given claims.
fn token(claims: serde_json::Value) -> String {
  jsonwebtoken::encode(
    &jsonwebtoken::Header::new(Algorithm::HS256),
    &claims,
    &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
  )
  .expect("a signed token")
}

/// Seconds since the epoch, offset by `delta`.
fn at(delta: i64) -> i64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs() as i64;
  now + delta
}

/// The simplest configuration that verifies anything.
fn hmac_config() -> JwtConfig {
  JwtConfig {
    jwks_url: None,
    hmac_secret: Some(SECRET.to_string()),
    issuer: None,
    audience: Vec::new(),
    claims: BTreeMap::new(),
    cookie: None,
  }
}

#[tokio::test]
async fn a_good_token_is_accepted_and_names_who_presented_it() {
  let state = test_state();
  let t = token(serde_json::json!({"sub": "u-1", "email": "alice@example.com", "exp": at(600)}));
  let verified = verify(&state, &hmac_config(), &t)
    .await
    .expect("a valid token");
  // The email rather than `sub`: an opaque provider id means nothing to the
  // application behind the tunnel, and this is what #109 forwards.
  assert_eq!(verified.who.as_deref(), Some("alice@example.com"));
}

#[tokio::test]
async fn the_subject_is_the_identity_when_there_is_no_email() {
  let state = test_state();
  let t = token(serde_json::json!({"sub": "u-1", "exp": at(600)}));
  let verified = verify(&state, &hmac_config(), &t).await.expect("valid");
  assert_eq!(verified.who.as_deref(), Some("u-1"));
}

#[tokio::test]
async fn a_token_signed_by_somebody_else_is_refused() {
  let state = test_state();
  let forged = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(Algorithm::HS256),
    &serde_json::json!({"sub": "u-1", "exp": at(600)}),
    &jsonwebtoken::EncodingKey::from_secret(b"a-different-secret-entirely-here"),
  )
  .unwrap();
  assert!(verify(&state, &hmac_config(), &forged).await.is_none());

  // And so is a token nobody signed: `alg: none` is the classic way in.
  let unsigned = format!(
    "{}.{}.",
    base64_url(br#"{"alg":"none","typ":"JWT"}"#),
    base64_url(br#"{"sub":"u-1"}"#)
  );
  assert!(verify(&state, &hmac_config(), &unsigned).await.is_none());
}

#[tokio::test]
async fn an_expired_token_is_refused_and_one_with_no_expiry_never_arrives() {
  let state = test_state();
  // Well past the library's default sixty seconds of clock leeway, which is
  // what a token one minute old is still inside of.
  let t = token(serde_json::json!({"sub": "u-1", "exp": at(-3600)}));
  assert!(verify(&state, &hmac_config(), &t).await.is_none());

  // `exp` is required whatever the file says: a token with no expiry is one
  // that never stops working.
  let forever = token(serde_json::json!({"sub": "u-1"}));
  assert!(verify(&state, &hmac_config(), &forever).await.is_none());
}

#[tokio::test]
async fn the_issuer_and_audience_are_only_required_when_the_file_asks() {
  let state = test_state();
  let t = token(serde_json::json!({
    "sub": "u-1", "exp": at(600), "iss": "https://accounts.example.com", "aud": "aperio"
  }));

  // Unset: not checked. The library's default is the opposite, and an unset
  // `aud` reading as "accept any audience" would be a silent widening.
  assert!(verify(&state, &hmac_config(), &t).await.is_some());

  let mut cfg = hmac_config();
  cfg.issuer = Some("https://accounts.example.com".to_string());
  cfg.audience = vec!["aperio".to_string()];
  assert!(verify(&state, &cfg, &t).await.is_some());

  cfg.issuer = Some("https://somewhere.else".to_string());
  assert!(verify(&state, &cfg, &t).await.is_none());

  let mut cfg = hmac_config();
  cfg.audience = vec!["something-else".to_string()];
  assert!(verify(&state, &cfg, &t).await.is_none());

  // A token carrying neither against a configuration that wants them. The
  // library only checks these when they are present, so requiring one has to
  // mean requiring the claim as well, or the token the rule was written to
  // keep out is exactly the one that gets in.
  let bare = token(serde_json::json!({"sub": "u-1", "exp": at(600)}));
  let mut cfg = hmac_config();
  cfg.audience = vec!["aperio".to_string()];
  assert!(verify(&state, &cfg, &bare).await.is_none(), "no aud");
  let mut cfg = hmac_config();
  cfg.issuer = Some("https://accounts.example.com".to_string());
  assert!(verify(&state, &cfg, &bare).await.is_none(), "no iss");
}

#[tokio::test]
async fn a_required_claim_must_be_present_and_equal() {
  let state = test_state();
  let mut cfg = hmac_config();
  cfg
    .claims
    .insert("groups".to_string(), "engineering".to_string());

  let ok = token(serde_json::json!({"sub": "u", "exp": at(600), "groups": "engineering"}));
  assert!(verify(&state, &cfg, &ok).await.is_some());

  let wrong = token(serde_json::json!({"sub": "u", "exp": at(600), "groups": "sales"}));
  assert!(verify(&state, &cfg, &wrong).await.is_none());

  let missing = token(serde_json::json!({"sub": "u", "exp": at(600)}));
  assert!(verify(&state, &cfg, &missing).await.is_none());
}

#[tokio::test]
async fn a_numeric_claim_matches_the_text_the_file_writes() {
  // An issuer sending `{"tier": 2}` and a file saying `tier: "2"` are the
  // same intention twice; refusing that would read as the claim missing.
  let state = test_state();
  let mut cfg = hmac_config();
  cfg.claims.insert("tier".to_string(), "2".to_string());
  let t = token(serde_json::json!({"sub": "u", "exp": at(600), "tier": 2}));
  assert!(verify(&state, &cfg, &t).await.is_some());
}

#[tokio::test]
async fn a_key_set_is_never_fetched_from_a_destination_the_outbound_policy_refuses() {
  // The URL comes from a configuration file and makes the server issue a
  // request, which is exactly the shape the policy exists to fence.
  let mut cfg = crate::test_support::test_config();
  cfg.outbound_policy = crate::outbound::OutboundPolicy {
    allowlist: Vec::new(),
    block_private: true,
  };
  let state = crate::test_support::test_state_with(cfg);
  let jwks = JwtConfig {
    jwks_url: Some("http://127.0.0.1:9/keys.json".to_string()),
    hmac_secret: None,
    ..hmac_config()
  };
  let t = token(serde_json::json!({"sub": "u", "exp": at(600)}));
  assert!(verify(&state, &jwks, &t).await.is_none());
  assert!(
    state.jwks_cache.lock().await.is_empty(),
    "nothing was fetched, so nothing is cached"
  );
}

#[test]
fn a_token_must_name_its_key_once_there_is_more_than_one() {
  // Guessing which of several keys signed something is how a verifier accepts
  // a signature the issuer did not mean to make.
  let one: JwkSet = serde_json::from_str(ONE_KEY).unwrap();
  assert!(pick(&one, None).is_some(), "one key needs no name");
  assert!(pick(&one, Some("k1")).is_some());
  assert!(pick(&one, Some("nope")).is_none());

  let two: JwkSet = serde_json::from_str(TWO_KEYS).unwrap();
  assert!(
    pick(&two, None).is_none(),
    "two keys and no `kid` is a guess, not a verification"
  );
  assert!(pick(&two, Some("k2")).is_some());
}

/// base64url without padding, for hand-built tokens.
fn base64_url(bytes: &[u8]) -> String {
  use base64::prelude::*;
  BASE64_URL_SAFE_NO_PAD.encode(bytes)
}

const ONE_KEY: &str = r#"{"keys":[
  {"kty":"oct","kid":"k1","k":"c2VjcmV0"}
]}"#;

const TWO_KEYS: &str = r#"{"keys":[
  {"kty":"oct","kid":"k1","k":"c2VjcmV0"},
  {"kty":"oct","kid":"k2","k":"c2VjcmV0Mg"}
]}"#;

#[tokio::test]
async fn a_key_set_url_cannot_redirect_the_server_past_the_outbound_policy() {
  // The policy vets the URL in the file. If a `Location` were followed, the
  // destination it vetted would not be the destination that gets the request,
  // and an issuer, or anyone able to answer as one, could point the server at
  // whatever the fence exists to refuse.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let reached = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
  let reached_task = reached.clone();
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let reached = reached_task.clone();
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        let out = if head.contains("/moved") {
          reached.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
          let body = r#"{"keys":[]}"#;
          format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
          )
        } else {
          "HTTP/1.1 302 Found\r\nlocation: /moved\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            .to_string()
        };
        let _ = sock.write_all(out.as_bytes()).await;
      });
    }
  });

  let state = test_state();
  let cfg = JwtConfig {
    jwks_url: Some(format!("http://127.0.0.1:{port}/keys.json")),
    hmac_secret: None,
    ..hmac_config()
  };
  let t = token(serde_json::json!({"sub": "u", "exp": at(600)}));
  assert!(verify(&state, &cfg, &t).await.is_none());
  assert_eq!(
    reached.load(std::sync::atomic::Ordering::SeqCst),
    0,
    "the redirect target was fetched"
  );
}
