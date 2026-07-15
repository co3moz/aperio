//! OpenAPI 3.1 specification for the admin/auth API, generated from the
//! `#[utoipa::path]` annotations on the handlers. Served (behind the
//! dashboard session, like every admin endpoint) at
//! `GET /aperio/api/openapi.json` — point Swagger UI, Bruno, or a codegen
//! at it.

use axum::Json;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
  info(
    title = "Aperio Admin API",
    description = "Administrative API of the Aperio tunnel server: statistics, traffic \
      inspection, dynamic tokens, ephemeral tunnels, webhooks, maintenance mode, share \
      links, and server settings.\n\nAuthentication: dashboard endpoints require an \
      `aperio_session` cookie (log in at `/aperio/auth`). `POST /aperio/api/tunnels` also \
      accepts the master token as a Bearer header; `POST /aperio/api/tokens/refresh` \
      authenticates with the dynamic token secret itself.",
    version = env!("CARGO_PKG_VERSION"),
    license(name = "MIT")
  ),
  paths(
    crate::api::health_handler,
    crate::api::metrics::metrics_handler,
    crate::api::metrics::stage_stats_handler,
    crate::api::clients::stats_handler,
    crate::api::clients::stats_history_handler,
    crate::api::clients::uptime_handler,
    crate::api::users::totp_setup_handler,
    crate::api::users::sessions_list_handler,
    crate::api::users::session_revoke_handler,
    crate::api::users::sessions_clear_handler,
    crate::api::users::totp_enable_handler,
    crate::api::users::totp_disable_handler,
    crate::api::users::totp_admin_reset_handler,
    crate::webauthn::passkey_available_handler,
    crate::webauthn::passkey_discoverable_start_handler,
    crate::webauthn::passkey_discoverable_finish_handler,
    crate::webauthn::passkey_login_start_handler,
    crate::webauthn::passkey_login_finish_handler,
    crate::webauthn::passkeys_list_handler,
    crate::webauthn::passkey_register_start_handler,
    crate::webauthn::passkey_register_finish_handler,
    crate::webauthn::passkey_delete_handler,
    crate::api::clients::logs_handler,
    crate::api::clients::live_stream_handler,
    crate::api::clients::client_override_handler,
    crate::api::clients::client_enabled_handler,
    crate::api::inspector::request_detail_handler,
    crate::api::inspector::request_replay_handler,
    crate::api::maintenance::maintenance_list_handler,
    crate::api::maintenance::maintenance_set_handler,
    crate::api::settings::settings_get_handler,
    crate::api::settings::settings_put_handler,
    crate::api::export::export_handler,
    crate::api::export::import_handler,
    crate::api::tokens::tokens_list_handler,
    crate::api::tokens::tokens_create_handler,
    crate::api::tokens::tokens_update_handler,
    crate::api::tokens::tokens_revoke_handler,
    crate::api::tokens::tokens_refresh_handler,
    crate::api::tunnels::tunnels_create_handler,
    crate::api::tunnels::tunnels_delete_handler,
    crate::api::webhooks::audit_handler,
    crate::api::webhooks::webhooks_list_handler,
    crate::api::webhooks::webhooks_create_handler,
    crate::api::webhooks::webhook_deliveries_handler,
    crate::api::webhooks::webhook_redeliver_handler,
    crate::api::webhooks::webhooks_delete_handler,
    crate::share::share_create_handler,
    crate::auth::auth_login_handler,
    crate::auth::auth_logout_handler,
    crate::auth::auth_session_handler,
    crate::api::users::users_list_handler,
    crate::api::users::users_create_handler,
    crate::api::users::users_update_handler,
    crate::api::users::users_delete_handler,
  ),
  tags(
    (name = "public", description = "Unauthenticated (or token-gated) operational endpoints"),
    (name = "auth", description = "Login, logout, and session lifetime"),
    (name = "dashboard", description = "Statistics, traffic, clients, inspector, settings, maintenance, share links"),
    (name = "tokens", description = "Dynamic API token lifecycle"),
    (name = "tunnels", description = "Programmatic ephemeral tunnel provisioning"),
    (name = "webhooks", description = "Webhook definitions and the audit trail"),
    (name = "users", description = "Dashboard users and roles (admin only)")
  )
)]
pub(crate) struct ApiDoc;

/// Serves the generated OpenAPI document (`GET /aperio/api/openapi.json`).
pub(crate) async fn openapi_handler() -> Json<utoipa::openapi::OpenApi> {
  Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_openapi_document_is_complete_and_serializable() {
    let doc = ApiDoc::openapi();
    // Every annotated route is present (paths with multiple methods share
    // one entry, e.g. /aperio/api/tokens carries GET and POST).
    let paths: Vec<&String> = doc.paths.paths.keys().collect();
    for expected in [
      "/aperio/health",
      "/aperio/metrics",
      "/aperio/api/stats",
      "/aperio/api/stats/history",
      "/aperio/api/uptime",
      "/aperio/api/me/totp",
      "/aperio/api/me/totp/setup",
      "/aperio/api/me/totp/enable",
      "/aperio/api/users/{id}/totp",
      "/aperio/auth/passkey",
      "/aperio/auth/passkey/start",
      "/aperio/auth/passkey/finish",
      "/aperio/api/me/passkeys",
      "/aperio/api/me/passkeys/{id}",
      "/aperio/api/me/passkeys/register/start",
      "/aperio/api/me/passkeys/register/finish",
      "/aperio/api/logs",
      "/aperio/api/stream",
      "/aperio/api/session",
      "/aperio/api/clients/{id}/override",
      "/aperio/api/clients/{id}/enabled",
      "/aperio/api/tokens",
      "/aperio/api/tokens/{id}",
      "/aperio/api/tokens/refresh",
      "/aperio/api/tunnels",
      "/aperio/api/tunnels/{id}",
      "/aperio/api/requests/{id}",
      "/aperio/api/requests/{id}/replay",
      "/aperio/api/audit",
      "/aperio/api/maintenance",
      "/aperio/api/share",
      "/aperio/api/settings",
      "/aperio/api/webhooks",
      "/aperio/api/webhooks/{id}",
      "/aperio/auth",
      "/aperio/auth/logout",
      "/aperio/api/users",
      "/aperio/api/users/{id}",
    ] {
      assert!(
        paths.iter().any(|p| p.as_str() == expected),
        "missing path {expected}; got: {paths:?}"
      );
    }
    // The document serializes to valid JSON with schemas included.
    let json = serde_json::to_string(&doc).expect("openapi serializes");
    assert!(json.contains("EnhancedServerStats"));
    assert!(json.contains("TokenCreateRequest"));
  }
}
