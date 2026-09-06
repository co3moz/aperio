//! The OIDC flow end to end: where the provider is sent, what the callback
//! accepts, and every way it can fail without minting a session.

use super::super::tests::*;
use super::*;
use crate::test_support::*;
use std::sync::Arc;

// --- OIDC helpers -----------------------------------------------------------

/// Constructs an [`OidcRuntime`] pointing at `base`, with a fixed redirect
/// override so the callback flow needs no Host header.
fn oidc_runtime(base: &str, allowed: Vec<String>) -> crate::oidc::OidcRuntime {
  crate::oidc::OidcRuntime {
    authorization_endpoint: format!("{base}/authorize"),
    token_endpoint: format!("{base}/token"),
    userinfo_endpoint: format!("{base}/userinfo"),
    client_id: "cid".to_string(),
    client_secret: "secret".to_string(),
    scopes: "openid email".to_string(),
    allowed_emails: allowed,
    redirect_url_override: Some("http://localhost/aperio/oidc/callback".to_string()),
    grants: crate::oidc::OidcGrantPolicy::default(),
  }
}

/// A throwaway HTTP server: POST requests (the token exchange) get
/// `token`, everything else (userinfo) gets `info`. Returns its base URL.
async fn mock_oidc_server(
  token_status: u16,
  token_body: impl Into<String>,
  info_status: u16,
  info_body: impl Into<String>,
) -> String {
  // Owned rather than `&'static str`, so a case can serve a body the size a
  // real one is. While these were literals every body in the file was a few
  // dozen bytes, and a mutation run showed what that cost: the cap on how much
  // an OIDC endpoint may return could be dropped from 256 KiB to 1280 bytes
  // and every one of them still fit.
  let token_body: String = token_body.into();
  let info_body: String = info_body.into();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    while let Ok((mut sock, _)) = listener.accept().await {
      let (token_body, info_body) = (token_body.clone(), info_body.clone());
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
          return;
        }
        let is_token = buf.starts_with(b"POST");
        let (status, body) = if is_token {
          (token_status, token_body.as_str())
        } else {
          (info_status, info_body.as_str())
        };
        let resp = format!(
          "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
      });
    }
  });
  format!("http://{addr}")
}

async fn seed_oidc_state(state: &AppState, token: &str, bound: Option<String>) {
  state.oidc_states.lock().await.insert(
    token.to_string(),
    (
      "/after".to_string(),
      bound,
      "http://dash.test/aperio/oidc/callback".to_string(),
      Instant::now() + Duration::from_secs(600),
    ),
  );
}

fn oidc_query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
  pairs
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

async fn call_oidc_callback(state: Arc<AppState>, query: HashMap<String, String>) -> Response {
  oidc_callback_handler(
    State(state),
    axum::extract::Query(query),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
  )
  .await
}

#[tokio::test]
async fn oidc_callback_success_creates_session() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"user@allow.com\"}",
  )
  .await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  record_for(&state, "user@allow.com").await;
  seed_oidc_state(&state, "csrf1", None).await;
  let resp = call_oidc_callback(
    state.clone(),
    oidc_query(&[("code", "c"), ("state", "csrf1")]),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  assert_eq!(resp.headers().get("location").unwrap(), "/after");
  assert!(resp.headers().get("set-cookie").is_some());
  assert_eq!(state.sessions.lock().await.len(), 1);
}

/// A JWT of about `bytes`, in the shape a provider actually sends: three
/// base64url segments, the payload carrying the claims that make a real one
/// large (groups, roles, a picture URL, the issuer's own metadata).
fn jwt_of(bytes: usize) -> String {
  let head = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6Ims1In0";
  let sig = "c2lnbmF0dXJl";
  let filler = "a".repeat(bytes.saturating_sub(head.len() + sig.len() + 2));
  format!("{head}.{filler}.{sig}")
}

