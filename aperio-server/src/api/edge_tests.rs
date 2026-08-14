//! Tests for the edge-integration endpoints (Caddy `ask`, Traefik provider).

use super::*;
use crate::settings::ServerConfig;
use crate::store::tokens::TokenSpec;
use crate::test_support::{mock_client, test_config, test_state_with};
use axum::http::HeaderValue;

/// A config with the edge integration enabled.
fn edge_config() -> ServerConfig {
  let mut config = test_config();
  config.edge_token = Some("edge-secret".to_string());
  config.edge_service_url = Some("http://aperio:8080".to_string());
  config.edge_entrypoints = vec!["websecure".to_string()];
  config.edge_cert_resolver = Some("letsencrypt".to_string());
  config
}

fn bearer(token: &str) -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
  );
  headers
}

fn q(pairs: &[(&str, &str)]) -> Query<HashMap<String, String>> {
  Query(
    pairs
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
  )
}

/// Registers a connected client serving `hostname`.
async fn with_client(state: &Arc<AppState>, id: &str, hostname: &str) {
  let handle = mock_client(Some(hostname), None, None, None);
  state.clients.write().await.insert(id.to_string(), handle);
}

#[tokio::test]
async fn ask_answers_200_for_a_served_hostname_and_404_otherwise() {
  let state = Arc::new(test_state_with(edge_config()));
  with_client(&state, "c1", "app.example.com").await;

  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "app.example.com")]),
    bearer("edge-secret"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  // A hostname nobody serves must not authorize a certificate.
  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "other.example.com")]),
    bearer("edge-secret"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);

  // Caddy sends the domain verbatim; a trailing dot, a port, or different
  // casing still refer to the same host.
  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "APP.example.com.")]),
    bearer("edge-secret"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn ask_validates_its_input() {
  let state = Arc::new(test_state_with(edge_config()));

  // No domain at all.
  let resp = edge_ask_handler(State(state.clone()), q(&[]), bearer("edge-secret")).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

  // Not a hostname.
  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "not a hostname/")]),
    bearer("edge-secret"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn edge_endpoints_require_the_token() {
  let state = Arc::new(test_state_with(edge_config()));
  with_client(&state, "c1", "app.example.com").await;

  // No credential.
  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "app.example.com")]),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

  // Wrong credential.
  let resp = edge_traefik_handler(State(state.clone()), q(&[]), bearer("nope")).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

  // The query parameter is accepted too: Caddy's `ask` carries no headers.
  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "app.example.com"), ("token", "edge-secret")]),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn edge_endpoints_are_absent_without_a_configured_token() {
  // Feature off: 404 rather than 401, so the route's existence never leaks.
  let state = Arc::new(test_state_with(test_config()));
  let resp = edge_ask_handler(
    State(state.clone()),
    q(&[("domain", "app.example.com")]),
    bearer("anything"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
  let resp = edge_traefik_handler(State(state.clone()), q(&[]), bearer("anything")).await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn traefik_document_lists_one_router_per_hostname() {
  let state = Arc::new(test_state_with(edge_config()));
  with_client(&state, "c1", "b.example.com").await;
  with_client(&state, "c2", "a.example.com").await;

  let resp = edge_traefik_handler(State(state.clone()), q(&[]), bearer("edge-secret")).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = crate::test_support::json_body(resp).await;

  let routers = body["http"]["routers"].as_object().unwrap();
  assert_eq!(routers.len(), 2);
  let router = &routers["aperio-a.example.com"];
  assert_eq!(router["rule"], "Host(`a.example.com`)");
  assert_eq!(router["service"], "aperio");
  assert_eq!(router["entryPoints"], serde_json::json!(["websecure"]));
  assert_eq!(router["tls"]["certResolver"], "letsencrypt");

  // One shared service pointing back at this server, with the Host header
  // preserved (Aperio routes by it).
  let lb = &body["http"]["services"]["aperio"]["loadBalancer"];
  assert_eq!(lb["passHostHeader"], true);
  assert_eq!(lb["servers"][0]["url"], "http://aperio:8080");
}

#[tokio::test]
async fn traefik_document_omits_optional_router_fields() {
  let mut config = edge_config();
  config.edge_entrypoints = Vec::new();
  config.edge_cert_resolver = None;
  let state = Arc::new(test_state_with(config));
  with_client(&state, "c1", "app.example.com").await;

  let resp = edge_traefik_handler(State(state.clone()), q(&[]), bearer("edge-secret")).await;
  let body = crate::test_support::json_body(resp).await;
  let router = &body["http"]["routers"]["aperio-app.example.com"];
  assert!(router.get("entryPoints").is_none());
  assert!(router.get("tls").is_none());
}

#[tokio::test]
async fn traefik_document_needs_a_service_url() {
  let mut config = edge_config();
  config.edge_service_url = None;
  let state = Arc::new(test_state_with(config));
  let resp = edge_traefik_handler(State(state.clone()), q(&[]), bearer("edge-secret")).await;
  assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn served_hostnames_are_sorted_deduped_and_include_offline_only_on_request() {
  let state = Arc::new(test_state_with(edge_config()));
  with_client(&state, "c1", "b.example.com").await;
  with_client(&state, "c2", "a.example.com").await;
  // Two clients serving the same hostname (load balancing) yield one entry.
  with_client(&state, "c3", "a.example.com").await;
  // A token permits a hostname nobody is serving right now.
  state.token_store.lock().await.create(TokenSpec {
    name: "offline".to_string(),
    hostnames: vec!["offline.example.com".to_string()],
    ..Default::default()
  });

  // Sorted order is what keeps Traefik from churning routers between polls.
  assert_eq!(
    served_hostnames(&state).await,
    vec!["a.example.com".to_string(), "b.example.com".to_string()]
  );

  let mut config = edge_config();
  config.edge_include_offline = true;
  let state2 = Arc::new(test_state_with(config));
  with_client(&state2, "c1", "a.example.com").await;
  state2.token_store.lock().await.create(TokenSpec {
    name: "offline".to_string(),
    hostnames: vec!["offline.example.com".to_string(), "*".to_string()],
    ..Default::default()
  });
  // The wildcard permission is not a hostname and must never become a router.
  assert_eq!(
    served_hostnames(&state2).await,
    vec![
      "a.example.com".to_string(),
      "offline.example.com".to_string()
    ]
  );
}
