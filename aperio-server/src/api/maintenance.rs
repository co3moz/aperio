use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::orgs::normalize_org_hostname_pattern;

/// Payload for toggling maintenance mode on a hostname (dashboard).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct MaintenanceRequest {
  /// Hostname to toggle: an exact hostname (`robogon.com`), a subdomain
  /// wildcard (`*.robogon.com`, every subdomain at any depth but not the
  /// apex, so an operator who wants both sets both), or `*` for every
  /// hostname on the server.
  pub(crate) hostname: String,
  pub(crate) enabled: bool,
}

/// Lists hostnames currently in maintenance mode.
#[utoipa::path(get, path = "/aperio/api/maintenance", tag = "dashboard",
  description = "Hostnames and patterns currently in maintenance mode (`*` = every hostname, `*.example.com` = every subdomain of it).",
  responses((status = 200, description = "Hostname list", body = Vec<String>)))]
pub(crate) async fn maintenance_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<String>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let set = state.maintenance.lock().await;
  // Only the maintenance flags set within the caller's effective org.
  let mut list: Vec<String> = set
    .iter()
    .filter(|(_, o)| **o == org)
    .map(|(h, _)| h.clone())
    .collect();
  list.sort();
  Json(list)
}

/// Enables/disables maintenance mode for a hostname. In-memory only, like
/// bind overrides: a server restart clears all maintenance flags.
#[utoipa::path(post, path = "/aperio/api/maintenance", tag = "dashboard",
  description = "Turns maintenance mode on/off for a hostname, a `*.example.com` subdomain wildcard, or `*` (503 page while on). In-memory; cleared by a restart.",
  request_body = MaintenanceRequest,
  responses((status = 200, description = "Maintenance state changed")))]
pub(crate) async fn maintenance_set_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<MaintenanceRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  let raw = payload.hostname.trim();
  // Same shape as an organization's allowlist: an exact hostname, a
  // subdomain wildcard, or `*`. One spelling of "everything under this
  // domain" across the product beats a second one invented here.
  let hostname = match normalize_org_hostname_pattern(raw) {
    Some(h) => h,
    None => {
      return (
        StatusCode::BAD_REQUEST,
        format!("Invalid hostname: {}", raw),
      )
        .into_response();
    }
  };

  let org = crate::auth::effective_org(&state, &headers).await;
  // The `*` wildcard puts every hostname into maintenance, a server-wide
  // switch reserved for the master organization.
  if hostname == "*" && org.is_some() {
    return (
      StatusCode::FORBIDDEN,
      "the * wildcard is reserved for the master organization",
    )
      .into_response();
  }
  // A specific hostname may only be toggled by the organization that may serve
  // it, so one org cannot 503 another org's site.
  if payload.enabled
    && hostname != "*"
    && !state
      .org_may_claim_hostname(org.as_deref(), &hostname)
      .await
  {
    return (
      StatusCode::FORBIDDEN,
      "that hostname is not served by your organization",
    )
      .into_response();
  }
  let changed = {
    let mut set = state.maintenance.lock().await;
    if payload.enabled {
      set.insert(hostname.clone(), org.clone()).is_none()
    } else {
      // Only clear a flag your own organization set.
      if set.get(&hostname).map(|o| *o == org).unwrap_or(false) {
        set.remove(&hostname).is_some()
      } else {
        false
      }
    }
  };
  if changed {
    let event = if payload.enabled {
      "maintenance_on"
    } else {
      "maintenance_off"
    };
    info!(
      "Maintenance mode {} for {}",
      if payload.enabled {
        "enabled"
      } else {
        "disabled"
      },
      hostname
    );
    state
      .audit_session(
        event,
        &headers,
        &actor_ip,
        &format!("hostname={}", hostname),
      )
      .await;
    state
      .emit_event_in(
        event,
        serde_json::json!({"hostname": hostname}),
        org.clone(),
      )
      .await;
  }
  (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
