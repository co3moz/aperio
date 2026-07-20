//! Unit tests for the WebAuthn (passkey) ceremony handlers.
//!
//! Full register + authenticate ceremonies are driven by a software
//! authenticator (`SoftPasskey` from `webauthn-authenticator-rs`, a dev
//! dependency pinned to the same `webauthn-rs-proto` version so the credential
//! types unify). This lets the tests exercise the real cryptographic
//! finish-ceremony paths, not just the guard/error branches.

use super::*;
use crate::state::AppState;
use crate::store::users::Role;
use crate::test_support::*;
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use std::sync::Arc;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
// `Webauthn`, `WebauthnBuilder`, `Url`, the challenge/credential types,
// `Base64UrlSafeData` and `Uuid` all reach us through `use super::*`
// (webauthn.rs re-exports `webauthn_rs::prelude::*`).

const ORIGIN: &str = "https://tunnel.example.com";
const RP_ID: &str = "tunnel.example.com";

/// Serializes `build_webauthn`'s env-var access across the parallel test
/// threads (`APERIO_WEBAUTHN_ORIGIN` is process-global).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn origin_url() -> Url {
  Url::parse(ORIGIN).unwrap()
}

/// A `Webauthn` verifier mirroring `build_webauthn`'s setup.
fn test_webauthn() -> Webauthn {
  WebauthnBuilder::new(RP_ID, &origin_url())
    .unwrap()
    .rp_name("Aperio")
    .build()
    .unwrap()
}

/// An `AppState` with passkey sign-in enabled.
fn enabled_state() -> Arc<AppState> {
  let mut st = test_state();
  st.webauthn = Some(test_webauthn());
  Arc::new(st)
}

/// An `AppState` with passkey sign-in enabled and the given config.
fn enabled_state_with(cfg: crate::settings::ServerConfig) -> Arc<AppState> {
  let mut st = test_state_with(cfg);
  st.webauthn = Some(test_webauthn());
  Arc::new(st)
}

fn soft() -> WebauthnAuthenticator<SoftPasskey> {
  // falsify_uv = true: report user verification so UV-required passkeys verify.
  WebauthnAuthenticator::new(SoftPasskey::new(true))
}

/// Creates a named dashboard user and a signed-in session for it. Returns the
/// user id and the Cookie header carrying the session.
async fn named_user(state: &AppState, username: &str) -> (String, HeaderMap) {
  let user = state
    .users
    .lock()
    .await
    .create(username, "password123", Role::Operator, None)
    .unwrap();
  let token = seed_session(state, Role::Operator, Some(username), None).await;
  (user.id, cookie_headers(&token))
}

/// Runs the register-start handler and returns (ceremony_id, creation challenge).
async fn register_start(
  state: &Arc<AppState>,
  cookie: &HeaderMap,
) -> (String, CreationChallengeResponse) {
  let resp = passkey_register_start_handler(State(state.clone()), cookie.clone()).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let ceremony_id = body["ceremony_id"].as_str().unwrap().to_string();
  let ccr: CreationChallengeResponse = serde_json::from_value(body["challenge"].clone()).unwrap();
  (ceremony_id, ccr)
}

/// Drives a full passkey registration and returns the credential produced by
/// the authenticator (its raw id is the stored credential id).
async fn register_full(
  state: &Arc<AppState>,
  cookie: &HeaderMap,
  auth: &mut WebauthnAuthenticator<SoftPasskey>,
  usernameless: bool,
) -> RegisterPublicKeyCredential {
  let (ceremony_id, ccr) = register_start(state, cookie).await;
  let cred = auth.do_registration(origin_url(), ccr).unwrap();
  let req = PasskeyRegisterFinishRequest {
    ceremony_id,
    name: Some("Test Key".to_string()),
    usernameless,
    credential: cred.clone(),
  };
  let resp = passkey_register_finish_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    cookie.clone(),
    Json(req),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  cred
}

// ---------------------------------------------------------------------------
// build_webauthn (env-driven setup)
// ---------------------------------------------------------------------------

#[test]
fn build_webauthn_unset_is_none() {
  let _g = ENV_LOCK.lock().unwrap();
  unsafe { std::env::remove_var("APERIO_WEBAUTHN_ORIGIN") };
  assert!(build_webauthn().is_none());
}

