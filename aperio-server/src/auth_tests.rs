use super::*;
use crate::test_support::*;
use axum::http::HeaderMap;

fn ip(s: &str) -> IpAddr {
  s.parse().unwrap()
}

// --- shared helpers for the handler / session-helper tests ------------------

/// Builds a `Basic` authorization header (and optional Host) from a raw
/// `user:pass` credential string.
fn basic_headers(creds: &str, host: Option<&str>) -> HeaderMap {
  use base64::prelude::*;
  let mut h = HeaderMap::new();
  h.insert(
    "authorization",
    format!("Basic {}", BASE64_STANDARD.encode(creds))
      .parse()
      .unwrap(),
  );
  if let Some(host) = host {
    h.insert("host", host.parse().unwrap());
  }
  h
}

/// Computes the RFC 6238 TOTP code for a base32 secret at a step counter,
/// mirroring the private `totp::code_at` so tests can forge valid codes.
fn totp_code_at(secret_b32: &str, step: i64) -> String {
  use hmac::{Hmac, Mac};
  use sha1::Sha1;
  let secret = crate::totp::base32_decode(secret_b32).unwrap();
  let mut mac = Hmac::<Sha1>::new_from_slice(&secret).unwrap();
  mac.update(&(step as u64).to_be_bytes());
  let d = mac.finalize().into_bytes();
  let off = (d[19] & 0x0f) as usize;
  let bin = (u32::from(d[off]) & 0x7f) << 24
    | u32::from(d[off + 1]) << 16
    | u32::from(d[off + 2]) << 8
    | u32::from(d[off + 3]);
  format!("{:06}", bin % 1_000_000)
}

fn totp_code(secret: &str, now: u64) -> String {
  totp_code_at(secret, (now / 30) as i64)
}

/// A 6-digit code guaranteed not to be valid for the current step or its
/// neighbours (so the wrong-code login path is exercised deterministically).
fn totp_wrong(secret: &str, now: u64) -> String {
  let step = (now / 30) as i64;
  let valid: Vec<String> = (step - 2..=step + 2)
    .map(|s| totp_code_at(secret, s))
    .collect();
  for n in 0..2000u32 {
    let c = format!("{:06}", n);
    if !valid.contains(&c) {
      return c;
    }
  }
  "999999".to_string()
}

/// Inserts a session with full control over its fields and returns the token.
async fn seed_custom(
  state: &AppState,
  expires_at: u64,
  scope_host: Option<String>,
  username: Option<&str>,
  role: Role,
  selected_org: Option<String>,
  bound_org: Option<String>,
) -> String {
  let token = uuid::Uuid::new_v4().to_string();
  let now = crate::store::sessions::now_secs();
  state.sessions.lock().await.insert(
    &token,
    SessionInfo {
      expires_at,
      created_at: now,
      ip: Some("127.0.0.1".to_string()),
      user_agent: None,
      scope_host,
      username: username.map(|s| s.to_string()),
      role,
      selected_org,
      bound_org,
    },
  );
  token
}

// --- session cookie ---------------------------------------------------------

#[test]
fn session_cookie_parses_named_value_among_others() {
  let mut h = HeaderMap::new();
  h.insert(
    "cookie",
    "foo=1; aperio_session=abc-123; bar=2".parse().unwrap(),
  );
  assert_eq!(session_cookie(&h), Some("abc-123"));

  // Only the aperio_session cookie is returned; other cookies are ignored.
  let mut other = HeaderMap::new();
  other.insert("cookie", "foo=1; bar=2".parse().unwrap());
  assert_eq!(session_cookie(&other), None);

  // A leading cookie without spaces is still matched after trimming.
  let mut lead = HeaderMap::new();
  lead.insert("cookie", "aperio_session=xyz".parse().unwrap());
  assert_eq!(session_cookie(&lead), Some("xyz"));

  assert_eq!(session_cookie(&HeaderMap::new()), None);
}

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

// --- ip_allowed / cidr ------------------------------------------------------

#[test]
fn ip_allowed_empty_and_wildcards() {
  assert!(ip_allowed(ip("1.2.3.4"), &[]));
  for w in ["*", "0.0.0.0/0", "::/0", "0.0.0.0"] {
    assert!(ip_allowed(ip("9.9.9.9"), &[w.to_string()]));
  }
}

