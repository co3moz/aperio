//! The one resolution every scope question is built on: which credential a
//! request carries, what it holds per organization, and which organization
//! it is acting in. The rule pinned hardest is that a named Admin in master
//! is no longer the super-admin unless a grant says so, and that the role is
//! read from the store on every request.

use super::*;
use crate::store::grants::Grant;
use crate::test_support::*;

fn child(id: &str, role: Role) -> Grant {
  Grant::new(GrantOrg::Child(id.to_string()), role)
}

async fn make_user(state: &AppState, name: &str, home: Option<&str>, grants: Vec<Grant>) {
  state
    .users
    .lock()
    .await
    .create_with_grants(name, "long-password", home.map(str::to_string), grants)
    .unwrap();
}

#[tokio::test]
async fn the_built_in_account_holds_star_admin() {
  let state = test_state();
  let headers = admin_headers(&state).await;
  let caller = resolve_caller(&state, &headers).await.unwrap();
  assert!(matches!(caller.identity, Identity::BuiltIn { .. }));
  assert_eq!(caller.role_in(None), Some(Role::Admin));
  assert_eq!(caller.role_in(Some("anything")), Some(Role::Admin));
  assert!(caller.is_master_admin());
  assert!(caller.holds_all_admin());
  assert_eq!(caller.effective_org(), None);
  assert_eq!(caller.actor(), "aperio");
}

#[tokio::test]
async fn a_named_user_is_what_its_grants_say_not_its_session_role() {
  let state = test_state();
  make_user(
    &state,
    "carol",
    None,
    vec![child("acme", Role::Admin), child("beta", Role::Viewer)],
  )
  .await;
  // The session recorded Admin at login; the grants reach no master role.
  let token = seed_session(&state, Role::Admin, Some("carol"), None).await;
  let headers = cookie_headers(&token);
  let caller = resolve_caller(&state, &headers).await.unwrap();
  assert!(!caller.is_master_admin(), "Admin in master takes a grant");
  assert!(!caller.holds_all_admin());
  assert_eq!(caller.role_in(None), None);
  assert_eq!(caller.role_in(Some("acme")), Some(Role::Admin));
  assert_eq!(caller.role_in(Some("beta")), Some(Role::Viewer));
  assert_eq!(caller.role_in(Some("gamma")), None);
  // Nothing selected and no master grant: the first child by id.
  assert_eq!(caller.effective_org(), Some("acme".to_string()));
  assert_eq!(caller.actor(), "carol");
}

#[tokio::test]
async fn the_selection_is_honoured_only_where_a_grant_reaches() {
  let state = test_state();
  make_user(
    &state,
    "carol",
    None,
    vec![child("acme", Role::Admin), child("beta", Role::Viewer)],
  )
  .await;
  let token = seed_session(&state, Role::Admin, Some("carol"), Some("beta".to_string())).await;
  let caller = resolve_caller(&state, &cookie_headers(&token))
    .await
    .unwrap();
  assert_eq!(caller.effective_org(), Some("beta".to_string()));
  assert!(caller.may_select(Some("acme")));
  assert!(!caller.may_select(None), "no master grant");
  assert!(!caller.may_select(Some("gamma")));

  // A selection nothing reaches falls back to the default rather than
  // showing an organization the user cannot act in.
  let token = seed_session(
    &state,
    Role::Admin,
    Some("carol"),
    Some("gamma".to_string()),
  )
  .await;
  let caller = resolve_caller(&state, &cookie_headers(&token))
    .await
    .unwrap();
  assert_eq!(caller.effective_org(), Some("acme".to_string()));
}

#[tokio::test]
async fn a_master_grant_makes_master_the_default_and_star_reaches_everything() {
  let state = test_state();
  make_user(
    &state,
    "dave",
    None,
    vec![Grant::new(GrantOrg::Master, Role::Admin)],
  )
  .await;
  let token = seed_session(&state, Role::Viewer, Some("dave"), None).await;
  let caller = resolve_caller(&state, &cookie_headers(&token))
    .await
    .unwrap();
  assert!(caller.is_master_admin());
  assert!(!caller.holds_all_admin());
  assert_eq!(caller.effective_org(), None);
  assert_eq!(
    caller.role_in(Some("acme")),
    None,
    "master Admin is not every org"
  );

  make_user(
    &state,
    "erin",
    None,
    vec![
      Grant::new(GrantOrg::All, Role::Viewer),
      child("acme", Role::Operator),
    ],
  )
  .await;
  let token = seed_session(&state, Role::Viewer, Some("erin"), None).await;
  let caller = resolve_caller(&state, &cookie_headers(&token))
    .await
    .unwrap();
  assert_eq!(caller.role_in(None), Some(Role::Viewer));
  assert_eq!(caller.role_in(Some("acme")), Some(Role::Operator));
  assert_eq!(caller.role_in(Some("beta")), Some(Role::Viewer));
  assert!(!caller.is_master_admin());
  assert!(caller.may_select(Some("whatever")));
}

