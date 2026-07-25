//! Edge integration: publishes the hostnames this server currently serves to
//! a dynamic reverse proxy running in front of it.
//!
//! Aperio's hostnames are born at runtime, when a client connects and claims a
//! bind, so an edge proxy configured from static files or container labels
//! cannot know them. Two endpoints close that gap, both gated by
//! `APERIO_EDGE_TOKEN` (unset = the endpoints do not exist):
//!
//! * `GET /aperio/api/edge/ask?domain=<host>` answers 200 when the hostname is
//!   served and 404 when it is not. That is exactly Caddy's on-demand TLS
//!   `ask` contract, so Caddy issues a certificate the moment a tunnel for
//!   that hostname appears, with no configuration at all. It is also a plain
//!   probe any script can use.
//! * `GET /aperio/api/edge/traefik` returns a Traefik dynamic-configuration
//!   document: one router per served hostname, all pointing at this server.
//!   Point Traefik's HTTP provider at it and new tunnels become routable
//!   within one poll interval.
//!
//! Neither endpoint needs the dashboard, and both live outside the dashboard
//! session middleware: the edge proxy authenticates with the edge token alone.
//!
//! Note the deliberate choice of *connected* rather than *routable* clients: a
//! client whose backend is failing its health probe still owns its hostname,
//! and withdrawing the route (or the certificate) would turn a recoverable 504
//! into a TLS failure or a wrong-site answer.

use axum::{
  Json,
  extract::{Query, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::constant_time_eq_str;
use crate::routing::normalize_hostname_bind;
use crate::state::AppState;

/// Validates the edge credential, presented either as `Authorization: Bearer`
/// (Traefik's HTTP provider can set static headers) or as `?token=` (Caddy's
/// `ask` carries no headers, so the token rides in the configured URL).
/// `Err` carries the response to return; the endpoints answer 404 rather than
/// 401 when the feature is off, so a server without an edge token does not
/// advertise that the route exists at all.
fn authorize(
  state: &AppState,
  headers: &HeaderMap,
  query: &HashMap<String, String>,
) -> Result<(), Box<Response>> {
  let Some(ref token) = state.config().edge_token else {
    return Err(Box::new(
      (StatusCode::NOT_FOUND, "edge integration is not enabled").into_response(),
    ));
  };
  let bearer_ok = headers
    .get("authorization")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.strip_prefix("Bearer "))
    .is_some_and(|t| constant_time_eq_str(t, token));
  let query_ok = query
    .get("token")
    .is_some_and(|t| constant_time_eq_str(t, token));
  if bearer_ok || query_ok {
    Ok(())
  } else {
    Err(Box::new(
      (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    ))
  }
}

/// The hostnames this server can answer for right now, sorted and deduped.
///
/// Sorting matters: Traefik re-polls this document every few seconds and
/// compares it against the running configuration, so an unstable order would
/// churn routers for no reason.
pub(crate) async fn served_hostnames(state: &AppState) -> Vec<String> {
  let mut hosts: Vec<String> = {
    let clients = state.clients.lock().await;
    clients
      .values()
      .flat_map(|c| c.effective_hostnames().into_iter().cloned())
      .collect()
  };
  // Opt-in: hostnames a token permits but no client is currently serving. It
  // lets a certificate exist before the first client connects, at the cost of
  // letting a tenant provoke an ACME request for any hostname its token
  // covers, so it is off by default.
  if state.config().edge_include_offline {
    let store = state.token_store.lock().await;
    for token in store.list() {
      if token.is_expired() {
        continue;
      }
      hosts.extend(token.hostnames.iter().filter(|h| *h != "*").cloned());
    }
  }
  hosts.sort();
  hosts.dedup();
  hosts
}

/// Caddy on-demand TLS `ask` endpoint (and a generic "is this hostname
/// served?" probe): 200 when the hostname is served, 404 when it is not.
#[utoipa::path(get, path = "/aperio/api/edge/ask", tag = "public",
  description = "Answers 200 when the hostname is currently served, 404 otherwise. Matches Caddy's on-demand TLS `ask` contract. Requires APERIO_EDGE_TOKEN.",
  params(
    ("domain" = String, Query, description = "Hostname to check"),
    ("token" = Option<String>, Query, description = "Edge token (alternative to the Authorization header)")
  ),
  responses(
    (status = 200, description = "The hostname is served"),
    (status = 400, description = "Missing or malformed domain"),
    (status = 401, description = "Missing/invalid edge token"),
    (status = 404, description = "Not served (or the feature is disabled)")))]
