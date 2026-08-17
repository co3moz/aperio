//! The whole HTTP surface in one table: the dashboard, the API, the public
//! endpoints and the proxy fallback everything else falls through to.
//!
//! Order matters here in a way it does not elsewhere, so the table is one
//! function and reads top to bottom.

use crate::api::clients::{
  client_enabled_handler, client_override_handler, live_stream_handler, logs_handler,
  stats_handler, stats_history_handler, uptime_handler,
};
use crate::api::inspector::{request_detail_handler, request_replay_handler};
use crate::api::maintenance::{maintenance_list_handler, maintenance_set_handler};
use crate::api::metrics::metrics_handler;
use crate::api::settings::{settings_get_handler, settings_put_handler};
use crate::api::tokens::{
  tokens_create_handler, tokens_list_handler, tokens_refresh_handler, tokens_revoke_handler,
  tokens_rotate_handler, tokens_update_handler,
};
use crate::api::tunnels::{
  tunnels_create_handler, tunnels_declared_handler, tunnels_delete_handler,
};
use crate::api::webhooks::{
  audit_handler, audit_verify_handler, webhook_deliveries_handler, webhook_redeliver_handler,
  webhooks_create_handler, webhooks_delete_handler, webhooks_list_handler,
};
use crate::api::{dashboard_asset_handler, dashboard_handler, health_handler};
use crate::auth::{
  auth_login_handler, auth_logout_handler, auth_page_handler, auth_session_handler,
  oidc_callback_handler, oidc_login_handler, safe_redirect_path,
};
use crate::proxy::proxy_handler;
use crate::share::share_create_handler;
use crate::state::AppState;
use crate::tunnel::tcp::{
  tcp_ws_handler, tunnels_discovery_handler, tunnels_list_handler, udp_ws_handler,
};
use crate::tunnel::ws::ws_handler;
use crate::*;
use axum::{
  Router,
  body::Body,
  http::StatusCode,
  response::Response,
  routing::{any, get},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

/// Assembles the whole HTTP surface: the proxy fallback, the dashboard and
/// its API (when enabled), the auth and tunnel endpoints, the admin 404
/// fence, and the outermost catch-panic layer. Pure assembly: nothing here
/// spawns, binds, or reads the environment, so a test can drive the result
/// with `tower::ServiceExt::oneshot`.
pub(crate) fn build_router(state: Arc<AppState>, metrics_enabled: bool) -> Router {
  let mut app = Router::new().fallback(any(proxy_handler));

  let dashboard_enabled = state.dashboard_enabled;
  if dashboard_enabled {
    let mut dash_router = Router::new()
      .route("/", get(dashboard_handler))
      .route("/api/stats", get(stats_handler))
      .route("/api/stats/history", get(stats_history_handler))
      .route("/api/uptime", get(uptime_handler))
      .route("/api/topology", get(crate::api::topology::topology_handler))
      .route("/api/logs", get(logs_handler))
      .route("/api/stream", get(live_stream_handler))
      .route("/api/session", get(auth_session_handler))
      .route(
        "/api/clients/{id}/config",
        get(crate::api::clients::client_config_handler),
      )
      .route(
        "/api/clients/{id}/override",
        axum::routing::post(client_override_handler),
      )
      .route(
        "/api/clients/{id}/enabled",
        axum::routing::post(client_enabled_handler),
      )
      .route(
        "/api/tokens",
        get(tokens_list_handler).post(tokens_create_handler),
      )
      .route(
        "/api/tokens/{id}",
        axum::routing::put(tokens_update_handler).delete(tokens_revoke_handler),
      )
      .route(
        "/api/tokens/{id}/rotate",
        axum::routing::post(tokens_rotate_handler),
      )
      .route(
        "/api/purge",
        axum::routing::post(crate::api::purge::purge_handler),
      )
      .route(
        "/api/slow-endpoints",
        get(crate::api::metrics::slow_endpoints_handler),
      )
      .route(
        "/api/bandwidth",
        get(crate::api::metrics::bandwidth_handler),
      )
      .route(
        "/api/route-trends",
        get(crate::api::metrics::route_trends_handler),
      )
      .route("/api/activity", get(crate::api::metrics::activity_handler))
      .route(
        "/api/cache/purge",
        axum::routing::post(crate::api::purge::cache_purge_handler),
      )
      .route(
        "/api/publish",
        axum::routing::post(crate::api::publish::publish_handler),
      )
      .route(
        "/api/subscribers",
        get(crate::api::publish::subscribers_handler),
      )
      .route(
        "/api/cache/stats",
        get(crate::api::purge::cache_stats_handler),
      )
      .route(
        "/api/inbox",
        get(crate::api::inbox::inbox_list_handler).delete(crate::api::inbox::inbox_clear_handler),
      )
      .route(
        "/api/inbox/{id}",
        get(crate::api::inbox::inbox_detail_handler)
          .delete(crate::api::inbox::inbox_delete_handler),
      )
      .route(
        "/api/inbox/{id}/refire",
        axum::routing::post(crate::api::inbox::inbox_refire_handler),
      )
      .route("/api/requests/{id}", get(request_detail_handler))
      .route(
        "/api/requests/{id}/replay",
        axum::routing::post(request_replay_handler),
      )
      .route("/api/audit", get(audit_handler))
      .route(
        "/api/export/audit.csv",
        get(crate::api::webhooks::audit_csv_handler),
      )
      .route("/api/audit/verify", get(audit_verify_handler))
      .route(
        "/api/self-health",
        get(crate::api::observe::self_health_handler),
      )
      .route(
        "/api/export/traffic.csv",
        get(crate::api::observe::traffic_csv_handler),
      )
      .route(
        "/api/admin-keys",
        get(crate::api::admin_keys::admin_keys_list_handler)
          .post(crate::api::admin_keys::admin_keys_create_handler),
      )
      .route(
        "/api/admin-keys/{id}",
        axum::routing::delete(crate::api::admin_keys::admin_keys_revoke_handler),
      )
      .route(
        "/api/maintenance",
        get(maintenance_list_handler).post(maintenance_set_handler),
      )
      .route("/api/explain", get(crate::api::explain::explain_handler))
      .route("/api/share", axum::routing::post(share_create_handler))
      .route(
        "/api/settings",
        get(settings_get_handler).put(settings_put_handler),
      )
      .route("/api/export", get(crate::api::export::export_handler))
      .route(
        "/api/import",
        axum::routing::post(crate::api::export::import_handler),
      )
      .route(
        "/api/webhooks",
        get(webhooks_list_handler).post(webhooks_create_handler),
      )
      .route(
        "/api/webhooks/{id}",
        axum::routing::delete(webhooks_delete_handler),
      )
      .route("/api/webhooks/deliveries", get(webhook_deliveries_handler))
      .route(
        "/api/webhooks/deliveries/{id}/redeliver",
        axum::routing::post(webhook_redeliver_handler),
      )
      .route(
        "/api/webhooks/{id}/test",
        axum::routing::post(crate::api::webhooks::webhook_test_handler),
      )
      .route(
        "/api/openapi.json",
        get(crate::api::openapi::openapi_handler),
      )
      .route(
        "/api/config/schema/{kind}",
        get(crate::api::config_schema::config_schema_handler),
      )
      .route(
        "/api/stage-stats",
        get(crate::api::metrics::stage_stats_handler),
      )
      .route(
        "/api/orgs",
        get(crate::api::orgs::orgs_list_handler).post(crate::api::orgs::orgs_create_handler),
      )
      .route(
        "/api/orgs/{id}",
        axum::routing::delete(crate::api::orgs::orgs_delete_handler),
      )
      .route(
        "/api/orgs/{id}/quota",
        axum::routing::put(crate::api::orgs::orgs_quota_handler),
      )
      .route(
        "/api/scaling",
        get(crate::api::scaling::scaling_list_handler),
      )
      .route(
        "/api/scaling/{id}",
        axum::routing::delete(crate::api::scaling::scaling_delete_handler),
      )
      .route(
        "/api/orgs/{id}/custom-name",
        axum::routing::put(crate::api::orgs::orgs_custom_name_handler),
      )
      .route(
        "/api/orgs/{id}/hostnames",
        axum::routing::put(crate::api::orgs::orgs_hostnames_handler),
      )
      .route(
        "/api/orgs/{id}/usage",
        get(crate::api::orgs::orgs_usage_handler),
      )
      .route(
        "/api/orgs/{id}/oidc",
        axum::routing::put(crate::api::orgs::orgs_oidc_handler),
      )
      .route(
        "/api/orgs/select",
        axum::routing::post(crate::api::orgs::orgs_select_handler),
      )
      .route(
        "/api/sessions",
        get(crate::api::users::sessions_list_handler)
          .delete(crate::api::users::sessions_clear_handler),
      )
      .route(
        "/api/sessions/{id}",
        axum::routing::delete(crate::api::users::session_revoke_handler),
      )
      .route(
        "/api/users",
        get(crate::api::users::users_list_handler).post(crate::api::users::users_create_handler),
      )
      .route(
        "/api/users/{id}/totp",
        axum::routing::delete(crate::api::users::totp_admin_reset_handler),
      )
      .route(
        "/api/me/totp/setup",
        axum::routing::post(crate::api::users::totp_setup_handler),
      )
      .route(
        "/api/me/totp/enable",
        axum::routing::post(crate::api::users::totp_enable_handler),
      )
      .route(
        "/api/me/totp",
        axum::routing::delete(crate::api::users::totp_disable_handler),
      )
      .route(
        "/api/me/passkeys",
        get(crate::webauthn::passkeys_list_handler),
      )
      .route(
        "/api/me/passkeys/register/start",
        axum::routing::post(crate::webauthn::passkey_register_start_handler),
      )
      .route(
        "/api/me/passkeys/register/finish",
        axum::routing::post(crate::webauthn::passkey_register_finish_handler),
      )
      .route(
        "/api/me/passkeys/{id}",
        axum::routing::delete(crate::webauthn::passkey_delete_handler),
      )
      .route(
        "/api/users/{id}",
        axum::routing::put(crate::api::users::users_update_handler)
          .delete(crate::api::users::users_delete_handler),
      );

    let state_clone = state.clone();
    dash_router = dash_router.layer(axum::middleware::from_fn(
      move |req: axum::extract::Request, next: axum::middleware::Next| {
        let state = state_clone.clone();
        async move {
          // Check for valid session cookie, then enforce the role floor of
          // the route: user management and settings are admin-only, any
          // other mutation needs operator, reads are open to viewers.
          if let Some(role) = crate::auth::dashboard_role(&state, req.headers()).await {
            let required = required_role(req.uri().path(), req.method());
            if role >= required {
              return next.run(req).await;
            }
            return Response::builder()
              .status(StatusCode::FORBIDDEN)
              .body(Body::from(format!(
                "This action requires the {} role (you are {})",
                required.as_str(),
                role.as_str()
              )))
              .unwrap();
          }
          // Redirect to login page, preserving the original path. The nested
          // router sees the path with the /aperio prefix stripped ("/" for
          // the dashboard itself), so the prefix must be re-added or the
          // post-login redirect lands on the proxied site instead.
          let nested_path = req.uri().path();
          let full_path = if nested_path == "/" {
            "/aperio".to_string()
          } else {
            format!("/aperio{}", nested_path)
          };
          let redirect_url = format!("/aperio/auth?redirect={}", safe_redirect_path(&full_path));
          Response::builder()
            .status(StatusCode::FOUND)
            .header("Location", redirect_url)
            .body(Body::empty())
            .unwrap()
        }
      },
    ));

    // Network-level fence for the admin surface (APERIO_ADMIN_ALLOWED_IPS).
    // Added after the session layer so it runs first (outermost): a blocked
    // source IP is rejected with 403 before any auth check. Registered before
    // the assets route below so public login assets stay reachable, and it
    // wraps only the dashboard + /api routes, the login page, auth endpoints,
    // health and OIDC routes live on the root router and are never fenced, so
    // APERIO_SERVER_AUTH-protected proxied sites keep working from any address.
    let ip_state = state.clone();
    dash_router = dash_router.layer(axum::middleware::from_fn(
      move |req: axum::extract::Request, next: axum::middleware::Next| {
        let state = ip_state.clone();
        async move {
          let cfg = state.config();
          if !cfg.admin_allowed_ips.is_empty() {
            let peer = req
              .extensions()
              .get::<axum::extract::ConnectInfo<SocketAddr>>()
              .map(|ci| ci.0.ip());
            let allowed = peer.is_some_and(|peer_ip| {
              let client_ip = crate::routing::extract_client_ip(
                req.headers(),
                peer_ip,
                cfg.trust_proxy,
                cfg.real_ip_header.as_deref(),
                &cfg.trusted_proxies,
              );
              crate::routing::ip_in_ranges(client_ip, &cfg.admin_allowed_ips)
            });
            if !allowed {
              return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from(
                  "Admin surface is restricted to allowed source IPs",
                ))
                .unwrap();
            }
          }
          next.run(req).await
        }
      },
    ));

    // Static assets are registered after the session layer on purpose: they
    // are public, because the login page needs them before any session exists.
    dash_router = dash_router.route("/assets/{*path}", get(dashboard_asset_handler));

    app = app.nest("/aperio", dash_router);
  } else {
    // Even with the dashboard disabled the login page (used by
    // APERIO_SERVER_AUTH-protected proxied sites) still needs its assets.
    app = app.nest(
      "/aperio",
      Router::new().route("/assets/{*path}", get(dashboard_asset_handler)),
    );
  }

  // Anything else under `/aperio/` is the admin surface's namespace, and a
  // path that matches nothing in it is a mistake, not traffic. Without this it
  // reaches the proxy: a typo in an API path, or a probe for an endpoint that
  // does not exist, would be served whatever a tunnel client happens to
  // answer, under the URL the docs call the admin surface. The routes
  // registered on the router directly (`/aperio/health`, `/aperio/ws`, the
  // auth endpoints) are literals and still win over this.
  app = app.route(
    "/aperio/{*rest}",
    any(|| async { (StatusCode::NOT_FOUND, "404 Not Found\n") }),
  );

  // `/aperio/` is not the dashboard: nesting matches `/aperio` and
  // `/aperio/<something>`, so the trailing slash falls through to the proxy
  // and a visitor gets whatever a tunnel client answers, a 504, or worse, a
  // stranger's site. It is the same address to everyone typing it, so it
  // redirects to the one that works, query string and all.
  app = app.route(
    "/aperio/",
    any(|uri: axum::http::Uri| async move {
      let target = match uri.query() {
        Some(q) if !q.is_empty() => format!("/aperio?{q}"),
        _ => "/aperio".to_string(),
      };
      axum::response::Redirect::permanent(&target)
    }),
  );

  // Health endpoint is intentionally registered outside the dashboard auth
  // middleware so that external load balancers / monitoring tools can probe
  // server liveness without dashboard credentials.
  app = app.route("/aperio/health", get(health_handler));
  // Split probes for container runtimes: liveness answers "is the process
  // serving" with no body and no locks, readiness answers "should traffic
  // come here", which stops being true the moment a shutdown signal lands.
  app = app.route("/aperio/healthz", get(crate::api::healthz_handler));
  // The OTel bridge's receiving end. Outside the dashboard auth middleware
  // like the tunnel endpoints, because it authenticates with a tunnel token
  // rather than a dashboard session.
  app = app.route(
    "/aperio/otlp/v1/{signal}",
    axum::routing::post(crate::api::otlp::otlp_handler),
  );
  app = app.route("/aperio/readyz", get(crate::api::readyz_handler));
  app = app.route(
    "/aperio/auth",
    get(auth_page_handler).post(auth_login_handler),
  );
  // Logout clears the session server-side and expires the cookie. Registered
  // outside the dashboard session middleware so it works with any cookie state.
  app = app.route(
    "/aperio/auth/logout",
    axum::routing::post(auth_logout_handler),
  );
  // Passkey (WebAuthn) sign-in: challenge + finish live next to the login
  // form, outside the session middleware (they create the session).
  app = app.route(
    "/aperio/auth/passkey",
    get(crate::webauthn::passkey_available_handler),
  );
  app = app
    .route(
      "/aperio/auth/passkey/start",
      axum::routing::post(crate::webauthn::passkey_login_start_handler),
    )
    .route(
      "/aperio/auth/passkey/discoverable/start",
      axum::routing::post(crate::webauthn::passkey_discoverable_start_handler),
    )
    .route(
      "/aperio/auth/passkey/discoverable/finish",
      axum::routing::post(crate::webauthn::passkey_discoverable_finish_handler),
    );
  app = app.route(
    "/aperio/auth/passkey/finish",
    axum::routing::post(crate::webauthn::passkey_login_finish_handler),
  );
  // Programmatic tunnel provisioning. Registered outside the dashboard
  // session middleware on purpose: it authenticates with the master token in
  // a header (or a session cookie), so CI jobs can mint ephemeral tunnels
  // even when the dashboard is disabled.
  // Token self-refresh. Also outside the session middleware: it authenticates
  // with the token secret itself, so a CI job or client can keep its
  // short-lived token alive without dashboard credentials.
  app = app.route(
    "/aperio/api/tokens/refresh",
    axum::routing::post(tokens_refresh_handler),
  );
  app = app.route(
    "/aperio/api/tunnels",
    get(tunnels_declared_handler).post(tunnels_create_handler),
  );
  app = app.route(
    "/aperio/api/tunnels/{id}",
    axum::routing::delete(tunnels_delete_handler),
  );
  app = app.route("/aperio/ws", get(ws_handler));
  app = app.route("/aperio/tcp", get(tcp_ws_handler));
  app = app.route("/aperio/udp", get(udp_ws_handler));
  // Tunnel discovery for --bind-tunnels consumers: same token the client
  // connected with (or master), explicit client id, never a listing.
  app = app.route("/aperio/tunnels", get(tunnels_discovery_handler));
  app = app.route("/aperio/tunnels/{client_id}", get(tunnels_list_handler));
  app = app.route("/aperio/oidc/login", get(oidc_login_handler));
  app = app.route("/aperio/oidc/callback", get(oidc_callback_handler));

  // Prometheus metrics endpoint, registered outside the dashboard session
  // middleware. Access control is handled by APERIO_METRICS_TOKEN if set.
  if metrics_enabled {
    app = app.route("/aperio/metrics", get(metrics_handler));
    info!("Prometheus metrics endpoint enabled at /aperio/metrics");
  }

  // Edge integration. Registered outside the dashboard session middleware on
  // purpose: the edge proxy holds only APERIO_EDGE_TOKEN, and the endpoints
  // must work with the dashboard disabled.
  //
  // The routes exist whether or not the feature is on, and the handlers answer
  // 404 when it is off. Registering them conditionally instead left the paths
  // unmatched, so they fell through to the visitor proxy and came back as a
  // 504 "no client connected": a tunnel error for what is a configuration
  // question, and never the documented 404. Reserving them also keeps a
  // request for them from ever being forwarded to somebody's backend, and lets
  // the token be enabled by config reload without a restart.
  app = app
    .route(
      "/aperio/api/edge/ask",
      get(crate::api::edge::edge_ask_handler),
    )
    .route(
      "/aperio/api/edge/traefik",
      get(crate::api::edge::edge_traefik_handler),
    );
  if state.config().edge_token.is_some() {
    info!(
      "Edge integration enabled at /aperio/api/edge/ask and /aperio/api/edge/traefik{}",
      if state.config().edge_service_url.is_some() {
        ""
      } else {
        " (set APERIO_EDGE_SERVICE_URL for the Traefik document)"
      }
    );
  }

  // The source-IP deny list, outside every route and every other check: a
  // blocked address must not reach a handler, spend a rate-limit bucket,
  // occupy a request slot or open a tunnel. It covers proxied traffic, the
  // dashboard and the tunnel endpoints alike, which is what an operator
  // blocking an address means by it.
  let deny_state = state.clone();
  let app = app.layer(axum::middleware::from_fn(
    move |req: axum::extract::Request, next: axum::middleware::Next| {
      let state = deny_state.clone();
      async move {
        let cfg = state.config();
        if !cfg.denied_ips.is_empty()
          && let Some(peer) = req
            .extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
        {
          // The same address resolution the rest of the server uses, so a
          // deployment behind a trusted proxy blocks the visitor rather than
          // the proxy, and one without trust_proxy cannot be fooled by a
          // forged header into unblocking anybody.
          let client_ip = crate::routing::extract_client_ip(
            req.headers(),
            peer,
            cfg.trust_proxy,
            cfg.real_ip_header.as_deref(),
            &cfg.trusted_proxies,
          );
          if cfg.denied_ips.blocks(client_ip) {
            return Response::builder()
              .status(StatusCode::FORBIDDEN)
              .body(Body::from("Forbidden"))
              .unwrap();
          }
        }
        next.run(req).await
      }
    },
  ));

  // Outermost layer: a panic in any handler (proxy or dashboard) becomes a
  // clean 500 for that one request instead of abruptly dropping the
  // connection. The panic is still logged by the global hook (see
  // `install_panic_logger`); every other in-flight request and the process
  // are unaffected.
  app
    .with_state(state)
    .layer(tower_http::catch_panic::CatchPanicLayer::new())
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
