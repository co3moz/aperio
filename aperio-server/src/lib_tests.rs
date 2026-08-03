use super::{SHUTDOWN_DRAIN_AUTO_CAP, shutdown_drain_budget};
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
use crate::test_support::test_state;
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
fn test_token_authentication() {
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
async fn test_rate_limiting() {
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
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
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
    access_log_sample_rate: 1.0,
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
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let (client_connected_tx, _) = watch::channel(false);
  let state = AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    telemetry_tx: tokio::sync::mpsc::channel(1).0,
    pending_messages: Mutex::new(HashMap::new()),
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
async fn test_proxy_handler_gateway_timeout_offline() {
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
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
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
    access_log_sample_rate: 1.0,
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
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let (client_connected_tx, _) = watch::channel(false);
  let state = Arc::new(AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    telemetry_tx: tokio::sync::mpsc::channel(1).0,
    pending_messages: Mutex::new(HashMap::new()),
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
async fn test_proxy_handler_success() {
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
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
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
    access_log_sample_rate: 1.0,
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
    stream_pause_bytes: 2 * 1024 * 1024,
    stream_resume_bytes: 512 * 1024,
    stream_backlog_limit: 16 * 1024 * 1024,
    outbound_policy: Default::default(),
  };

  let (client_connected_tx, _) = watch::channel(true);
  let state = Arc::new(AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    telemetry_tx: tokio::sync::mpsc::channel(1).0,
    pending_messages: Mutex::new(HashMap::new()),
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
      metrics_labels: Vec::new(),
      drain_secs: None,
      service_custom_name: None,
      tx: tx_write,
      disconnect: std::sync::Arc::new(tokio::sync::Notify::new()),
      connected_at: Instant::now(),
      client_ip: "127.0.0.1".to_string(),
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
      capture: true,
      declared_client_id: None,
      config_notes: Vec::new(),
      last_ping_at: None,
      perms: ClientPerms::master(),
      max_concurrent: None,
      inflight_limiter: None,
      draining: false,
      admin_enabled: true,
      tcp_enabled: false,
      client_version: None,
      client_protocol: None,
      backend_healthy: true,
      backend_probed: true,
      cpu_percent: None,
      rss_bytes: None,
      rtt_ms: None,
      jitter_ms: None,
      reconnects: None,
      priority: 0,
      reported_instance_id: None,
      instance_group: None,
      subscriptions: Vec::new(),
      bandwidth_bps: Arc::new(AtomicU64::new(0)),
      service_name: None,
      public: false,
      public_denied_warned: false,
      visitor_auth: None,
      visitor_auth_denied_warned: false,
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
fn test_path_matches_bind_segment_boundary() {
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
fn test_normalize_path_bind() {
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
fn test_sanitize_uri_strips_query() {
  assert_eq!(sanitize_uri("/api/users?id=42&token=secret"), "/api/users");
  assert_eq!(sanitize_uri("/api"), "/api");
  assert_eq!(sanitize_uri("/api?"), "/api");
  // Multiple '?' → first split wins
  assert_eq!(sanitize_uri("/api?a=1?b=2"), "/api");
}

#[test]
fn test_extract_client_ip_trusted() {
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
fn test_extract_client_ip_untrusted_ignores_headers() {
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

fn test_user_store() -> crate::store::users::UserStore {
  let dir = crate::test_support::test_temp_root().join(format!("users-{}", uuid::Uuid::new_v4()));
  crate::store::users::UserStore::load(&dir.to_string_lossy())
}

fn test_inbox_store() -> crate::store::inbox::InboxStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-inbox-{}", uuid::Uuid::new_v4()));
  crate::store::inbox::InboxStore::load(&dir.to_string_lossy())
}

fn test_token_store() -> TokenStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-store-{}", uuid::Uuid::new_v4()));
  TokenStore::load(&dir.to_string_lossy())
}

fn test_admin_key_store() -> crate::store::admin_keys::AdminKeyStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-adminkeys-{}", uuid::Uuid::new_v4()));
  crate::store::admin_keys::AdminKeyStore::load(&dir.to_string_lossy())
}

fn test_audit_log() -> AuditLog {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-audit-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3)
}

fn test_stats_store() -> StatsStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-stats-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  StatsStore::load(&dir.to_string_lossy())
}

fn test_webhook_store() -> WebhookStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-hooks-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  WebhookStore::load(&dir.to_string_lossy())
}

fn test_org_store() -> crate::store::orgs::OrgStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-orgs-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  crate::store::orgs::OrgStore::load(&dir.to_string_lossy())
}

fn test_delivery_log() -> std::sync::Arc<Mutex<crate::store::webhooks::DeliveryLog>> {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-deliveries-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  std::sync::Arc::new(Mutex::new(crate::store::webhooks::DeliveryLog::load(
    &dir.to_string_lossy(),
  )))
}

fn test_session_store() -> crate::store::sessions::SessionStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-sessions-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  crate::store::sessions::SessionStore::load(&dir.to_string_lossy())
}

fn test_uptime_store() -> crate::store::uptime::UptimeStore {
  let dir =
    crate::test_support::test_temp_root().join(format!("test-uptime-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  crate::store::uptime::UptimeStore::load(&dir.to_string_lossy())
}

fn mock_client(
  hostname_bind: Option<&str>,
  path_bind: Option<&str>,
  override_hostname: Option<&str>,
  override_path: Option<&str>,
) -> ClientHandle {
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    metrics_labels: Vec::new(),
    drain_secs: None,
    service_custom_name: None,
    tx,
    disconnect: std::sync::Arc::new(tokio::sync::Notify::new()),
    connected_at: Instant::now(),
    client_ip: "127.0.0.1".to_string(),
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
    declared_client_id: None,
    config_notes: Vec::new(),
    last_ping_at: None,
    perms: ClientPerms::master(),
    max_concurrent: None,
    inflight_limiter: None,
    draining: false,
    admin_enabled: true,
    tcp_enabled: false,
    client_version: None,
    client_protocol: None,
    backend_healthy: true,
    backend_probed: true,
    cpu_percent: None,
    rss_bytes: None,
    rtt_ms: None,
    jitter_ms: None,
    reconnects: None,
    priority: 0,
    reported_instance_id: None,
    instance_group: None,
    subscriptions: Vec::new(),
    bandwidth_bps: Arc::new(AtomicU64::new(0)),
    service_name: None,
    public: false,
    public_denied_warned: false,
    visitor_auth: None,
    visitor_auth_denied_warned: false,
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
  }
}

#[test]
fn test_share_token_roundtrip() {
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
fn test_apply_settings_overrides() {
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
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
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
    access_log_sample_rate: 1.0,
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
  assert_eq!(c.auth_credentials.as_deref(), Some("user:pass"));
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
  assert_eq!(c2.auth_credentials, None);
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
fn test_normalize_random_subdomain_pattern() {
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
fn test_find_affinity_match() {
  let mut clients = HashMap::new();
  let mut a = mock_client(None, None, None, None);
  a.reported_instance_id = Some("instance-a".to_string());
  let b = mock_client(None, None, None, None);
  clients.insert("conn-a".to_string(), a);
  clients.insert("conn-b".to_string(), b);
  let pool = vec!["conn-a".to_string(), "conn-b".to_string()];

  // Matches by instance ID (survives reconnects) and by connection ID.
  assert_eq!(
    find_affinity_match(&pool, &clients, "instance-a"),
    Some("conn-a".to_string())
  );
  assert_eq!(
    find_affinity_match(&pool, &clients, "conn-b"),
    Some("conn-b".to_string())
  );
  // Unknown affinity falls back to rotation (None).
  assert_eq!(find_affinity_match(&pool, &clients, "gone"), None);
  // A client that left the pool no longer matches.
  assert_eq!(
    find_affinity_match(&["conn-b".to_string()], &clients, "instance-a"),
    None
  );
}

#[test]
fn test_method_retryable() {
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
fn test_apply_lb_strategy_primary_standby() {
  let mut clients = HashMap::new();
  let primary = mock_client(None, None, None, None);
  let mut standby = mock_client(None, None, None, None);
  standby.priority = 1;
  clients.insert("primary".to_string(), primary);
  clients.insert("standby".to_string(), standby);

  let pool = vec!["primary".to_string(), "standby".to_string()];
  // Round-robin keeps the whole pool.
  assert_eq!(
    apply_lb_strategy(pool.clone(), &clients, LbStrategy::RoundRobin).len(),
    2
  );
  // Primary-standby narrows to the lowest priority tier.
  assert_eq!(
    apply_lb_strategy(pool, &clients, LbStrategy::PrimaryStandby),
    vec!["primary".to_string()]
  );
  // Once the primary is out of the pool, the standby takes over.
  assert_eq!(
    apply_lb_strategy(
      vec!["standby".to_string()],
      &clients,
      LbStrategy::PrimaryStandby
    ),
    vec!["standby".to_string()]
  );
}

#[test]
fn test_select_client_pool_excludes_unhealthy() {
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
  assert_eq!(pool, vec!["fresh".to_string()]);

  // The stale client recovers with a new ping -> back in the pool
  clients.get_mut("stale").unwrap().last_ping_at = Some(Instant::now());
  let (pool, _) = select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
  assert_eq!(pool.len(), 2);
}

#[test]
fn test_ip_allowed() {
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
fn test_normalize_hostname_bind() {
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
fn test_extract_request_host() {
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
fn test_select_client_pool_hostname_routing() {
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
  assert_eq!(pool, vec!["a".to_string()]);
  assert_eq!(key, (Some("a.example.com".to_string()), None));

  // Unknown host → falls back to unbound client
  let (pool, key) =
    select_client_pool(&clients, "/", Some("c.example.com"), false, TEST_THRESHOLD).unwrap();
  assert_eq!(pool, vec!["unbound".to_string()]);
  assert_eq!(key, (None, None));

  // Strict mode: unknown host → no client at all
  assert!(select_client_pool(&clients, "/", Some("c.example.com"), true, TEST_THRESHOLD).is_none());
  // Strict mode: matching host still works
  let (pool, _) =
    select_client_pool(&clients, "/", Some("b.example.com"), true, TEST_THRESHOLD).unwrap();
  assert_eq!(pool, vec!["b".to_string()]);
  // Strict mode: no Host header → no client
  assert!(select_client_pool(&clients, "/", None, true, TEST_THRESHOLD).is_none());
}

#[test]
fn test_select_client_pool_hostname_and_path_combined() {
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
  assert_eq!(pool, vec!["host-api".to_string()]);
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
  assert_eq!(pool, vec!["host-root".to_string()]);
}

#[test]
fn test_select_client_pool_override_wins() {
  let mut clients = HashMap::new();
  // Client reported no hostname, dashboard overruled it to a.example.com
  clients.insert(
    "overruled".to_string(),
    mock_client(None, None, Some("a.example.com"), None),
  );

  let (pool, _) =
    select_client_pool(&clients, "/", Some("a.example.com"), true, TEST_THRESHOLD).unwrap();
  assert_eq!(pool, vec!["overruled".to_string()]);

  // With the override active, the client is no longer an unbound fallback
  assert!(
    select_client_pool(&clients, "/", Some("x.example.com"), false, TEST_THRESHOLD).is_none()
  );
}

#[test]
fn test_select_client_pool_longest_path_bind_wins() {
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
  assert_eq!(pool, vec!["long".to_string()]);
  assert_eq!(key, (None, Some("/api/v2".to_string())));

  let (pool, _) = select_client_pool(&clients, "/api/other", None, false, TEST_THRESHOLD).unwrap();
  assert_eq!(pool, vec!["short".to_string()]);
}

#[test]
fn test_safe_redirect_path() {
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
fn test_effective_body_limit() {
  use crate::proxy::effective_body_limit;
  // No declared cap: the global limit applies.
  assert_eq!(effective_body_limit(1024, None), 1024);
  // A declared cap tightens the global limit.
  assert_eq!(effective_body_limit(1024, Some(100)), 100);
  // A declared cap can never widen the global limit.
  assert_eq!(effective_body_limit(1024, Some(10_000)), 1024);
}

#[test]
fn test_route_trends_minute_buckets() {
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
// required_role, minimum dashboard role per route
// ---------------------------------------------------------------------------

#[test]
fn test_required_role_self_service_routes() {
  use crate::required_role;
  use crate::store::users::Role;
  use axum::http::Method;
  // Self-service /api/me/* is open to any signed-in role, even for mutations.
  assert_eq!(
    required_role("/api/me/totp/setup", &Method::POST),
    Role::Viewer
  );
  assert_eq!(required_role("/api/me/totp", &Method::DELETE), Role::Viewer);
  assert_eq!(
    required_role("/api/me/passkeys", &Method::GET),
    Role::Viewer
  );
}

#[test]
fn test_required_role_admin_only_routes() {
  use crate::required_role;
  use crate::store::users::Role;
  use axum::http::Method;
  // Routes that can change who controls the server are admin-only, including
  // their GETs.
  for (path, method) in [
    ("/api/users", Method::GET),
    ("/api/users", Method::POST),
    ("/api/users/123", Method::PUT),
    ("/api/settings", Method::GET),
    ("/api/settings", Method::PUT),
    ("/api/export", Method::GET),
    ("/api/import", Method::POST),
    ("/api/sessions", Method::GET),
    ("/api/sessions/abc", Method::DELETE),
    ("/api/orgs", Method::GET),
    ("/api/orgs/o1/quota", Method::PUT),
    ("/api/admin-keys", Method::GET),
    ("/api/admin-keys/k1", Method::DELETE),
  ] {
    assert_eq!(
      required_role(path, &method),
      Role::Admin,
      "{path} {method} must be admin-only"
    );
  }
}

#[test]
fn test_required_role_reads_vs_mutations() {
  use crate::required_role;
  use crate::store::users::Role;
  use axum::http::Method;
  // Generic reads are open to viewers...
  assert_eq!(required_role("/api/stats", &Method::GET), Role::Viewer);
  assert_eq!(required_role("/api/logs", &Method::HEAD), Role::Viewer);
  // ...and generic mutations require operator.
  assert_eq!(required_role("/api/purge", &Method::POST), Role::Operator);
  assert_eq!(
    required_role("/api/tokens/t1", &Method::DELETE),
    Role::Operator
  );
  assert_eq!(
    required_role("/api/clients/c1/enabled", &Method::POST),
    Role::Operator
  );
}

// ---------------------------------------------------------------------------
// observe_service_availability, per-entity uptime snapshot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_observe_service_availability_states() {
  use crate::observe_service_availability;
  use crate::store::uptime::Availability;

  let state = crate::test_support::test_state();

  // No clients → empty snapshot.
  assert!(observe_service_availability(&state).await.is_empty());

  // Healthy client, keyed by its service_name → Up.
  let mut up = mock_client(None, None, None, None);
  up.service_name = Some("web".to_string());
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
  bad_backend.backend_healthy = false;
  state
    .clients
    .write()
    .await
    .insert("c-badbackend".to_string(), bad_backend);

  // Admin-disabled → Degraded as well.
  let mut disabled = mock_client(None, None, None, None);
  disabled.admin_enabled = false;
  disabled.service_name = Some("disabled-svc".to_string());
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
async fn test_observe_service_availability_down_and_best_state_wins() {
  use crate::observe_service_availability;
  use crate::store::uptime::Availability;

  // Short down-threshold so a stale heartbeat marks the client down.
  let mut cfg = crate::test_support::test_config();
  cfg.client_down_threshold = Duration::from_secs(1);
  let state = crate::test_support::test_state_with(cfg);

  // Stale heartbeat → Down.
  let mut stale = mock_client(None, None, None, None);
  stale.service_name = Some("svc".to_string());
  stale.last_ping_at = Some(Instant::now() - Duration::from_secs(120));
  state
    .clients
    .write()
    .await
    .insert("c-stale".to_string(), stale);

  // A second, healthy connection for the SAME entity → the best state wins.
  let mut healthy = mock_client(None, None, None, None);
  healthy.service_name = Some("svc".to_string());
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

// ---------------------------------------------------------------------------
// bind_listener, plain and SO_REUSEPORT TCP binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_bind_listener_plain_and_reuseport() {
  use crate::bind_listener;

  // Plain listener on an ephemeral port.
  let l = bind_listener("127.0.0.1", 0, false)
    .await
    .expect("plain bind");
  assert!(l.local_addr().unwrap().port() > 0);

  // SO_REUSEPORT path over IPv4 (Domain::IPV4 branch).
  let l = bind_listener("127.0.0.1", 0, true)
    .await
    .expect("reuseport v4 bind");
  assert!(l.local_addr().unwrap().ip().is_ipv4());

  // SO_REUSEPORT path over IPv6 (Domain::IPV6 branch). Skipped gracefully on
  // hosts without a loopback ::1.
  if let Ok(l) = bind_listener("::1", 0, true).await {
    assert!(l.local_addr().unwrap().ip().is_ipv6());
  }

  // An unresolvable host returns an error instead of panicking.
  assert!(
    bind_listener("no.such.host.invalid.", 0, true)
      .await
      .is_err()
  );
}

// ---------------------------------------------------------------------------
// verify_audit, the --verify-audit CLI over the audit hash chain
// ---------------------------------------------------------------------------

/// Serializes the tests below that read/write the process-global
/// APERIO_DATA_DIR environment variable.
static AUDIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn restore_data_dir(prev: Option<String>) {
  match prev {
    Some(v) => unsafe { std::env::set_var("APERIO_DATA_DIR", v) },
    None => unsafe { std::env::remove_var("APERIO_DATA_DIR") },
  }
}

#[test]
fn test_verify_audit_intact_and_missing() {
  use crate::verify_audit;
  let _lock = AUDIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let prev = std::env::var("APERIO_DATA_DIR").ok();

  // A freshly written, well-formed audit log verifies intact → exit 0.
  let dir =
    crate::test_support::test_temp_root().join(format!("verify-ok-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  {
    let mut log = AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3);
    log.record("login", "admin", "127.0.0.1", None, "ok");
    log.record("logout", "admin", "127.0.0.1", None, "bye");
  }
  unsafe { std::env::set_var("APERIO_DATA_DIR", &dir) };
  assert_eq!(verify_audit(), 0);

  // A directory with no audit log → nothing to verify → exit 0.
  let empty =
    crate::test_support::test_temp_root().join(format!("verify-empty-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&empty).unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", &empty) };
  assert_eq!(verify_audit(), 0);

  restore_data_dir(prev);
}

#[test]
fn test_verify_audit_detects_tampering_across_generations() {
  use crate::verify_audit;
  let _lock = AUDIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let prev = std::env::var("APERIO_DATA_DIR").ok();

  let dir =
    crate::test_support::test_temp_root().join(format!("verify-bad-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  {
    let mut log = AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3);
    log.record("login", "admin", "127.0.0.1", None, "a");
    log.record("login", "admin", "127.0.0.1", None, "b");
  }
  // Keep an intact rotated generation, then tamper the active file so the
  // verifier walks both files and reports exactly one broken chain → exit 1.
  std::fs::copy(dir.join("audit.jsonl"), dir.join("audit.jsonl.1")).unwrap();
  std::fs::write(
    dir.join("audit.jsonl"),
    "{\"not\":\"a valid chained audit line\"}\n",
  )
  .unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", &dir) };
  assert_eq!(verify_audit(), 1);

  restore_data_dir(prev);
}

#[test]
fn test_verify_audit_reports_unreadable_file() {
  use crate::verify_audit;
  let _lock = AUDIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let prev = std::env::var("APERIO_DATA_DIR").ok();

  // An audit.jsonl that is actually a directory cannot be read as a file, so
  // the verifier reports it as unreadable (the `Err` arm) → exit 1.
  let dir =
    crate::test_support::test_temp_root().join(format!("verify-unread-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", &dir) };
  assert_eq!(verify_audit(), 1);

  restore_data_dir(prev);
}

// ---------------------------------------------------------------------------
// build_state + build_router: the composed app, driven in-process.
// ---------------------------------------------------------------------------

use crate::state::AppState as ComposedState;
use crate::{build_router, build_state};
use axum::Router;
use axum::body::Body as ComposedBody;
use axum::response::Response as ComposedResponse;

/// Boots the real startup path (env -> stores -> state -> router) inside the
/// test process, under the shared config lock, with a throwaway data dir.
fn composed_app<T>(
  f: impl FnOnce(std::sync::Arc<ComposedState>, Router, &tokio::runtime::Runtime) -> T,
) -> T {
  let _lock = crate::test_support::config_lock();
  let dir = crate::test_support::test_temp_root().join(format!("boot-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let vars = [
    ("APERIO_SERVER_TOKEN", "0123456789abcdef0123456789abcdef"),
    ("APERIO_DATA_DIR", dir.to_str().unwrap()),
    ("APERIO_METRICS", "1"),
  ];
  for (k, v) in vars {
    unsafe { std::env::set_var(k, v) };
  }
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let (state, app) = rt.block_on(async {
    let bundle = build_state().await.expect("a clean env must build");
    let app = build_router(bundle.state.clone(), bundle.metrics_enabled);
    (bundle.state, app)
  });
  let out = f(state, app, &rt);
  for (k, _) in vars {
    unsafe { std::env::remove_var(k) };
  }
  out
}

/// One in-process request against the composed router. The connect-info the
/// serve loop would attach per socket is injected as an extension, since no
/// socket exists here.
async fn drive(app: &Router, mut request: axum::http::Request<ComposedBody>) -> ComposedResponse {
  use tower::ServiceExt;
  request
    .extensions_mut()
    .insert(axum::extract::connect_info::ConnectInfo(
      std::net::SocketAddr::from(([127, 0, 0, 1], 40000)),
    ));
  app.clone().oneshot(request).await.unwrap()
}

fn get_req(path: &str) -> axum::http::Request<ComposedBody> {
  axum::http::Request::builder()
    .uri(path)
    .body(ComposedBody::empty())
    .unwrap()
}

#[test]
fn the_composed_router_answers_its_own_surface() {
  composed_app(|state, app, rt| {
    rt.block_on(async {
      // Liveness needs no credential; monitors depend on that.
      let resp = drive(&app, get_req("/aperio/health")).await;
      assert_eq!(resp.status(), StatusCode::OK);

      // The container probes, also uncredentialed.
      let resp = drive(&app, get_req("/aperio/healthz")).await;
      assert_eq!(resp.status(), StatusCode::OK);
      let resp = drive(&app, get_req("/aperio/readyz")).await;
      assert_eq!(resp.status(), StatusCode::OK);

      // Readiness is the one that turns on a shutdown signal, so a load
      // balancer stops sending traffic while the process is still serving what
      // it already has. Liveness must not: restarting here would kill the
      // drain it is meant to protect.
      // send_replace, not send: with no subscriber a plain send fails and
      // leaves the value untouched, which would make this pass for the wrong
      // reason.
      state.shutdown.send_replace(true);
      let resp = drive(&app, get_req("/aperio/readyz")).await;
      assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
      let resp = drive(&app, get_req("/aperio/healthz")).await;
      assert_eq!(resp.status(), StatusCode::OK);
      state.shutdown.send_replace(false);

      // The admin 404 fence: a path matching nothing in the namespace is a
      // 404, never proxied to a tunnel client.
      let resp = drive(&app, get_req("/aperio/api/definitely-not-a-route")).await;
      assert_eq!(resp.status(), StatusCode::NOT_FOUND);

      // The trailing-slash redirect keeps the query string.
      let resp = drive(&app, get_req("/aperio/?tab=tokens")).await;
      assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
      assert_eq!(
        resp.headers().get("location").unwrap(),
        "/aperio?tab=tokens"
      );

      // The dashboard API without a session is refused, not served.
      let resp = drive(&app, get_req("/aperio/api/stats")).await;
      assert!(
        resp.status() == StatusCode::UNAUTHORIZED || resp.status().is_redirection(),
        "unauthenticated admin API answered {}",
        resp.status()
      );

      // The metrics endpoint exists (APERIO_METRICS=1) and is gated by its
      // token rather than open.
      let resp = drive(&app, get_req("/aperio/metrics")).await;
      assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

      // A request through the proxy fallback is answered by the composed
      // stack; what it answers is routing's business, the assertion is that
      // it answered and that no handler panicked (the catch-panic layer
      // would turn that into a 500).
      let resp = drive(&app, get_req("/")).await;
      assert_ne!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

      // The state the router serves is the one build_state assembled.
      assert!(state.dashboard_enabled);
      assert!(state.config().metrics_token.is_some());
    });
  });
}

#[test]
fn build_state_refuses_a_partial_trust_configuration() {
  let _lock = crate::test_support::config_lock();
  let dir = crate::test_support::test_temp_root().join(format!("boot-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  unsafe {
    std::env::set_var("APERIO_SERVER_TOKEN", "0123456789abcdef0123456789abcdef");
    std::env::set_var("APERIO_DATA_DIR", dir.to_str().unwrap());
    std::env::set_var("APERIO_TRUSTED_PROXIES", "not-an-ip-range");
  }
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let refused = rt.block_on(build_state());
  unsafe {
    std::env::remove_var("APERIO_TRUSTED_PROXIES");
    std::env::remove_var("APERIO_SERVER_TOKEN");
    std::env::remove_var("APERIO_DATA_DIR");
  }
  assert!(
    refused.is_none(),
    "a partial trusted-proxy list must refuse startup"
  );
}

// ---------------------------------------------------------------------------
// The background loops, one beat at a time.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_uptime_tick_observes_and_accrues() {
  use crate::{flush_stats_once, uptime_tick_once};
  let state = std::sync::Arc::new(test_state());
  state.clients.write().await.insert(
    "c1".to_string(),
    crate::test_support::mock_client(Some("up.example.com"), None, None, None),
  );
  uptime_tick_once(&state).await;
  let snap = state.uptime.lock().await.snapshot();
  // The entity key is the service name / instance id / connection id, in
  // that order of preference; the mock declares none, so its connection id.
  let entity = snap.get("c1").expect("the tick recorded the live service");
  assert_eq!(entity.status, crate::store::uptime::Availability::Up);
  // And the flush beat writes it out without complaint.
  flush_stats_once(&state).await;
}

#[tokio::test]
async fn one_expiry_tick_warns_once_and_rearms_on_refresh() {
  use crate::token_expiry_tick_once;
  let state = std::sync::Arc::new(test_state());
  let now = crate::store::tokens::now_secs();
  let (expiring_soon, _) = {
    let mut store = state.token_store.lock().await;
    store.create(
      "expiring".into(),
      vec![],
      vec![],
      vec![],
      Some(1800), // expires in half an hour, inside the 24h window
      None,
      None,
      false,
      false,
      false,
      None,
      vec![],
      None,
    )
  };
  {
    let mut store = state.token_store.lock().await;
    store.create(
      "fresh".into(),
      vec![],
      vec![],
      vec![],
      Some(7 * 24 * 3600), // a week out, outside the window
      None,
      None,
      false,
      false,
      false,
      None,
      vec![],
      None,
    );
  }

  let mut warned = std::collections::HashSet::new();
  token_expiry_tick_once(&state, 24 * 3600, now, &mut warned).await;
  assert_eq!(warned.len(), 1, "only the token inside the window");

  let events = state.audit.lock().await.recent();
  let expiring_events = events
    .iter()
    .filter(|e| e.event == "token_expiring")
    .count();
  assert_eq!(expiring_events, 1);
  assert!(
    events
      .iter()
      .any(|e| e.event == "token_expiring" && e.details.contains("name=expiring")),
    "the warning names the token"
  );

  // A second beat with the same set warns nobody again.
  token_expiry_tick_once(&state, 24 * 3600, now, &mut warned).await;
  let events = state.audit.lock().await.recent();
  assert_eq!(
    events
      .iter()
      .filter(|e| e.event == "token_expiring")
      .count(),
    1,
    "once per token per expiry"
  );

  // A refresh moves expires_at, which re-arms the warning: the old entry is
  // swept the beat after the recorded expiry passes.
  let past_old_expiry = now + 3600;
  token_expiry_tick_once(&state, 24 * 3600, past_old_expiry, &mut warned).await;
  assert!(
    warned.is_empty(),
    "a passed expiry is forgotten so a refreshed token can warn again"
  );
  let _ = expiring_soon;
}

#[test]
fn one_hot_reload_tick_applies_a_changed_file_and_audits_it() {
  use crate::hot_reload_tick_once;
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
      let _ = crate::config_file::reload();
    }
  }
  let _cleanup = Cleanup;
  let file =
    crate::test_support::test_temp_root().join(format!("hotreload-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(&file, "gateway_timeout: 10\n").unwrap();
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", file.to_str().unwrap()) };
  crate::config_file::load();

  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let state = std::sync::Arc::new(test_state());
    let mtime = std::fs::metadata(&file)
      .ok()
      .and_then(|m| m.modified().ok());

    // Nothing moved: the beat is a no-op and keeps the mtime.
    let same = hot_reload_tick_once(&state, &file, mtime).await;
    assert_eq!(same, mtime);
    assert!(
      state
        .audit
        .lock()
        .await
        .recent()
        .iter()
        .all(|e| e.event != "config_reloaded"),
      "an unchanged file reloads nothing"
    );

    // The file changes: the beat re-applies it, the live setting moves, and
    // the audit trail says which key. `None` as the remembered mtime stands
    // in for "it moved": filesystem mtime granularity is up to a second, and
    // a test must not sleep its way across it.
    std::fs::write(&file, "gateway_timeout: 42\n").unwrap();
    let next = hot_reload_tick_once(&state, &file, None).await;
    assert!(
      next.is_some(),
      "the new mtime is what the next beat compares to"
    );
    assert_eq!(state.config().gateway_timeout, Duration::from_secs(42));
    let events = state.audit.lock().await.recent();
    let entry = events
      .iter()
      .find(|e| e.event == "config_reloaded")
      .expect("the reload is audited");
    assert!(
      entry.details.contains("gateway_timeout"),
      "{}",
      entry.details
    );
  });
}

#[tokio::test]
async fn bind_listener_binds_plain_reuseport_and_reports_a_taken_port() {
  use crate::bind_listener;
  // Plain bind on an ephemeral port.
  let plain = bind_listener("127.0.0.1", 0, false).await.unwrap();
  let taken = plain.local_addr().unwrap().port();

  // The SO_REUSEPORT path builds its socket by hand; prove it yields a
  // working listener too.
  let shared = bind_listener("127.0.0.1", 0, true).await.unwrap();
  assert!(shared.local_addr().unwrap().port() > 0);

  // A port someone plainly holds is refused for a plain second bind: this is
  // the branch serve_until_shutdown turns into its startup error.
  assert!(bind_listener("127.0.0.1", taken, false).await.is_err());

  // And a hostname that resolves to nothing is an error, not a hang.
  assert!(
    bind_listener("definitely-not-a-host.invalid", 0, true)
      .await
      .is_err()
  );
}

// ---------------------------------------------------------------------------
// shutdown_drain_budget (planned_features #58)
// ---------------------------------------------------------------------------

#[test]
fn shutdown_drain_defaults_to_not_waiting() {
  // Unset is the behavior the server has always had: notify, flush, close.
  // Waiting is something an operator asks for, not something a version bump
  // starts doing to their deploys.
  assert_eq!(
    shutdown_drain_budget(None, false, []),
    std::time::Duration::ZERO
  );
}

#[test]
fn shutdown_drain_uses_the_configured_number_over_anything_announced() {
  // The operator's number wins even when clients ask for more: this is the
  // one place the platform's SIGKILL timer is known, and it is not known here.
  assert_eq!(
    shutdown_drain_budget(Some(5), true, [60, 90]),
    std::time::Duration::from_secs(5)
  );
}

#[test]
fn shutdown_drain_auto_takes_the_longest_client_and_caps_it() {
  // The longest, not the average: the drain is over when the slowest client
  // has finished, and an average cuts short exactly the one that needed time.
  assert_eq!(
    shutdown_drain_budget(None, true, [3, 12, 7]),
    std::time::Duration::from_secs(12)
  );
  // A client is not the operator, so what it announces cannot hold the
  // process past what the platform will wait before SIGKILL.
  assert_eq!(
    shutdown_drain_budget(None, true, [3600]),
    std::time::Duration::from_secs(SHUTDOWN_DRAIN_AUTO_CAP)
  );
  // `auto` with nothing connected has nothing to size itself from.
  assert_eq!(
    shutdown_drain_budget(None, true, []),
    std::time::Duration::ZERO
  );
}
