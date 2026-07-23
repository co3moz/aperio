use super::*;

use crate::settings::{FailoverMode, LbStrategy, ServerConfig, SettingsOverrides};
use crate::state::{
  ClientHandle, ClientPerms, ConnectionState, DurationHistogram, EndpointStats, RequestTimeline,
  RouteTrends, ServerStats, StageStats,
};
use axum::extract::ws::Message;
use axum::http::{HeaderMap, HeaderValue};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};

fn test_config(metrics_token: Option<String>) -> ServerConfig {
  ServerConfig {
    token: "test".to_string(),
    gateway_timeout: Duration::from_secs(1),
    gateway_response_timeout: Duration::from_secs(1),
    max_body_size: 1024,
    max_tunnels: 1,
    ip_limit_max: 100.0,
    ip_limit_refill: 1.0,
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token,
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
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
  }
}

fn tmp_dir(kind: &str) -> String {
  let dir = std::env::temp_dir().join(format!(
    "aperio-metrics-test-{}-{}",
    kind,
    uuid::Uuid::new_v4()
  ));
  let _ = std::fs::create_dir_all(&dir);
  dir.to_string_lossy().into_owned()
}

fn build_state(config: ServerConfig) -> Arc<AppState> {
  let (client_connected_tx, _) = watch::channel(false);
  Arc::new(AppState {
    clients: Mutex::new(HashMap::new()),
    client_connected: client_connected_tx,
    connection_state: Mutex::new(ConnectionState {
      connected: false,
      last_disconnect: None,
    }),
    server_start_time: Instant::now(),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 7,
      successful_requests: 5,
      failed_requests: 2,
      total_bytes_transferred: 4242,
    }),
    recent_logs: Mutex::new(VecDeque::new()),
    traffic_tx: tokio::sync::broadcast::channel(16).0,
    config_store: std::sync::RwLock::new(Arc::new(config.clone())),
    config_env_defaults: Arc::new(config),
    settings_overrides: Mutex::new(SettingsOverrides::default()),
    settings_path: std::env::temp_dir().join(format!(
      "aperio-metrics-settings-{}.json",
      uuid::Uuid::new_v4()
    )),
    dashboard_enabled: true,
    shutdown: watch::channel(false).0,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    active_ws_connections: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(crate::store::sessions::SessionStore::load(&tmp_dir(
      "sessions",
    ))),
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
    token_store: Mutex::new(crate::store::tokens::TokenStore::load(&tmp_dir("tokens"))),
    admin_key_store: Mutex::new(crate::store::admin_keys::AdminKeyStore::load(&tmp_dir(
      "adminkeys",
    ))),
    inbox_store: Mutex::new(crate::store::inbox::InboxStore::load(&tmp_dir("inbox"))),
    users: Mutex::new(crate::store::users::UserStore::load(&tmp_dir("users"))),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::new()),
    audit: Mutex::new(crate::store::audit::AuditLog::load(
      &tmp_dir("audit"),
      10 * 1024 * 1024,
      3,
    )),
    persistent_stats: Mutex::new(crate::store::stats::StatsStore::load(&tmp_dir("stats"))),
    webhook_deliveries: Arc::new(Mutex::new(crate::store::webhooks::DeliveryLog::load(
      &tmp_dir("deliveries"),
    ))),
    webhook_store: Mutex::new(crate::store::webhooks::WebhookStore::load(&tmp_dir(
      "hooks",
    ))),
    org_store: Mutex::new(crate::store::orgs::OrgStore::load(&tmp_dir("orgs"))),
    uptime: Mutex::new(crate::store::uptime::UptimeStore::load(&tmp_dir("uptime"))),
    webauthn: None,
    webauthn_ceremonies: Mutex::new(crate::webauthn::WebauthnCeremonies::default()),
    oidc: None,
    org_oidc: Mutex::new(HashMap::new()),
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    udp_streams: Mutex::new(HashMap::new()),
    response_cache: Mutex::new(crate::cache::ResponseCache::default()),
    cache_inflight: std::sync::Mutex::new(std::collections::HashMap::new()),
    stage_stats: Mutex::new(StageStats::default()),
    endpoint_stats: Mutex::new(EndpointStats::default()),
    route_trends: Mutex::new(RouteTrends::default()),
    maintenance: Mutex::new(std::collections::HashMap::new()),
    access_log: None,
    access_log_path: None,
    duration_histogram: DurationHistogram::default(),
  })
}