#[test]
fn ip_allowed_exact_and_cidr() {
  let list = vec!["10.0.0.0/8".to_string(), "192.168.1.5".to_string()];
  assert!(ip_allowed(ip("10.1.2.3"), &list)); // inside /8
  assert!(ip_allowed(ip("192.168.1.5"), &list)); // exact
  assert!(!ip_allowed(ip("192.168.1.6"), &list)); // no match
  assert!(!ip_allowed(ip("11.0.0.1"), &list)); // outside /8
}

#[test]
fn ip_allowed_ipv6_cidr_and_family_mismatch() {
  let list = vec!["2001:db8::/32".to_string()];
  assert!(ip_allowed(ip("2001:db8::1"), &list));
  assert!(!ip_allowed(ip("2001:dead::1"), &list));
  // A v4 address never matches a v6 CIDR.
  assert!(!ip_allowed(ip("10.0.0.1"), &list));
}

#[test]
fn ip_allowed_rejects_malformed_entries() {
  assert!(!ip_allowed(ip("1.2.3.4"), &["not-an-ip".to_string()]));
  assert!(!ip_allowed(ip("1.2.3.4"), &["1.2.3.4/notnum".to_string()]));
  // Prefix out of range → the entry never matches.
  assert!(!ip_allowed(ip("1.2.3.4"), &["1.2.3.4/40".to_string()]));
}

// --- valid_ip_entry ---------------------------------------------------------

#[test]
fn valid_ip_entry_accepts_and_rejects() {
  for good in ["*", "1.2.3.4", "10.0.0.0/8", "2001:db8::/32", "::1"] {
    assert!(valid_ip_entry(good), "{good} should be valid");
  }
  for bad in ["garbage", "1.2.3.4/33", "2001:db8::/129", "1.2.3.4/x", ""] {
    assert!(!valid_ip_entry(bad), "{bad} should be invalid");
  }
}

// --- constant_time_eq_str ---------------------------------------------------

#[test]
fn constant_time_eq_str_semantics() {
  assert!(constant_time_eq_str("hunter2", "hunter2"));
  assert!(!constant_time_eq_str("hunter2", "hunter3"));
  // Length differences are handled (both sides are hashed first).
  assert!(!constant_time_eq_str("short", "a-much-longer-secret"));
  assert!(constant_time_eq_str("", ""));
}

// --- safe_redirect_path -----------------------------------------------------

#[test]
fn safe_redirect_path_blocks_open_redirects() {
  assert_eq!(safe_redirect_path("/dashboard"), "/dashboard");
  assert_eq!(safe_redirect_path("/a/b?c=d"), "/a/b?c=d");
  // Protocol-relative and backslash bypasses collapse to root.
  assert_eq!(safe_redirect_path("//evil.com"), "/");
  assert_eq!(safe_redirect_path("/\\evil.com"), "/");
  assert_eq!(safe_redirect_path("https://evil.com"), "/");
  assert_eq!(safe_redirect_path("relative"), "/");
}

// --- LockoutTracker ----------------------------------------------------------

#[test]
fn lockout_triggers_after_threshold_and_escalates() {
  let mut t = LockoutTracker::new(3, Duration::from_secs(60));
  let ip: IpAddr = "203.0.113.5".parse().unwrap();
  let now = Instant::now();

  // Below the threshold: no lockout.
  assert_eq!(t.record_failure(ip, now), None);
  assert_eq!(t.record_failure(ip, now), None);
  assert!(t.locked(ip, now).is_none());

  // Third failure crosses the threshold: 60s window.
  assert_eq!(t.record_failure(ip, now), Some(Duration::from_secs(60)));
  assert!(t.locked(ip, now).is_some());
  // Still locked just before the window ends; free right after.
  assert!(t.locked(ip, now + Duration::from_secs(59)).is_some());
  assert!(t.locked(ip, now + Duration::from_secs(61)).is_none());

  // A repeat offender escalates: the second lockout doubles to 120s.
  let later = now + Duration::from_secs(120);
  assert_eq!(t.record_failure(ip, later), None);
  assert_eq!(t.record_failure(ip, later), None);
  assert_eq!(t.record_failure(ip, later), Some(Duration::from_secs(120)));
}