/// A token response the size a real one is, is accepted.
///
/// The cap on how much an OIDC endpoint may make one login cost is
/// `OIDC_MAX_ANSWER_BYTES`, and nothing held it to a usable value: every body
/// in this file was a few dozen bytes, so the constant could be anything from
/// one kilobyte upwards and the suite stayed green. A mutation run found it by
/// turning `256 * 1024` into `256 + 1024`, which breaks every real login and
/// broke no test.
///
/// Asserting acceptance rather than refusal is deliberate, and it is what the
/// surviving mutant pointed at: the cap being too *tight* is the failure that
/// nothing else notices, because a cap being too loose at least still logs
/// people in. The refusal side is covered where the bound itself lives, in
/// `read_bounded`'s own tests.
#[tokio::test]
async fn a_normal_sized_token_response_is_not_refused_as_oversized() {
  // Comfortably past 1280 bytes and nowhere near 256 KiB: an access token and
  // an id token of the size providers issue, which is where a real response
  // spends its length.
  let token_body = format!(
    "{{\"access_token\":\"{}\",\"id_token\":\"{}\",\"token_type\":\"Bearer\",\"expires_in\":3600}}",
    jwt_of(1800),
    jwt_of(2400),
  );
  assert!(
    token_body.len() > 4000,
    "the fixture has to be big enough to matter: {}",
    token_body.len()
  );
  let base = mock_oidc_server(200, token_body, 200, "{\"email\":\"user@allow.com\"}").await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  record_for(&state, "user@allow.com").await;
  seed_oidc_state(&state, "csrf-big", None).await;
  let resp = call_oidc_callback(
    state.clone(),
    oidc_query(&[("code", "c"), ("state", "csrf-big")]),
  )
  .await;
  assert_eq!(
    resp.status(),
    StatusCode::FOUND,
    "a normal token response must log the visitor in; a 302 back to the login \
     page here means the answer was read as oversized"
  );
  assert_eq!(state.sessions.lock().await.len(), 1, "a session was minted");
}