pub(crate) async fn edge_ask_handler(
  State(state): State<Arc<AppState>>,
  Query(query): Query<HashMap<String, String>>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = authorize(&state, &headers, &query) {
    return *resp;
  }
  let Some(raw) = query.get("domain") else {
    return (StatusCode::BAD_REQUEST, "missing domain parameter").into_response();
  };
  let Some(domain) = normalize_hostname_bind(raw) else {
    return (StatusCode::BAD_REQUEST, "malformed domain parameter").into_response();
  };
  // Matching goes through the same predicate the proxy uses, so a certificate
  // is only ever authorized for a hostname a request would actually reach.
  let served = served_hostnames(&state).await.contains(&domain);
  if served {
    (StatusCode::OK, "ok").into_response()
  } else {
    (StatusCode::NOT_FOUND, "not served").into_response()
  }
}

/// Builds the Traefik dynamic-configuration document: one router per served
/// hostname, all pointing at the single `aperio` service.
fn traefik_document(
  hostnames: &[String],
  service_url: &str,
  entrypoints: &[String],
  cert_resolver: Option<&str>,
) -> serde_json::Value {
  let mut routers = serde_json::Map::new();
  for host in hostnames {
    let mut router = serde_json::Map::new();
    // Backtick-quoted matcher value, per Traefik's rule syntax. Hostnames are
    // normalized (lowercase, no port, label charset checked) before they ever
    // reach a bind, so there is nothing here to escape.
    router.insert(
      "rule".into(),
      serde_json::Value::String(format!("Host(`{host}`)")),
    );
    router.insert("service".into(), serde_json::Value::String("aperio".into()));
    if !entrypoints.is_empty() {
      router.insert("entryPoints".into(), serde_json::json!(entrypoints));
    }
    if let Some(resolver) = cert_resolver {
      router.insert(
        "tls".into(),
        serde_json::json!({ "certResolver": resolver }),
      );
    }
    // The hostname itself keys the router: stable across polls (so Traefik
    // never recreates an unchanged router) and unique by construction.
    routers.insert(format!("aperio-{host}"), serde_json::Value::Object(router));
  }
  serde_json::json!({
    "http": {
      "routers": routers,
      "services": {
        "aperio": {
          "loadBalancer": {
            // Aperio routes by Host header, so it must survive the hop.
            "passHostHeader": true,
            "servers": [{ "url": service_url }],
          }
        }
      }
    }
  })
}

/// Traefik HTTP-provider document for the live hostname inventory.
#[utoipa::path(get, path = "/aperio/api/edge/traefik", tag = "public",
  description = "Traefik dynamic configuration: one router per served hostname, pointing at this server. For Traefik's HTTP provider. Requires APERIO_EDGE_TOKEN and APERIO_EDGE_SERVICE_URL.",
  params(("token" = Option<String>, Query, description = "Edge token (alternative to the Authorization header)")),
  responses(
    (status = 200, description = "Traefik dynamic configuration", body = serde_json::Value),
    (status = 401, description = "Missing/invalid edge token"),
    (status = 404, description = "The feature is disabled"),
    (status = 503, description = "APERIO_EDGE_SERVICE_URL is not configured")))]
pub(crate) async fn edge_traefik_handler(
  State(state): State<Arc<AppState>>,
  Query(query): Query<HashMap<String, String>>,
  headers: HeaderMap,
) -> Response {
  if let Err(resp) = authorize(&state, &headers, &query) {
    return *resp;
  }
  let config = state.config();
  let Some(ref service_url) = config.edge_service_url else {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      "APERIO_EDGE_SERVICE_URL must be set to the URL the edge proxy should forward to",
    )
      .into_response();
  };
  let hostnames = served_hostnames(&state).await;
  Json(traefik_document(
    &hostnames,
    service_url,
    &config.edge_entrypoints,
    config.edge_cert_resolver.as_deref(),
  ))
  .into_response()
}

#[cfg(test)]
#[path = "edge_tests.rs"]
mod tests;
