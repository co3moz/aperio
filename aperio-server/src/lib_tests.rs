//! The server assembled: token authentication, rate limiting, the proxy handler's
//! answers with and without a client, path-bind matching at segment boundaries,
//! client-IP extraction with and without a trusted proxy, and that each store opens
//! where it is told to.

use crate::access_log::sanitize_uri;
use crate::auth::{extract_and_verify_token, ip_allowed, safe_redirect_path, valid_ip_entry};
use crate::protocol::TunnelMessage;
use crate::proxy::proxy_handler;
use crate::routing::{
  apply_lb_strategy, extract_client_ip, extract_request_host, find_affinity_match,
  method_retryable, normalize_hostname_bind, normalize_path_bind,
  normalize_random_subdomain_pattern, path_matches_bind, random_subdomain_hostname,
  select_client_pool,
};
use crate::settings::{
  FailoverMode, LbStrategy, ServerConfig, SettingsOverrides, apply_settings_overrides,
  override_keys,
};
use crate::share::{
  ShareClaims, share_claims_cover, share_signing_key, sign_share_claims, verify_share_token,
};
use crate::state::{
  AppState, ClientHandle, ClientPerms, ConnectionState, DurationHistogram, ServerStats,
  TunnelResponse,
};
use crate::store::audit::AuditLog;
use crate::store::stats::StatsStore;
use crate::store::tokens::TokenStore;
use crate::store::webhooks::WebhookStore;
use axum::{
  body::Body,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, HeaderValue, StatusCode},
};
use base64::Engine;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};

#[test]
pub(crate) fn test_token_authentication() {
  let mut headers = HeaderMap::new();
  assert!(!extract_and_verify_token(&headers, "secret"));

  headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
  assert!(extract_and_verify_token(&headers, "secret"));
  assert!(!extract_and_verify_token(&headers, "wrong_secret"));

  headers.clear();
  headers.insert("x-auth-token", HeaderValue::from_static("secret"));
  assert!(extract_and_verify_token(&headers, "secret"));
  assert!(!extract_and_verify_token(&headers, "wrong_secret"));
}

#[tokio::test]
pub(crate) async fn test_rate_limiting() {
  let config = ServerConfig {
    token: "test".to_string(),
    gateway_timeout: Duration::from_secs(1),
    gateway_response_timeout: Duration::from_secs(1),
    max_body_size: 1024,
    max_tunnels: 1,
    max_connections_per_service: 16,
    inspector: true,
    access_events: true,
    ip_limit_max: 2.0,
    ip_limit_refill: 0.0, // No refill for testing strict burst limit
    visitor_auth_block: None,
    visitor_auth: crate::visitor_auth::Policy::default(),
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    default_access: crate::settings::DefaultAccess::Allow,
    metrics_token: None,
    scaling_enabled: false,
    scaling_allow_http: false,
    scaling_allow_private: false,
    scaling_record_ttl: Duration::from_secs(3600),
    edge_token: None,
    edge_service_url: None,
    edge_entrypoints: Vec::new(),
    edge_cert_resolver: None,
    edge_include_offline: false,
    random_subdomain_suffix: None,
    client_down_threshold: Duration::from_secs(3600),
    tunnel_compression: false,
    custom_504_page: None,
    custom_503_page: None,
    lb_strategy: LbStrategy::RoundRobin,
    failover_mode: FailoverMode::Fail,
    failover_max_jumps: 2,
    failover_window: Duration::from_secs(15),
    failover_all_methods: false,
    retry_on_5xx: false,
    retry_statuses: Vec::new(),
    outlier_ejection: false,
    outlier_max_failures: 5,
    outlier_window: Duration::from_secs(30),
    outlier_eject: Duration::from_secs(30),
    cache_enabled: false,
    max_concurrent_requests: 100,
    max_ws_connections: 10_000,
    login_lockout_threshold: 5,
    login_lockout_secs: 60,
    audit_max_size: 10 * 1024 * 1024,
    audit_max_files: 3,
    ui_language: "en".to_string(),
    header_rules: Default::default(),
    static_routes: Default::default(),
    error_pages: Default::default(),
    route_limits: Default::default(),
    fallbacks: Default::default(),
    waf: Default::default(),
    maintenance_windows: Default::default(),
    alert_rules: Default::default(),
    denied_ips: Default::default(),
    identity_headers: false,
    visitor_identity_headers: false,
    access_log_sample_rate: 1.0,
    alternate_servers: Vec::new(),
    max_streams_per_ip: 0,
    otel_bridge: false,
    shutdown_drain: None,
    shutdown_drain_auto: false,
    shutdown_timeout: 10,
    request_id_enabled: true,
    request_id_header: "x-request-id".to_string(),
    request_id_trust_inbound: false,
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
    stream_min_throughput: 0,
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let (client_connected_tx, _) = watch::channel(false);
  let state = AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    stream_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
    telemetry_tx: tokio::sync::mpsc::channel(1).0,
    pending_messages: Mutex::new(HashMap::new()),
    jwks_cache: Mutex::new(HashMap::new()),
    forward_auth_cache: Mutex::new(HashMap::new()),
    message_metrics: Default::default(),
    client_connected: client_connected_tx,
    connection_state: Mutex::new(ConnectionState {
      connected: false,
      last_disconnect: None,
    }),
    server_start_time: Instant::now(),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 0,
      successful_requests: 0,
      failed_requests: 0,
      total_bytes_transferred: 0,
    }),
    recent_logs: Mutex::new(VecDeque::new()),
    traffic_tx: tokio::sync::broadcast::channel(16).0,
    events_tx: tokio::sync::broadcast::channel(16).0,
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: crate::test_support::test_temp_root()
      .join(format!("settings-{}.json", uuid::Uuid::new_v4())),
    dashboard_enabled: true,
    shutdown: watch::channel(false).0,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    active_ws_connections: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(test_session_store()),
    rate_limiter: Mutex::new(HashMap::new()),
    login_lockout: tokio::sync::Mutex::new(crate::auth::LockoutTracker::new(
      5,
      std::time::Duration::from_secs(60),
    )),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    token_seen_ips: Mutex::new(HashMap::new()),
    route_rate: Mutex::new(HashMap::new()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(test_token_store()),
    admin_key_store: Mutex::new(test_admin_key_store()),
    inbox_store: Mutex::new(test_inbox_store()),
    users: Mutex::new(test_user_store()),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(test_audit_log()),
    persistent_stats: Mutex::new(test_stats_store()),
    webhook_deliveries: test_delivery_log(),
    scaling_store: Mutex::new(crate::test_support::test_scaling_store()),
    scaling_runtime: Mutex::new(crate::scaling::ScalingRuntime::default()),
    scaling_calls: crate::scaling::call_semaphore(),
    webhook_store: Mutex::new(test_webhook_store()),
    org_store: Mutex::new(test_org_store()),
    uptime: Mutex::new(test_uptime_store()),
    webauthn: None,
    webauthn_ceremonies: Mutex::new(crate::webauthn::WebauthnCeremonies::default()),
    oidc: None,
    org_oidc: Mutex::new(HashMap::new()),
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    udp_streams: Mutex::new(HashMap::new()),
    response_cache: Mutex::new(crate::cache::ResponseCache::default()),
    cache_inflight: std::sync::Mutex::new(std::collections::HashMap::new()),
    stage_stats: Mutex::new(crate::state::StageStats::default()),
    endpoint_stats: Mutex::new(crate::state::EndpointStats::default()),
    route_trends: Mutex::new(crate::state::RouteTrends::default()),
    activity: Mutex::new(crate::state::Activity::default()),
    maintenance: Mutex::new(std::collections::HashMap::new()),
    access_log: None,
    duration_histogram: DurationHistogram::default(),
    limit_counters: Default::default(),
  };

  let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

  // First request should pass
  assert!(state.check_rate_limit(ip).await);
  // Second request should pass
  assert!(state.check_rate_limit(ip).await);
  // Third request should be rate limited (max burst is 2.0)
  assert!(!state.check_rate_limit(ip).await);
}