#[tokio::test]
async fn oidc_callback_email_denied() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"bad@x.com\"}",
  )
  .await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["good@x.com".to_string()]));
  let state = Arc::new(state);
  seed_oidc_state(&state, "csrf1", None).await;
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "csrf1")])).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn oidc_callback_token_rejected() {
  let base = mock_oidc_server(400, "no", 200, "{}").await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  seed_oidc_state(&state, "csrf1", None).await;
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "csrf1")])).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_callback_token_parse_error() {
  let base = mock_oidc_server(200, "not-json", 200, "{}").await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  seed_oidc_state(&state, "csrf1", None).await;
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "csrf1")])).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn oidc_callback_userinfo_error_and_parse() {
  // userinfo non-success.
  let base = mock_oidc_server(200, "{\"access_token\":\"AT\"}", 500, "boom").await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  seed_oidc_state(&state, "csrf1", None).await;
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "csrf1")])).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

  // userinfo success but unparseable body.
  let base2 = mock_oidc_server(200, "{\"access_token\":\"AT\"}", 200, "not-json").await;
  let mut state2 = test_state();
  state2.oidc = Some(oidc_runtime(&base2, vec!["*".to_string()]));
  let state2 = Arc::new(state2);
  seed_oidc_state(&state2, "csrf2", None).await;
  let resp2 = call_oidc_callback(state2, oidc_query(&[("code", "c"), ("state", "csrf2")])).await;
  assert_eq!(resp2.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn oidc_callback_token_connection_error() {
  // Port 1 is not listening -> the token exchange request errors out.
  let mut state = test_state();
  state.oidc = Some(oidc_runtime("http://127.0.0.1:1", vec!["*".to_string()]));
  let state = Arc::new(state);
  seed_oidc_state(&state, "csrf1", None).await;
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "csrf1")])).await;
  assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn oidc_callback_bad_requests() {
  let mut state = test_state();
  state.oidc = Some(oidc_runtime("http://127.0.0.1:1", vec!["*".to_string()]));
  let state = Arc::new(state);
  // Missing code/state.
  let resp = call_oidc_callback(state.clone(), oidc_query(&[])).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  // Unknown / expired CSRF state.
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "nope")])).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oidc_callback_rate_limited() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let mut state = test_state_with(cfg);
  state.oidc = Some(oidc_runtime("http://127.0.0.1:1", vec!["*".to_string()]));
  let state = Arc::new(state);
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "x")])).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn oidc_callback_bound_org_unresolvable() {
  let mut state = test_state();
  state.oidc = Some(oidc_runtime("http://127.0.0.1:1", vec!["*".to_string()]));
  let state = Arc::new(state);
  // CSRF state references an org with no OIDC config -> NOT_FOUND.
  seed_oidc_state(&state, "csrf1", Some("ghost-org".to_string())).await;
  let resp = call_oidc_callback(state, oidc_query(&[("code", "c"), ("state", "csrf1")])).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// --- oidc_login_handler -----------------------------------------------------

async fn call_oidc_login(
  state: Arc<AppState>,
  query: HashMap<String, String>,
  headers: HeaderMap,
) -> Response {
  oidc_login_handler(
    State(state),
    axum::extract::Query(query),
    ConnectInfo(test_peer()),
    headers,
  )
  .await
}

#[tokio::test]
async fn oidc_login_redirects_to_provider() {
  let mut state = test_state();
  // No redirect override -> the redirect URI is derived from the Host header.
  let mut rt = oidc_runtime("http://idp.test", vec!["*".to_string()]);
  rt.redirect_url_override = None;
  state.oidc = Some(rt);
  let state = Arc::new(state);
  let mut headers = HeaderMap::new();
  headers.insert("host", "dash.local".parse().unwrap());
  let resp = call_oidc_login(state.clone(), oidc_query(&[("redirect", "/dash")]), headers).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  let loc = resp.headers().get("location").unwrap().to_str().unwrap();
  assert!(loc.starts_with("http://idp.test/authorize"));
  assert!(loc.contains("state="));
  // A CSRF state was registered.
  assert_eq!(state.oidc_states.lock().await.len(), 1);
}

#[tokio::test]
async fn oidc_login_trust_proxy_proto() {
  let mut cfg = test_config();
  cfg.trust_proxy = true;
  let mut state = test_state_with(cfg);
  let mut rt = oidc_runtime("http://idp.test", vec!["*".to_string()]);
  rt.redirect_url_override = None;
  state.oidc = Some(rt);
  let state = Arc::new(state);
  let mut headers = HeaderMap::new();
  headers.insert("host", "dash.local".parse().unwrap());
  headers.insert("x-forwarded-proto", "https".parse().unwrap());
  let resp = call_oidc_login(state, oidc_query(&[]), headers).await;
  // The redirect_uri (https-derived) is embedded in the authorize URL.
  let loc = resp.headers().get("location").unwrap().to_str().unwrap();
  assert!(loc.contains("https%3A%2F%2Fdash.local"));
}

#[tokio::test]
async fn oidc_login_missing_host() {
  let mut state = test_state();
  let mut rt = oidc_runtime("http://idp.test", vec!["*".to_string()]);
  rt.redirect_url_override = None;
  state.oidc = Some(rt);
  let state = Arc::new(state);
  // No Host header and no override -> cannot build the redirect URI.
  let resp = call_oidc_login(state, oidc_query(&[]), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oidc_login_not_configured() {
  let state = Arc::new(test_state());
  let resp = call_oidc_login(state, oidc_query(&[]), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn oidc_login_rate_limited() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let mut state = test_state_with(cfg);
  state.oidc = Some(oidc_runtime("http://idp.test", vec!["*".to_string()]));
  let state = Arc::new(state);
  let resp = call_oidc_login(state, oidc_query(&[]), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn oidc_login_per_org() {
  let mut state = test_state();
  // The redirect URI is derived from the global runtime's override, so a
  // per-org login still requires a global OIDC runtime to be present.
  state.oidc = Some(oidc_runtime(
    "http://global-idp.test",
    vec!["*".to_string()],
  ));
  // A cached per-org runtime resolves the org path and binds the session.
  state.org_oidc.lock().await.insert(
    "org-1".to_string(),
    oidc_runtime("http://org-idp.test", vec!["*".to_string()]),
  );
  let state = Arc::new(state);
  let mut headers = HeaderMap::new();
  headers.insert(
    "host",
    axum::http::HeaderValue::from_static("dash.example.com"),
  );
  let resp = call_oidc_login(state.clone(), oidc_query(&[("org", "org-1")]), headers).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  // The registered CSRF state carries the bound org.
  let states = state.oidc_states.lock().await;
  let (_, bound, _, _) = states.values().next().unwrap();
  assert_eq!(bound.as_deref(), Some("org-1"));
}

#[tokio::test]
async fn oidc_login_per_org_unconfigured() {
  let state = Arc::new(test_state());
  // `?org=` for an org with no OIDC -> NOT_FOUND (org-specific message).
  let resp = call_oidc_login(state, oidc_query(&[("org", "ghost")]), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// --- resolve_org_oidc -------------------------------------------------------

#[tokio::test]
async fn resolve_org_oidc_cache_and_misses() {
  let state = test_state();
  // Cached hit.
  state
    .org_oidc
    .lock()
    .await
    .insert("org-1".to_string(), oidc_runtime("http://x", vec![]));
  assert!(resolve_org_oidc(&state, "org-1").await.is_some());
  // Unknown org -> None.
  assert!(resolve_org_oidc(&state, "missing").await.is_none());
  // Existing org without an OIDC override -> None.
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap();
  assert!(resolve_org_oidc(&state, &org.id).await.is_none());
}

#[tokio::test]
async fn the_visitor_password_is_not_a_dashboard_credential() {
  // `server_auth` is documented as a login form in front of proxied traffic:
  // an operator hands it to whoever should see the *site*. If the session it
  // creates also opens the dashboard, that value is an admin credential and
  // nothing says so.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("visitor:pass");
  let state = Arc::new(test_state_with(cfg));
  let res = call_login(
    state.clone(),
    basic_headers("visitor:pass", Some("site.test")),
    login_query(Some("/some/page")),
  )
  .await
  .unwrap();
  // From the `Set-Cookie`, since the store keys sessions by a hash of the
  // token rather than by the token: reading the map would give a key no
  // request could ever present, and the assertion below would then pass for
  // the wrong reason.
  let set_cookie = res
    .headers()
    .get("set-cookie")
    .unwrap()
    .to_str()
    .unwrap()
    .to_string();
  let token = set_cookie
    .split(';')
    .next()
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .expect("a session token in the cookie");
  let headers = crate::test_support::cookie_headers(&token);

  // First prove the cookie is real and found, or the assertion below would
  // pass for the wrong reason: a session nobody can read opens nothing.
  assert!(
    crate::auth::validate_session(&state, &headers).await,
    "the session cookie should be valid for a proxied request"
  );
  assert!(
    crate::auth::dashboard_role(&state, &headers)
      .await
      .is_none(),
    "the visitor password opened the dashboard"
  );
}

#[tokio::test]
async fn a_visitor_session_still_reaches_every_proxied_host() {
  // The scope and the plane are different questions, and the fix must not
  // answer the first one by accident: the server's gate is server-wide, so a
  // visitor who signed in on one hostname should not be asked again on the
  // next. Only the dashboard is closed to them.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("visitor:pass");
  let state = Arc::new(test_state_with(cfg));
  let res = call_login(
    state.clone(),
    basic_headers("visitor:pass", Some("one.example.com")),
    login_query(Some("/some/page")),
  )
  .await
  .unwrap();
  let token = res
    .headers()
    .get("set-cookie")
    .and_then(|v| v.to_str().ok())
    .and_then(|c| c.split(';').next())
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .expect("a session token");
  let headers = crate::test_support::cookie_headers(&token);

  for host in ["one.example.com", "two.example.com"] {
    assert!(
      crate::auth::validate_session_for_visitor(&state, &headers, Some(host)).await,
      "the visitor gate should admit this session on {host}"
    );
  }
  assert!(
    crate::auth::dashboard_role(&state, &headers)
      .await
      .is_none()
  );
}

#[tokio::test]
async fn an_admin_session_is_still_an_admin_session() {
  // The master token and a named user administer Aperio, and this change must
  // not quietly demote them.
  let state = Arc::new(test_state());
  let master = format!("aperio:{}", state.config().token);
  let res = call_login(
    state.clone(),
    basic_headers(&master, Some("site.test")),
    login_query(Some("/aperio")),
  )
  .await
  .unwrap();
  let token = res
    .headers()
    .get("set-cookie")
    .and_then(|v| v.to_str().ok())
    .and_then(|c| c.split(';').next())
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .expect("a session token");
  let headers = crate::test_support::cookie_headers(&token);
  assert_eq!(
    crate::auth::dashboard_role(&state, &headers).await,
    Some(Role::Admin)
  );
}

// ---------------------------------------------------------------------------
// the record behind an OIDC login (planned_features.md #154)
// ---------------------------------------------------------------------------

use crate::store::grants::{Grant, GrantOrg};

/// A dashboard user record for `email`, Viewer in master, the shape an admin
/// creates ahead of somebody's first login.
async fn record_for(state: &AppState, email: &str) -> String {
  state
    .users
    .lock()
    .await
    .create_sso(
      email,
      None,
      vec![Grant::new(GrantOrg::Master, Role::Viewer)],
    )
    .unwrap()
    .id
}

/// A runtime whose grant policy is `policy`.
fn oidc_runtime_with(base: &str, policy: crate::oidc::OidcGrantPolicy) -> crate::oidc::OidcRuntime {
  let mut rt = oidc_runtime(base, vec!["*".to_string()]);
  rt.grants = policy;
  rt
}

/// Like `mock_oidc_server`, but the userinfo answer changes per call, so two
/// logins in a row can arrive with different groups.
async fn mock_oidc_sequence(token_body: &str, info_bodies: Vec<String>) -> String {
  use std::sync::atomic::{AtomicUsize, Ordering};
  let token_body = token_body.to_string();
  let info_bodies = Arc::new(info_bodies);
  let calls = Arc::new(AtomicUsize::new(0));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    while let Ok((mut sock, _)) = listener.accept().await {
      let (token_body, info_bodies, calls) =
        (token_body.clone(), info_bodies.clone(), calls.clone());
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
          return;
        }
        let body = if buf.starts_with(b"POST") {
          token_body
        } else {
          let i = calls.fetch_add(1, Ordering::SeqCst);
          info_bodies[i.min(info_bodies.len() - 1)].clone()
        };
        let resp = format!(
          "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
      });
    }
  });
  format!("http://{addr}")
}

async fn grants_of(state: &AppState, email: &str) -> Vec<String> {
  state
    .users
    .lock()
    .await
    .list()
    .iter()
    .find(|u| u.username.eq_ignore_ascii_case(email))
    .map(|u| u.grants.iter().map(|g| g.label()).collect())
    .unwrap_or_default()
}

async fn login(state: &Arc<AppState>, csrf: &str, bound: Option<&str>) -> Response {
  seed_oidc_state(state, csrf, bound.map(str::to_string)).await;
  call_oidc_callback(state.clone(), oidc_query(&[("code", "c"), ("state", csrf)])).await
}

#[tokio::test]
async fn an_email_with_a_record_is_what_the_record_says() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"user@allow.com\"}",
  )
  .await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  record_for(&state, "user@allow.com").await;
  let resp = login(&state, "csrf-rec", None).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  let cookie = resp
    .headers()
    .get("set-cookie")
    .and_then(|v| v.to_str().ok())
    .and_then(|c| c.split(';').next())
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .unwrap();
  let headers = cookie_headers(&cookie);
  // Viewer in master, as the record says, and not the super-admin the login
  // used to mint.
  assert_eq!(dashboard_role(&state, &headers).await, Some(Role::Viewer));
  assert!(!is_master_admin(&state, &headers).await);
}

#[tokio::test]
async fn an_email_with_no_record_and_no_defaults_is_refused_and_recorded() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"new@allow.com\"}",
  )
  .await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime(&base, vec!["*".to_string()]));
  let state = Arc::new(state);
  let resp = login(&state, "csrf-new", None).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  assert_eq!(
    state.sessions.lock().await.len(),
    0,
    "no session was minted"
  );
  // The record exists, with no password and nothing granted, so an admin has
  // a name to grant something to.
  let users = state.users.lock().await;
  let rec = users
    .list()
    .iter()
    .find(|u| u.username == "new@allow.com")
    .expect("a record was created");
  assert!(rec.sso());
  assert!(rec.grants.is_empty());
  assert!(users.verify("new@allow.com", "anything").is_none());
}