fn mock_client() -> ClientHandle {
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    tx,
    disconnect: Arc::new(tokio::sync::Notify::new()),
    connected_at: Instant::now(),
    client_ip: "127.0.0.1".to_string(),
    request_count: Arc::new(AtomicU64::new(3)),
    declared_path: None,
    assigned_path: None,
    declared_hostname: None,
    declared_hostnames: Vec::new(),
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

/// A timeline with only server-measured stages (queue + serve populated),
/// enough to register a route in the stage/endpoint windows.
fn timeline(dispatched_us: u64, total_us: u64) -> RequestTimeline {
  RequestTimeline::assemble(dispatched_us, total_us, total_us + 500, None)
}

async fn body_string(resp: Response) -> String {
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  String::from_utf8(bytes.to_vec()).unwrap()
}

async fn body_json(resp: Response) -> serde_json::Value {
  serde_json::from_str(&body_string(resp).await).unwrap()
}

// ---- metrics_handler --------------------------------------------------------

#[tokio::test]
async fn metrics_no_token_renders_all_families() {
  let state = build_state(test_config(None));
  // A connected client makes the per-client loop emit a labelled line.
  state
    .clients
    .lock()
    .await
    .insert("client-1".to_string(), mock_client());
  // Seed per-token / per-hostname persistent counters with a label that
  // exercises every escape branch (backslash, quote, newline).
  {
    let mut ps = state.persistent_stats.lock().await;
    ps.record_request_labeled(
      true,
      100,
      200,
      5,
      Some("a\"b\\c\nd"),
      Some("host\"x\\y\nz"),
      None,
    );
    ps.record_request_labeled(
      false,
      10,
      20,
      3,
      Some("a\"b\\c\nd"),
      Some("host\"x\\y\nz"),
      None,
    );
  }

  let resp = metrics_handler(
    State(state.clone()),
    axum::extract::Query(HashMap::new()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = body_string(resp).await;

  // Core counter/gauge families.
  assert!(body.contains("aperio_requests_total 7"));
  assert!(body.contains("aperio_requests_success_total 5"));
  assert!(body.contains("aperio_requests_failed_total 2"));
  assert!(body.contains("aperio_bytes_transferred_total 4242"));
  assert!(body.contains("aperio_connected_clients 1"));
  assert!(body.contains("aperio_pending_requests 0"));
  assert!(body.contains("aperio_ws_streams_active 0"));
  assert!(body.contains("aperio_uptime_seconds"));
  // Per-client counter line.
  assert!(body.contains("aperio_client_requests_total{client_id=\"client-1\"} 3"));
  // Labelled families are present (render_labeled non-empty branch).
  assert!(body.contains("aperio_token_requests_total"));
  assert!(body.contains("aperio_hostname_requests_total"));
  // Escaping: the label value contains escaped backslash, quote and newline.
  assert!(body.contains("a\\\"b\\\\c\\nd"));
  assert!(body.contains("host\\\"x\\\\y\\nz"));
}

#[tokio::test]
async fn metrics_token_required_rejects_missing() {
  let state = build_state(test_config(Some("m".to_string())));
  let resp = metrics_handler(
    State(state),
    axum::extract::Query(HashMap::new()),
    HeaderMap::new(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn metrics_token_via_bearer_header() {
  let state = build_state(test_config(Some("m".to_string())));
  let mut headers = HeaderMap::new();
  headers.insert("authorization", HeaderValue::from_static("Bearer m"));
  let resp = metrics_handler(State(state), axum::extract::Query(HashMap::new()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_token_via_query_param() {
  let state = build_state(test_config(Some("m".to_string())));
  let mut query = HashMap::new();
  query.insert("token".to_string(), "m".to_string());
  let resp = metrics_handler(State(state), axum::extract::Query(query), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

// ---- stage_stats_handler ----------------------------------------------------

#[tokio::test]
async fn stage_stats_empty_is_array() {
  let state = build_state(test_config(None));
  let Json(rows) = stage_stats_handler(State(state), HeaderMap::new()).await;
  assert!(rows.is_empty());
}

#[tokio::test]
async fn stage_stats_filters_by_org() {
  let state = build_state(test_config(None));
  {
    let mut s = state.stage_stats.lock().await;
    s.record(Some("mine.example.com"), None, &timeline(100, 5000));
    // A route for a different org must not appear for the org-None caller.
    s.record(
      Some("other.example.com"),
      Some("org-b"),
      &timeline(200, 8000),
    );
  }
  let Json(rows) = stage_stats_handler(State(state), HeaderMap::new()).await;
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0]["host"], "mine.example.com");
  // Each route carries the seven stage rows.
  assert_eq!(rows[0]["stages"].as_array().unwrap().len(), 7);
}

// ---- slow_endpoints_handler -------------------------------------------------

#[tokio::test]
async fn slow_endpoints_empty_is_array() {
  let state = build_state(test_config(None));
  let Json(rows) = slow_endpoints_handler(State(state), HeaderMap::new()).await;
  assert!(rows.is_empty());
}

#[tokio::test]
async fn slow_endpoints_filters_by_org_and_min_samples() {
  let state = build_state(test_config(None));
  {
    let mut e = state.endpoint_stats.lock().await;
    // Enough samples (>= ENDPOINT_MIN_SAMPLES) to be reported for org None.
    for i in 0..crate::state::ENDPOINT_MIN_SAMPLES {
      e.record(Some("mine.example.com"), "/api", 200, 10 + i as u64, None);
    }
    // One 5xx to bump the error count.
    e.record(Some("mine.example.com"), "/api", 500, 999, None);
    // A different org's endpoint (also enough samples) must be excluded.
    for _ in 0..crate::state::ENDPOINT_MIN_SAMPLES {
      e.record(Some("other.example.com"), "/x", 200, 5, Some("org-b"));
    }
    // Too few samples for org None: excluded despite matching org.
    e.record(Some("sparse.example.com"), "/y", 200, 1, None);
  }
  let Json(rows) = slow_endpoints_handler(State(state), HeaderMap::new()).await;
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0]["host"], "mine.example.com");
  assert_eq!(rows[0]["path"], "/api");
  assert_eq!(rows[0]["errors"].as_u64().unwrap(), 1);
}

// ---- route_trends_handler ---------------------------------------------------

#[tokio::test]
async fn route_trends_empty_is_array() {
  let state = build_state(test_config(None));
  let Json(rows) = route_trends_handler(State(state), HeaderMap::new()).await;
  assert!(rows.is_empty());
}

#[tokio::test]
async fn route_trends_filters_by_org() {
  let state = build_state(test_config(None));
  let now = crate::store::tokens::now_secs();
  {
    let mut t = state.route_trends.lock().await;
    t.record(Some("mine.example.com"), 200, None, now);
    t.record(Some("mine.example.com"), 503, None, now);
    // Different org, excluded from the org-None view.
    t.record(Some("other.example.com"), 200, Some("org-b"), now);
  }
  let Json(rows) = route_trends_handler(State(state), HeaderMap::new()).await;
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0]["host"], "mine.example.com");
  assert_eq!(rows[0]["total"].as_u64().unwrap(), 2);
  // One 5xx of two requests → error_rate 0.5.
  assert_eq!(rows[0]["error_rate"].as_f64().unwrap(), 0.5);
}

// ---- bandwidth_handler ------------------------------------------------------

#[tokio::test]
async fn bandwidth_rejects_invalid_unit() {
  let state = build_state(test_config(None));
  let mut params = HashMap::new();
  params.insert("unit".to_string(), "hour".to_string());
  let resp = bandwidth_handler(State(state), HeaderMap::new(), axum::extract::Query(params)).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bandwidth_day_default_with_data() {
  let state = build_state(test_config(None));
  {
    let mut ps = state.persistent_stats.lock().await;
    ps.record_request_labeled(true, 111, 222, 4, Some("tok"), Some("h.example.com"), None);
  }
  let resp = bandwidth_handler(
    State(state),
    HeaderMap::new(),
    axum::extract::Query(HashMap::new()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let json = body_json(resp).await;
  assert_eq!(json["unit"], "day");
  assert_eq!(json["periods"].as_array().unwrap().len(), 14);
  let by_token = json["by_token"].as_array().unwrap();
  assert_eq!(by_token.len(), 1);
  assert_eq!(by_token[0]["label"], "tok");
  // 111 + 222 accumulated across the day bucket.
  assert_eq!(by_token[0]["total_bytes"].as_u64().unwrap(), 333);
  let by_hostname = json["by_hostname"].as_array().unwrap();
  assert_eq!(by_hostname[0]["label"], "h.example.com");
}

#[tokio::test]
async fn bandwidth_month_with_count() {
  let state = build_state(test_config(None));
  {
    let mut ps = state.persistent_stats.lock().await;
    ps.record_request_labeled(true, 5, 5, 1, Some("tok"), Some("h.example.com"), None);
  }
  let mut params = HashMap::new();
  params.insert("unit".to_string(), "month".to_string());
  params.insert("count".to_string(), "3".to_string());
  let resp = bandwidth_handler(State(state), HeaderMap::new(), axum::extract::Query(params)).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let json = body_json(resp).await;
  assert_eq!(json["unit"], "month");
  assert_eq!(json["periods"].as_array().unwrap().len(), 3);
}