#[test]
fn build_webauthn_empty_is_none() {
  let _g = ENV_LOCK.lock().unwrap();
  unsafe { std::env::set_var("APERIO_WEBAUTHN_ORIGIN", "   ") };
  assert!(build_webauthn().is_none());
  unsafe { std::env::remove_var("APERIO_WEBAUTHN_ORIGIN") };
}

#[test]
fn build_webauthn_invalid_url_is_none() {
  let _g = ENV_LOCK.lock().unwrap();
  unsafe { std::env::set_var("APERIO_WEBAUTHN_ORIGIN", "not a url") };
  assert!(build_webauthn().is_none());
  unsafe { std::env::remove_var("APERIO_WEBAUTHN_ORIGIN") };
}

#[test]
fn build_webauthn_ip_origin_is_none() {
  let _g = ENV_LOCK.lock().unwrap();
  // An IP address has no domain, so it cannot be an RP ID.
  unsafe { std::env::set_var("APERIO_WEBAUTHN_ORIGIN", "https://127.0.0.1:8443") };
  assert!(build_webauthn().is_none());
  unsafe { std::env::remove_var("APERIO_WEBAUTHN_ORIGIN") };
}

#[test]
fn build_webauthn_valid_is_some() {
  let _g = ENV_LOCK.lock().unwrap();
  unsafe { std::env::set_var("APERIO_WEBAUTHN_ORIGIN", ORIGIN) };
  assert!(build_webauthn().is_some());
  unsafe { std::env::remove_var("APERIO_WEBAUTHN_ORIGIN") };
}