#[test]
fn lockout_cleared_on_success_and_isolated_per_ip() {
  let mut t = LockoutTracker::new(2, Duration::from_secs(60));
  let a: IpAddr = "203.0.113.5".parse().unwrap();
  let b: IpAddr = "203.0.113.6".parse().unwrap();
  let now = Instant::now();

  assert_eq!(t.record_failure(a, now), None);
  // A successful login resets the counter (and the escalation history).
  t.clear(a);
  assert_eq!(t.record_failure(a, now), None);
  assert_eq!(t.record_failure(a, now), Some(Duration::from_secs(60)));

  // Another IP is unaffected by A's lockout.
  assert!(t.locked(b, now).is_none());
  assert_eq!(t.record_failure(b, now), None);
}

#[test]
fn lockout_window_is_capped() {
  let mut t = LockoutTracker::new(1, Duration::from_secs(3000));
  let ip: IpAddr = "203.0.113.7".parse().unwrap();
  let mut now = Instant::now();
  // Every failure locks (threshold 1); the second window would be 6000s but
  // is capped at one hour.
  assert_eq!(t.record_failure(ip, now), Some(Duration::from_secs(3000)));
  now += Duration::from_secs(3001);
  assert!(t.locked(ip, now).is_none());
  assert_eq!(t.record_failure(ip, now), Some(Duration::from_secs(3600)));
}

// --- LockoutTracker: gc / set_policy ----------------------------------------

#[test]
fn lockout_gc_drops_stale_and_set_policy() {
  let mut t = LockoutTracker::new(2, Duration::from_secs(60));
  let now = Instant::now();
  // Fill past the gc trigger (1024) with stale entries, then a fresh failure
  // runs gc() which retains only recent ones.
  for i in 0..1100u32 {
    let a = IpAddr::V4(std::net::Ipv4Addr::from(i));
    t.record_failure(a, now);
  }
  // A failure far in the future evicts the now-stale earlier entries.
  let future = now + Duration::from_secs(25 * 3600);
  let fresh: IpAddr = "198.51.100.7".parse().unwrap();
  t.record_failure(fresh, future);
  // Policy can be swapped at runtime; clamps to sane minimums.
  t.set_policy(0, Duration::from_millis(1));
  let ip: IpAddr = "198.51.100.9".parse().unwrap();
  // threshold clamped to >=1, so the first failure locks immediately.
  assert!(t.record_failure(ip, future).is_some());
}

// --- auth_login_handler -----------------------------------------------------

fn login_query(redirect: Option<&str>) -> HashMap<String, String> {
  let mut q = HashMap::new();
  if let Some(r) = redirect {
    q.insert("redirect".to_string(), r.to_string());
  }
  q
}

async fn call_login(
  state: Arc<AppState>,
  headers: HeaderMap,
  query: HashMap<String, String>,
) -> Result<Response, StatusCode> {
  auth_login_handler(
    State(state),
    ConnectInfo(test_peer()),
    axum::extract::Query(query),
    headers,
  )
  .await
}

#[tokio::test]
async fn login_master_token_creates_global_session() {
  let state = Arc::new(test_state());
  // master bearer token is `test` -> Basic aperio:test grants full access.
  let res = call_login(
    state.clone(),
    basic_headers("aperio:test", Some("dash.local")),
    login_query(Some("/aperio/dashboard")),
  )
  .await
  .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  assert!(res.headers().get("set-cookie").is_some());
  // A session was persisted.
  assert_eq!(state.sessions.lock().await.len(), 1);
}

#[tokio::test]
async fn login_dashboard_password_env_grants_access() {
  let state = Arc::new(test_state());
  unsafe {
    std::env::set_var("APERIO_DASHBOARD_AUTH", "dashsecret");
  }
  let res = call_login(
    state.clone(),
    basic_headers("aperio:dashsecret", Some("dash.local")),
    login_query(None),
  )
  .await;
  unsafe {
    std::env::remove_var("APERIO_DASHBOARD_AUTH");
  }
  let res = res.unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  assert!(res.headers().get("set-cookie").is_some());
}