#[tokio::test]
async fn default_grants_seed_the_first_login_only() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"new@allow.com\"}",
  )
  .await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime_with(
    &base,
    crate::oidc::OidcGrantPolicy {
      default_grants: vec![Grant::new(GrantOrg::Master, Role::Operator)],
      ..Default::default()
    },
  ));
  let state = Arc::new(state);
  assert_eq!(
    login(&state, "csrf-d1", None).await.status(),
    StatusCode::FOUND
  );
  assert_eq!(
    grants_of(&state, "new@allow.com").await,
    vec!["master:operator"]
  );
  // Narrowed by hand afterwards, the defaults do not come back.
  let id = state
    .users
    .lock()
    .await
    .find_by_username("new@allow.com")
    .unwrap()
    .id
    .clone();
  state
    .users
    .lock()
    .await
    .set_grants(&id, vec![Grant::new(GrantOrg::Master, Role::Viewer)])
    .unwrap();
  assert_eq!(
    login(&state, "csrf-d2", None).await.status(),
    StatusCode::FOUND
  );
  assert_eq!(
    grants_of(&state, "new@allow.com").await,
    vec!["master:viewer"]
  );
}

#[tokio::test]
async fn the_group_map_adds_at_one_login_and_takes_away_at_the_next() {
  let base = mock_oidc_sequence(
    "{\"access_token\":\"AT\"}",
    vec![
      "{\"email\":\"ops@allow.com\",\"groups\":[\"acme-ops\",\"auditors\"]}".to_string(),
      "{\"email\":\"ops@allow.com\",\"groups\":[\"auditors\"]}".to_string(),
    ],
  )
  .await;
  let mut state = test_state();
  let acme = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  state.oidc = Some(oidc_runtime_with(
    &base,
    crate::oidc::OidcGrantPolicy {
      default_grants: Vec::new(),
      groups_claim: String::new(),
      group_grants: vec![
        // By handle, which the login resolves to the id.
        (
          "acme-ops".into(),
          Grant::new(GrantOrg::Child("acme".into()), Role::Admin),
        ),
        ("auditors".into(), Grant::new(GrantOrg::All, Role::Viewer)),
      ],
    },
  ));
  let state = Arc::new(state);
  // A hand-written grant, which the map leaves alone.
  state
    .users
    .lock()
    .await
    .create_sso(
      "ops@allow.com",
      None,
      vec![Grant::new(GrantOrg::Master, Role::Viewer)],
    )
    .unwrap();

  assert_eq!(
    login(&state, "csrf-g1", None).await.status(),
    StatusCode::FOUND
  );
  assert_eq!(
    grants_of(&state, "ops@allow.com").await,
    vec![
      "*:viewer".to_string(),
      "master:viewer".to_string(),
      format!("{acme}:admin")
    ]
  );
  let sources: Vec<Option<String>> = state
    .users
    .lock()
    .await
    .find_by_username("ops@allow.com")
    .unwrap()
    .grants
    .iter()
    .map(|g| g.source.clone())
    .collect();
  assert_eq!(
    sources,
    vec![
      Some("auditors".to_string()),
      None,
      Some("acme-ops".to_string())
    ]
  );

  // Out of the ops group: the Acme grant goes, the rest stays.
  assert_eq!(
    login(&state, "csrf-g2", None).await.status(),
    StatusCode::FOUND
  );
  assert_eq!(
    grants_of(&state, "ops@allow.com").await,
    vec!["*:viewer".to_string(), "master:viewer".to_string()]
  );
}