// ---------------------------------------------------------------------------
// Disabled (APERIO_WEBAUTHN_ORIGIN unset) => 501 for every ceremony handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handlers_501_when_disabled() {
  let state = Arc::new(test_state()); // webauthn = None
  let headers = HeaderMap::new();

  let r = passkey_register_start_handler(State(state.clone()), headers.clone()).await;
  assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);

  let r = passkey_register_finish_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(PasskeyRegisterFinishRequest {
      ceremony_id: "x".into(),
      name: None,
      usernameless: false,
      credential: dummy_register_credential(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);

  let r = passkey_login_start_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(PasskeyLoginStartRequest {
      username: "u".into(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);

  let r = passkey_login_finish_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id: "x".into(),
      credential: dummy_login_credential(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);

  let r = passkey_discoverable_start_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
  )
  .await;
  assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);

  let r = passkey_discoverable_finish_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(PasskeyLoginFinishRequest {
      ceremony_id: "x".into(),
      credential: dummy_login_credential(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);
}

/// A syntactically valid but semantically empty registration credential, used
/// only where the handler returns before touching it.
fn dummy_register_credential() -> RegisterPublicKeyCredential {
  serde_json::from_value(serde_json::json!({
    "id": "AAAA",
    "rawId": "AAAA",
    "response": { "attestationObject": "AAAA", "clientDataJSON": "AAAA" },
    "type": "public-key"
  }))
  .unwrap()
}

fn dummy_login_credential() -> PublicKeyCredential {
  serde_json::from_value(serde_json::json!({
    "id": "AAAA",
    "rawId": "AAAA",
    "response": {
      "authenticatorData": "AAAA",
      "clientDataJSON": "AAAA",
      "signature": "AAAA"
    },
    "type": "public-key"
  }))
  .unwrap()
}

// ---------------------------------------------------------------------------
// Availability probe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn available_reflects_config() {
  let disabled = Arc::new(test_state());
  let r = passkey_available_handler(State(disabled)).await;
  assert_eq!(json_body(r).await["available"], serde_json::json!(false));

  let enabled = enabled_state();
  let r = passkey_available_handler(State(enabled)).await;
  assert_eq!(json_body(r).await["available"], serde_json::json!(true));
}

// ---------------------------------------------------------------------------
// session_user guard (via register-start)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_start_requires_named_user() {
  let state = enabled_state();
  // No session cookie => not a named dashboard user.
  let r = passkey_register_start_handler(State(state), HeaderMap::new()).await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_start_unknown_user() {
  let state = enabled_state();
  // A session naming a user that does not exist in the store.
  let token = seed_session(&state, Role::Operator, Some("ghost"), None).await;
  let r = passkey_register_start_handler(State(state), cookie_headers(&token)).await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Full registration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_full_flow_stores_passkey() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  register_full(&state, &cookie, &mut auth, false).await;

  let users = state.users.lock().await;
  assert_eq!(users.get(&user_id).unwrap().passkeys.len(), 1);
  assert_eq!(users.get(&user_id).unwrap().passkeys[0].name, "Test Key");
}

#[tokio::test]
async fn register_start_excludes_existing_credentials() {
  let state = enabled_state();
  let (_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  // First real passkey.
  register_full(&state, &cookie, &mut auth, false).await;
  // A second start should now build an exclude list from the stored passkey.
  let (_ceremony, ccr) = register_start(&state, &cookie).await;
  assert!(
    ccr
      .public_key
      .exclude_credentials
      .as_ref()
      .is_some_and(|v| !v.is_empty()),
    "exclude list should contain the already-registered credential"
  );
}

#[tokio::test]
async fn register_finish_requires_named_user() {
  let state = enabled_state();
  // No session cookie: the session_user guard rejects before touching the body.
  let req = PasskeyRegisterFinishRequest {
    ceremony_id: "x".into(),
    name: None,
    usernameless: false,
    credential: dummy_register_credential(),
  };
  let r = passkey_register_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(req),
  )
  .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_finish_unknown_ceremony() {
  let state = enabled_state();
  let (_id, cookie) = named_user(&state, "alice").await;
  let req = PasskeyRegisterFinishRequest {
    ceremony_id: "does-not-exist".into(),
    name: None,
    usernameless: false,
    credential: dummy_register_credential(),
  };
  let r =
    passkey_register_finish_handler(State(state), ConnectInfo(test_peer()), cookie, Json(req))
      .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_finish_wrong_owner() {
  let state = enabled_state();
  let (_a_id, a_cookie) = named_user(&state, "alice").await;
  let (_b_id, b_cookie) = named_user(&state, "bob").await;
  let mut auth = soft();
  // Ceremony owned by alice.
  let (ceremony_id, ccr) = register_start(&state, &a_cookie).await;
  let cred = auth.do_registration(origin_url(), ccr).unwrap();
  // Bob tries to finish alice's ceremony.
  let req = PasskeyRegisterFinishRequest {
    ceremony_id,
    name: None,
    usernameless: false,
    credential: cred,
  };
  let r =
    passkey_register_finish_handler(State(state), ConnectInfo(test_peer()), b_cookie, Json(req))
      .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_finish_bad_credential() {
  let state = enabled_state();
  let (_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  // Credential produced for ceremony A ...
  let (_ceremony_a, ccr_a) = register_start(&state, &cookie).await;
  let cred_a = auth.do_registration(origin_url(), ccr_a).unwrap();
  // ... submitted against ceremony B: the challenge won't match.
  let (ceremony_b, _ccr_b) = register_start(&state, &cookie).await;
  let req = PasskeyRegisterFinishRequest {
    ceremony_id: ceremony_b,
    name: None,
    usernameless: false,
    credential: cred_a,
  };
  let r =
    passkey_register_finish_handler(State(state), ConnectInfo(test_peer()), cookie, Json(req))
      .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_finish_store_rejects_when_full() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  // Pre-fill 10 passkey slots with non-parseable credential JSON so the
  // exclude list stays empty (start still succeeds) but add_passkey caps out.
  {
    let mut users = state.users.lock().await;
    for i in 0..10 {
      users
        .add_passkey(&user_id, &format!("filler{i}"), "{}", false)
        .unwrap();
    }
  }
  let mut auth = soft();
  let (ceremony_id, ccr) = register_start(&state, &cookie).await;
  let cred = auth.do_registration(origin_url(), ccr).unwrap();
  let req = PasskeyRegisterFinishRequest {
    ceremony_id,
    name: None,
    usernameless: false,
    credential: cred,
  };
  let r =
    passkey_register_finish_handler(State(state), ConnectInfo(test_peer()), cookie, Json(req))
      .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// List / delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_and_delete_passkeys() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  register_full(&state, &cookie, &mut auth, false).await;

  let r = passkeys_list_handler(State(state.clone()), cookie.clone()).await;
  let list = json_body(r).await;
  assert_eq!(list.as_array().unwrap().len(), 1);
  let id = list[0]["id"].as_str().unwrap().to_string();

  // Delete it.
  let r = passkey_delete_handler(
    State(state.clone()),
    axum::extract::Path(id.clone()),
    ConnectInfo(test_peer()),
    cookie.clone(),
  )
  .await;
  assert_eq!(r.status(), StatusCode::OK);
  assert_eq!(
    state
      .users
      .lock()
      .await
      .get(&user_id)
      .unwrap()
      .passkeys
      .len(),
    0
  );

  // Deleting again => 404.
  let r = passkey_delete_handler(
    State(state.clone()),
    axum::extract::Path(id),
    ConnectInfo(test_peer()),
    cookie,
  )
  .await;
  assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_requires_named_user() {
  let state = enabled_state();
  let r = passkeys_list_handler(State(state), HeaderMap::new()).await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_requires_named_user() {
  let state = enabled_state();
  let r = passkey_delete_handler(
    State(state),
    axum::extract::Path("x".into()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Username-first login
// ---------------------------------------------------------------------------

async fn login_start(state: &Arc<AppState>, username: &str) -> (String, RequestChallengeResponse) {
  let resp = passkey_login_start_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginStartRequest {
      username: username.into(),
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let ceremony_id = body["ceremony_id"].as_str().unwrap().to_string();
  let rcr: RequestChallengeResponse = serde_json::from_value(body["challenge"].clone()).unwrap();
  (ceremony_id, rcr)
}

#[tokio::test]
async fn login_full_flow_issues_session() {
  let state = enabled_state();
  let (_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  register_full(&state, &cookie, &mut auth, false).await;

  let (ceremony_id, rcr) = login_start(&state, "alice").await;
  let cred = auth.do_authentication(origin_url(), rcr).unwrap();
  let resp = passkey_login_finish_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let set_cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(set_cookie.contains("aperio_session="));
}

#[tokio::test]
async fn login_start_unknown_user_is_401() {
  let state = enabled_state();
  let r = passkey_login_start_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginStartRequest {
      username: "nobody".into(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_start_user_without_passkeys_is_401() {
  let state = enabled_state();
  named_user(&state, "alice").await; // no passkeys registered
  let r = passkey_login_start_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginStartRequest {
      username: "alice".into(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_start_rate_limited() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 1.0;
  cfg.ip_limit_refill = 0.0;
  let state = enabled_state_with(cfg);
  named_user(&state, "alice").await;
  // Drain the single token so the handler's own check fails.
  assert!(state.check_rate_limit(test_peer().ip()).await);
  let r = passkey_login_start_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginStartRequest {
      username: "alice".into(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn login_start_locked_out() {
  let state = enabled_state();
  named_user(&state, "alice").await;
  {
    let mut lock = state.login_lockout.lock().await;
    let now = std::time::Instant::now();
    for _ in 0..5 {
      lock.record_failure(test_peer().ip(), now);
    }
  }
  let r = passkey_login_start_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginStartRequest {
      username: "alice".into(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn login_finish_unknown_ceremony() {
  let state = enabled_state();
  let r = passkey_login_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id: "nope".into(),
      credential: dummy_login_credential(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_finish_bad_credential_records_lockout() {
  let state = enabled_state();
  let (_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  register_full(&state, &cookie, &mut auth, false).await;
  // Pre-load 4 failures so this failed finish is the 5th and triggers lockout
  // (exercising the lockout-audit branch).
  {
    let mut lock = state.login_lockout.lock().await;
    let now = std::time::Instant::now();
    for _ in 0..4 {
      lock.record_failure(test_peer().ip(), now);
    }
  }
  let (ceremony_id, _rcr) = login_start(&state, "alice").await;
  // Submit a credential the ceremony never issued a challenge for.
  let r = passkey_login_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: dummy_login_credential(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_finish_disabled_user() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  register_full(&state, &cookie, &mut auth, false).await;
  let (ceremony_id, rcr) = login_start(&state, "alice").await;
  let cred = auth.do_authentication(origin_url(), rcr).unwrap();
  // Disable the user after the ceremony started but before it finishes.
  state
    .users
    .lock()
    .await
    .update(&user_id, None, Some(false), None)
    .unwrap();
  let r = passkey_login_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Usernameless (discoverable) login
// ---------------------------------------------------------------------------

async fn discoverable_start(state: &Arc<AppState>) -> (String, RequestChallengeResponse) {
  let resp = passkey_discoverable_start_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let ceremony_id = body["ceremony_id"].as_str().unwrap().to_string();
  let rcr: RequestChallengeResponse = serde_json::from_value(body["challenge"].clone()).unwrap();
  (ceremony_id, rcr)
}

/// Signs the discoverable challenge with the software authenticator. A real
/// browser feeds the resident credential automatically; SoftPasskey needs the
/// credential id in `allowCredentials`, and does not emit a user handle, so we
/// inject the one we registered with (the user's UUID) to mimic a resident
/// credential's assertion.
fn sign_discoverable(
  auth: &mut WebauthnAuthenticator<SoftPasskey>,
  rcr: RequestChallengeResponse,
  raw_id: &Base64UrlSafeData,
  user_uuid: Option<uuid::Uuid>,
) -> PublicKeyCredential {
  // Inject the credential id into `allowCredentials` (via JSON, so we don't
  // need the non-prelude `AllowCredentials` type) so SoftPasskey signs with it.
  let mut v = serde_json::to_value(&rcr).unwrap();
  v["publicKey"]["allowCredentials"] = serde_json::json!([{
    "type": "public-key",
    "id": serde_json::to_value(raw_id).unwrap(),
  }]);
  let rcr: RequestChallengeResponse = serde_json::from_value(v).unwrap();
  let mut cred = auth.do_authentication(origin_url(), rcr).unwrap();
  cred.response.user_handle = user_uuid.map(|u| Base64UrlSafeData::from(u.as_bytes().to_vec()));
  cred
}

#[tokio::test]
async fn discoverable_full_flow_issues_session() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  let reg = register_full(&state, &cookie, &mut auth, true).await;

  let (ceremony_id, rcr) = discoverable_start(&state).await;
  let uuid = uuid::Uuid::parse_str(&user_id).unwrap();
  let cred = sign_discoverable(&mut auth, rcr, &reg.raw_id, Some(uuid));
  let resp = passkey_discoverable_finish_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert!(
    resp
      .headers()
      .get("set-cookie")
      .unwrap()
      .to_str()
      .unwrap()
      .contains("aperio_session=")
  );
}

#[tokio::test]
async fn discoverable_finish_unknown_ceremony() {
  let state = enabled_state();
  let r = passkey_discoverable_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id: "nope".into(),
      credential: dummy_login_credential(),
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn discoverable_finish_unidentifiable_credential() {
  let state = enabled_state();
  let (_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  let reg = register_full(&state, &cookie, &mut auth, true).await;
  // Pre-load 4 failures so the failed identify locks out (audit branch).
  {
    let mut lock = state.login_lockout.lock().await;
    let now = std::time::Instant::now();
    for _ in 0..4 {
      lock.record_failure(test_peer().ip(), now);
    }
  }
  let (ceremony_id, rcr) = discoverable_start(&state).await;
  // No user handle => cannot be identified.
  let cred = sign_discoverable(&mut auth, rcr, &reg.raw_id, None);
  let r = passkey_discoverable_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discoverable_finish_no_passkeys_for_user() {
  let state = enabled_state();
  // A user that exists but has no passkeys; its UUID goes in the user handle.
  let (empty_id, _c) = named_user(&state, "empty").await;
  // Another user with a real (usernameless) passkey to produce a signature.
  let (_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  let reg = register_full(&state, &cookie, &mut auth, true).await;

  let (ceremony_id, rcr) = discoverable_start(&state).await;
  let uuid = uuid::Uuid::parse_str(&empty_id).unwrap();
  let cred = sign_discoverable(&mut auth, rcr, &reg.raw_id, Some(uuid));
  let r = passkey_discoverable_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discoverable_finish_not_opted_in() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  // Registered WITHOUT the usernameless opt-in.
  let reg = register_full(&state, &cookie, &mut auth, false).await;

  let (ceremony_id, rcr) = discoverable_start(&state).await;
  let uuid = uuid::Uuid::parse_str(&user_id).unwrap();
  let cred = sign_discoverable(&mut auth, rcr, &reg.raw_id, Some(uuid));
  let r = passkey_discoverable_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discoverable_finish_verification_failure() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  let reg = register_full(&state, &cookie, &mut auth, true).await;

  // Ceremony 1 is the one we submit to; ceremony 2 supplies the challenge we
  // actually sign, so the assertion won't verify against ceremony 1.
  let (ceremony_1, _rcr_1) = discoverable_start(&state).await;
  let (_ceremony_2, rcr_2) = discoverable_start(&state).await;
  let uuid = uuid::Uuid::parse_str(&user_id).unwrap();
  let cred = sign_discoverable(&mut auth, rcr_2, &reg.raw_id, Some(uuid));
  let r = passkey_discoverable_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id: ceremony_1,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discoverable_finish_verification_failure_locks_out() {
  let state = enabled_state();
  let (user_id, cookie) = named_user(&state, "alice").await;
  let mut auth = soft();
  let reg = register_full(&state, &cookie, &mut auth, true).await;
  // Pre-load 4 failures so a verification failure is the 5th and locks out,
  // exercising the lockout-audit branch inside the finish error path.
  {
    let mut lock = state.login_lockout.lock().await;
    let now = std::time::Instant::now();
    for _ in 0..4 {
      lock.record_failure(test_peer().ip(), now);
    }
  }
  let (ceremony_1, _rcr_1) = discoverable_start(&state).await;
  let (_ceremony_2, rcr_2) = discoverable_start(&state).await;
  let uuid = uuid::Uuid::parse_str(&user_id).unwrap();
  let cred = sign_discoverable(&mut auth, rcr_2, &reg.raw_id, Some(uuid));
  let r = passkey_discoverable_finish_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(PasskeyLoginFinishRequest {
      ceremony_id: ceremony_1,
      credential: cred,
    }),
  )
  .await;
  assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discoverable_start_rate_limited() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 1.0;
  cfg.ip_limit_refill = 0.0;
  let state = enabled_state_with(cfg);
  assert!(state.check_rate_limit(test_peer().ip()).await);
  let r =
    passkey_discoverable_start_handler(State(state), ConnectInfo(test_peer()), HeaderMap::new())
      .await;
  assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn discoverable_start_locked_out() {
  let state = enabled_state();
  {
    let mut lock = state.login_lockout.lock().await;
    let now = std::time::Instant::now();
    for _ in 0..5 {
      lock.record_failure(test_peer().ip(), now);
    }
  }
  let r =
    passkey_discoverable_start_handler(State(state), ConnectInfo(test_peer()), HeaderMap::new())
      .await;
  assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// Ceremony store garbage collection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ceremonies_gc_evicts_expired() {
  let webauthn = test_webauthn();
  let uuid = uuid::Uuid::new_v4();
  let (_ccr, reg_state) = webauthn
    .start_passkey_registration(uuid, "alice", "alice", None)
    .unwrap();
  let (_rcr, disc_state) = webauthn.start_discoverable_authentication().unwrap();

  let mut c = WebauthnCeremonies::default();
  let stale = std::time::Instant::now() - CHALLENGE_TTL - std::time::Duration::from_secs(1);
  let fresh = std::time::Instant::now();
  c.reg
    .insert("stale".into(), (stale, "u".into(), reg_state.clone()));
  c.reg.insert("fresh".into(), (fresh, "u".into(), reg_state));
  c.disc.insert("stale".into(), (stale, disc_state.clone()));
  c.disc.insert("fresh".into(), (fresh, disc_state));

  c.gc();

  assert!(c.reg.contains_key("fresh"));
  assert!(!c.reg.contains_key("stale"));
  assert!(c.disc.contains_key("fresh"));
  assert!(!c.disc.contains_key("stale"));
}
