//! Tests for the dashboard server-settings API: the settings GET/PUT
//! handlers, their master-admin auth guards, every validation error path in
//! `apply_overrides_validated`, the environment report, and the live
//! side-effects driven by `swap_config`.

use super::*;
use crate::test_support::*;

use crate::settings::SettingsOverrides;
use crate::store::users::Role;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};

// ---------------------------------------------------------------------------
// GET /aperio/api/settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_requires_a_session() {
  let state = Arc::new(test_state());
  let resp = settings_get_handler(State(state), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_forbidden_for_non_master() {
  let state = Arc::new(test_state());
  // A viewer has a valid session but is not the master super-admin.
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = settings_get_handler(State(state), cookie_headers(&token)).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_returns_effective_defaults_overrides_and_environment() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = settings_get_handler(State(state), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert!(body["effective"].is_object());
  assert!(body["defaults"].is_object());
  assert!(body["overrides"].is_object());
  assert!(body["environment"].is_object());
  // Effective values reflect the test config.
  assert_eq!(body["effective"]["ui_language"], "en");
  assert_eq!(body["effective"]["max_tunnels"], 8);
  // Environment report carries a runtime label and a flags array.
  assert!(body["environment"]["runtime"].is_string());
  assert!(body["environment"]["flags"].is_array());
}

#[tokio::test]
async fn get_environment_report_reflects_configured_flags() {
  // Exercise the "set" branches of environment_report (trusted proxies,
  // real-ip header, metrics token, secure cookies, ignore auth).
  let mut cfg = test_config();
  cfg.trust_proxy = true;
  cfg.trusted_proxies = vec![("10.0.0.0".parse().unwrap(), 8u32)];
  cfg.real_ip_header = Some("x-real-ip".to_string());
  cfg.secure_cookies = true;
  cfg.ignore_client_auth = true;
  cfg.metrics_token = Some("secret".to_string());
  let state = Arc::new(test_state_with(cfg));
  let headers = admin_headers(&state).await;
  let resp = settings_get_handler(State(state), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let flags = body["environment"]["flags"].as_array().unwrap();
  let find = |key: &str| {
    flags
      .iter()
      .find(|f| f["key"] == key)
      .map(|f| f["value"].as_str().unwrap().to_string())
      .unwrap()
  };
  assert_eq!(find("APERIO_TRUST_PROXY"), "on");
  assert_eq!(find("APERIO_SECURE_COOKIES"), "on");
  assert_eq!(find("APERIO_IGNORE_CLIENT_AUTH"), "on");
  assert_eq!(find("APERIO_REAL_IP_HEADER"), "x-real-ip");
  // The metrics token value is hidden; only its presence is reported.
  assert_eq!(find("APERIO_METRICS_TOKEN"), "set (value hidden)");
  // OIDC is not configured in the test state.
  assert_eq!(find("APERIO_OIDC_*"), "not configured");
}

// ---------------------------------------------------------------------------
// PUT /aperio/api/settings — auth guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_requires_a_session() {
  let state = Arc::new(test_state());
  let resp = settings_put_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    Json(SettingsOverrides::default()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn put_forbidden_for_non_master() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = settings_put_handler(
    State(state),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
    Json(SettingsOverrides::default()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// PUT /aperio/api/settings — success + persistence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_applies_and_persists_valid_overrides() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let overrides = SettingsOverrides {
    max_tunnels: Some(16),
    ui_language: Some("de".to_string()),
    lb_strategy: Some("sticky".to_string()),
    failover_mode: Some("retry".to_string()),
    ..Default::default()
  };
  let resp = settings_put_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(overrides),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["effective"]["max_tunnels"], 16);
  assert_eq!(body["effective"]["ui_language"], "de");
  assert_eq!(body["effective"]["lb_strategy"], "sticky");

  // Live config was swapped in.
  assert_eq!(state.config().max_tunnels, 16);
  assert_eq!(state.config().ui_language, "de");
  // Overrides were stored in memory and persisted to disk.
  assert_eq!(state.settings_overrides.lock().await.max_tunnels, Some(16));
  assert!(state.settings_path.exists());
  let persisted = std::fs::read_to_string(&state.settings_path).unwrap();
  assert!(persisted.contains("\"max_tunnels\": 16"));
}

// ---------------------------------------------------------------------------
// PUT /aperio/api/settings — validation error paths (via the handler, so the
// 400 mapping is exercised end to end).
// ---------------------------------------------------------------------------

async fn put_expecting_400(overrides: SettingsOverrides) -> String {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = settings_put_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(overrides),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn put_rejects_invalid_lb_strategy() {
  let msg = put_expecting_400(SettingsOverrides {
    lb_strategy: Some("nonsense".to_string()),
    ..Default::default()
  })
  .await;
  assert!(msg.contains("Invalid lb_strategy"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_invalid_failover_mode() {
  let msg = put_expecting_400(SettingsOverrides {
    failover_mode: Some("nonsense".to_string()),
    ..Default::default()
  })
  .await;
  assert!(msg.contains("Invalid failover_mode"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_auth_credentials_without_colon() {
  let msg = put_expecting_400(SettingsOverrides {
    auth_credentials: Some("nopassword".to_string()),
    ..Default::default()
  })
  .await;
  assert!(msg.contains("user:password"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_oversized_custom_504_page() {
  let msg = put_expecting_400(SettingsOverrides {
    custom_504_page: Some("x".repeat(512 * 1024 + 1)),
    ..Default::default()
  })
  .await;
  assert!(msg.contains("custom_504_page exceeds 512 KB"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_oversized_custom_503_page() {
  let msg = put_expecting_400(SettingsOverrides {
    custom_503_page: Some("x".repeat(512 * 1024 + 1)),
    ..Default::default()
  })
  .await;
  assert!(msg.contains("custom_503_page exceeds 512 KB"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_unsupported_ui_language() {
  let msg = put_expecting_400(SettingsOverrides {
    ui_language: Some("xx".to_string()),
    ..Default::default()
  })
  .await;
  assert!(msg.contains("Unsupported ui_language"), "got: {msg}");
}

#[tokio::test]
async fn put_rejects_zero_cache_max_bytes() {
  let msg = put_expecting_400(SettingsOverrides {
    cache_max_bytes: Some(0),
    ..Default::default()
  })
  .await;
  assert!(
    msg.contains("cache_max_bytes must be positive"),
    "got: {msg}"
  );
}

// ---------------------------------------------------------------------------
// apply_overrides_validated — accepted edge cases and swap_config side effects
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_auth_credentials_are_accepted() {
  // Empty credentials clear visitor auth; the `!creds.is_empty()` guard means
  // this must not be rejected even though it contains no colon.
  let state = Arc::new(test_state());
  let out = apply_overrides_validated(
    &state,
    SettingsOverrides {
      auth_credentials: Some(String::new()),
      ..Default::default()
    },
  )
  .await;
  assert!(out.is_ok());
}

#[tokio::test]
async fn changing_subdomain_suffix_reassigns_connected_clients() {
  // test_config has no random subdomain suffix, so enabling one must trigger
  // reassign_random_hostnames for every connected client.
  let state = Arc::new(test_state());
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));

  let out = apply_overrides_validated(
    &state,
    SettingsOverrides {
      random_subdomain_suffix: Some("example.com".to_string()),
      ..Default::default()
    },
  )
  .await;
  assert!(out.is_ok());
  assert_eq!(
    state.config().random_subdomain_suffix.as_deref(),
    Some("*.example.com")
  );
  // The connected client was handed a fresh random hostname.
  let clients = state.clients.lock().await;
  let c = clients.get("c1").unwrap();
  assert!(c.random_hostname.is_some());
  assert!(
    c.assigned_hostnames
      .iter()
      .any(|h| Some(h) == c.random_hostname.as_ref())
  );
}

#[tokio::test]
async fn enabling_compression_offers_it_to_connected_clients() {
  // old tunnel_compression=false → new=true, with a connected client, drives
  // offer_compression_to_connected.
  let state = Arc::new(test_state());
  state
    .clients
    .lock()
    .await
    .insert("c1".to_string(), mock_client(None, None, None, None));

  let out = apply_overrides_validated(
    &state,
    SettingsOverrides {
      tunnel_compression: Some(true),
      ..Default::default()
    },
  )
  .await;
  assert!(out.is_ok());
  assert!(state.config().tunnel_compression);
}

#[tokio::test]
async fn disabling_cache_clears_the_response_cache() {
  // Start with the cache enabled so disabling it takes the clear() branch.
  let mut cfg = test_config();
  cfg.cache_enabled = true;
  let state = Arc::new(test_state_with(cfg));

  let out = apply_overrides_validated(
    &state,
    SettingsOverrides {
      cache_enabled: Some(false),
      ..Default::default()
    },
  )
  .await;
  assert!(out.is_ok());
  assert!(!state.config().cache_enabled);
}

#[tokio::test]
async fn changing_lockout_and_audit_rotation_updates_live_structures() {
  // Distinct values from test_config drive both the login-lockout policy and
  // the audit-rotation reconfiguration branches of swap_config.
  let state = Arc::new(test_state());
  let out = apply_overrides_validated(
    &state,
    SettingsOverrides {
      login_lockout_threshold: Some(10),
      login_lockout_secs: Some(120),
      audit_max_size: Some(20 * 1024 * 1024),
      audit_max_files: Some(7),
      ..Default::default()
    },
  )
  .await;
  assert!(out.is_ok());
  assert_eq!(state.config().login_lockout_threshold, 10);
  assert_eq!(state.config().login_lockout_secs, 120);
  assert_eq!(state.config().audit_max_files, 7);
}
