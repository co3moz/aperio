//! Shared fixtures for the server unit tests: a fully-populated [`AppState`]
//! backed by throwaway temp-dir stores, a mock tunnel client, and helpers to
//! forge authenticated dashboard requests (session cookie / master token).
//!
//! Only compiled under `cfg(test)`. Individual `*_tests.rs` modules pull what
//! they need with `use crate::test_support::*`.
#![allow(dead_code)]

use crate::settings::{FailoverMode, LbStrategy, ServerConfig, SettingsOverrides};
use crate::state::{
  AppState, ClientHandle, ClientPerms, ConnectionState, DurationHistogram, ServerStats,
  SessionInfo, TunnelResponse,
};
use crate::store::audit::AuditLog;
use crate::store::sessions::SessionStore;
use crate::store::stats::StatsStore;
use crate::store::tokens::TokenStore;
use crate::store::users::Role;
use crate::store::webhooks::WebhookStore;
use axum::extract::ws::Message;
use axum::http::{HeaderMap, HeaderValue};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};

/// Generous health threshold so mock clients (which never ping) stay eligible.
pub(crate) const TEST_THRESHOLD: Duration = Duration::from_secs(3600);

fn tmp(prefix: &str) -> String {
  let dir = std::env::temp_dir().join(format!("aperio-test-{prefix}-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  dir.to_string_lossy().into_owned()
}

pub(crate) fn test_user_store() -> crate::store::users::UserStore {
  crate::store::users::UserStore::load(&tmp("users"))
}
pub(crate) fn test_inbox_store() -> crate::store::inbox::InboxStore {
  crate::store::inbox::InboxStore::load(&tmp("inbox"))
}
pub(crate) fn test_token_store() -> TokenStore {
  TokenStore::load(&tmp("store"))
}
pub(crate) fn test_admin_key_store() -> crate::store::admin_keys::AdminKeyStore {
  crate::store::admin_keys::AdminKeyStore::load(&tmp("adminkeys"))
}
pub(crate) fn test_audit_log() -> AuditLog {
  AuditLog::load(&tmp("audit"), 10 * 1024 * 1024, 3)
}
pub(crate) fn test_stats_store() -> StatsStore {
  StatsStore::load(&tmp("stats"))
}
pub(crate) fn test_webhook_store() -> WebhookStore {
  WebhookStore::load(&tmp("hooks"))
}
pub(crate) fn test_org_store() -> crate::store::orgs::OrgStore {
  crate::store::orgs::OrgStore::load(&tmp("orgs"))
}
pub(crate) fn test_delivery_log() -> Arc<Mutex<crate::store::webhooks::DeliveryLog>> {
  Arc::new(Mutex::new(crate::store::webhooks::DeliveryLog::load(&tmp(
    "deliveries",
  ))))
}
pub(crate) fn test_session_store() -> SessionStore {
  SessionStore::load(&tmp("sessions"))
}
pub(crate) fn test_uptime_store() -> crate::store::uptime::UptimeStore {
  crate::store::uptime::UptimeStore::load(&tmp("uptime"))
}

/// A minimal, fully-populated server config suitable for unit tests. Token is
/// `"test"`; rate limiting is generous; auth and TLS features are off.
pub(crate) fn test_config() -> ServerConfig {
  ServerConfig {
    token: "test".to_string(),
    gateway_timeout: Duration::from_secs(1),
    gateway_response_timeout: Duration::from_secs(1),
    max_body_size: 1024 * 1024,
    max_tunnels: 8,
    ip_limit_max: 1000.0,
    ip_limit_refill: 100.0,
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token: None,
    random_subdomain_suffix: None,
    client_down_threshold: TEST_THRESHOLD,
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
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
  }
}

/// A full [`AppState`] with the given config and fresh throwaway stores.
pub(crate) fn test_state_with(config: ServerConfig) -> AppState {
  let (client_connected_tx, _) = watch::channel(false);
  AppState {
    clients: Mutex::new(HashMap::new()),
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
    settings_path: std::env::temp_dir().join(format!(
      "aperio-test-settings-{}.json",
      uuid::Uuid::new_v4()
    )),
    dashboard_enabled: true,
    shutdown: watch::channel(false).0,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    active_ws_connections: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(test_session_store()),
    rate_limiter: Mutex::new(HashMap::new()),
    login_lockout: Mutex::new(crate::auth::LockoutTracker::new(5, Duration::from_secs(60))),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    token_seen_ips: Mutex::new(HashMap::new()),
    route_rate: Mutex::new(HashMap::new()),
    last_session_gc: Mutex::new(Instant::now()),
    last_rate_gc: Mutex::new(Instant::now()),
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
    cache_inflight: std::sync::Mutex::new(HashMap::new()),
    stage_stats: Mutex::new(crate::state::StageStats::default()),
    endpoint_stats: Mutex::new(crate::state::EndpointStats::default()),
    route_trends: Mutex::new(crate::state::RouteTrends::default()),
    maintenance: Mutex::new(HashMap::new()),
    access_log: None,
    access_log_path: None,
    duration_histogram: DurationHistogram::default(),
  }
}

/// A full [`AppState`] with the default [`test_config`].
pub(crate) fn test_state() -> AppState {
  test_state_with(test_config())
}

/// A mock tunnel client handle (master perms, admin enabled, healthy).
pub(crate) fn mock_client(
  hostname_bind: Option<&str>,
  path_bind: Option<&str>,
  override_hostname: Option<&str>,
  override_path: Option<&str>,
) -> ClientHandle {
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    tx,
    disconnect: Arc::new(tokio::sync::Notify::new()),
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
    override_hostname_bind: override_hostname.map(|s| s.to_string()),
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
    priority: 0,
    reported_instance_id: None,
    instance_group: None,
    bandwidth_bps: Arc::new(AtomicU64::new(0)),
    service_name: None,
    public: false,
    public_denied_warned: false,
    visitor_auth: None,
    visitor_auth_denied_warned: false,
    allowed_ips: Vec::new(),
    allowed_ips_invalid_warned: false,
    tunnels: Vec::new(),
    cache: false,
    resilience: false,
    max_request_body: None,
    response_timeout: None,
    webhook_inbox: false,
    denied: None,
    recent_failures: VecDeque::new(),
    ejected_until: None,
  }
}

/// Seeds a dashboard session with the given role/username/org and returns the
/// raw session token (set it as `aperio_session=<token>` in the Cookie header).
pub(crate) async fn seed_session(
  state: &AppState,
  role: Role,
  username: Option<&str>,
  org: Option<String>,
) -> String {
  let token = uuid::Uuid::new_v4().to_string();
  let now = crate::store::sessions::now_secs();
  state.sessions.lock().await.insert(
    &token,
    SessionInfo {
      expires_at: now + 86400,
      created_at: now,
      ip: Some("127.0.0.1".to_string()),
      user_agent: None,
      scope_host: None,
      username: username.map(|u| u.to_string()),
      role,
      selected_org: org,
      bound_org: None,
    },
  );
  token
}

/// A Cookie header carrying the given session token.
pub(crate) fn cookie_headers(token: &str) -> HeaderMap {
  let mut h = HeaderMap::new();
  h.insert(
    "cookie",
    HeaderValue::from_str(&format!("aperio_session={token}")).unwrap(),
  );
  h
}

/// Seeds a master-admin session (built-in admin, no named user, master org)
/// and returns its Cookie header.
pub(crate) async fn admin_headers(state: &AppState) -> HeaderMap {
  let token = seed_session(state, Role::Admin, None, None).await;
  cookie_headers(&token)
}

/// A header map carrying the master bearer token (`test`).
pub(crate) fn master_token_headers() -> HeaderMap {
  let mut h = HeaderMap::new();
  h.insert("authorization", HeaderValue::from_static("Bearer test"));
  h
}

/// A localhost `ConnectInfo` peer address for handlers that require one.
pub(crate) fn test_peer() -> std::net::SocketAddr {
  std::net::SocketAddr::from(([127, 0, 0, 1], 40000))
}

/// Decodes a JSON response body into a `serde_json::Value`.
pub(crate) async fn json_body(resp: axum::response::Response) -> serde_json::Value {
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// A no-op tunnel response for handlers that need a [`TunnelResponse`].
pub(crate) fn ok_tunnel_response() -> TunnelResponse {
  TunnelResponse {
    status: 200,
    headers: Vec::new(),
    body: None,
    trailers: None,
    stream_rx: None,
    timings: None,
  }
}