#[tokio::test]
pub(crate) async fn test_proxy_handler_gateway_timeout_offline() {
  let config = ServerConfig {
    token: "test".to_string(),
    gateway_timeout: Duration::from_millis(100),
    gateway_response_timeout: Duration::from_millis(100),
    max_body_size: 1024,
    max_tunnels: 1,
    max_connections_per_service: 16,
    inspector: true,
    access_events: true,
    ip_limit_max: 100.0,
    ip_limit_refill: 10.0,
    visitor_auth_block: None,
    visitor_auth: crate::visitor_auth::Policy::default(),
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    default_access: crate::settings::DefaultAccess::Allow,
    metrics_token: None,
    scaling_enabled: false,
    scaling_allow_http: false,
    scaling_allow_private: false,
    scaling_record_ttl: Duration::from_secs(3600),
    edge_token: None,
    edge_service_url: None,
    edge_entrypoints: Vec::new(),
    edge_cert_resolver: None,
    edge_include_offline: false,
    random_subdomain_suffix: None,
    client_down_threshold: Duration::from_secs(3600),
    tunnel_compression: false,
    custom_504_page: None,
    custom_503_page: None,
    lb_strategy: LbStrategy::RoundRobin,
    failover_mode: FailoverMode::Fail,
    failover_max_jumps: 2,
    failover_window: Duration::from_secs(15),
    failover_all_methods: false,
    retry_on_5xx: false,
    retry_statuses: Vec::new(),
    outlier_ejection: false,
    outlier_max_failures: 5,
    outlier_window: Duration::from_secs(30),
    outlier_eject: Duration::from_secs(30),
    cache_enabled: false,
    max_concurrent_requests: 100,
    max_ws_connections: 10_000,
    login_lockout_threshold: 5,
    login_lockout_secs: 60,
    audit_max_size: 10 * 1024 * 1024,
    audit_max_files: 3,
    ui_language: "en".to_string(),
    header_rules: Default::default(),
    static_routes: Default::default(),
    error_pages: Default::default(),
    route_limits: Default::default(),
    fallbacks: Default::default(),
    waf: Default::default(),
    maintenance_windows: Default::default(),
    alert_rules: Default::default(),
    denied_ips: Default::default(),
    identity_headers: false,
    visitor_identity_headers: false,
    access_log_sample_rate: 1.0,
    alternate_servers: Vec::new(),
    max_streams_per_ip: 0,
    otel_bridge: false,
    shutdown_drain: None,
    shutdown_drain_auto: false,
    shutdown_timeout: 10,
    request_id_enabled: true,
    request_id_header: "x-request-id".to_string(),
    request_id_trust_inbound: false,
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
    stream_min_throughput: 0,
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let (client_connected_tx, _) = watch::channel(false);
  let state = Arc::new(AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    stream_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
    telemetry_tx: tokio::sync::mpsc::channel(1).0,
    pending_messages: Mutex::new(HashMap::new()),
    jwks_cache: Mutex::new(HashMap::new()),
    forward_auth_cache: Mutex::new(HashMap::new()),
    message_metrics: Default::default(),
    client_connected: client_connected_tx,
    connection_state: Mutex::new(ConnectionState {
      connected: false,
      last_disconnect: None,
    }),
    // Set start time to 2 minutes ago to trigger immediate timeout
    server_start_time: Instant::now() - Duration::from_secs(120),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 0,
      successful_requests: 0,
      failed_requests: 0,
      total_bytes_transferred: 0,
    }),
    recent_logs: Mutex::new(VecDeque::new()),
    traffic_tx: tokio::sync::broadcast::channel(16).0,
    events_tx: tokio::sync::broadcast::channel(16).0,
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: crate::test_support::test_temp_root()
      .join(format!("settings-{}.json", uuid::Uuid::new_v4())),
    dashboard_enabled: true,
    shutdown: watch::channel(false).0,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    active_ws_connections: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(test_session_store()),
    rate_limiter: Mutex::new(HashMap::new()),
    login_lockout: tokio::sync::Mutex::new(crate::auth::LockoutTracker::new(
      5,
      std::time::Duration::from_secs(60),
    )),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    token_seen_ips: Mutex::new(HashMap::new()),
    route_rate: Mutex::new(HashMap::new()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(test_token_store()),
    admin_key_store: Mutex::new(test_admin_key_store()),
    inbox_store: Mutex::new(test_inbox_store()),
    users: Mutex::new(test_user_store()),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(test_audit_log()),
    persistent_stats: Mutex::new(test_stats_store()),
    webhook_deliveries: test_delivery_log(),
    scaling_store: Mutex::new(crate::test_support::test_scaling_store()),
    scaling_runtime: Mutex::new(crate::scaling::ScalingRuntime::default()),
    scaling_calls: crate::scaling::call_semaphore(),
    webhook_store: Mutex::new(test_webhook_store()),
    org_store: Mutex::new(test_org_store()),
    uptime: Mutex::new(test_uptime_store()),
    webauthn: None,
    webauthn_ceremonies: Mutex::new(crate::webauthn::WebauthnCeremonies::default()),
    oidc: None,
    org_oidc: Mutex::new(HashMap::new()),
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    udp_streams: Mutex::new(HashMap::new()),
    response_cache: Mutex::new(crate::cache::ResponseCache::default()),
    cache_inflight: std::sync::Mutex::new(std::collections::HashMap::new()),
    stage_stats: Mutex::new(crate::state::StageStats::default()),
    endpoint_stats: Mutex::new(crate::state::EndpointStats::default()),
    route_trends: Mutex::new(crate::state::RouteTrends::default()),
    activity: Mutex::new(crate::state::Activity::default()),
    maintenance: Mutex::new(std::collections::HashMap::new()),
    access_log: None,
    duration_histogram: DurationHistogram::default(),
    limit_counters: Default::default(),
  });

  // A fresh install (no client ever, no traffic ever) redirects the bare
  // root to the dashboard instead of showing a 504.
  let response = proxy_handler(
    State(state.clone()),
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
    axum::extract::Request::new(Body::empty()),
  )
  .await;
  assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
  assert_eq!(
    response
      .headers()
      .get("location")
      .unwrap()
      .to_str()
      .unwrap(),
    "/aperio"
  );

  // Any other path still answers 504 while no client is connected.
  let mut req = axum::extract::Request::new(Body::empty());
  *req.uri_mut() = "/hello".parse().unwrap();
  let response = proxy_handler(
    State(state),
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
    req,
  )
  .await;
  assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
