//! Every way in, and mostly every way not in: the session cookie and the prefix
//! that keeps a neighbour from displacing it, token extraction and its
//! constant-time comparison, IP allowlists, TOTP, and the lockout that makes
//! guessing expensive.

use super::*;
use crate::test_support::*;
use axum::http::HeaderMap;

pub(super) fn ip(s: &str) -> IpAddr {
  s.parse().unwrap()
}

// --- shared helpers for the handler / session-helper tests ------------------

/// Builds a `Basic` authorization header (and optional Host) from a raw
/// `user:pass` credential string.
pub(super) fn basic_headers(creds: &str, host: Option<&str>) -> HeaderMap {
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
pub(super) fn totp_code_at(secret_b32: &str, step: i64) -> String {
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

pub(super) fn totp_code(secret: &str, now: u64) -> String {
  totp_code_at(secret, (now / 30) as i64)
}

/// A 6-digit code guaranteed not to be valid for the current step or its
/// neighbours (so the wrong-code login path is exercised deterministically).
pub(super) fn totp_wrong(secret: &str, now: u64) -> String {
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
pub(super) async fn seed_custom(
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
      plane: crate::store::sessions::Plane::Admin,
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

// --- safe_redirect_path -----------------------------------------------------

#[test]
pub(super) fn safe_redirect_path_blocks_open_redirects() {
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
pub(super) fn lockout_triggers_after_threshold_and_escalates() {
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

/// The instant the window ends, the lockout is served rather than still on.
///
/// The test above steps around this on purpose, checking 59 seconds and 61.
/// A mutation run found what the step hid: `until > now` becoming
/// `until >= now` survived every test in the suite. It is one instant, and
/// with `Instant::now()` at all three call sites it is not reachable in
/// practice, so it is close to an equivalent mutant and was nearly left alone.
///
/// It is pinned because of what the caller does with the difference, not
/// because of the mutant. `locked()` returning `Some(ZERO)` is a lockout with
/// nothing left to serve, and `auth_login_handler` turns any `Some` into a
/// 429 refusing the login, logging "locked out for 0s more". Refusing a login
/// and telling somebody to wait zero seconds is a wrong answer however narrow
/// the window is, and `locked()` takes `now` as an argument, so stating the
/// rule at its edge costs one assertion.
#[test]
pub(super) fn a_lockout_is_served_at_the_instant_it_expires() {
  let mut t = LockoutTracker::new(3, Duration::from_secs(60));
  let ip: IpAddr = "203.0.113.9".parse().unwrap();
  let now = Instant::now();
  for _ in 0..3 {
    t.record_failure(ip, now);
  }
  let ends = now + Duration::from_secs(60);
  assert!(t.locked(ip, ends - Duration::from_nanos(1)).is_some());
  assert!(
    t.locked(ip, ends).is_none(),
    "at the deadline the window has been served; a `Some` here is a 429 \
     telling the visitor to wait no time at all"
  );
  // Served means the counter went back to zero, not merely that this one call
  // said no: the next failure starts a fresh count rather than re-locking.
  assert_eq!(t.record_failure(ip, ends), None);
}

#[test]
pub(super) fn lockout_cleared_on_success_and_isolated_per_ip() {
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
pub(super) fn lockout_window_is_capped() {
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

/// The failure map is bounded, which is the only thing standing between a
/// login endpoint and one entry per source address.
///
/// This test used to run `gc` and assert nothing about it: it filled the map,
/// recorded a failure far enough in the future to trigger a sweep, and then
/// checked `set_policy`. A mutation run found the consequence, nine survivors
/// in nine lines, including replacing the whole of `gc` with `()`. Every bound
/// it enforces was unheld: whether it runs at all, when it starts running, and
/// how old is old.
///
/// It matters more than a leak: the map is keyed on the caller's IP and grown
/// by anyone who can reach the login form, so unbounded is a memory-exhaustion
/// path that needs no credentials and no valid account.
#[test]
pub(super) fn lockout_gc_bounds_the_failure_map() {
  let now = Instant::now();
  let addr = |i: u32| IpAddr::V4(std::net::Ipv4Addr::from(i));

  // Below the trigger, nothing is swept however old it gets. Cheapness is the
  // reason: `gc` runs inline on every recorded failure.
  let mut small = LockoutTracker::new(2, Duration::from_secs(60));
  for i in 0..100u32 {
    small.record_failure(addr(i), now);
  }
  small.record_failure(addr(9999), now + Duration::from_secs(48 * 3600));
  assert_eq!(
    small.map.len(),
    101,
    "under the trigger the map is left alone, ancient entries included"
  );

  let mut t = LockoutTracker::new(2, Duration::from_secs(60));
  for i in 0..1100u32 {
    t.record_failure(addr(i), now);
  }
  assert_eq!(t.map.len(), 1100, "nothing is swept while every entry is fresh");

  // Two hours on, past the trigger: still fresh, so still all there. This is
  // what says the window is a day rather than an hour, and what a `24 * 3600`
  // that became `24 + 3600` would break.
  t.record_failure(addr(2001), now + Duration::from_secs(2 * 3600));
  assert_eq!(
    t.map.len(),
    1101,
    "a two-hour-old entry is not stale; the window is twenty-four hours"
  );

  // A day and an hour on, the sweep runs and keeps only what is recent.
  let future = now + Duration::from_secs(25 * 3600);
  t.record_failure(addr(3001), future);
  assert!(
    t.map.len() < 10,
    "the sweep must drop the day-old entries, {} left",
    t.map.len()
  );
  assert!(
    t.map.contains_key(&addr(3001)),
    "the failure that triggered the sweep is not itself stale"
  );
}

#[test]
pub(super) fn lockout_policy_can_be_swapped_at_runtime() {
  let mut t = LockoutTracker::new(2, Duration::from_secs(60));
  let now = Instant::now();
  // Clamps to sane minimums: a threshold of 0 would lock nobody out or
  // everybody, depending on which way the comparison fell.
  t.set_policy(0, Duration::from_millis(1));
  let ip: IpAddr = "198.51.100.9".parse().unwrap();
  assert!(
    t.record_failure(ip, now).is_some(),
    "threshold clamped to at least one, so the first failure locks"
  );
}

// --- auth_login_handler -----------------------------------------------------

pub(super) fn login_query(redirect: Option<&str>) -> HashMap<String, String> {
  let mut q = HashMap::new();
  if let Some(r) = redirect {
    q.insert("redirect".to_string(), r.to_string());
  }
  q
}

pub(super) async fn call_login(
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
pub(super) async fn login_master_token_creates_global_session() {
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
pub(super) async fn login_rejects_the_removed_dashboard_password() {
  // APERIO_DASHBOARD_AUTH was a second dashboard credential; it is gone, and
  // the server refuses to start while it is set. Should that guard ever be
  // bypassed, the value must not authenticate anything on its own.
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
  assert_eq!(res.unwrap_err(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
pub(super) async fn login_named_user_without_totp() {
  let state = test_state();
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap();
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
pub(super) async fn login_wrong_password_fails() {
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

pub(super) async fn totp_user(state: &AppState, username: &str) -> (String, String) {
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
pub(super) async fn login_totp_required_when_code_missing() {
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
pub(super) async fn login_totp_valid_code_succeeds() {
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
pub(super) async fn login_totp_wrong_code_fails() {
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
pub(super) async fn login_totp_recovery_code_consumed() {
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
pub(super) async fn login_server_visitor_password_global() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("visitor:pass");
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
pub(super) async fn login_visitor_credentials_host_scoped() {
  let state = test_state();
  // A connected client bound to `site.test` sets a per-service visitor password.
  let mut client = mock_client(Some("site.test"), None, None, None);
  client.sole_mut().visitor_auth = Some("guest:letmein".to_string());
  state.clients.write().await.insert("c1".to_string(), client);
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
pub(super) async fn login_admits_any_user_a_clients_policy_names() {
  // A `basic` method naming several users has no scalar spelling, so the
  // per-route lookup that reads the scalar found nothing and every credential
  // the policy listed was refused at this form, on a route the gate had sent
  // the visitor to. The gate was unopenable by anyone it was written for.
  let state = test_state();
  let mut client = mock_client(Some("site.test"), None, None, None);
  client.sole_mut().visitor_auth_policy = Some(crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: basic, users: [\"alice:one\", \"bob:two\"]}").unwrap(),
  ));
  state.clients.write().await.insert("c1".to_string(), client);
  let state = Arc::new(state);

  for creds in ["alice:one", "bob:two"] {
    let res = call_login(
      state.clone(),
      basic_headers(creds, Some("site.test")),
      login_query(Some("/app")),
    )
    .await
    .unwrap_or_else(|_| panic!("{creds} is one of the users the policy names"));
    assert_eq!(res.status(), StatusCode::OK);
    let (_, info) = state.sessions.lock().await.entries().pop().unwrap();
    assert_eq!(
      info.scope_host.as_deref(),
      Some("site.test"),
      "a client's own gate scopes its session to that client's host"
    );
  }

  // And nobody else, which is the half that would have kept passing.
  assert!(
    call_login(
      state.clone(),
      basic_headers("alice:wrong", Some("site.test")),
      login_query(Some("/app")),
    )
    .await
    .is_err()
  );
}

#[tokio::test]
pub(super) async fn login_invalid_credentials_and_lockout_audit() {
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
pub(super) async fn login_rate_limited() {
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
pub(super) async fn login_no_auth_header_fails() {
  let state = Arc::new(test_state());
  // No Authorization header at all -> straight to the failure path.
  let err = call_login(state, HeaderMap::new(), login_query(None))
    .await
    .unwrap_err();
  assert_eq!(err, StatusCode::UNAUTHORIZED);
}
