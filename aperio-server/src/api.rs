use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::protocol::PROTOCOL_VERSION;
use crate::state::AppState;

pub(crate) mod admin_keys;
pub(crate) mod clients;
pub(crate) mod config_schema;
pub(crate) mod edge;
pub(crate) mod explain;
pub(crate) mod export;
pub(crate) mod inbox;
pub(crate) mod inspector;
pub(crate) mod maintenance;
pub(crate) mod metrics;
pub(crate) mod observe;
pub(crate) mod openapi;
pub(crate) mod orgs;
pub(crate) mod otlp;
pub(crate) mod publish;
pub(crate) mod purge;
pub(crate) mod scaling;
pub(crate) mod settings;
pub(crate) mod tokens;
pub(crate) mod topology;
pub(crate) mod tunnels;
pub(crate) mod users;
pub(crate) mod webhooks;

/// Dashboard frontend built from `aperio-dashboard/` (Vite + React) by
/// build.rs. In release builds the files are embedded into the binary; in
/// debug builds rust-embed reads them from disk so a rebuilt `dist/` is
/// picked up without recompiling.
#[derive(rust_embed::RustEmbed)]
#[folder = "../aperio-dashboard/dist"]
struct DashboardAssets;

/// Content-Security-Policy for the dashboard and login pages. The build emits
/// only external module scripts and an external stylesheet (no inline script),
/// so `script-src 'self'` holds; Radix Themes uses inline `style` attributes
/// (needs `style-src 'unsafe-inline'`) and the app sets a `data:` favicon
/// (needs `img-src data:`). HSTS is intentionally left to the TLS-terminating
/// proxy so a plain-HTTP self-hosted setup is not locked to HTTPS.
const DASHBOARD_CSP: &str = "default-src 'self'; img-src 'self' data:; \
   font-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; \
   connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";

/// Serves a file from the embedded dashboard build. Hashed assets are safe to
/// cache forever; HTML entry points must always be revalidated. Security
/// headers are attached so the dashboard/login pages cannot be framed,
/// MIME-sniffed, or leak referrers.
pub(crate) fn serve_embedded(path: &str, immutable: bool) -> Response {
  use axum::http::header;
  match DashboardAssets::get(path) {
    Some(file) => {
      let mime = mime_guess::from_path(path).first_or_octet_stream();
      let cache_control = if immutable {
        "public, max-age=31536000, immutable"
      } else {
        "no-cache"
      };
      (
        [
          (header::CONTENT_TYPE, mime.as_ref()),
          (header::CACHE_CONTROL, cache_control),
          (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
          (header::X_FRAME_OPTIONS, "DENY"),
          (header::REFERRER_POLICY, "no-referrer"),
          (header::CONTENT_SECURITY_POLICY, DASHBOARD_CSP),
        ],
        file.data.into_owned(),
      )
        .into_response()
    }
    None => (StatusCode::NOT_FOUND, "Not found").into_response(),
  }
}

/// Handler serving the embedded dashboard SPA.
pub(crate) async fn dashboard_handler() -> Response {
  serve_embedded("index.html", false)
}

/// Serves the hashed static assets (JS/CSS) of the dashboard build. These are
/// public: the login page needs them before any session exists.
pub(crate) async fn dashboard_asset_handler(
  axum::extract::Path(path): axum::extract::Path<String>,
) -> Response {
  serve_embedded(&format!("assets/{path}"), true)
}

/// Health check: liveness for anyone, the numbers for a credential.
///
/// Without a credential the body is `status` and `ui_language`, nothing
/// else. A monitor reads the status, and the login page reads the default
/// language before any session exists, which is why that one field stays
/// public. The version, the tunnel protocol version, the connected client
/// count, the uptime and the request total are answered to the master token,
/// a tunnel token, a dashboard session or an admin key, which is what `aperio
/// check`, `aperio api health` and the dashboard present. They used to be
/// public, and a version and a client count are the first two lines of
/// anyone's notes on a server they are sizing up.
///
/// A credential presented here is checked, so the request pays the tunnel
/// handshake's rate-limit price; a bare probe pays nothing, since a probe that
/// runs every few seconds must never be the thing that empties the bucket. A
/// credential that does not verify gets the anonymous body rather than a
/// refusal: this is a liveness endpoint, and a monitor with a stale token
/// should still see the server is up.
#[utoipa::path(get, path = "/aperio/health", tag = "public",
  description = "Liveness probe. Without a credential: status and the default UI language. With the master token, a tunnel token, a dashboard session or an admin key: also the server version, tunnel protocol version, connected client count, uptime and request total.",
  responses((status = 200, description = "Server is up", body = serde_json::Value)))]
