//! The control an operator has over a connected client: taking one of its
//! services out of routing without dropping the connection. Which hostnames
//! it serves is the client's and its token's to say, and nothing else's.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Payload for the client enable/disable toggle.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ClientEnabledRequest {
  pub(crate) enabled: bool,
}

/// Dashboard kill switch: temporarily removes a connected client from the
/// routing pool (or puts it back). In-flight requests always complete.
#[utoipa::path(post, path = "/aperio/api/clients/{id}/enabled", tag = "dashboard",
  description = "Kill switch: enable/disable routing to one client without dropping its tunnel.",
  params(("id" = String, Path, description = "Client connection id")),
  request_body = ClientEnabledRequest,
  responses((status = 200, description = "State changed"), (status = 404, description = "No such client")))]
pub(crate) async fn client_enabled_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  axum::extract::Query(which): axum::extract::Query<ServiceQuery>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientEnabledRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Org isolation: a caller may only enable/disable a client of their org.
  let org = crate::auth::effective_org(&state, &headers).await;
  let found = {
    let mut clients = state.clients.write().await;
    match clients.get_mut(&client_id) {
      Some(handle) if handle.perms.org_id == org => {
        let Some(service) = handle.services.get_mut(which.service) else {
          return (StatusCode::NOT_FOUND, "No such service on this client").into_response();
        };
        service.admin_enabled = payload.enabled;
        true
      }
      _ => false,
    }
  };
  if found {
    info!(
      "Client {} {} via dashboard",
      client_id,
      if payload.enabled {
        "enabled"
      } else {
        "disabled"
      }
    );
    state
      .audit_session(
        if payload.enabled {
          "client_enabled"
        } else {
          "client_disabled"
        },
        &headers,
        &actor_ip,
        &format!("client={}", client_id),
      )
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