pub(crate) async fn test_proxy_handler_success() {
  let config = ServerConfig {
    token: "test".to_string(),
    gateway_timeout: Duration::from_millis(200),
    gateway_response_timeout: Duration::from_millis(500),
    max_body_size: 1024,
    max_tunnels: 2,
    max_connections_per_service: 16,
    inspector: true,
    access_events: true,
    ip_limit_max: 100.0,
    ip_limit_refill: 10.0,
    visitor_auth_block: None,
    visitor_auth: crate::visitor_auth::Policy::default(),
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    default_access: crate::settings::DefaultAccess::Allow,
    metrics_token: None,
    scaling_enabled: false,
    scaling_allow_http: false,
    scaling_allow_private: false,
    scaling_record_ttl: Duration::from_secs(3600),
    edge_token: None,
    edge_service_url: None,
    edge_entrypoints: Vec::new(),
    edge_cert_resolver: None,
    edge_include_offline: false,
    random_subdomain_suffix: None,
    client_down_threshold: Duration::from_secs(3600),
    tunnel_compression: false,
    custom_504_page: None,
    custom_503_page: None,
    lb_strategy: LbStrategy::RoundRobin,
    failover_mode: FailoverMode::Fail,
    failover_max_jumps: 2,
    failover_window: Duration::from_secs(15),
    failover_all_methods: false,
    retry_on_5xx: false,
    retry_statuses: Vec::new(),
    outlier_ejection: false,
    outlier_max_failures: 5,
    outlier_window: Duration::from_secs(30),
    outlier_eject: Duration::from_secs(30),
    cache_enabled: false,
    max_concurrent_requests: 100,
    max_ws_connections: 10_000,
    login_lockout_threshold: 5,
    login_lockout_secs: 60,
    audit_max_size: 10 * 1024 * 1024,
    audit_max_files: 3,
    ui_language: "en".to_string(),
    header_rules: Default::default(),
    static_routes: Default::default(),
    error_pages: Default::default(),
    route_limits: Default::default(),
    fallbacks: Default::default(),
    waf: Default::default(),
    maintenance_windows: Default::default(),
    alert_rules: Default::default(),
    denied_ips: Default::default(),
    identity_headers: false,
    visitor_identity_headers: false,
    access_log_sample_rate: 1.0,
    alternate_servers: Vec::new(),
    max_streams_per_ip: 0,
    otel_bridge: false,
    shutdown_drain: None,
    shutdown_drain_auto: false,
    shutdown_timeout: 10,
    request_id_enabled: true,
    request_id_header: "x-request-id".to_string(),
    request_id_trust_inbound: false,
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
    stream_min_throughput: 0,
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let (client_connected_tx, _) = watch::channel(true);
  let state = Arc::new(AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    stream_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
    telemetry_tx: tokio::sync::mpsc::channel(1).0,
    pending_messages: Mutex::new(HashMap::new()),
    jwks_cache: Mutex::new(HashMap::new()),
    forward_auth_cache: Mutex::new(HashMap::new()),
    message_metrics: Default::default(),
    client_connected: client_connected_tx,
    connection_state: Mutex::new(ConnectionState {
      connected: true,
      last_disconnect: None,
    }),
    server_start_time: Instant::now(),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 0,
      successful_requests: 0,
      failed_requests: 0,
      total_bytes_transferred: 0,
    }),
    recent_logs: Mutex::new(VecDeque::new()),
    traffic_tx: tokio::sync::broadcast::channel(16).0,
    events_tx: tokio::sync::broadcast::channel(16).0,
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: crate::test_support::test_temp_root()
      .join(format!("settings-{}.json", uuid::Uuid::new_v4())),
    dashboard_enabled: true,
    shutdown: watch::channel(false).0,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    active_ws_connections: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(test_session_store()),
    rate_limiter: Mutex::new(HashMap::new()),
    login_lockout: tokio::sync::Mutex::new(crate::auth::LockoutTracker::new(
      5,
      std::time::Duration::from_secs(60),
    )),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    token_seen_ips: Mutex::new(HashMap::new()),
    route_rate: Mutex::new(HashMap::new()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(test_token_store()),
    admin_key_store: Mutex::new(test_admin_key_store()),
    inbox_store: Mutex::new(test_inbox_store()),
    users: Mutex::new(test_user_store()),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(test_audit_log()),
    persistent_stats: Mutex::new(test_stats_store()),
    webhook_deliveries: test_delivery_log(),
    scaling_store: Mutex::new(crate::test_support::test_scaling_store()),
    scaling_runtime: Mutex::new(crate::scaling::ScalingRuntime::default()),
    scaling_calls: crate::scaling::call_semaphore(),
    webhook_store: Mutex::new(test_webhook_store()),
    org_store: Mutex::new(test_org_store()),
    uptime: Mutex::new(test_uptime_store()),
    webauthn: None,
    webauthn_ceremonies: Mutex::new(crate::webauthn::WebauthnCeremonies::default()),
    oidc: None,
    org_oidc: Mutex::new(HashMap::new()),
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    udp_streams: Mutex::new(HashMap::new()),
    response_cache: Mutex::new(crate::cache::ResponseCache::default()),
    cache_inflight: std::sync::Mutex::new(std::collections::HashMap::new()),
    stage_stats: Mutex::new(crate::state::StageStats::default()),
    endpoint_stats: Mutex::new(crate::state::EndpointStats::default()),
    route_trends: Mutex::new(crate::state::RouteTrends::default()),
    activity: Mutex::new(crate::state::Activity::default()),
    maintenance: Mutex::new(std::collections::HashMap::new()),
    access_log: None,
    duration_histogram: DurationHistogram::default(),
    limit_counters: Default::default(),
  });

  let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);
  let client_req_count = Arc::new(AtomicU64::new(0));

  state.clients.write().await.insert(
    "mock-client-1".to_string(),
    ClientHandle {
      drain_secs: None,
      tx: tx_write,
      disconnect: std::sync::Arc::new(tokio::sync::Notify::new()),
      connected_at: Instant::now(),
      client_ip: "127.0.0.1".to_string(),
      declared_client_id: None,
      last_ping_at: None,
      perms: ClientPerms::master(),
      draining: false,
      client_version: None,
      client_protocol: None,
      cpu_percent: None,
      rss_bytes: None,
      rtt_ms: None,
      jitter_ms: None,
      reconnects: None,
      reported_instance_id: None,
      instance_group: None,
      subscriptions: Vec::new(),
      services: vec![crate::state::ServiceState {
        metrics_labels: Vec::new(),
        service_custom_name: None,
        request_count: client_req_count,
        declared_path: None,
        assigned_path: None,
        declared_hostname: None,
        declared_hostnames: Vec::new(),
        assigned_hostnames: Vec::new(),
        random_hostname: None,
        override_path_bind: None,
        override_hostname_binds: Vec::new(),
        connections: None,
        connections_min: None,
        connections_max: None,
        capture: true,
        config_notes: Vec::new(),
        max_concurrent: None,
        max_concurrent_ceiling: None,
        inflight_limiter: None,
        admin_enabled: true,
        tcp_enabled: false,
        backend_healthy: true,
        backend_probed: true,
        priority: 0,
        bandwidth_bps: Arc::new(AtomicU64::new(0)),
        service_name: None,
        public: false,
        public_denied_warned: false,
        visitor_auth: None,
        visitor_auth_policy: None,
        visitor_auth_denied_warned: false,
        ungated_warned: false,
        allowed_ips: Vec::new(),
        allowed_ips_invalid_warned: false,
        scaling_invalid_warned: false,
        tunnels: Vec::new(),
        cache: false,
        cache_ignored_warned: false,
        resilience: false,
        max_request_body: None,
        response_timeout: None,
        webhook_inbox: false,
        denied: None,
        recent_failures: VecDeque::new(),
        ejected_until: None,
      }],
    },
  );

  let state_clone = state.clone();
  tokio::spawn(async move {
    if let Some(Message::Text(text)) = rx_write.recv().await
      && let Ok(TunnelMessage::Request { id, .. }) = serde_json::from_str::<TunnelMessage>(&text)
    {
      let mut pending = state_clone.pending_requests.lock().await;
      if let Some(req) = pending.remove(&id) {
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        let _ = req.tx.send(TunnelResponse {
          trailers: None,
          status: 200,
          headers,
          body: Some(base64::prelude::BASE64_STANDARD.encode(r#"{"status":"ok"}"#)),
          body_raw: None,
          stream_rx: None,
          timings: None,
        });
      }
    }
  });

  let response = proxy_handler(
    State(state),
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
    axum::extract::Request::new(Body::empty()),
  )
  .await;

  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(
    response
      .headers()
      .get("content-type")
      .unwrap()
      .to_str()
      .unwrap(),
    "application/json"
  );
}

#[test]
pub(crate) fn test_path_matches_bind_segment_boundary() {
  // Exact match
  assert!(path_matches_bind("/api", "/api"));
  // Segment boundary: trailing slash should match
  assert!(path_matches_bind("/api/users", "/api"));
  // Non-boundary prefix must NOT match (the original bug)
  assert!(!path_matches_bind("/apixyz", "/api"));
  assert!(!path_matches_bind("/api-v2", "/api"));
  // Empty bind semantics
  assert!(!path_matches_bind("/", "/api"));
}

#[test]
pub(crate) fn test_normalize_path_bind() {
  // Empty / root → None
  assert_eq!(normalize_path_bind(""), None);
  assert_eq!(normalize_path_bind("/"), None);
  assert_eq!(normalize_path_bind("   "), None);
  // Adds leading slash
  assert_eq!(normalize_path_bind("api"), Some("/api".to_string()));
  // Strips trailing slashes
  assert_eq!(normalize_path_bind("/api/"), Some("/api".to_string()));
  assert_eq!(normalize_path_bind("/api///"), Some("/api".to_string()));
  // Nested paths preserved
  assert_eq!(normalize_path_bind("/api/v2"), Some("/api/v2".to_string()));
  // Path traversal rejected
  assert_eq!(normalize_path_bind("/api/../etc"), None);
  assert_eq!(normalize_path_bind("/.."), None);
  assert_eq!(normalize_path_bind("/./api"), None);
  // Unsafe characters rejected
  assert_eq!(normalize_path_bind("/api;rm -rf"), None);
  assert_eq!(normalize_path_bind("/api?x=1"), None);
  // Allowed special characters
  assert_eq!(
    normalize_path_bind("/api_v2.1"),
    Some("/api_v2.1".to_string())
  );
  assert_eq!(normalize_path_bind("/a-b~c"), Some("/a-b~c".to_string()));
}

#[test]
pub(crate) fn test_sanitize_uri_strips_query() {
  assert_eq!(sanitize_uri("/api/users?id=42&token=secret"), "/api/users");
  assert_eq!(sanitize_uri("/api"), "/api");
  assert_eq!(sanitize_uri("/api?"), "/api");
  // Multiple '?' → first split wins
  assert_eq!(sanitize_uri("/api?a=1?b=2"), "/api");
}

#[test]
pub(crate) fn test_extract_client_ip_trusted() {
  let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

  // No headers → fallback to socket address
  let headers = HeaderMap::new();
  assert_eq!(extract_client_ip(&headers, direct, true, None, &[]), direct);

  // X-Forwarded-For with single IP
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, None, &[]),
    "198.51.100.10".parse::<IpAddr>().unwrap()
  );

  // X-Forwarded-For with chained proxies → leftmost (original client)
  let mut headers = HeaderMap::new();
  headers.insert(
    "x-forwarded-for",
    HeaderValue::from_static("198.51.100.10, 10.0.0.1, 10.0.0.2"),
  );
  assert_eq!(
    extract_client_ip(&headers, direct, true, None, &[]),
    "198.51.100.10".parse::<IpAddr>().unwrap()
  );

  // X-Real-IP fallback when X-Forwarded-For absent
  let mut headers = HeaderMap::new();
  headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, None, &[]),
    "198.51.100.20".parse::<IpAddr>().unwrap()
  );

  // Malformed X-Forwarded-For → fallback
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
  assert_eq!(extract_client_ip(&headers, direct, true, None, &[]), direct);

  // A configured real-IP header (e.g. CF-Connecting-IP) wins over
  // X-Forwarded-For, which chained proxies often reset to the CDN edge.
  let mut headers = HeaderMap::new();
  headers.insert(
    "x-forwarded-for",
    HeaderValue::from_static("162.158.14.210"),
  );
  headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.18"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, Some("cf-connecting-ip"), &[]),
    "203.0.113.18".parse::<IpAddr>().unwrap()
  );
  // ...but only when trust_proxy is on.
  assert_eq!(
    extract_client_ip(&headers, direct, false, Some("cf-connecting-ip"), &[]),
    direct
  );
}

