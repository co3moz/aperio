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

use crate::api::tokens::{org_token_quota_reached, validate_token_perms};
use crate::auth::{constant_time_eq_str, extract_token};
use crate::routing::{extract_client_ip, normalize_hostname_bind, random_subdomain_hostname};
use crate::state::AppState;

/// Payload for the programmatic tunnel provisioning endpoint
/// (`POST /aperio/api/tunnels`).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct TunnelCreateRequest {
  /// Label for the ephemeral token; defaults to "tunnel".
  pub(crate) name: Option<String>,
  /// Explicit hostname to bind. Omitted = a random subdomain is generated
  /// (requires APERIO_RANDOM_SUBDOMAIN on the server).
  pub(crate) hostname: Option<String>,
  /// Source IPs/CIDRs allowed to connect with the minted token.
  #[serde(default)]
  pub(crate) allowed_ips: Vec<String>,
  /// Token lifetime in seconds; defaults to 1 hour, capped at 7 days.
  pub(crate) ttl_seconds: Option<u64>,
}

/// Default lifetime of a programmatically provisioned tunnel token.
const TUNNEL_DEFAULT_TTL_SECS: u64 = 3_600;
/// Maximum lifetime accepted by the tunnels endpoint.
const TUNNEL_MAX_TTL_SECS: u64 = 7 * 24 * 3_600;

/// Authorizes programmatic tunnel API calls: the master server token
/// presented as `Authorization: Bearer` / `x-auth-token`, or a dashboard
/// session (or admin key) of at least the Operator role, minting a tunnel
/// credential is an operator-level action, so a read-only Viewer is refused.
/// Header auth makes the endpoint usable from CI without a browser login flow.
async fn tunnel_api_authorized(state: &AppState, headers: &HeaderMap) -> bool {
  if let Some(presented) = extract_token(headers)
    && constant_time_eq_str(&presented, &state.config().token)
  {
    return true;
  }
  crate::auth::dashboard_role(state, headers)
    .await
    .is_some_and(|role| role >= crate::store::users::Role::Operator)
}

/// Lists the tunnels declared by the clients of the caller's organization,
/// the dashboard's view of what `--bind-tunnels` can reach.
///
/// Read-only and organization-scoped, so a Viewer may see what exists without
/// being able to bind anything: binding needs a tunnel token, which is a
/// separate credential with its own capability.
#[utoipa::path(get, path = "/aperio/api/tunnels", tag = "tunnels",
  description = "Lists the tunnels declared by this organization's connected clients.",
  responses((status = 200, description = "Declared tunnels", body = serde_json::Value)))]
pub(crate) async fn tunnels_declared_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<crate::tunnel::registry::TunnelView>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  Json(crate::tunnel::registry::visible_in_org(&state, org.as_deref()).await)
}

/// Provisions an ephemeral tunnel: mints a short-lived, hostname-scoped
/// dynamic token and returns it together with the hostname (once, the
/// secret is never shown again). Designed for automation such as per-PR
/// preview environments.
#[utoipa::path(post, path = "/aperio/api/tunnels", tag = "tunnels",
  description = "Programmatically provisions an ephemeral tunnel (scoped short-lived token + hostname). Master token (header) or dashboard session.",
  request_body = TunnelCreateRequest,
  responses((status = 200, description = "Ephemeral tunnel token + hostname", body = serde_json::Value), (status = 401, description = "Unauthorized")))]
