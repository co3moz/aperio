//! OpenAPI 3.1 specification for the admin/auth API, generated from the
//! `#[utoipa::path]` annotations on the handlers. Served (behind the
//! dashboard session, like every admin endpoint) at
//! `GET /aperio/api/openapi.json`, point Swagger UI, Bruno, or a codegen
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
    crate::api::healthz_handler,
    crate::api::readyz_handler,
    crate::api::metrics::metrics_handler,
    crate::api::config_schema::config_schema_handler,
    crate::api::edge::edge_ask_handler,
    crate::api::edge::edge_traefik_handler,
    crate::api::metrics::stage_stats_handler,
    crate::api::metrics::slow_endpoints_handler,
    crate::api::metrics::bandwidth_handler,
    crate::api::metrics::route_trends_handler,
    crate::api::metrics::activity_handler,
    crate::api::clients::stats_handler,
    crate::api::clients::stats_history_handler,
    crate::api::clients::uptime_handler,
    crate::api::topology::topology_handler,
    crate::api::users::totp_setup_handler,
    crate::api::users::sessions_list_handler,
    crate::api::orgs::orgs_list_handler,
    crate::api::orgs::orgs_create_handler,
    crate::api::orgs::orgs_delete_handler,
    crate::api::orgs::orgs_quota_handler,
    crate::api::orgs::orgs_custom_name_handler,
    crate::api::orgs::orgs_hostnames_handler,
    crate::api::orgs::orgs_usage_handler,
    crate::api::orgs::orgs_oidc_handler,
    crate::api::orgs::orgs_select_handler,
    crate::api::admin_keys::admin_keys_list_handler,
    crate::api::admin_keys::admin_keys_create_handler,
    crate::api::admin_keys::admin_keys_revoke_handler,
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
    crate::api::clients::client_config_handler,
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
    crate::api::tokens::tokens_rotate_handler,
    crate::api::scaling::scaling_list_handler,
    crate::api::scaling::scaling_delete_handler,
    crate::api::purge::purge_handler,
    crate::api::publish::publish_handler,
    crate::api::publish::subscribers_handler,
    crate::api::purge::cache_purge_handler,
    crate::api::purge::cache_stats_handler,
    crate::api::observe::self_health_handler,
    crate::api::observe::traffic_csv_handler,
    crate::api::inbox::inbox_list_handler,
    crate::api::inbox::inbox_clear_handler,
    crate::api::inbox::inbox_detail_handler,
    crate::api::inbox::inbox_delete_handler,
    crate::api::inbox::inbox_refire_handler,
    crate::api::tunnels::tunnels_declared_handler,
    crate::api::tunnels::tunnels_create_handler,
    crate::api::tunnels::tunnels_delete_handler,
    crate::api::webhooks::audit_handler,
    crate::api::webhooks::audit_verify_handler,
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
#[path = "openapi_tests.rs"]
mod tests;