#[test]
pub(crate) fn test_extract_client_ip_untrusted_ignores_headers() {
  let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

  // When trust_proxy is false, spoofed X-Forwarded-For must be ignored.
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
  assert_eq!(
    extract_client_ip(&headers, direct, false, None, &[]),
    direct
  );

  // Spoofed X-Real-IP must also be ignored.
  let mut headers = HeaderMap::new();
  headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
  assert_eq!(
    extract_client_ip(&headers, direct, false, None, &[]),
    direct
  );

  // No headers → fallback to socket address
  let headers = HeaderMap::new();
  assert_eq!(
    extract_client_ip(&headers, direct, false, None, &[]),
    direct
  );
}

/// Generous health threshold so mock clients (no pings) stay eligible.
const TEST_THRESHOLD: Duration = Duration::from_secs(3600);

pub(crate) fn test_user_store() -> crate::store::users::UserStore {
  let dir = crate::test_support::test_temp_root().join(format!("users-{}", uuid::Uuid::new_v4()));
  crate::store::users::UserStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_inbox_store() -> crate::store::inbox::InboxStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-inbox-{}", uuid::Uuid::new_v4()));
  crate::store::inbox::InboxStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_token_store() -> TokenStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-store-{}", uuid::Uuid::new_v4()));
  TokenStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_admin_key_store() -> crate::store::admin_keys::AdminKeyStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-adminkeys-{}", uuid::Uuid::new_v4()));
  crate::store::admin_keys::AdminKeyStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_audit_log() -> AuditLog {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-audit-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3)
}

pub(crate) fn test_stats_store() -> StatsStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-stats-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  StatsStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_webhook_store() -> WebhookStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-hooks-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  WebhookStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_org_store() -> crate::store::orgs::OrgStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-orgs-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  crate::store::orgs::OrgStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_delivery_log() -> std::sync::Arc<Mutex<crate::store::webhooks::DeliveryLog>> {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-deliveries-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  std::sync::Arc::new(Mutex::new(crate::store::webhooks::DeliveryLog::load(
    &dir.to_string_lossy(),
  )))
}