#[tokio::test]
async fn login_named_user_without_totp() {
  let state = test_state();
  let org = state.org_store.lock().await.create("acme").unwrap();
  state
    .users
    .lock()
    .await
    .create("alice", "password1", Role::Operator, Some(org.id.clone()))
    .unwrap();
  let state = Arc::new(state);
  let res = call_login(
    state.clone(),
    basic_headers("alice:password1", Some("dash.local")),
    login_query(None),
  )
  .await
  .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  // The stored session carries the user's identity.
  let (_, info) = state.sessions.lock().await.entries().pop().unwrap();
  assert_eq!(info.username.as_deref(), Some("alice"));
  assert_eq!(info.role, Role::Operator);
}

#[tokio::test]
async fn login_wrong_password_fails() {
  let state = test_state();
  state
    .users
    .lock()
    .await
    .create("alice", "password1", Role::Admin, None)
    .unwrap();
  let state = Arc::new(state);
  let err = call_login(
    state,
    basic_headers("alice:wrongpass", Some("dash.local")),
    login_query(None),
  )
  .await
  .unwrap_err();
  assert_eq!(err, StatusCode::UNAUTHORIZED);
}

async fn totp_user(state: &AppState, username: &str) -> (String, String) {
  let uid = state
    .users
    .lock()
    .await
    .create(username, "password1", Role::Admin, None)
    .unwrap()
    .id;
  let secret = state.users.lock().await.totp_begin(&uid).unwrap();
  let now = crate::store::sessions::now_secs();
  let code = totp_code(&secret, now);
  state
    .users
    .lock()
    .await
    .totp_enable(&uid, &code, now)
    .unwrap();
  (uid, secret)
}

#[tokio::test]
async fn login_totp_required_when_code_missing() {
  let state = test_state();
  totp_user(&state, "totpuser").await;
  let state = Arc::new(state);
  // Right password, no X-Aperio-Totp header -> 401 with the "required" hint,
  // and no lockout-worthy failure recorded.
  let res = call_login(
    state.clone(),
    basic_headers("totpuser:password1", Some("dash.local")),
    login_query(None),
  )
  .await
  .unwrap();
  assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(
    res
      .headers()
      .get("x-aperio-totp")
      .unwrap()
      .to_str()
      .unwrap(),
    "required"
  );
}

#[tokio::test]
async fn login_totp_valid_code_succeeds() {
  let state = test_state();
  let (_uid, secret) = totp_user(&state, "totpuser").await;
  let state = Arc::new(state);
  let now = crate::store::sessions::now_secs();
  let mut headers = basic_headers("totpuser:password1", Some("dash.local"));
  headers.insert("x-aperio-totp", totp_code(&secret, now).parse().unwrap());
  let res = call_login(state.clone(), headers, login_query(None))
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  assert!(res.headers().get("set-cookie").is_some());
}