pub(crate) async fn health_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let mut health_info = HashMap::new();
  health_info.insert("status", serde_json::json!("healthy"));
  health_info.insert("ui_language", serde_json::json!(state.config().ui_language));
  // Whose panel this hostname is, for the login page to say so. An
  // organization's handle and display name are what its own people already
  // know; nothing about the server travels here.
  let panel_host = crate::server::panel::request_host(&headers);
  match state.panel_org(panel_host.as_deref()).await {
    Some(None) => {
      health_info.insert(
        "panel",
        serde_json::json!({ "org": crate::store::orgs::MASTER_ID }),
      );
    }
    Some(Some(org_id)) => {
      let named =
        state.org_store.lock().await.find(&org_id).map(
          |o| serde_json::json!({ "org": o.id, "name": o.name, "custom_name": o.custom_name }),
        );
      if let Some(named) = named {
        health_info.insert("panel", named);
      }
    }
    None => {}
  }

  let presented = crate::auth::extract_token(&headers).is_some() || headers.contains_key("cookie");
  if !presented {
    return (StatusCode::OK, Json(health_info)).into_response();
  }
  let cfg = state.config();
  let client_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    cfg.trust_proxy,
    cfg.real_ip_header.as_deref(),
    &cfg.trusted_proxies,
  );
  if !state
    .check_rate_limit_cost(client_ip, crate::state::RateCost::Cheap)
    .await
  {
    return StatusCode::TOO_MANY_REQUESTS.into_response();
  }
  let disclose = crate::auth::dashboard_role(&state, &headers)
    .await
    .is_some()
    || crate::auth::authorize_tunnel_token(&state, &headers, client_ip)
      .await
      .is_some();
  if !disclose {
    return (StatusCode::OK, Json(health_info)).into_response();
  }

  let clients_count = state.clients.read().await.len();
  let stats = state.stats.lock().await;
  let uptime = state.server_start_time.elapsed().as_secs();
  health_info.insert("version", serde_json::json!(env!("CARGO_PKG_VERSION")));
  health_info.insert("protocol", serde_json::json!(PROTOCOL_VERSION));
  health_info.insert("connected_clients", serde_json::json!(clients_count));
  health_info.insert("uptime_seconds", serde_json::json!(uptime));
  health_info.insert("total_requests", serde_json::json!(stats.total_requests));
  (StatusCode::OK, Json(health_info)).into_response()
}

/// Liveness probe for a container runtime: no body, no locks.
///
/// `/aperio/health` builds a JSON document and takes two locks to do it, which
/// is more than a `HEALTHCHECK` running every five seconds needs, and under
/// contention a probe that waits on a lock reports the process as dead when it
/// is merely busy, which is the worst possible time to restart it. This
/// answers the only question liveness actually asks: is the process still
/// serving HTTP.
#[utoipa::path(get, path = "/aperio/healthz", tag = "public",
  description = "Liveness probe: 200 with an empty body, no locks taken. For container HEALTHCHECKs and Kubernetes livenessProbe.",
  responses((status = 200, description = "The process is serving")))]
pub(crate) async fn healthz_handler() -> impl IntoResponse {
  StatusCode::OK
}

/// Readiness probe: should traffic be sent here right now.
///
/// Distinct from liveness in exactly the case that matters: from the moment a
/// shutdown signal arrives this answers 503 while the process is still
/// serving, which is what tells a load balancer to take the instance out of
/// rotation. That is the other half of `shutdown_drain`, which then gives the
/// requests already in flight time to finish. Restarting on this would be
/// wrong, which is why it is not the liveness endpoint.
#[utoipa::path(get, path = "/aperio/readyz", tag = "public",
  description = "Readiness probe: 200 while the server should receive traffic, 503 once it is shutting down. For Kubernetes readinessProbe.",
  responses((status = 200, description = "Ready for traffic"),
            (status = 503, description = "Shutting down")))]
pub(crate) async fn readyz_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
  if *state.shutdown.borrow() {
    return (StatusCode::SERVICE_UNAVAILABLE, "shutting down");
  }
  (StatusCode::OK, "ready")
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