pub(crate) fn test_session_store() -> crate::store::sessions::SessionStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-sessions-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  crate::store::sessions::SessionStore::load(&dir.to_string_lossy())
}

pub(crate) fn test_uptime_store() -> crate::store::uptime::UptimeStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-uptime-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  crate::store::uptime::UptimeStore::load(&dir.to_string_lossy())
}

pub(crate) fn mock_client(
  hostname_bind: Option<&str>,
  path_bind: Option<&str>,
  override_hostname: Option<&str>,
  override_path: Option<&str>,
) -> ClientHandle {
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    drain_secs: None,
    tx,
    disconnect: std::sync::Arc::new(tokio::sync::Notify::new()),
    connected_at: Instant::now(),
    client_ip: "127.0.0.1".to_string(),
    declared_client_id: None,
    last_ping_at: None,
    perms: ClientPerms::master(),
    draining: false,
    client_version: None,
    client_protocol: None,
    cpu_percent: None,
    rss_bytes: None,
    rtt_ms: None,
    jitter_ms: None,
    reconnects: None,
    reported_instance_id: None,
    instance_group: None,
    subscriptions: Vec::new(),
    services: vec![crate::state::ServiceState {
      metrics_labels: Vec::new(),
      service_custom_name: None,
      request_count: Arc::new(AtomicU64::new(0)),
      declared_path: path_bind.map(|s| s.to_string()),
      assigned_path: None,
      declared_hostname: hostname_bind.map(|s| s.to_string()),
      declared_hostnames: Vec::new(),
      assigned_hostnames: Vec::new(),
      random_hostname: None,
      override_path_bind: override_path.map(|s| s.to_string()),
      override_hostname_binds: override_hostname
        .map(|s| s.to_string())
        .into_iter()
        .collect(),
      capture: true,
      connections: None,
      connections_min: None,
      connections_max: None,
      config_notes: Vec::new(),
      max_concurrent: None,
      max_concurrent_ceiling: None,
      inflight_limiter: None,
      admin_enabled: true,
      tcp_enabled: false,
      backend_healthy: true,
      backend_probed: true,
      priority: 0,
      bandwidth_bps: Arc::new(AtomicU64::new(0)),
      service_name: None,
      public: false,
      public_denied_warned: false,
      visitor_auth: None,
      visitor_auth_policy: None,
      visitor_auth_denied_warned: false,
      ungated_warned: false,
      allowed_ips: Vec::new(),
      allowed_ips_invalid_warned: false,
      scaling_invalid_warned: false,
      tunnels: Vec::new(),
      cache: false,
      cache_ignored_warned: false,
      resilience: false,
      max_request_body: None,
      response_timeout: None,
      webhook_inbox: false,
      denied: None,
      recent_failures: VecDeque::new(),
      ejected_until: None,
    }],
  }
}

#[test]
pub(crate) fn test_share_token_roundtrip() {
  let key = share_signing_key("master-token");
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs();
  let claims = ShareClaims {
    host: "app.example.com".to_string(),
    path: Some("/docs".to_string()),
    exp: Some(now + 60),
    id: "abc12345".to_string(),
  };
  let token = sign_share_claims(&claims, &key);

  // Valid token verifies and covers its scope.
  let verified = verify_share_token(&token, &key).expect("token must verify");
  assert_eq!(verified.host, "app.example.com");
  assert!(share_claims_cover(
    &verified,
    Some("app.example.com"),
    "/docs/intro"
  ));
  // Different host or out-of-scope path is not covered.
  assert!(!share_claims_cover(
    &verified,
    Some("other.example.com"),
    "/docs"
  ));
  assert!(!share_claims_cover(
    &verified,
    Some("app.example.com"),
    "/admin"
  ));
  // Segment boundary: /docsX must not match the /docs prefix.
  assert!(!share_claims_cover(
    &verified,
    Some("app.example.com"),
    "/docsecret"
  ));

  // Tampered signature and wrong key are rejected.
  assert!(verify_share_token(&format!("{}x", token), &key).is_none());
  assert!(verify_share_token(&token, &share_signing_key("other-token")).is_none());

  // Expired token is rejected.
  let expired = ShareClaims {
    host: "app.example.com".to_string(),
    path: None,
    exp: Some(now - 1),
    id: "expired1".to_string(),
  };
  let expired_token = sign_share_claims(&expired, &key);
  assert!(verify_share_token(&expired_token, &key).is_none());

  // A pathless token covers the whole host.
  let whole = ShareClaims {
    host: "app.example.com".to_string(),
    path: None,
    exp: Some(now + 60),
    id: "whole1234".to_string(),
  };
  let whole_token = sign_share_claims(&whole, &key);
  let verified = verify_share_token(&whole_token, &key).unwrap();
  assert!(share_claims_cover(
    &verified,
    Some("app.example.com"),
    "/anything"
  ));

  // exp: None = the link never expires.
  let forever = ShareClaims {
    host: "app.example.com".to_string(),
    path: None,
    exp: None,
    id: "forever12".to_string(),
  };
  let forever_token = sign_share_claims(&forever, &key);
  assert!(verify_share_token(&forever_token, &key).is_some());
}

