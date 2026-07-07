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
use tokio::sync::{Mutex, Semaphore, mpsc, watch};

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
    ip_limit_max: 2.0,
    ip_limit_refill: 0.0, // No refill for testing strict burst limit
    auth_credentials: None,
    trust_proxy: false,
    real_ip_header: None,
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token: None,
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
  };

  let (client_connected_tx, _) = watch::channel(false);
  let state = AppState {
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
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: std::env::temp_dir().join(format!(
      "aperio-test-settings-{}.json",
      uuid::Uuid::new_v4()
    )),
    concurrency_semaphore: Semaphore::new(10),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(HashMap::new()),
    rate_limiter: Mutex::new(HashMap::new()),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    last_session_gc: Mutex::new(Instant::now()),
    last_rate_gc: Mutex::new(Instant::now()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(test_token_store()),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(test_audit_log()),
    persistent_stats: Mutex::new(test_stats_store()),
    webhook_store: Mutex::new(test_webhook_store()),
    oidc: None,
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    maintenance: Mutex::new(std::collections::HashSet::new()),
    access_log: None,
    duration_histogram: DurationHistogram::default(),
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
    ip_limit_max: 100.0,
    ip_limit_refill: 10.0,
    auth_credentials: None,
    trust_proxy: false,
    real_ip_header: None,
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token: None,
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
  };

  let (client_connected_tx, _) = watch::channel(false);
  let state = Arc::new(AppState {
    clients: Mutex::new(HashMap::new()),
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
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: std::env::temp_dir().join(format!(
      "aperio-test-settings-{}.json",
      uuid::Uuid::new_v4()
    )),
    concurrency_semaphore: Semaphore::new(10),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(HashMap::new()),
    rate_limiter: Mutex::new(HashMap::new()),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    last_session_gc: Mutex::new(Instant::now()),
    last_rate_gc: Mutex::new(Instant::now()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(test_token_store()),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(test_audit_log()),
    persistent_stats: Mutex::new(test_stats_store()),
    webhook_store: Mutex::new(test_webhook_store()),
    oidc: None,
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    maintenance: Mutex::new(std::collections::HashSet::new()),
    access_log: None,
    duration_histogram: DurationHistogram::default(),
  });

  let response = proxy_handler(
    State(state),
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))),
    axum::extract::Request::new(Body::empty()),
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
    ip_limit_max: 100.0,
    ip_limit_refill: 10.0,
    auth_credentials: None,
    trust_proxy: false,
    real_ip_header: None,
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token: None,
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
  };

  let (client_connected_tx, _) = watch::channel(true);
  let state = Arc::new(AppState {
    clients: Mutex::new(HashMap::new()),
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
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: std::env::temp_dir().join(format!(
      "aperio-test-settings-{}.json",
      uuid::Uuid::new_v4()
    )),
    concurrency_semaphore: Semaphore::new(10),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(HashMap::new()),
    rate_limiter: Mutex::new(HashMap::new()),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    last_session_gc: Mutex::new(Instant::now()),
    last_rate_gc: Mutex::new(Instant::now()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(test_token_store()),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(test_audit_log()),
    persistent_stats: Mutex::new(test_stats_store()),
    webhook_store: Mutex::new(test_webhook_store()),
    oidc: None,
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    maintenance: Mutex::new(std::collections::HashSet::new()),
    access_log: None,
    duration_histogram: DurationHistogram::default(),
  });

  let (tx_write, mut rx_write) = mpsc::channel::<Message>(100);
  let client_req_count = Arc::new(AtomicU64::new(0));

  state.clients.lock().await.insert(
    "mock-client-1".to_string(),
    ClientHandle {
      tx: tx_write,
      connected_at: Instant::now(),
      client_ip: "127.0.0.1".to_string(),
      request_count: client_req_count,
      declared_path: None,
      assigned_path: None,
      declared_hostname: None,
      assigned_hostnames: Vec::new(),
      random_hostname: None,
      override_path_bind: None,
      override_hostname_bind: None,
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
      priority: 0,
      reported_instance_id: None,
      bandwidth_bps: Arc::new(AtomicU64::new(0)),
      service_name: None,
      public: false,
      public_denied_warned: false,
      tunnels: Vec::new(),
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
          status: 200,
          headers,
          body: Some(base64::prelude::BASE64_STANDARD.encode(r#"{"status":"ok"}"#)),
          stream_rx: None,
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
  assert_eq!(extract_client_ip(&headers, direct, true, None), direct);

  // X-Forwarded-For with single IP
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, None),
    "198.51.100.10".parse::<IpAddr>().unwrap()
  );

  // X-Forwarded-For with chained proxies → leftmost (original client)
  let mut headers = HeaderMap::new();
  headers.insert(
    "x-forwarded-for",
    HeaderValue::from_static("198.51.100.10, 10.0.0.1, 10.0.0.2"),
  );
  assert_eq!(
    extract_client_ip(&headers, direct, true, None),
    "198.51.100.10".parse::<IpAddr>().unwrap()
  );

  // X-Real-IP fallback when X-Forwarded-For absent
  let mut headers = HeaderMap::new();
  headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, None),
    "198.51.100.20".parse::<IpAddr>().unwrap()
  );

  // Malformed X-Forwarded-For → fallback
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
  assert_eq!(extract_client_ip(&headers, direct, true, None), direct);

  // A configured real-IP header (e.g. CF-Connecting-IP) wins over
  // X-Forwarded-For, which chained proxies often reset to the CDN edge.
  let mut headers = HeaderMap::new();
  headers.insert(
    "x-forwarded-for",
    HeaderValue::from_static("162.158.14.210"),
  );
  headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.18"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, Some("cf-connecting-ip")),
    "203.0.113.18".parse::<IpAddr>().unwrap()
  );
  // ...but only when trust_proxy is on.
  assert_eq!(
    extract_client_ip(&headers, direct, false, Some("cf-connecting-ip")),
    direct
  );
}