pub(crate) async fn tunnels_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TunnelCreateRequest>,
) -> Response {
  let client_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  // Rate limit before auth so credential guessing is throttled like login.
  if !state.check_rate_limit(client_ip).await {
    return StatusCode::TOO_MANY_REQUESTS.into_response();
  }
  if !tunnel_api_authorized(&state, &headers).await {
    state
      .audit(
        "tunnel_denied",
        "-",
        &client_ip.to_string(),
        "invalid credentials",
      )
      .await;
    return (
      StatusCode::UNAUTHORIZED,
      "Bearer master token or dashboard session required",
    )
      .into_response();
  }

  let ttl = payload.ttl_seconds.unwrap_or(TUNNEL_DEFAULT_TTL_SECS);
  if ttl == 0 || ttl > TUNNEL_MAX_TTL_SECS {
    return (
      StatusCode::BAD_REQUEST,
      format!("ttl_seconds must be between 1 and {}", TUNNEL_MAX_TTL_SECS),
    )
      .into_response();
  }

  let name = payload
    .name
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .unwrap_or("tunnel")
    .to_string();
  if name.len() > 64 {
    return (
      StatusCode::BAD_REQUEST,
      "Tunnel name must be at most 64 characters",
    )
      .into_response();
  }

  let hostname = match payload
    .hostname
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
  {
    Some(raw) => match normalize_hostname_bind(raw) {
      Some(h) => h,
      None => {
        return (
          StatusCode::BAD_REQUEST,
          format!("Invalid hostname: {}", raw),
        )
          .into_response();
      }
    },
    None => match state.config().random_subdomain_suffix {
      Some(ref pattern) => random_subdomain_hostname(pattern),
      None => {
        return (
          StatusCode::BAD_REQUEST,
          "No hostname given and APERIO_RANDOM_SUBDOMAIN is not configured on the server",
        )
          .into_response();
      }
    },
  };

  let (_, _, allowed_ips) = match validate_token_perms(&[], &[], &payload.allowed_ips) {
    Ok(v) => v,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };

  // The ephemeral token belongs to the caller's effective org (master token
  // auth → master/None; a session → its selected/own org).
  let org = crate::auth::effective_org(&state, &headers).await;
  // Organization fence: an explicitly requested hostname must be one the org
  // may claim. A server-generated random subdomain is exempt, since the caller
  // cannot influence it and it can never collide with another org's hostname.
  if payload.hostname.is_some() {
    let allowlist = state.org_store.lock().await.hostnames_of(org.as_deref());
    if !crate::store::orgs::hostname_in_org_allowlist(&hostname, &allowlist) {
      return (
        StatusCode::FORBIDDEN,
        format!(
          "hostname {} is outside this organization's allowlist ({})",
          hostname,
          allowlist.join(", ")
        ),
      )
        .into_response();
    }
  }
  // These are real credentials in the same store as dynamic tokens, so the
  // organization's token quota applies here exactly as it does there.
  let quota_max = state
    .org_quota(org.as_deref())
    .await
    .and_then(|q| q.max_tokens);
  let (record, secret) = {
    let mut store = state.token_store.lock().await;
    if let Some(resp) = org_token_quota_reached(&store, org.as_deref(), quota_max) {
      return resp;
    }
    store.create(
      name,
      vec![hostname.clone()],
      Vec::new(),
      allowed_ips,
      Some(ttl),
      None,
      None,
      false,
      // An ephemeral tunnel token serves one hostname; it has no business
      // reaching into other clients' tunnels.
      false,
      false,
      org,
      // Nor sending messages to the organization it is briefly a guest of.
      Vec::new(),
      // Nor opening a fan of parallel connections: one preview hostname is
      // one service, and the server's own ceiling is the generous case here.
      None,
      // Nor exporting telemetry through it. An ephemeral token exists for a
      // preview URL; every capability it does not need is one it does not get.
      false,
    )
  };
  info!(
    "Ephemeral tunnel provisioned: {} → {} (id={}, expires_at={:?})",
    record.name, hostname, record.id, record.expires_at
  );
  state
    .audit_session(
      "tunnel_created",
      &headers,
      &client_ip.to_string(),
      &format!(
        "name={} id={} hostname={} expires_at={:?}",
        record.name, record.id, hostname, record.expires_at
      ),
    )
    .await;
  state
    .emit_event_in(
      "tunnel_created",
      serde_json::json!({
        "id": record.id,
        "name": record.name,
        "hostname": hostname,
        "expires_at": record.expires_at,
      }),
      record.org_id.clone(),
    )
    .await;

  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": record.id,
      "name": record.name,
      "hostname": hostname,
      "url": format!("https://{}", hostname),
      "token": secret,
      "expires_at": record.expires_at,
    })),
  )
    .into_response()
}

/// Tears down a provisioned tunnel by revoking its ephemeral token and
/// dropping any live connections using it (mirroring dynamic-token revoke,
/// the tunnel would otherwise keep serving until its client reconnected).
/// Same authentication as tunnel creation so CI jobs can clean up after
/// themselves.
#[utoipa::path(delete, path = "/aperio/api/tunnels/{id}", tag = "tunnels",
  description = "Deletes an ephemeral tunnel: revokes its token and drops its live connection.",
  params(("id" = String, Path, description = "Ephemeral tunnel (token) id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown id")))]
pub(crate) async fn tunnels_delete_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let client_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  if !state.check_rate_limit(client_ip).await {
    return StatusCode::TOO_MANY_REQUESTS.into_response();
  }
  if !tunnel_api_authorized(&state, &headers).await {
    state
      .audit(
        "tunnel_denied",
        "-",
        &client_ip.to_string(),
        "invalid credentials",
      )
      .await;
    return (
      StatusCode::UNAUTHORIZED,
      "Bearer master token or dashboard session required",
    )
      .into_response();
  }
  // Isolation: only ephemeral tunnels in the caller's effective org may be torn
  // down (master-token callers act in master; a session in its selected org).
  let org = crate::auth::effective_org(&state, &headers).await;
  let in_org = state
    .token_store
    .lock()
    .await
    .list()
    .iter()
    .any(|t| t.id == id && t.org_id == org);
  if !in_org {
    return (StatusCode::NOT_FOUND, "Unknown tunnel id").into_response();
  }
  let revoked = state.token_store.lock().await.revoke(&id);
  if revoked {
    info!("Ephemeral tunnel deleted: {}", id);
    let dropped = state.disconnect_token_clients(&id).await;
    if dropped > 0 {
      info!(
        "Disconnecting {} live client(s) using the deleted tunnel {}",
        dropped, id
      );
    }
    state
      .audit_session(
        "tunnel_deleted",
        &headers,
        &client_ip.to_string(),
        &format!("id={} disconnected_clients={}", id, dropped),
      )
      .await;
    state
      .emit_event_in("tunnel_deleted", serde_json::json!({"id": id}), org.clone())
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Tunnel not found").into_response()
  }
}

#[cfg(test)]
#[path = "tunnels_tests.rs"]
mod tests;