#[test]
pub(crate) fn test_apply_settings_overrides() {
  let base = ServerConfig {
    token: "t".to_string(),
    gateway_timeout: Duration::from_secs(10),
    gateway_response_timeout: Duration::from_secs(30),
    max_body_size: 10 * 1024 * 1024,
    max_tunnels: 10,
    max_connections_per_service: 16,
    inspector: true,
    access_events: true,
    ip_limit_max: 100.0,
    ip_limit_refill: 5.0,
    visitor_auth_block: None,
    visitor_auth: crate::visitor_auth::Policy::default(),
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    default_access: crate::settings::DefaultAccess::Allow,
    metrics_token: None,
    scaling_enabled: false,
    scaling_allow_http: false,
    scaling_allow_private: false,
    scaling_record_ttl: Duration::from_secs(3600),
    edge_token: None,
    edge_service_url: None,
    edge_entrypoints: Vec::new(),
    edge_cert_resolver: None,
    edge_include_offline: false,
    random_subdomain_suffix: None,
    client_down_threshold: Duration::from_secs(15),
    tunnel_compression: false,
    custom_504_page: None,
    custom_503_page: None,
    lb_strategy: LbStrategy::RoundRobin,
    failover_mode: FailoverMode::Fail,
    failover_max_jumps: 2,
    failover_window: Duration::from_secs(15),
    failover_all_methods: false,
    retry_on_5xx: false,
    retry_statuses: Vec::new(),
    outlier_ejection: false,
    outlier_max_failures: 5,
    outlier_window: Duration::from_secs(30),
    outlier_eject: Duration::from_secs(30),
    cache_enabled: false,
    max_concurrent_requests: 100,
    max_ws_connections: 10_000,
    login_lockout_threshold: 5,
    login_lockout_secs: 60,
    audit_max_size: 10 * 1024 * 1024,
    audit_max_files: 3,
    ui_language: "en".to_string(),
    header_rules: Default::default(),
    static_routes: Default::default(),
    error_pages: Default::default(),
    route_limits: Default::default(),
    fallbacks: Default::default(),
    waf: Default::default(),
    maintenance_windows: Default::default(),
    alert_rules: Default::default(),
    denied_ips: Default::default(),
    identity_headers: false,
    visitor_identity_headers: false,
    access_log_sample_rate: 1.0,
    alternate_servers: Vec::new(),
    max_streams_per_ip: 0,
    otel_bridge: false,
    shutdown_drain: None,
    shutdown_drain_auto: false,
    shutdown_timeout: 10,
    request_id_enabled: true,
    request_id_header: "x-request-id".to_string(),
    request_id_trust_inbound: false,
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
    stream_min_throughput: 0,
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let overrides = SettingsOverrides {
    gateway_timeout_secs: Some(20),
    lb_strategy: Some("sticky".to_string()),
    failover_mode: Some("retry-wait".to_string()),
    random_subdomain_suffix: Some("*.e2e.local".to_string()),
    custom_504_page: Some("<h1>down</h1>".to_string()),
    auth_credentials: Some("user:pass".to_string()),
    ..Default::default()
  };
  let c = apply_settings_overrides(&base, &overrides);
  assert_eq!(c.gateway_timeout, Duration::from_secs(20));
  assert_eq!(c.lb_strategy, LbStrategy::Sticky);
  assert_eq!(c.failover_mode, FailoverMode::RetryWait);
  assert_eq!(c.random_subdomain_suffix.as_deref(), Some("*.e2e.local"));
  assert_eq!(c.custom_504_page.as_deref(), Some("<h1>down</h1>"));
  assert_eq!(c.visitor_auth.as_single_credential(), Some("user:pass"));
  // Untouched fields keep the base values; the token never changes.
  assert_eq!(c.max_body_size, base.max_body_size);
  assert_eq!(c.token, "t");

  // Empty strings clear optional values; invalid enum values are skipped.
  let clearing = SettingsOverrides {
    auth_credentials: Some(String::new()),
    lb_strategy: Some("bogus".to_string()),
    ..Default::default()
  };
  let c2 = apply_settings_overrides(&c, &clearing);
  assert!(!c2.visitor_auth.gates(), "an emptied credential is no gate");
  assert_eq!(c2.lb_strategy, c.lb_strategy);

  assert_eq!(
    override_keys(&overrides),
    vec![
      "auth_credentials",
      "custom_504_page",
      "failover_mode",
      "gateway_timeout_secs",
      "lb_strategy",
      "random_subdomain_suffix",
    ]
  );
}

#[test]
pub(crate) fn test_normalize_random_subdomain_pattern() {
  // Bare domain gets the implicit leading wildcard label.
  assert_eq!(
    normalize_random_subdomain_pattern("example.com").as_deref(),
    Some("*.example.com")
  );
  // Canonical form is accepted as-is.
  assert_eq!(
    normalize_random_subdomain_pattern("*.example.com").as_deref(),
    Some("*.example.com")
  );
  // Same-level suffix pattern is preserved, not turned into *.-test....
  assert_eq!(
    normalize_random_subdomain_pattern("*-test.example.com").as_deref(),
    Some("*-test.example.com")
  );
  assert_eq!(
    normalize_random_subdomain_pattern("  *.Example.COM.  ").as_deref(),
    Some("*.example.com")
  );
  // Invalid: wildcard outside the leftmost label, multiple wildcards,
  // no domain part, empty.
  assert_eq!(
    normalize_random_subdomain_pattern("test.*.example.com"),
    None
  );
  assert_eq!(normalize_random_subdomain_pattern("*.*.example.com"), None);
  assert_eq!(normalize_random_subdomain_pattern("*"), None);
  assert_eq!(normalize_random_subdomain_pattern(""), None);

  // Generation replaces the placeholder in place.
  let host = random_subdomain_hostname("*-pi.example.com");
  assert!(host.ends_with("-pi.example.com"), "got {host}");
  assert!(!host.contains('*'));
  let host = random_subdomain_hostname("*.example.com");
  assert!(host.ends_with(".example.com") && !host.contains('*'));
}

#[test]
pub(crate) fn test_find_affinity_match() {
  let mut clients = HashMap::new();
  let mut a = mock_client(None, None, None, None);
  a.reported_instance_id = Some("instance-a".to_string());
  let b = mock_client(None, None, None, None);
  clients.insert("conn-a".to_string(), a);
  clients.insert("conn-b".to_string(), b);
  let pool = refs(&["conn-a", "conn-b"]);

  // Matches by instance ID (survives reconnects) and by connection ID.
  assert_eq!(
    find_affinity_match(&pool, &clients, "instance-a").map(|r| r.client),
    Some("conn-a".to_string())
  );
  assert_eq!(
    find_affinity_match(&pool, &clients, "conn-b").map(|r| r.client),
    Some("conn-b".to_string())
  );
  // Unknown affinity falls back to rotation (None).
  assert_eq!(find_affinity_match(&pool, &clients, "gone"), None);
  // A client that left the pool no longer matches.
  assert_eq!(
    find_affinity_match(&refs(&["conn-b"]), &clients, "instance-a"),
    None
  );
}

#[test]
pub(crate) fn test_method_retryable() {
  // Idempotent methods may always fail over.
  for m in ["GET", "HEAD", "OPTIONS", "PUT", "DELETE", "TRACE"] {
    assert!(method_retryable(m, false), "{m} must be retryable");
  }
  // Non-idempotent methods need the explicit opt-in.
  for m in ["POST", "PATCH"] {
    assert!(!method_retryable(m, false), "{m} must not retry by default");
    assert!(method_retryable(m, true), "{m} must retry with the opt-in");
  }
}

#[test]
pub(crate) fn test_apply_lb_strategy_primary_standby() {
  let mut clients = HashMap::new();
  let primary = mock_client(None, None, None, None);
  let mut standby = mock_client(None, None, None, None);
  standby.sole_mut().priority = 1;
  clients.insert("primary".to_string(), primary);
  clients.insert("standby".to_string(), standby);

  let pool = refs(&["primary", "standby"]);
  // Round-robin keeps the whole pool.
  assert_eq!(
    apply_lb_strategy(pool.clone(), &clients, LbStrategy::RoundRobin).len(),
    2
  );
  // Primary-standby narrows to the lowest priority tier.
  assert_eq!(
    ids(&apply_lb_strategy(
      pool,
      &clients,
      LbStrategy::PrimaryStandby
    )),
    vec!["primary".to_string()]
  );
  // Once the primary is out of the pool, the standby takes over.
  assert_eq!(
    ids(&apply_lb_strategy(
      refs(&["standby"]),
      &clients,
      LbStrategy::PrimaryStandby
    )),
    vec!["standby".to_string()]
  );
}

#[test]
pub(crate) fn test_select_client_pool_excludes_unhealthy() {
  let mut clients = HashMap::new();
  let mut stale = mock_client(None, None, None, None);
  // Last heartbeat far in the past -> down
  stale.last_ping_at = Some(Instant::now() - Duration::from_secs(120));
  clients.insert("stale".to_string(), stale);

  // Only client is unhealthy -> nothing selectable
  assert!(select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).is_none());

  // A fresh client joins -> traffic goes only to it
  let mut fresh = mock_client(None, None, None, None);
  fresh.last_ping_at = Some(Instant::now());
  clients.insert("fresh".to_string(), fresh);
  let (pool, _) = select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
  assert_eq!(ids(&pool), vec!["fresh".to_string()]);

  // The stale client recovers with a new ping -> back in the pool
  clients.get_mut("stale").unwrap().last_ping_at = Some(Instant::now());
  let (pool, _) = select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
  assert_eq!(pool.len(), 2);
}

