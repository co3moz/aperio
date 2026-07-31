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
use crate::state::MaintenanceFlag;
use crate::store::orgs::normalize_org_hostname_pattern;

/// Longest maintenance window an operator can ask for in one call: a year.
/// Past that the number is almost certainly a mistake (milliseconds, or a
/// unix timestamp pasted into a duration), and an open-ended flag is what
/// "indefinitely" already means.
const MAX_TTL_SECS: u64 = 365 * 24 * 3600;

/// Payload for toggling maintenance mode on a hostname (dashboard).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct MaintenanceRequest {
  /// Hostname to toggle: an exact hostname (`robogon.com`), a subdomain
  /// wildcard (`*.robogon.com`, every subdomain at any depth but not the
  /// apex, so an operator who wants both sets both), or `*` for every
  /// hostname on the server.
  pub(crate) hostname: String,
  pub(crate) enabled: bool,
  /// Why, in one line. Shown on the 503 page and in the dashboard, so a
  /// visitor and the next operator read the same sentence. Omitted or empty
  /// = none.
  #[serde(default)]
  pub(crate) reason: Option<String>,
  /// Seconds until the flag lifts by itself. Omitted or `0` = until someone
  /// turns it off, which is what this always did. The flag that causes an
  /// outage is the one switched on for twenty minutes of work and forgotten,
  /// so the window is worth stating when it is known.
  #[serde(default)]
  pub(crate) ttl_seconds: Option<u64>,
}

/// One entry of the maintenance list.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct MaintenanceEntry {
  /// The hostname, `*.example.com` wildcard, or `*`.
  pub(crate) hostname: String,
  pub(crate) reason: Option<String>,
  /// Unix seconds when it lifts by itself, absent when open-ended.
  pub(crate) until: Option<u64>,
  /// Unix seconds when it was set, and by whom.
  pub(crate) since: u64,
  pub(crate) actor: String,
}

/// Lists hostnames currently in maintenance mode.
#[utoipa::path(get, path = "/aperio/api/maintenance", tag = "dashboard",
  description = "Hostnames and patterns currently in maintenance mode (`*` = every hostname, `*.example.com` = every subdomain of it).",
  responses((status = 200, description = "Maintenance flags", body = Vec<MaintenanceEntry>)))]
pub(crate) async fn maintenance_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<MaintenanceEntry>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let now = crate::store::tokens::now_secs();
  let mut set = state.maintenance.lock().await;
  // Reading the list is also when expired flags go: no timer to forget to
  // start, and the proxy already treats them as gone.
  set.retain(|_, flag| !flag.expired(now));
  // Only the maintenance flags set within the caller's effective org.
  let mut list: Vec<MaintenanceEntry> = set
    .iter()
    .filter(|(_, flag)| flag.org == org)
    .map(|(hostname, flag)| MaintenanceEntry {
      hostname: hostname.clone(),
      reason: flag.reason.clone(),
      until: flag.until,
      since: flag.since,
      actor: flag.actor.clone(),
    })
    .collect();
  list.sort_by(|a, b| a.hostname.cmp(&b.hostname));
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
  let reason = payload
    .reason
    .as_deref()
    .map(str::trim)
    .filter(|r| !r.is_empty())
    .map(|r| r.chars().take(200).collect::<String>());
  let ttl = payload.ttl_seconds.filter(|t| *t > 0);
  if ttl.is_some_and(|t| t > MAX_TTL_SECS) {
    return (
      StatusCode::BAD_REQUEST,
      format!(
        "ttl_seconds must be at most {MAX_TTL_SECS} (a year); omit it for an open-ended flag"
      ),
    )
      .into_response();
  }
  let now = crate::store::tokens::now_secs();
  let actor = state.session_actor(&headers).await;
  let changed = {
    let mut set = state.maintenance.lock().await;
    set.retain(|_, flag| !flag.expired(now));
    if payload.enabled {
      let flag = MaintenanceFlag {
        org: org.clone(),
        reason: reason.clone(),
        until: ttl.map(|t| now + t),
        since: now,
        actor: actor.clone(),
      };
      // Re-flagging with a different reason or window is a change, not a
      // no-op: an operator extending a window wants the new one recorded and
      // announced, and the old one was the answer on someone's screen.
      let previous = set.insert(hostname.clone(), flag.clone());
      match previous {
        Some(before) => before.reason != flag.reason || before.until != flag.until,
        None => true,
      }
    } else {
      // Clear a flag your own organization set, and, for master, any flag at
      // all: master owns the server, and a flag it cannot clear is a 503 with
      // no screen to turn it off from, since the list is org-scoped.
      let mine = set.get(&hostname).map(|f| f.org == org).unwrap_or(false);
      if mine || org.is_none() {
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
      "Maintenance mode {} for {}{}{}",
      if payload.enabled {
        "enabled"
      } else {
        "disabled"
      },
      hostname,
      reason
        .as_deref()
        .map(|r| format!(" ({r})"))
        .unwrap_or_default(),
      ttl.map(|t| format!(" for {t}s")).unwrap_or_default(),
    );
    state
      .audit_session(
        event,
        &headers,
        &actor_ip,
        &format!(
          "hostname={}{}{}",
          hostname,
          reason
            .as_deref()
            .map(|r| format!(" reason={r}"))
            .unwrap_or_default(),
          ttl.map(|t| format!(" ttl={t}s")).unwrap_or_default(),
        ),
      )
      .await;
    state
      .emit_event_in(
        event,
        serde_json::json!({
          "hostname": hostname,
          "reason": reason,
          "until": ttl.map(|t| now + t),
        }),
        org.clone(),
      )
      .await;
  }
  (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
