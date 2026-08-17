//! Who a session is and what it may act on: the scope it carries, the
//! organization it resolves to, the dashboard role behind it, and the
//! master-admin gate.

use super::super::tests::*;
use super::*;
use crate::test_support::*;

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
  // An unfenced (master) global session works for any host.
  assert!(validate_session_for_host(&state, &cookie_headers(&global), Some("anything")).await);
  // Scoped session only for its exact host.
  assert!(validate_session_for_host(&state, &cookie_headers(&scoped), Some("host.test")).await);
  assert!(!validate_session_for_host(&state, &cookie_headers(&scoped), Some("other")).await);
  assert!(!validate_session_for_host(&state, &HeaderMap::new(), Some("host.test")).await);
}

#[tokio::test]
async fn a_clients_own_gate_is_fenced_by_organization_too() {
  // The gate a client declares for itself is checked before the server's own,
  // and it asked only "is this a global session", which the fix for the
  // server's gate had already established is the wrong question here: a
  // session fixed to one organization reaches every hostname on the server.
  // So a Viewer of org A could open org B's site whenever B's gate was B's
  // own, which is the ordinary way a client gates itself.
  let state = test_state();
  let org = state
    .org_store
    .lock()
    .await
    .create("orga", vec!["a.example.com".to_string()], None)
    .expect("an organization");
  let now = crate::store::sessions::now_secs();
  let fenced = seed_custom(
    &state,
    now + 100,
    None,
    None,
    Role::Viewer,
    None,
    Some(org.id.clone()),
  )
  .await;
  let headers = cookie_headers(&fenced);

  // Real and global, or the refusal below would pass for the wrong reason.
  assert!(validate_session(&state, &headers).await);
  assert!(
    validate_session_for_host(&state, &headers, Some("a.example.com")).await,
    "its own organization's hostname is still reachable"
  );
  assert!(
    !validate_session_for_host(&state, &headers, Some("b.example.com")).await,
    "another organization's hostname is not"
  );
  assert!(
    !validate_session_for_host(&state, &headers, None).await,
    "nor is a request that names no hostname to fence against"
  );
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
  // An expired session grants nothing even before any sweep runs...
  assert!(validate_session(&state, &cookie_headers(&live)).await);
  assert!(!validate_session(&state, &cookie_headers(&expired)).await);
  // ...and the background gc beat is what removes the row itself.
  state.gc_tick_once(Instant::now()).await;
  assert!(state.sessions.lock().await.get(&expired).is_none());
  assert!(state.sessions.lock().await.get(&live).is_some());
}

// --- caller_org / is_master_admin / effective_org ---------------------------

#[tokio::test]
async fn caller_org_resolution() {
  let state = test_state();
  // Built-in master admin (no username) -> master (None).
  let master = seed_session(&state, Role::Admin, None, None).await;
  assert_eq!(caller_org(&state, &cookie_headers(&master)).await, None);

  // Named user -> their own org.
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
  let (_key, secret) = state
    .admin_key_store
    .lock()
    .await
    .create(
      "k".to_string(),
      Role::Admin,
      Some("keyorg".to_string()),
      None,
    )
    .expect("the test store can be written to");
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
    .create("cara", "password1", Role::Admin, Some(org.id.clone()))
    .unwrap();
  let child_admin = seed_session(&state, Role::Admin, Some("cara"), None).await;
  assert!(!is_master_admin(&state, &cookie_headers(&child_admin)).await);
}

#[tokio::test]
async fn disabled_user_session_grants_nothing() {
  let state = test_state();
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap();
  let user = state
    .users
    .lock()
    .await
    .create("cara", "password1", Role::Admin, Some(org.id.clone()))
    .unwrap();
  let token = seed_session(&state, Role::Admin, Some("cara"), None).await;
  let headers = cookie_headers(&token);
  assert!(!is_master_admin(&state, &headers).await);

  // Disabling the account must revoke its live session outright. Before this
  // guard the user's org lookup started failing, which read as "master org"
  // and promoted the disabled sub-org admin to super-admin.
  state
    .users
    .lock()
    .await
    .update(&user.id, None, Some(false), None)
    .unwrap();
  assert_eq!(dashboard_role(&state, &headers).await, None);
  assert_eq!(dashboard_username(&state, &headers).await, None);
  assert!(!validate_session(&state, &headers).await);
  assert!(!is_master_admin(&state, &headers).await);
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
  let (_key, secret) = state
    .admin_key_store
    .lock()
    .await
    .create("k".to_string(), Role::Viewer, None, None)
    .expect("the test store can be written to");
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