#[tokio::test]
async fn login_totp_wrong_code_fails() {
  let state = test_state();
  let (_uid, secret) = totp_user(&state, "totpuser").await;
  let state = Arc::new(state);
  let now = crate::store::sessions::now_secs();
  let mut headers = basic_headers("totpuser:password1", Some("dash.local"));
  headers.insert("x-aperio-totp", totp_wrong(&secret, now).parse().unwrap());
  let err = call_login(state, headers, login_query(None))
    .await
    .unwrap_err();
  assert_eq!(err, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_totp_recovery_code_consumed() {
  let state = test_state();
  let uid = state
    .users
    .lock()
    .await
    .create("recov", "password1", Role::Admin, None)
    .unwrap()
    .id;
  let secret = state.users.lock().await.totp_begin(&uid).unwrap();
  let now = crate::store::sessions::now_secs();
  let code = totp_code(&secret, now);
  let recovery = state
    .users
    .lock()
    .await
    .totp_enable(&uid, &code, now)
    .unwrap();
  let state = Arc::new(state);
  // A recovery code (not a 6-digit TOTP) takes the consume_recovery path.
  let mut headers = basic_headers("recov:password1", Some("dash.local"));
  headers.insert("x-aperio-totp", recovery[0].parse().unwrap());
  let res = call_login(state.clone(), headers, login_query(None))
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  // The same recovery code cannot be reused.
  let mut again = basic_headers("recov:password1", Some("dash.local"));
  again.insert("x-aperio-totp", recovery[0].parse().unwrap());
  let err = call_login(state, again, login_query(None))
    .await
    .unwrap_err();
  assert_eq!(err, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_server_visitor_password_global() {
  let mut cfg = test_config();
  cfg.auth_credentials = Some("visitor:pass".to_string());
  let state = Arc::new(test_state_with(cfg));
  // No client override on this route -> the server password unlocks it.
  let res = call_login(
    state.clone(),
    basic_headers("visitor:pass", Some("site.test")),
    login_query(Some("/")),
  )
  .await
  .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  let (_, info) = state.sessions.lock().await.entries().pop().unwrap();
  assert!(info.scope_host.is_none());
}

#[tokio::test]
async fn login_visitor_credentials_host_scoped() {
  let state = test_state();
  // A connected client bound to `site.test` sets a per-service visitor password.
  let mut client = mock_client(Some("site.test"), None, None, None);
  client.visitor_auth = Some("guest:letmein".to_string());
  state.clients.lock().await.insert("c1".to_string(), client);
  let state = Arc::new(state);
  let res = call_login(
    state.clone(),
    basic_headers("guest:letmein", Some("site.test")),
    login_query(Some("/app")),
  )
  .await
  .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  let (_, info) = state.sessions.lock().await.entries().pop().unwrap();
  // The session is scoped to just this host.
  assert_eq!(info.scope_host.as_deref(), Some("site.test"));
}

#[tokio::test]
async fn login_invalid_credentials_and_lockout_audit() {
  let state = test_state();
  // A single failure trips the lockout so the lockout-audit branch runs.
  state
    .login_lockout
    .lock()
    .await
    .set_policy(1, Duration::from_secs(60));
  let state = Arc::new(state);
  let err = call_login(
    state.clone(),
    basic_headers("nobody:nope", Some("dash.local")),
    login_query(None),
  )
  .await
  .unwrap_err();
  assert_eq!(err, StatusCode::UNAUTHORIZED);
  // The next attempt is refused outright (locked out).
  let err = call_login(
    state,
    basic_headers("nobody:nope", Some("dash.local")),
    login_query(None),
  )
  .await
  .unwrap_err();
  assert_eq!(err, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn login_rate_limited() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 0.0;
  cfg.ip_limit_refill = 0.0;
  let state = Arc::new(test_state_with(cfg));
  let err = call_login(state, basic_headers("aperio:test", None), login_query(None))
    .await
    .unwrap_err();
  assert_eq!(err, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn login_no_auth_header_fails() {
  let state = Arc::new(test_state());
  // No Authorization header at all -> straight to the failure path.
  let err = call_login(state, HeaderMap::new(), login_query(None))
    .await
    .unwrap_err();
  assert_eq!(err, StatusCode::UNAUTHORIZED);
}

// --- logout / session-status / page handlers --------------------------------

#[tokio::test]
async fn logout_clears_session_and_cookie() {
  let mut cfg = test_config();
  cfg.secure_cookies = true;
  let state = Arc::new(test_state_with(cfg));
  let token = seed_session(&state, Role::Admin, None, None).await;
  let resp = auth_logout_handler(State(state.clone()), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
  assert!(cookie.contains("Max-Age=0"));
  assert!(cookie.contains("Secure"));
  // The session is gone from the store.
  assert!(state.sessions.lock().await.get(&token).is_none());
}

#[tokio::test]
async fn logout_without_cookie_still_ok() {
  let state = Arc::new(test_state());
  let resp = auth_logout_handler(State(state), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn session_handler_reports_named_user_and_totp() {
  let state = test_state();
  let (_uid, _secret) = totp_user(&state, "sess").await;
  let token = seed_session(&state, Role::Operator, Some("sess"), None).await;
  let state = Arc::new(state);
  let resp = auth_session_handler(State(state), cookie_headers(&token)).await;
  let body = json_body(resp).await;
  assert_eq!(body["username"], "sess");
  assert_eq!(body["role"], "operator");
  assert_eq!(body["totp"], true);
  assert_eq!(body["master_admin"], false);
}

#[tokio::test]
async fn session_handler_defaults_without_cookie() {
  let state = Arc::new(test_state());
  let resp = auth_session_handler(State(state), HeaderMap::new()).await;
  let body = json_body(resp).await;
  assert_eq!(body["username"], "aperio");
  assert_eq!(body["expires_in_seconds"], 0);
}

#[tokio::test]
async fn session_handler_master_admin_selected_org() {
  let state = test_state();
  let token = seed_session(&state, Role::Admin, None, Some("org-9".to_string())).await;
  let state = Arc::new(state);
  let resp = auth_session_handler(State(state), cookie_headers(&token)).await;
  let body = json_body(resp).await;
  assert_eq!(body["master_admin"], true);
  assert_eq!(body["selected_org"], "org-9");
}

#[tokio::test]
async fn session_handler_unknown_token_defaults() {
  let state = Arc::new(test_state());
  // A cookie whose token is not in the store -> zeroed defaults.
  let resp = auth_session_handler(
    State(state),
    cookie_headers("11111111-1111-1111-1111-111111111111"),
  )
  .await;
  let body = json_body(resp).await;
  assert_eq!(body["expires_in_seconds"], 0);
}

#[tokio::test]
async fn auth_page_handler_serves() {
  let resp = auth_page_handler().await;
  // Embedded asset may be present (200) or absent in a bare build; either way
  // the handler returns a response without panicking.
  let _ = resp.status();
}

// --- session helpers: validate / scope --------------------------------------

#[tokio::test]
async fn validate_session_variants() {
  let state = test_state();
  let now = crate::store::sessions::now_secs();
  let global = seed_session(&state, Role::Admin, None, None).await;
  let scoped = seed_custom(
    &state,
    now + 100,
    Some("host.test".to_string()),
    None,
    Role::Admin,
    None,
    None,
  )
  .await;
  let expired = seed_custom(
    &state,
    now.saturating_sub(10),
    None,
    None,
    Role::Admin,
    None,
    None,
  )
  .await;

  assert!(validate_session(&state, &cookie_headers(&global)).await);
  // A host-scoped session is not a full/global session.
  assert!(!validate_session(&state, &cookie_headers(&scoped)).await);
  assert!(!validate_session(&state, &cookie_headers(&expired)).await);
  assert!(!validate_session(&state, &HeaderMap::new()).await);
  // A non-UUID cookie value is rejected without a store lookup.
  assert!(!validate_session(&state, &cookie_headers("not-a-uuid")).await);
}

#[tokio::test]
async fn validate_session_for_host_matches_scope() {
  let state = test_state();
  let now = crate::store::sessions::now_secs();
  let global = seed_session(&state, Role::Admin, None, None).await;
  let scoped = seed_custom(
    &state,
    now + 100,
    Some("host.test".to_string()),
    None,
    Role::Admin,
    None,
    None,
  )
  .await;
  // Global session works for any host.
  assert!(validate_session_for_host(&state, &cookie_headers(&global), Some("anything")).await);
  // Scoped session only for its exact host.
  assert!(validate_session_for_host(&state, &cookie_headers(&scoped), Some("host.test")).await);
  assert!(!validate_session_for_host(&state, &cookie_headers(&scoped), Some("other")).await);
  assert!(!validate_session_for_host(&state, &HeaderMap::new(), Some("host.test")).await);
}

#[tokio::test]
async fn session_scope_gc_prunes_expired() {
  let state = test_state();
  let now = crate::store::sessions::now_secs();
  let expired = seed_custom(
    &state,
    now.saturating_sub(10),
    None,
    None,
    Role::Admin,
    None,
    None,
  )
  .await;
  let live = seed_session(&state, Role::Admin, None, None).await;
  // Force the lazy GC to run on the next validate_session call.
  *state.last_session_gc.lock().await = Instant::now() - Duration::from_secs(400);
  assert!(validate_session(&state, &cookie_headers(&live)).await);
  // The expired session was pruned by the GC sweep.
  assert!(state.sessions.lock().await.get(&expired).is_none());
}

// --- caller_org / is_master_admin / effective_org ---------------------------

#[tokio::test]
async fn caller_org_resolution() {
  let state = test_state();
  // Built-in master admin (no username) -> master (None).
  let master = seed_session(&state, Role::Admin, None, None).await;
  assert_eq!(caller_org(&state, &cookie_headers(&master)).await, None);

  // Named user -> their own org.
  let org = state.org_store.lock().await.create("acme").unwrap();
  state
    .users
    .lock()
    .await
    .create("bob", "password1", Role::Operator, Some(org.id.clone()))
    .unwrap();
  let named = seed_session(&state, Role::Operator, Some("bob"), None).await;
  assert_eq!(
    caller_org(&state, &cookie_headers(&named)).await,
    Some(org.id.clone())
  );

  // A bound-org (per-org OIDC) session is pinned to its org.
  let now = crate::store::sessions::now_secs();
  let bound = seed_custom(
    &state,
    now + 100,
    None,
    Some("someone@org"),
    Role::Admin,
    None,
    Some("bound-1".to_string()),
  )
  .await;
  assert_eq!(
    caller_org(&state, &cookie_headers(&bound)).await,
    Some("bound-1".to_string())
  );
}

#[tokio::test]
async fn caller_org_from_admin_key() {
  let state = test_state();
  let (_key, secret) = state.admin_key_store.lock().await.create(
    "k".to_string(),
    Role::Admin,
    Some("keyorg".to_string()),
    None,
  );
  let mut h = HeaderMap::new();
  h.insert("authorization", format!("Bearer {secret}").parse().unwrap());
  assert_eq!(caller_org(&state, &h).await, Some("keyorg".to_string()));
}

#[tokio::test]
async fn is_master_admin_cases() {
  let state = test_state();
  let master = seed_session(&state, Role::Admin, None, None).await;
  assert!(is_master_admin(&state, &cookie_headers(&master)).await);

  // Non-admin role is never master.
  let viewer = seed_session(&state, Role::Viewer, None, None).await;
  assert!(!is_master_admin(&state, &cookie_headers(&viewer)).await);

  // Admin but pinned to a child org is not the master super-admin.
  let org = state.org_store.lock().await.create("acme").unwrap();
  state
    .users
    .lock()
    .await
    .create("cara", "password1", Role::Admin, Some(org.id.clone()))
    .unwrap();
  let child_admin = seed_session(&state, Role::Admin, Some("cara"), None).await;
  assert!(!is_master_admin(&state, &cookie_headers(&child_admin)).await);
}

#[tokio::test]
async fn effective_org_selection() {
  let state = test_state();
  // Master admin with a selected org sees that org.
  let sel = seed_session(&state, Role::Admin, None, Some("org-x".to_string())).await;
  assert_eq!(
    effective_org(&state, &cookie_headers(&sel)).await,
    Some("org-x".to_string())
  );
  // Master admin without a selection defaults to master (None).
  let master = seed_session(&state, Role::Admin, None, None).await;
  assert_eq!(effective_org(&state, &cookie_headers(&master)).await, None);

  // Named user is pinned to their org regardless of any selection.
  let org = state.org_store.lock().await.create("acme").unwrap();
  state
    .users
    .lock()
    .await
    .create("dan", "password1", Role::Operator, Some(org.id.clone()))
    .unwrap();
  let named = seed_session(
    &state,
    Role::Operator,
    Some("dan"),
    Some("ignored".to_string()),
  )
  .await;
  assert_eq!(
    effective_org(&state, &cookie_headers(&named)).await,
    Some(org.id)
  );
}

// --- dashboard_role / dashboard_username / require_master_admin --------------

#[tokio::test]
async fn dashboard_role_and_username() {
  let state = test_state();
  let now = crate::store::sessions::now_secs();

  let global = seed_session(&state, Role::Operator, Some("erin"), None).await;
  assert_eq!(
    dashboard_role(&state, &cookie_headers(&global)).await,
    Some(Role::Operator)
  );
  assert_eq!(
    dashboard_username(&state, &cookie_headers(&global)).await,
    Some("erin".to_string())
  );

  // Host-scoped session: no dashboard role/username (falls through to keys).
  let scoped = seed_custom(
    &state,
    now + 100,
    Some("h.test".to_string()),
    Some("erin"),
    Role::Operator,
    None,
    None,
  )
  .await;
  assert_eq!(dashboard_role(&state, &cookie_headers(&scoped)).await, None);
  assert_eq!(
    dashboard_username(&state, &cookie_headers(&scoped)).await,
    None
  );

  // Expired session: none.
  let expired = seed_custom(
    &state,
    now.saturating_sub(5),
    None,
    Some("erin"),
    Role::Operator,
    None,
    None,
  )
  .await;
  assert_eq!(
    dashboard_role(&state, &cookie_headers(&expired)).await,
    None
  );
  assert_eq!(
    dashboard_username(&state, &cookie_headers(&expired)).await,
    None
  );

  // Built-in admin session (no username) has no dashboard username.
  let master = seed_session(&state, Role::Admin, None, None).await;
  assert_eq!(
    dashboard_username(&state, &cookie_headers(&master)).await,
    None
  );
}

#[tokio::test]
async fn dashboard_role_from_admin_key() {
  let state = test_state();
  let (_key, secret) =
    state
      .admin_key_store
      .lock()
      .await
      .create("k".to_string(), Role::Viewer, None, None);
  let mut h = HeaderMap::new();
  h.insert("authorization", format!("Bearer {secret}").parse().unwrap());
  assert_eq!(dashboard_role(&state, &h).await, Some(Role::Viewer));
  // admin_key_identity surfaces the key name/role/org.
  let id = admin_key_identity(&state, &h).await.unwrap();
  assert_eq!(id.0, Role::Viewer);
  assert!(
    admin_key_identity(&state, &HeaderMap::new())
      .await
      .is_none()
  );
}

#[tokio::test]
async fn require_master_admin_gate() {
  let state = test_state();
  // No session -> 401.
  let err = require_master_admin(&state, &HeaderMap::new())
    .await
    .unwrap_err();
  assert_eq!(err.status(), StatusCode::UNAUTHORIZED);

  // Non-master admin -> 403.
  let org = state.org_store.lock().await.create("acme").unwrap();
  state
    .users
    .lock()
    .await
    .create("fred", "password1", Role::Admin, Some(org.id))
    .unwrap();
  let child = seed_session(&state, Role::Admin, Some("fred"), None).await;
  let err = require_master_admin(&state, &cookie_headers(&child))
    .await
    .unwrap_err();
  assert_eq!(err.status(), StatusCode::FORBIDDEN);

  // Master admin -> Ok.
  let master = seed_session(&state, Role::Admin, None, None).await;
  assert!(
    require_master_admin(&state, &cookie_headers(&master))
      .await
      .is_ok()
  );
}

#[tokio::test]
async fn session_token_reads_cookie() {
  let mut h = HeaderMap::new();
  h.insert("cookie", "aperio_session=tok-123".parse().unwrap());
  assert_eq!(session_token(&h), Some("tok-123".to_string()));
  assert_eq!(session_token(&HeaderMap::new()), None);
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
  let (_t, secret) = state.token_store.lock().await.create(
    "svc".to_string(),
    vec!["site.test".to_string()],
    Vec::new(),
    vec!["10.0.0.0/8".to_string()],
    None,
    None,
    None,
    false,
    false,
    Some("org-7".to_string()),
  );
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
  let (_t, secret) = state.token_store.lock().await.create(
    "decoy".to_string(),
    Vec::new(),
    Vec::new(),
    Vec::new(),
    None,
    None,
    None,
    false,
    true, // canary
    None,
  );
  let mut h = HeaderMap::new();
  h.insert("authorization", format!("Bearer {secret}").parse().unwrap());
  // Using a canary token authenticates but trips the breach alert path.
  assert!(
    authorize_tunnel_token(&state, &h, ip("203.0.113.1"))
      .await
      .is_some()
  );
}

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
  }
}

/// A throwaway HTTP server: POST requests (the token exchange) get
/// `token`, everything else (userinfo) gets `info`. Returns its base URL.
async fn mock_oidc_server(
  token_status: u16,
  token_body: &'static str,
  info_status: u16,
  info_body: &'static str,
) -> String {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    while let Ok((mut sock, _)) = listener.accept().await {
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
          return;
        }
        let is_token = buf.starts_with(b"POST");
        let (status, body) = if is_token {
          (token_status, token_body)
        } else {
          (info_status, info_body)
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
  let (_, bound, _) = states.values().next().unwrap();
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
  let org = state.org_store.lock().await.create("acme").unwrap();
  assert!(resolve_org_oidc(&state, &org.id).await.is_none());
}
