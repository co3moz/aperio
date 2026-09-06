//! A panel hostname: which requests it rewrites, whose it is, and who its
//! login admits.

use super::*;
use crate::store::grants::{Grant, GrantOrg};
use crate::store::users::Role;
use crate::test_support::*;

#[test]
fn the_request_host_is_the_name_alone() {
  let mut h = HeaderMap::new();
  h.insert("host", "Panel.Example.com:8443".parse().unwrap());
  assert_eq!(request_host(&h).as_deref(), Some("panel.example.com"));
  h.insert("host", "panel.example.com.".parse().unwrap());
  assert_eq!(request_host(&h).as_deref(), Some("panel.example.com"));
  h.insert("host", "[::1]:8080".parse().unwrap());
  assert_eq!(request_host(&h).as_deref(), Some("[::1]"));
  h.insert("host", "   ".parse().unwrap());
  assert_eq!(request_host(&h), None);
  assert_eq!(request_host(&HeaderMap::new()), None);
}

#[test]
fn the_root_is_the_dashboard_and_the_rest_moves_under_aperio() {
  assert_eq!(rewritten_path("/"), "/aperio");
  assert_eq!(rewritten_path(""), "/aperio");
  assert_eq!(rewritten_path("/api/session"), "/aperio/api/session");
  assert!(under_aperio("/aperio"));
  assert!(under_aperio("/aperio/api/x"));
  assert!(!under_aperio("/aperiox"));
  assert!(!under_aperio("/"));
}

/// An organization with a fence and a panel inside it, and the cache told.
async fn acme_with_panel(state: &AppState) -> String {
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["*.acme.test".to_string()], None)
    .unwrap();
  state
    .org_store
    .lock()
    .await
    .set_panel_hostname(&org.id, Some("panel.acme.test".to_string()))
    .unwrap();
  state.refresh_panel_hostnames().await;
  org.id
}

#[tokio::test]
async fn whose_panel_a_hostname_is() {
  let mut cfg = test_config();
  cfg.dashboard_hostname = Some("panel.test".to_string());
  let state = test_state_with(cfg);
  let acme = acme_with_panel(&state).await;

  assert!(state.is_panel_hostname("panel.test"));
  assert!(state.is_panel_hostname("panel.acme.test"));
  assert!(!state.is_panel_hostname("acme.test"));
  assert_eq!(state.panel_org(Some("panel.test")).await, Some(None));
  assert_eq!(
    state.panel_org(Some("panel.acme.test")).await,
    Some(Some(acme.clone()))
  );
  assert_eq!(state.panel_org(Some("www.acme.test")).await, None);
  assert_eq!(state.panel_org(None).await, None);

  // The login on an organization's panel admits a grant reaching it, the
  // super-admin's `*` included; the server's own panel and any other
  // hostname admit everyone.
  let acme_viewer = vec![Grant::new(GrantOrg::Child(acme.clone()), Role::Viewer)];
  let beta_admin = vec![Grant::new(GrantOrg::Child("beta".into()), Role::Admin)];
  assert!(
    state
      .panel_admits(Some("panel.acme.test"), &acme_viewer)
      .await
  );
  assert!(
    state
      .panel_admits(Some("panel.acme.test"), &crate::store::grants::all_admin())
      .await
  );
  assert!(
    !state
      .panel_admits(Some("panel.acme.test"), &beta_admin)
      .await
  );
  assert!(!state.panel_admits(Some("panel.acme.test"), &[]).await);
  assert!(state.panel_admits(Some("panel.test"), &beta_admin).await);
  assert!(state.panel_admits(Some("acme.test"), &beta_admin).await);
  assert!(state.panel_admits(None, &beta_admin).await);

  // Cleared, and the cache follows.
  state
    .org_store
    .lock()
    .await
    .set_panel_hostname(&acme, None)
    .unwrap();
  state.refresh_panel_hostnames().await;
  assert!(!state.is_panel_hostname("panel.acme.test"));
  assert!(state.is_panel_hostname("panel.test"));
}

#[tokio::test]
async fn the_layer_moves_a_panel_request_under_aperio_and_nothing_else() {
  use axum::body::Body;
  use tower::ServiceExt;
  let state = std::sync::Arc::new(test_state());
  acme_with_panel(&state).await;

  // A router that reports the path it was asked for.
  async fn echo(req: Request) -> Response {
    let seen = req.uri().path_and_query().map(|p| p.to_string()).unwrap();
    let original = req
      .extensions()
      .get::<PanelRequest>()
      .map(|p| p.original.clone())
      .unwrap_or_default();
    Response::new(Body::from(format!("{seen}|{original}")))
  }
  let app = axum::Router::new()
    .fallback(echo)
    .layer(axum::middleware::from_fn_with_state(state.clone(), rewrite));

  let ask = |host: &str, target: &str| {
    axum::http::Request::builder()
      .uri(target)
      .header("host", host)
      .body(Body::empty())
      .unwrap()
  };
  let body = |resp: Response| async move {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
      .await
      .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
  };

  // On the panel: the root is the dashboard, a path moves under `/aperio`
  // with its query, and the original is remembered.
  let resp = app
    .clone()
    .oneshot(ask("panel.acme.test", "/"))
    .await
    .unwrap();
  assert_eq!(body(resp).await, "/aperio|/");
  let resp = app
    .clone()
    .oneshot(ask("PANEL.acme.test:8443", "/api/session?x=1"))
    .await
    .unwrap();
  assert_eq!(body(resp).await, "/aperio/api/session?x=1|/api/session?x=1");
  // Already under `/aperio`: untouched, so `aperio-client api` keeps working.
  let resp = app
    .clone()
    .oneshot(ask("panel.acme.test", "/aperio/api/tokens"))
    .await
    .unwrap();
  assert_eq!(body(resp).await, "/aperio/api/tokens|");
  // Not a panel: untouched.
  let resp = app
    .clone()
    .oneshot(ask("www.acme.test", "/"))
    .await
    .unwrap();
  assert_eq!(body(resp).await, "/|");
  let resp = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .uri("/")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(body(resp).await, "/|");
}
