//! The two controls an operator has over a connected client: overruling the
//! binds it declared, and taking one of its services out of routing without
//! dropping the connection.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Request payload for the dashboard client override (overrule) endpoint.
/// Each field fully replaces the corresponding override: a non-empty value
/// sets it, an empty string/list or `null` clears it. Overrides are in-memory
/// only and disappear when the client reconnects or the server restarts.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ClientOverrideRequest {
  /// Single hostname to route this connection on, replacing every declared and
  /// assigned name. Superseded by `hostname_binds` when both are present.
  pub(crate) hostname_bind: Option<String>,
  /// Hostnames to route this connection on, replacing every declared and
  /// assigned name. Lets an operator retarget one of a client's names while
  /// keeping the others (blank entries are dropped).
  pub(crate) hostname_binds: Option<Vec<String>>,
  pub(crate) path_bind: Option<String>,
}

/// Applies a temporary hostname/path bind override to a connected client.
/// Protected by the dashboard session middleware.
#[utoipa::path(post, path = "/aperio/api/clients/{id}/override", tag = "dashboard",
  description = "Temporarily overrule a client's hostname/path bind server-side (empty values clear the override).",
  params(("id" = String, Path, description = "Client connection id")),
  request_body = ClientOverrideRequest,
  responses((status = 200, description = "Override applied"), (status = 404, description = "No such client")))]
pub(crate) async fn client_override_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  axum::extract::Query(which): axum::extract::Query<ServiceQuery>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<ClientOverrideRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Validate before mutating: reject invalid values with 400. `hostname_binds`
  // wins when both forms are sent; the singular form stays accepted so older
  // callers (and `aperio-client api client override`) keep working.
  let raw_hostnames: Vec<String> = match payload.hostname_binds {
    Some(ref list) => list.clone(),
    None => payload.hostname_bind.clone().into_iter().collect(),
  };
  let mut new_hostnames: Vec<String> = Vec::new();
  for raw in raw_hostnames.iter().filter(|r| !r.trim().is_empty()) {
    match normalize_hostname_bind(raw) {
      Some(h) => {
        if !new_hostnames.contains(&h) {
          new_hostnames.push(h);
        }
      }
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid hostname_bind value").into_response();
      }
    }
  }
  let new_path = match payload.path_bind.as_deref() {
    None | Some("") => None,
    Some(raw) => match normalize_path_bind(raw) {
      Some(p) => Some(p),
      None => {
        return (StatusCode::BAD_REQUEST, "Invalid path_bind value").into_response();
      }
    },
  };

  // Org isolation: a caller may only overrule a client of their effective org.
  // A cross-org (or unknown) client is indistinguishable, both 404, so a
  // client's existence never leaks across orgs.
  let org = crate::auth::effective_org(&state, &headers).await;
  // Organization fence: an overrule is the one place a bind is set without a
  // token permission behind it, so a fenced org must not be able to point one
  // of its clients at a hostname it does not own.
  if !new_hostnames.is_empty() {
    let allowlist = state.org_store.lock().await.hostnames_of(org.as_deref());
    for host in &new_hostnames {
      if !crate::store::orgs::hostname_in_org_allowlist(host, &allowlist) {
        return (
          StatusCode::FORBIDDEN,
          format!(
            "hostname {} is outside this organization's allowlist ({})",
            host,
            allowlist.join(", ")
          ),
        )
          .into_response();
      }
    }
  }
  let found = {
    let mut clients = state.clients.write().await;
    match clients.get_mut(&client_id) {
      Some(handle) if handle.perms.org_id == org => {
        let Some(service) = handle.services.get_mut(which.service) else {
          return (StatusCode::NOT_FOUND, "No such service on this client").into_response();
        };
        service.override_hostname_binds = new_hostnames.clone();
        service.override_path_bind = new_path.clone();
        true
      }
      _ => false,
    }
  };
  if found {
    info!(
      "Dashboard overrule applied to client {}: hostname_bind={:?} path_bind={:?}",
      client_id, new_hostnames, new_path
    );
    state
      .audit_session(
        "client_overrule",
        &headers,
        &actor_ip,
        &format!(
          "client={} hostname={:?} path={:?}",
          client_id, new_hostnames, new_path
        ),
      )
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Client not found").into_response()
  }
}

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