#[test]
fn test_extract_client_ip_untrusted_ignores_headers() {
  let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

  // When trust_proxy is false, spoofed X-Forwarded-For must be ignored.
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
  assert_eq!(extract_client_ip(&headers, direct, false, None), direct);

  // Spoofed X-Real-IP must also be ignored.
  let mut headers = HeaderMap::new();
  headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
  assert_eq!(extract_client_ip(&headers, direct, false, None), direct);

  // No headers → fallback to socket address
  let headers = HeaderMap::new();
  assert_eq!(extract_client_ip(&headers, direct, false, None), direct);
}

/// Generous health threshold so mock clients (no pings) stay eligible.
const TEST_THRESHOLD: Duration = Duration::from_secs(3600);

fn test_token_store() -> TokenStore {
  let dir = std::env::temp_dir().join(format!("aperio-test-store-{}", uuid::Uuid::new_v4()));
  TokenStore::load(&dir.to_string_lossy())
}

fn test_audit_log() -> AuditLog {
  let dir = std::env::temp_dir().join(format!("aperio-test-audit-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3)
}

fn test_stats_store() -> StatsStore {
  let dir = std::env::temp_dir().join(format!("aperio-test-stats-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  StatsStore::load(&dir.to_string_lossy())
}

fn test_webhook_store() -> WebhookStore {
  let dir = std::env::temp_dir().join(format!("aperio-test-hooks-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  WebhookStore::load(&dir.to_string_lossy())
}

fn mock_client(
  hostname_bind: Option<&str>,
  path_bind: Option<&str>,
  override_hostname: Option<&str>,
  override_path: Option<&str>,
) -> ClientHandle {
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    tx,
    connected_at: Instant::now(),
    client_ip: "127.0.0.1".to_string(),
    request_count: Arc::new(AtomicU64::new(0)),
    declared_path: path_bind.map(|s| s.to_string()),
    assigned_path: None,
    declared_hostname: hostname_bind.map(|s| s.to_string()),
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
    priority: 0,
    reported_instance_id: None,
    bandwidth_bps: Arc::new(AtomicU64::new(0)),
    service_name: None,
    public: false,
    public_denied_warned: false,
    tunnels: Vec::new(),
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
    ip_limit_max: 100.0,
    ip_limit_refill: 5.0,
    auth_credentials: None,
    trust_proxy: false,
    real_ip_header: None,
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token: None,
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
