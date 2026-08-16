//! Tests for the routing map.
//!
//! The map answers "how is a request routed", which includes routing with no
//! client behind it, so most of what it reports cannot be seen from the
//! Clients table. These tests cover the three parts that distinguish it: the
//! client-less `routes:`, the declared-but-offline binds, and the fact that
//! server-level routing belongs to master alone.

use super::*;
use crate::static_routes::{RespondRule, RouteRule, StaticRoutes};
use crate::store::tokens::TokenSpec;
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
      ..Default::default()
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
      ..Default::default()
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
  state.clients.write().await.insert("c1".to_string(), handle);

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
    tokens
      .create(TokenSpec {
        name: "deployed".into(),
        hostnames: vec!["live.example.com".into()],
        ..Default::default()
      })
      .expect("the test store can be written to");
    tokens
      .create(TokenSpec {
        name: "not-yet".into(),
        hostnames: vec!["offline.example.com".into(), "*".into()],
        paths: vec!["/api".into()],
        ..Default::default()
      })
      .expect("the test store can be written to");
  }
  state.clients.write().await.insert(
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
  state
    .token_store
    .lock()
    .await
    .create(TokenSpec {
      name: "expired".into(),
      hostnames: vec!["gone.example.com".into()],
      ttl_seconds: Some(0),
      ..Default::default()
    })
    .expect("the test store can be written to");
  let graph = topology_handler(State(state.clone()), admin_headers(&state).await)
    .await
    .0;
  assert!(
    graph.offline.is_empty(),
    "an expired token grants nothing to wait for: {:?}",
    graph.offline.iter().map(|o| &o.bind).collect::<Vec<_>>()
  );
}

// Not `#[tokio::test]`: the config file is written and reloaded under the
// shared config lock, which must not be held across a runtime's await points.
#[test]
fn expose_ports_report_who_serves_them_without_leaking_the_key() {
  // The uncovered half of the map: the `expose:` section, matched to a live
  // client the same way the relay matches one, by tunnel name + token, or by
  // the deprecated shared key. The key itself must never appear.
  let _guard = config_lock();
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
  let file = test_temp_root().join(format!("topo-expose-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(
    &file,
    "expose:\n  - port: 15432\n    tunnel: pg_main\n  - port: 15433\n    key: shared-secret\n  - port: 15434\n    tunnel: nothing_declares_this\n",
  )
  .unwrap();
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", file.to_str().unwrap()) };
  crate::config_file::load();

  let state = Arc::new(test_state());
  let decl = |name: &str, expose: Option<&str>| crate::protocol::TunnelDecl {
    name: Some(name.to_string()),
    custom_name: None,
    target: "127.0.0.1:5432".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    idle_timeout: None,
    expose: expose.map(str::to_string),
  };
  let mut by_name = mock_client(Some("a.example.com"), None, None, None);
  by_name.service.tunnels = vec![decl("pg_main", None)];
  let mut by_key = mock_client(Some("b.example.com"), None, None, None);
  by_key.service.tunnels = vec![decl("other", Some("shared-secret"))];
  // A draining client serves nothing, whatever it declares.
  let mut draining = mock_client(Some("c.example.com"), None, None, None);
  draining.service.tunnels = vec![decl("nothing_declares_this", None)];
  draining.draining = true;
  {
    let mut clients = state.clients.write().await;
    clients.insert("c-name".to_string(), by_name);
    clients.insert("c-key".to_string(), by_key);
    clients.insert("c-drain".to_string(), draining);
  }

  let graph = topology_handler(State(state.clone()), admin_headers(&state).await)
    .await
    .0;
  unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
  let _ = crate::config_file::reload();

  assert_eq!(graph.exposes.len(), 3);
  let by_port = |p: u16| graph.exposes.iter().find(|e| e.port == p).unwrap();
  assert_eq!(by_port(15432).served_by.as_deref(), Some("c-name"));
  assert_eq!(by_port(15433).served_by.as_deref(), Some("c-key"));
  assert!(!by_port(15434).served, "a draining client serves nothing");
  let raw = serde_json::to_string(&graph.exposes).unwrap();
  assert!(
    !raw.contains("shared-secret"),
    "the key never leaves: {raw}"
  );

  // A tenant sees no exposes at all: they are the operator's ports.
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let graph = topology_handler(State(state), cookie_headers(&token))
    .await
    .0;
  assert!(graph.exposes.is_empty());
  });
}
