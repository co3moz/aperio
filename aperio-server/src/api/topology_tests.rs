//! Tests for the routing map.
//!
//! The map answers "how is a request routed", which includes routing with no
//! client behind it, so most of what it reports cannot be seen from the
//! Clients table. These tests cover the three parts that distinguish it: the
//! client-less `routes:`, the declared-but-offline binds, and the fact that
//! server-level routing belongs to master alone.

use super::*;
use crate::static_routes::{RespondRule, RouteRule, StaticRoutes};
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::State;

/// A config carrying two client-less routes, one of each action.
fn config_with_routes() -> crate::settings::ServerConfig {
  let mut cfg = test_config();
  cfg.static_routes = StaticRoutes::compile(vec![
    RouteRule {
      hostname: Some("old.example.com".to_string()),
      path: None,
      redirect: Some("https://new.example.com".to_string()),
      permanent: true,
      preserve_path: false,
      respond: None,
    },
    RouteRule {
      hostname: None,
      path: Some("/robots.txt".to_string()),
      redirect: None,
      permanent: false,
      preserve_path: false,
      respond: Some(RespondRule {
        status: 503,
        content_type: "text/plain".to_string(),
        body: "away".to_string(),
      }),
    },
  ])
  .expect("the rules compile");
  cfg
}

#[tokio::test]
async fn the_map_reports_client_less_routes_with_their_action_and_status() {
  let state = Arc::new(test_state_with(config_with_routes()));
  let headers = admin_headers(&state).await;

  let graph = topology_handler(State(state), headers).await.0;
  assert_eq!(graph.routes.len(), 2);

  let redirect = &graph.routes[0];
  assert_eq!(redirect.action, "redirect");
  assert_eq!(redirect.hostname.as_deref(), Some("old.example.com"));
  assert_eq!(redirect.target.as_deref(), Some("https://new.example.com"));
  assert_eq!(redirect.status, 301, "permanent: true is a 301");

  let respond = &graph.routes[1];
  assert_eq!(respond.action, "respond");
  assert_eq!(respond.path.as_deref(), Some("/robots.txt"));
  assert_eq!(respond.target, None);
  assert_eq!(respond.status, 503, "the rule's own status, not a default");
}

#[tokio::test]
async fn a_tenant_sees_its_clients_and_none_of_the_servers_own_routing() {
  // Static routes and expose ports are the server's, not a tenant's: an
  // organization dashboard that listed them would be reading the operator's
  // configuration.
  let state = Arc::new(test_state_with(config_with_routes()));
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  let mut handle = mock_client(Some("acme.example.com"), None, None, None);
  handle.perms.org_id = Some(org.clone());
  state.clients.lock().await.insert("c1".to_string(), handle);

  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let graph = topology_handler(State(state.clone()), cookie_headers(&token))
    .await
    .0;
  assert_eq!(graph.clients.len(), 1, "its own client");
  assert!(graph.routes.is_empty(), "the routes: section is master's");
  assert!(graph.exposes.is_empty(), "so are the expose ports");

  // Master sees the routing it owns, and not the tenant's client.
  let graph = topology_handler(State(state.clone()), admin_headers(&state).await)
    .await
    .0;
  assert_eq!(graph.routes.len(), 2);
  assert!(graph.clients.is_empty());
}

#[tokio::test]
async fn a_granted_bind_no_client_serves_is_reported_as_offline() {
  // The part of the map the Clients table cannot show: a service that is
  // supposed to exist. A token grants the name, nothing is serving it.
  let state = Arc::new(test_state());
  {
    let mut tokens = state.token_store.lock().await;
    tokens.create(
      "deployed".into(),
      vec!["live.example.com".into()],
      vec![],
      vec![],
      None,
      None,
      None,
      false,
      false,
      false,
      None,
      vec![],
      None,
    );
    tokens.create(
      "not-yet".into(),
      vec!["offline.example.com".into(), "*".into()],
      vec!["/api".into()],
      vec![],
      None,
      None,
      None,
      false,
      false,
      false,
      None,
      vec![],
      None,
    );
  }
  state.clients.lock().await.insert(
    "c1".to_string(),
    mock_client(Some("live.example.com"), None, None, None),
  );

  let graph = topology_handler(State(state.clone()), admin_headers(&state).await)
    .await
    .0;
  let binds: Vec<&str> = graph.offline.iter().map(|o| o.bind.as_str()).collect();
  assert!(binds.contains(&"offline.example.com"), "{binds:?}");
  assert!(
    binds.contains(&"/api"),
    "a path grant counts too: {binds:?}"
  );
  assert!(
    !binds.contains(&"live.example.com"),
    "a bind a client serves is not offline: {binds:?}"
  );
  assert!(
    !binds.contains(&"*"),
    "a wildcard grant is not an expected service: {binds:?}"
  );
  let path_entry = graph.offline.iter().find(|o| o.bind == "/api").unwrap();
  assert_eq!(path_entry.kind, "path");
  assert_eq!(path_entry.token_name, "not-yet");
}

#[tokio::test]
async fn an_expired_tokens_binds_are_not_expected_services() {
  let state = Arc::new(test_state());
  state.token_store.lock().await.create(
    "expired".into(),
    vec!["gone.example.com".into()],
    vec![],
    vec![],
    Some(0),
    None,
    None,
    false,
    false,
    false,
    None,
    vec![],
    None,
  );
  let graph = topology_handler(State(state.clone()), admin_headers(&state).await)
    .await
    .0;
  assert!(
    graph.offline.is_empty(),
    "an expired token grants nothing to wait for: {:?}",
    graph.offline.iter().map(|o| &o.bind).collect::<Vec<_>>()
  );
}
