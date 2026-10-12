//! A visitor's single sign-on: which `sso` method a route is under, and that the
//! session it mints opens that gate alone, on that hostname, for an email the
//! method still lets in.

use super::*;
use crate::test_support::*;

const IDP: &str = "https://id.example.com";

fn sso_site(hostname: &str, emails: &str) -> String {
  format!(
    "- hostname: {hostname}\n  access:\n    method: sso\n    issuer: {IDP}\n    client_id: visitors\n    client_secret: s3cret\n    allowed_emails: \"{emails}\"\n"
  )
}

fn state_with_sites(yaml: &str) -> Arc<AppState> {
  let mut cfg = test_config();
  cfg.sites = sites(yaml);
  Arc::new(test_state_with(cfg))
}

/// A session as the callback mints it.
async fn sso_session(state: &AppState, host: &str, issuer: &str, email: &str) -> HeaderMap {
  let token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 600,
      created_at: crate::store::sessions::now_secs(),
      ip: None,
      user_agent: None,
      plane: crate::store::sessions::Plane::Visitor,
      scope_host: Some(host.to_string()),
      username: Some(email.to_string()),
      role: crate::store::users::Role::Viewer,
      selected_org: None,
      bound_org: None,
      login_host: Some(host.to_string()),
      sso_issuer: Some(issuer.to_string()),
    },
  );
  let mut headers = HeaderMap::new();
  headers.insert("cookie", format!("aperio_session={token}").parse().unwrap());
  headers.insert("host", host.parse().unwrap());
  headers
}

async fn gate(state: &Arc<AppState>, headers: &HeaderMap, host: &str) -> crate::proxy::VisitorGate {
  let uri: axum::http::Uri = "/".parse().unwrap();
  crate::proxy::check_visitor_gate(
    state,
    &axum::http::Method::GET,
    headers,
    &uri,
    Some(host),
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)),
  )
  .await
}

#[test]
fn a_routes_sign_in_is_its_sites_and_never_a_public_ones() {
  let state = state_with_sites(&format!(
    "{}- hostname: open.example.com\n  access: public\n",
    sso_site("app.example.com", "*@example.com")
  ));
  let found = sso_for(&state, "app.example.com", "/x").expect("the site's");
  assert_eq!(found.issuer, IDP);
  assert!(sso_for(&state, "open.example.com", "/").is_none());
  assert!(
    sso_for(&state, "other.example.com", "/").is_none(),
    "nothing asks there"
  );
}

#[tokio::test]
async fn the_signed_in_visitor_is_let_in_and_named() {
  let state = state_with_sites(&sso_site("app.example.com", "*@example.com"));
  let headers = sso_session(&state, "app.example.com", IDP, "ada@example.com").await;
  match gate(&state, &headers, "app.example.com").await {
    crate::proxy::VisitorGate::Allow(Some(who)) => {
      assert_eq!(who.how, "sso");
      assert_eq!(who.who.as_deref(), Some("ada@example.com"));
    }
    _ => panic!("the signed-in visitor is admitted"),
  }
}

#[tokio::test]
async fn the_session_is_for_its_own_hostname_issuer_and_allowlist_only() {
  let state = state_with_sites(&format!(
    "{}{}",
    sso_site("app.example.com", "*@example.com"),
    sso_site("b.example.com", "*@example.com")
  ));
  // Signed in on another hostname.
  let elsewhere = sso_session(&state, "b.example.com", IDP, "ada@example.com").await;
  assert!(matches!(
    gate(&state, &elsewhere, "app.example.com").await,
    crate::proxy::VisitorGate::Deny(_)
  ));
  // Vouched for by a different provider.
  let other_idp = sso_session(
    &state,
    "app.example.com",
    "https://other.example",
    "ada@example.com",
  )
  .await;
  assert!(matches!(
    gate(&state, &other_idp, "app.example.com").await,
    crate::proxy::VisitorGate::Deny(_)
  ));
  // An email the list no longer lets in.
  let outsider = sso_session(&state, "app.example.com", IDP, "eve@elsewhere.test").await;
  assert!(matches!(
    gate(&state, &outsider, "app.example.com").await,
    crate::proxy::VisitorGate::Deny(_)
  ));
}

#[tokio::test]
async fn a_browser_without_a_session_is_sent_to_the_sign_in() {
  let state = state_with_sites(&sso_site("app.example.com", "*"));
  let mut headers = HeaderMap::new();
  headers.insert("accept", "text/html".parse().unwrap());
  match gate(&state, &headers, "app.example.com").await {
    crate::proxy::VisitorGate::Deny(resp) => {
      let location = resp.headers().get("location").unwrap().to_str().unwrap();
      assert!(
        location.starts_with("/aperio/sso/login?redirect="),
        "{location}"
      );
    }
    _ => panic!("refused, with the way in"),
  }
  // A caller without a browser is told so rather than sent to a page.
  match gate(&state, &HeaderMap::new(), "app.example.com").await {
    crate::proxy::VisitorGate::Deny(resp) => assert_eq!(resp.status(), 401),
    _ => panic!("refused"),
  }
}

#[tokio::test]
async fn a_visitor_password_session_does_not_open_a_single_sign_on_gate() {
  let state = state_with_sites(&sso_site("app.example.com", "*"));
  // A global visitor session, as the server's own password mints.
  let token = uuid::Uuid::new_v4().to_string();
  state.sessions.lock().await.insert(
    &token,
    SessionInfo {
      expires_at: crate::store::sessions::now_secs() + 600,
      created_at: crate::store::sessions::now_secs(),
      ip: None,
      user_agent: None,
      plane: crate::store::sessions::Plane::Visitor,
      scope_host: None,
      username: None,
      role: crate::store::users::Role::Viewer,
      selected_org: None,
      bound_org: None,
      login_host: None,
      sso_issuer: None,
    },
  );
  let mut headers = HeaderMap::new();
  headers.insert("cookie", format!("aperio_session={token}").parse().unwrap());
  assert!(matches!(
    gate(&state, &headers, "app.example.com").await,
    crate::proxy::VisitorGate::Deny(_)
  ));
}
