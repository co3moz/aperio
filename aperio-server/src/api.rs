use axum::{
  Json,
  extract::State,
  http::StatusCode,
  response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::protocol::PROTOCOL_VERSION;
use crate::state::AppState;

pub(crate) mod clients;
pub(crate) mod inspector;
pub(crate) mod maintenance;
pub(crate) mod metrics;
pub(crate) mod openapi;
pub(crate) mod settings;
pub(crate) mod tokens;
pub(crate) mod tunnels;
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

/// Health check endpoint returning status, active connection counts, and uptime.
#[utoipa::path(get, path = "/aperio/health", tag = "public",
  description = "Liveness probe: server version, tunnel protocol version, and connected client count. No authentication.",
  responses((status = 200, description = "Server is up", body = serde_json::Value)))]
pub(crate) async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
  let clients_count = state.clients.lock().await.len();
  let stats = state.stats.lock().await;
  let uptime = state.server_start_time.elapsed().as_secs();

  let mut health_info = HashMap::new();
  health_info.insert("status", serde_json::json!("healthy"));
  health_info.insert("version", serde_json::json!(env!("CARGO_PKG_VERSION")));
  health_info.insert("protocol", serde_json::json!(PROTOCOL_VERSION));
  health_info.insert("connected_clients", serde_json::json!(clients_count));
  health_info.insert("uptime_seconds", serde_json::json!(uptime));
  health_info.insert("total_requests", serde_json::json!(stats.total_requests));

  (StatusCode::OK, Json(health_info))
}