#[test]
pub(crate) fn test_ip_allowed() {
  let ip = |s: &str| s.parse::<IpAddr>().unwrap();

  // Empty list or wildcards allow everything
  assert!(ip_allowed(ip("1.2.3.4"), &[]));
  assert!(ip_allowed(ip("1.2.3.4"), &["*".to_string()]));
  assert!(ip_allowed(ip("1.2.3.4"), &["0.0.0.0/0".to_string()]));
  assert!(ip_allowed(ip("::1"), &["::/0".to_string()]));

  // Exact IP match
  assert!(ip_allowed(ip("1.2.3.4"), &["1.2.3.4".to_string()]));
  assert!(!ip_allowed(ip("1.2.3.5"), &["1.2.3.4".to_string()]));

  // CIDR ranges
  assert!(ip_allowed(ip("10.1.2.3"), &["10.0.0.0/8".to_string()]));
  assert!(!ip_allowed(ip("11.1.2.3"), &["10.0.0.0/8".to_string()]));
  assert!(ip_allowed(
    ip("192.168.1.77"),
    &["192.168.1.0/24".to_string()]
  ));
  assert!(!ip_allowed(
    ip("192.168.2.77"),
    &["192.168.1.0/24".to_string()]
  ));

  // Multiple entries: any match wins
  assert!(ip_allowed(
    ip("203.0.113.9"),
    &["10.0.0.0/8".to_string(), "203.0.113.0/24".to_string()]
  ));

  // IPv6 CIDR
  assert!(ip_allowed(ip("fd00::1"), &["fd00::/8".to_string()]));
  assert!(!ip_allowed(ip("2001:db8::1"), &["fd00::/8".to_string()]));
  // Family mismatch never matches
  assert!(!ip_allowed(ip("1.2.3.4"), &["fd00::/8".to_string()]));

  // Malformed entries are ignored (do not match)
  assert!(!ip_allowed(ip("1.2.3.4"), &["not-an-ip".to_string()]));

  // Validation helper
  assert!(valid_ip_entry("10.0.0.0/8"));
  assert!(valid_ip_entry("1.2.3.4"));
  assert!(valid_ip_entry("::1"));
  assert!(valid_ip_entry("*"));
  assert!(!valid_ip_entry("10.0.0.0/33"));
  assert!(!valid_ip_entry("banana"));
}

#[test]
pub(crate) fn test_normalize_hostname_bind() {
  assert_eq!(
    normalize_hostname_bind("a.example.com"),
    Some("a.example.com".to_string())
  );
  // Case-insensitive
  assert_eq!(
    normalize_hostname_bind("A.Example.COM"),
    Some("a.example.com".to_string())
  );
  // Port stripped
  assert_eq!(
    normalize_hostname_bind("a.example.com:8080"),
    Some("a.example.com".to_string())
  );
  // Trailing dot stripped
  assert_eq!(
    normalize_hostname_bind("a.example.com."),
    Some("a.example.com".to_string())
  );
  // Invalid values rejected
  assert_eq!(normalize_hostname_bind(""), None);
  assert_eq!(normalize_hostname_bind("   "), None);
  assert_eq!(normalize_hostname_bind("exa mple.com"), None);
  assert_eq!(normalize_hostname_bind("example..com"), None);
  assert_eq!(normalize_hostname_bind("exa_mple.com"), None);
  assert_eq!(normalize_hostname_bind(&"a".repeat(300)), None);
}

#[test]
pub(crate) fn test_extract_request_host() {
  let mut headers = HeaderMap::new();
  assert_eq!(extract_request_host(&headers), None);

  headers.insert("host", HeaderValue::from_static("A.Example.com:443"));
  assert_eq!(
    extract_request_host(&headers),
    Some("a.example.com".to_string())
  );

  headers.insert("host", HeaderValue::from_static("[::1]:8080"));
  assert_eq!(extract_request_host(&headers), Some("::1".to_string()));
}

#[test]
pub(crate) fn test_select_client_pool_hostname_routing() {
  let mut clients = HashMap::new();
  clients.insert(
    "a".to_string(),
    mock_client(Some("a.example.com"), None, None, None),
  );
  clients.insert(
    "b".to_string(),
    mock_client(Some("b.example.com"), None, None, None),
  );
  clients.insert("unbound".to_string(), mock_client(None, None, None, None));

  // Host matches a.example.com → only client "a"
  let (pool, key) =
    select_client_pool(&clients, "/", Some("a.example.com"), false, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["a".to_string()]);
  assert_eq!(key, (Some("a.example.com".to_string()), None));

  // Unknown host → falls back to unbound client
  let (pool, key) =
    select_client_pool(&clients, "/", Some("c.example.com"), false, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["unbound".to_string()]);
  assert_eq!(key, (None, None));

  // Strict mode: unknown host → no client at all
  assert!(select_client_pool(&clients, "/", Some("c.example.com"), true, TEST_THRESHOLD).is_none());
  // Strict mode: matching host still works
  let (pool, _) =
    select_client_pool(&clients, "/", Some("b.example.com"), true, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["b".to_string()]);
  // Strict mode: no Host header → no client
  assert!(select_client_pool(&clients, "/", None, true, TEST_THRESHOLD).is_none());
}

#[test]
pub(crate) fn test_select_client_pool_hostname_and_path_combined() {
  let mut clients = HashMap::new();
  clients.insert(
    "host-api".to_string(),
    mock_client(Some("a.example.com"), Some("/api"), None, None),
  );
  clients.insert(
    "host-root".to_string(),
    mock_client(Some("a.example.com"), None, None, None),
  );

  // Path under /api on the bound host → path-bound client wins
  let (pool, key) = select_client_pool(
    &clients,
    "/api/users",
    Some("a.example.com"),
    false,
    TEST_THRESHOLD,
  )
  .unwrap();
  assert_eq!(ids(&pool), vec!["host-api".to_string()]);
  assert_eq!(
    key,
    (Some("a.example.com".to_string()), Some("/api".to_string()))
  );

  // Other paths on the bound host → unbound-path client
  let (pool, _) = select_client_pool(
    &clients,
    "/other",
    Some("a.example.com"),
    false,
    TEST_THRESHOLD,
  )
  .unwrap();
  assert_eq!(ids(&pool), vec!["host-root".to_string()]);
}

#[test]
pub(crate) fn test_select_client_pool_override_wins() {
  let mut clients = HashMap::new();
  // Client reported no hostname, dashboard overruled it to a.example.com
  clients.insert(
    "overruled".to_string(),
    mock_client(None, None, Some("a.example.com"), None),
  );

  let (pool, _) =
    select_client_pool(&clients, "/", Some("a.example.com"), true, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["overruled".to_string()]);

  // With the override active, the client is no longer an unbound fallback
  assert!(
    select_client_pool(&clients, "/", Some("x.example.com"), false, TEST_THRESHOLD).is_none()
  );
}