#[tokio::test]
async fn groups_in_the_id_token_count_when_userinfo_carries_none() {
  use base64::prelude::*;
  let payload = BASE64_URL_SAFE_NO_PAD.encode("{\"sub\":\"1\",\"groups\":[\"aperio-admins\"]}");
  let token_body =
    format!("{{\"access_token\":\"AT\",\"id_token\":\"eyJhbGciOiJub25lIn0.{payload}.c2ln\"}}");
  let base = mock_oidc_server(200, token_body, 200, "{\"email\":\"root@allow.com\"}").await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime_with(
    &base,
    crate::oidc::OidcGrantPolicy {
      group_grants: vec![(
        "aperio-admins".into(),
        Grant::new(GrantOrg::Master, Role::Admin),
      )],
      ..Default::default()
    },
  ));
  let state = Arc::new(state);
  assert_eq!(
    login(&state, "csrf-jwt", None).await.status(),
    StatusCode::FOUND
  );
  assert_eq!(
    grants_of(&state, "root@allow.com").await,
    vec!["master:admin"]
  );
}

#[tokio::test]
async fn a_per_org_login_lives_in_its_organization_and_reaches_nothing_else() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"staff@acme.com\"}",
  )
  .await;
  let state = test_state();
  let acme = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  let beta = state
    .org_store
    .lock()
    .await
    .create("beta", Vec::new(), None)
    .unwrap()
    .id;
  // The org's runtime, as `resolve_org_oidc` would build it: the default
  // role names the organization.
  let rt = oidc_runtime_with(
    &base,
    crate::oidc::OidcGrantPolicy {
      default_grants: vec![Grant::new(GrantOrg::Child(acme.clone()), Role::Operator)],
      ..Default::default()
    },
  );
  state.org_oidc.lock().await.insert(acme.clone(), rt);
  let state = Arc::new(state);

  let resp = login(&state, "csrf-org", Some(&acme)).await;
  assert_eq!(resp.status(), StatusCode::FOUND);
  let rec = state
    .users
    .lock()
    .await
    .find_by_username("staff@acme.com")
    .cloned()
    .unwrap();
  assert_eq!(rec.org_id.as_deref(), Some(acme.as_str()), "lives in Acme");
  assert_eq!(rec.role_in(Some(&acme)), Some(Role::Operator));
  let cookie = resp
    .headers()
    .get("set-cookie")
    .and_then(|v| v.to_str().ok())
    .and_then(|c| c.split(';').next())
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .unwrap();
  let headers = cookie_headers(&cookie);
  assert_eq!(effective_org(&state, &headers).await, Some(acme.clone()));
  assert_eq!(dashboard_role(&state, &headers).await, Some(Role::Operator));

  // A record granted elsewhere only: Acme's provider vouched for the email,
  // and that opens Acme alone, which here is nothing.
  state
    .users
    .lock()
    .await
    .create_sso(
      "beta@acme.com",
      None,
      vec![Grant::new(GrantOrg::Child(beta), Role::Admin)],
    )
    .unwrap();
  let base2 = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"beta@acme.com\"}",
  )
  .await;
  let rt = oidc_runtime_with(&base2, crate::oidc::OidcGrantPolicy::default());
  state.org_oidc.lock().await.insert(acme.clone(), rt);
  let resp = login(&state, "csrf-org2", Some(&acme)).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_disabled_record_is_refused_rather_than_recreated() {
  let base = mock_oidc_server(
    200,
    "{\"access_token\":\"AT\"}",
    200,
    "{\"email\":\"gone@allow.com\"}",
  )
  .await;
  let mut state = test_state();
  state.oidc = Some(oidc_runtime_with(
    &base,
    crate::oidc::OidcGrantPolicy {
      default_grants: vec![Grant::new(GrantOrg::Master, Role::Admin)],
      ..Default::default()
    },
  ));
  let state = Arc::new(state);
  let id = record_for(&state, "gone@allow.com").await;
  state
    .users
    .lock()
    .await
    .update(&id, None, Some(false), None)
    .unwrap();
  assert_eq!(
    login(&state, "csrf-gone", None).await.status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(state.users.lock().await.list().len(), 1, "no second record");
  assert_eq!(state.sessions.lock().await.len(), 0);
}