#[tokio::test]
async fn a_grant_change_is_read_on_the_next_request() {
  let state = test_state();
  make_user(&state, "fay", None, vec![child("acme", Role::Admin)]).await;
  let token = seed_session(&state, Role::Admin, Some("fay"), None).await;
  let headers = cookie_headers(&token);
  assert_eq!(
    resolve_caller(&state, &headers)
      .await
      .unwrap()
      .role_in(Some("acme")),
    Some(Role::Admin)
  );
  let id = state
    .users
    .lock()
    .await
    .find_by_username("fay")
    .unwrap()
    .id
    .clone();
  state
    .users
    .lock()
    .await
    .set_grants(&id, vec![child("acme", Role::Viewer)])
    .unwrap();
  assert_eq!(
    resolve_caller(&state, &headers)
      .await
      .unwrap()
      .role_in(Some("acme")),
    Some(Role::Viewer),
    "the session did not cache the old role"
  );
}

#[tokio::test]
async fn a_disabled_user_resolves_to_nothing_but_a_key_beside_it_still_does() {
  let state = test_state();
  make_user(&state, "gus", None, vec![child("acme", Role::Admin)]).await;
  let id = state
    .users
    .lock()
    .await
    .find_by_username("gus")
    .unwrap()
    .id
    .clone();
  state
    .users
    .lock()
    .await
    .update(&id, None, Some(false), None)
    .unwrap();
  let token = seed_session(&state, Role::Admin, Some("gus"), None).await;
  let mut headers = cookie_headers(&token);
  assert!(resolve_caller(&state, &headers).await.is_none());

  let (_, secret) = state
    .admin_key_store
    .lock()
    .await
    .create(
      "ci".into(),
      Role::Operator,
      GrantOrg::Child("acme".into()),
      None,
    )
    .unwrap();
  headers.insert(
    "authorization",
    axum::http::HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
  );
  let caller = resolve_caller(&state, &headers).await.unwrap();
  assert!(matches!(caller.identity, Identity::Key { .. }));
  assert_eq!(caller.effective_org(), Some("acme".to_string()));
  assert_eq!(caller.role_in(Some("acme")), Some(Role::Operator));
  assert_eq!(caller.role_in(None), None);
  assert!(
    !caller.may_select(Some("acme")),
    "a key has no session to switch"
  );
  assert_eq!(caller.actor(), "key:ci");
}

#[tokio::test]
async fn a_star_key_acts_in_master_and_a_bound_oidc_login_in_its_org() {
  let state = test_state();
  let (_, secret) = state
    .admin_key_store
    .lock()
    .await
    .create("root".into(), Role::Admin, GrantOrg::All, None)
    .unwrap();
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    axum::http::HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
  );
  let caller = resolve_caller(&state, &headers).await.unwrap();
  assert!(caller.is_master_admin());
  assert!(caller.holds_all_admin());
  assert_eq!(caller.effective_org(), None);

  // A per-organization OIDC session: Admin in its organization and nothing
  // else, whatever it selects.
  let token = seed_custom(
    &state,
    crate::store::sessions::now_secs() + 100,
    None,
    Some("sso@acme.example"),
    Role::Admin,
    Some("beta".to_string()),
    Some("acme".to_string()),
  )
  .await;
  let caller = resolve_caller(&state, &cookie_headers(&token))
    .await
    .unwrap();
  assert!(matches!(caller.identity, Identity::Oidc { .. }));
  assert_eq!(caller.effective_org(), Some("acme".to_string()));
  assert_eq!(caller.role_in(Some("acme")), Some(Role::Admin));
  assert!(!caller.is_master_admin());
  assert!(!caller.may_select(Some("beta")));

  // A global OIDC session with no row is master's Admin, as it always was
  // (`planned_features.md` #154 gives it a record).
  let token = seed_session(&state, Role::Admin, Some("sso@example.com"), None).await;
  let caller = resolve_caller(&state, &cookie_headers(&token))
    .await
    .unwrap();
  assert!(caller.holds_all_admin());
  assert_eq!(caller.actor(), "sso@example.com");
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
    crate::state::SessionInfo {
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
      login_host: None,
    },
  );
  token
}
