//! Dump export/import (`GET /aperio/api/export`, `POST /aperio/api/import`).
//!
//! The dump is a single JSON document with everything needed to rebuild a
//! server's persisted state on another instance or after an upgrade gone
//! wrong: dynamic tokens (hashes only — plaintext secrets are never stored),
//! webhooks, dashboard users (password hashes, TOTP secrets, passkeys) and
//! the dashboard settings overrides. Being a *logical* dump, it survives
//! schema changes that a raw `aperio.db` copy would not: unknown fields are
//! dropped, missing ones take their defaults.
//!
//! Not included: statistics/uptime history, audit log, sessions (everyone
//! signs in again) — the dump is a config failsafe, not a full backup.
//! Admin-only in both directions; an import *replaces* the stores.

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
use crate::settings::SettingsOverrides;
use crate::state::AppState;
use crate::store::orgs::Organization;
use crate::store::tokens::ApiToken;
use crate::store::users::User;
use crate::store::webhooks::Webhook;

/// The dump format version this build writes and accepts.
const FORMAT_VERSION: u32 = 1;

/// Returns the full configuration dump as a downloadable JSON document.
#[utoipa::path(get, path = "/aperio/api/export", tag = "dashboard",
  description = "Downloads a logical dump of tokens, webhooks, users and settings overrides (admin only).",
  responses((status = 200, description = "The dump document", body = serde_json::Value)))]
pub(crate) async fn export_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  let tokens = state.token_store.lock().await.list().to_vec();
  let webhooks = state.webhook_store.lock().await.list().to_vec();
  let users = state.users.lock().await.list().to_vec();
  let organizations = state.org_store.lock().await.list().to_vec();
  let settings_overrides = state.settings_overrides.lock().await.clone();

  state
    .audit(
      "export_created",
      &state.session_actor(&headers).await,
      &actor_ip,
      &format!(
        "tokens={} webhooks={} users={}",
        tokens.len(),
        webhooks.len(),
        users.len()
      ),
    )
    .await;

  let dump = serde_json::json!({
    "format_version": FORMAT_VERSION,
    "exported_at": chrono::Local::now().to_rfc3339(),
    "server_version": env!("CARGO_PKG_VERSION"),
    "tokens": tokens,
    "webhooks": webhooks,
    "users": users,
    "organizations": organizations,
    "settings_overrides": settings_overrides,
  });
  (
    StatusCode::OK,
    [
      ("content-type", "application/json".to_string()),
      (
        "content-disposition",
        format!(
          "attachment; filename=\"aperio-export-{}.json\"",
          chrono::Local::now().format("%Y%m%d-%H%M%S")
        ),
      ),
    ],
    serde_json::to_string_pretty(&dump).unwrap_or_default(),
  )
    .into_response()
}

/// A dump document accepted by the import endpoint. Sections are optional:
/// a missing section leaves that store untouched.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ImportDump {
  format_version: u32,
  tokens: Option<Vec<ApiToken>>,
  webhooks: Option<Vec<Webhook>>,
  /// Dashboard user records; the full stored shape (hashes, TOTP, passkeys).
  #[schema(value_type = Option<Vec<serde_json::Value>>)]
  users: Option<Vec<User>>,
  settings_overrides: Option<SettingsOverrides>,
  organizations: Option<Vec<Organization>>,
}

/// Applies a dump: each present section *replaces* the corresponding store.
#[utoipa::path(post, path = "/aperio/api/import", tag = "dashboard",
  description = "Applies a dump created by /aperio/api/export; present sections replace the stores (admin only).",
  request_body = ImportDump,
  responses((status = 200, description = "Import applied", body = serde_json::Value), (status = 400, description = "Invalid dump")))]
pub(crate) async fn import_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(dump): Json<ImportDump>,
) -> Response {
  if dump.format_version != FORMAT_VERSION {
    return (
      StatusCode::BAD_REQUEST,
      format!(
        "Unsupported format_version {} (this server reads version {})",
        dump.format_version, FORMAT_VERSION
      ),
    )
      .into_response();
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  // Settings first: they can fail validation, and a rejected import should
  // change nothing at all.
  if let Some(overrides) = dump.settings_overrides
    && let Err(msg) = super::settings::apply_overrides_validated(&state, overrides).await
  {
    return (
      StatusCode::BAD_REQUEST,
      format!("settings_overrides rejected: {}", msg),
    )
      .into_response();
  }

  let mut counts = serde_json::Map::new();
  if let Some(tokens) = dump.tokens {
    let n = state.token_store.lock().await.import(tokens);
    counts.insert("tokens".into(), n.into());
  }
  if let Some(webhooks) = dump.webhooks {
    let n = state.webhook_store.lock().await.import(webhooks);
    counts.insert("webhooks".into(), n.into());
  }
  if let Some(users) = dump.users {
    let n = state.users.lock().await.import(users);
    counts.insert("users".into(), n.into());
  }
  if let Some(organizations) = dump.organizations {
    let n = state.org_store.lock().await.import(organizations);
    counts.insert("organizations".into(), n.into());
  }

  let summary = counts
    .iter()
    .map(|(k, v)| format!("{}={}", k, v))
    .collect::<Vec<_>>()
    .join(" ");
  info!("Dump imported ({})", summary);
  state
    .audit(
      "import_applied",
      &state.session_actor(&headers).await,
      &actor_ip,
      &summary,
    )
    .await;
  state
    .emit_event("import_applied", serde_json::Value::Object(counts.clone()))
    .await;

  (
    StatusCode::OK,
    Json(serde_json::json!({"imported": counts})),
  )
    .into_response()
}