#[test]
pub(crate) fn test_select_client_pool_longest_path_bind_wins() {
  let mut clients = HashMap::new();
  clients.insert(
    "short".to_string(),
    mock_client(None, Some("/api"), None, None),
  );
  clients.insert(
    "long".to_string(),
    mock_client(None, Some("/api/v2"), None, None),
  );

  let (pool, key) =
    select_client_pool(&clients, "/api/v2/users", None, false, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["long".to_string()]);
  assert_eq!(key, (None, Some("/api/v2".to_string())));

  let (pool, _) = select_client_pool(&clients, "/api/other", None, false, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["short".to_string()]);
}

#[test]
pub(crate) fn test_safe_redirect_path() {
  // Normal relative paths should pass through
  assert_eq!(safe_redirect_path("/"), "/");
  assert_eq!(safe_redirect_path("/dashboard"), "/dashboard");
  assert_eq!(
    safe_redirect_path("/api/v1/users?page=1"),
    "/api/v1/users?page=1"
  );

  // Protocol-relative URLs must be rejected (open redirect to external host)
  assert_eq!(safe_redirect_path("//evil.com"), "/");
  assert_eq!(safe_redirect_path("//evil.com/phishing"), "/");

  // Backslash-based bypass attempts must be rejected
  assert_eq!(safe_redirect_path("/\\evil.com"), "/");

  // Non-path values must be rejected
  assert_eq!(safe_redirect_path("https://evil.com"), "/");
  assert_eq!(safe_redirect_path("javascript:alert(1)"), "/");
  assert_eq!(safe_redirect_path(""), "/");
  assert_eq!(safe_redirect_path("evil.com"), "/");
}

#[test]
pub(crate) fn test_effective_body_limit() {
  use crate::proxy::effective_body_limit;
  // No declared cap: the global limit applies.
  assert_eq!(effective_body_limit(1024, None), 1024);
  // A declared cap tightens the global limit.
  assert_eq!(effective_body_limit(1024, Some(100)), 100);
  // A declared cap can never widen the global limit.
  assert_eq!(effective_body_limit(1024, Some(10_000)), 1024);
}

#[test]
pub(crate) fn test_route_trends_minute_buckets() {
  use crate::state::RouteTrends;
  let mut trends = RouteTrends::default();
  let t0 = 6000u64; // minute 100
  trends.record(Some("a.example.com"), 200, None, t0);
  trends.record(Some("a.example.com"), 404, None, t0 + 10);
  trends.record(Some("a.example.com"), 500, None, t0 + 70); // next minute
  trends.record(None, 200, None, t0); // host-less traffic lands on "*"

  let trend = trends.routes.get("a.example.com").unwrap();
  let series = trend.series(2, (t0 + 70) / 60);
  assert_eq!(series.len(), 2);
  assert_eq!(series[0].total, 2);
  assert_eq!(series[0].s2xx, 1);
  assert_eq!(series[0].s4xx, 1);
  assert_eq!(series[1].total, 1);
  assert_eq!(series[1].s5xx, 1);
  // Gap minutes are zero-filled.
  let padded = trend.series(5, (t0 + 70) / 60 + 2);
  assert_eq!(padded.len(), 5);
  assert_eq!(padded[4].total, 0);
  assert!(trends.routes.contains_key("*"));
}

// ===========================================================================
// main.rs own helpers (below): the dashboard authorization floor, the audit
// verifier CLI, the TCP listener binder, and the uptime availability snapshot.
// The async `main`/`async_main` entrypoint and `shutdown_signal` are not
// unit-testable in-process (they bind sockets, install signal handlers, and
// never return), so they are deliberately left uncovered.
// ===========================================================================

// ---------------------------------------------------------------------------
// observe_service_availability, per-entity uptime snapshot
// ---------------------------------------------------------------------------

#[tokio::test]
pub(crate) async fn test_observe_service_availability_states() {
  use crate::observe_service_availability;
  use crate::store::uptime::Availability;

  let state = crate::test_support::test_state();

  // No clients → empty snapshot.
  assert!(observe_service_availability(&state).await.is_empty());

  // Healthy client, keyed by its service_name → Up.
  let mut up = mock_client(None, None, None, None);
  up.sole_mut().service_name = Some("web".to_string());
  state.clients.write().await.insert("c-up".to_string(), up);

  // Connected but draining → Degraded, keyed by reported_instance_id (no name).
  let mut drain = mock_client(None, None, None, None);
  drain.draining = true;
  drain.reported_instance_id = Some("inst-drain".to_string());
  state
    .clients
    .write()
    .await
    .insert("c-drain".to_string(), drain);

  // Backend probe failing → Degraded, keyed by connection id (no name/instance).
  let mut bad_backend = mock_client(None, None, None, None);
  bad_backend.sole_mut().backend_healthy = false;
  state
    .clients
    .write()
    .await
    .insert("c-badbackend".to_string(), bad_backend);

  // Admin-disabled → Degraded as well.
  let mut disabled = mock_client(None, None, None, None);
  disabled.sole_mut().admin_enabled = false;
  disabled.sole_mut().service_name = Some("disabled-svc".to_string());
  state
    .clients
    .write()
    .await
    .insert("c-disabled".to_string(), disabled);

  let snap = observe_service_availability(&state).await;
  assert_eq!(snap.get("web").unwrap().0, Availability::Up);
  assert_eq!(snap.get("inst-drain").unwrap().0, Availability::Degraded);
  assert_eq!(snap.get("c-badbackend").unwrap().0, Availability::Degraded);
  assert_eq!(snap.get("disabled-svc").unwrap().0, Availability::Degraded);
}

#[tokio::test]
pub(crate) async fn test_observe_service_availability_down_and_best_state_wins() {
  use crate::observe_service_availability;
  use crate::store::uptime::Availability;

  // Short down-threshold so a stale heartbeat marks the client down.
  let mut cfg = crate::test_support::test_config();
  cfg.client_down_threshold = Duration::from_secs(1);
  let state = crate::test_support::test_state_with(cfg);

  // Stale heartbeat → Down.
  let mut stale = mock_client(None, None, None, None);
  stale.sole_mut().service_name = Some("svc".to_string());
  stale.last_ping_at = Some(Instant::now() - Duration::from_secs(120));
  state
    .clients
    .write()
    .await
    .insert("c-stale".to_string(), stale);

  // A second, healthy connection for the SAME entity → the best state wins.
  let mut healthy = mock_client(None, None, None, None);
  healthy.sole_mut().service_name = Some("svc".to_string());
  healthy.last_ping_at = Some(Instant::now());
  state
    .clients
    .write()
    .await
    .insert("c-healthy".to_string(), healthy);

  let snap = observe_service_availability(&state).await;
  assert_eq!(snap.get("svc").unwrap().0, Availability::Up);

  // With only the stale connection left, the entity reads Down.
  state.clients.write().await.remove("c-healthy");
  let snap = observe_service_availability(&state).await;
  assert_eq!(snap.get("svc").unwrap().0, Availability::Down);
}

/// A routed pool as connection ids, which is what these assertions were
/// written against and still the readable thing to compare. The pool itself
/// is `(connection, service)` pairs now.
fn ids(pool: &[crate::routing::ServiceRef]) -> Vec<String> {
  pool.iter().map(|r| r.client.clone()).collect()
}

/// A pool built from connection ids, every one of them the connection's only
/// service.
fn refs(ids: &[&str]) -> Vec<crate::routing::ServiceRef> {
  ids
    .iter()
    .map(|id| crate::routing::ServiceRef {
      client: (*id).to_string(),
      index: 0,
    })
    .collect()
}
