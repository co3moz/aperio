//! The server assembled: token authentication, rate limiting, the proxy handler's
//! answers with and without a client, path-bind matching at segment boundaries,
//! client-IP extraction with and without a trusted proxy, and that each store opens
//! where it is told to.

use crate::access_log::sanitize_uri;
use crate::auth::safe_redirect_path;
use crate::routing::select_client_pool;
use crate::state::{ClientHandle, ClientPerms};
use axum::extract::ws::Message;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[test]
pub(crate) fn test_sanitize_uri_strips_query() {
  assert_eq!(sanitize_uri("/api/users?id=42&token=secret"), "/api/users");
  assert_eq!(sanitize_uri("/api"), "/api");
  assert_eq!(sanitize_uri("/api?"), "/api");
  // Multiple '?' → first split wins
  assert_eq!(sanitize_uri("/api?a=1?b=2"), "/api");
}

/// Generous health threshold so mock clients (no pings) stay eligible.
pub(crate) const TEST_THRESHOLD: Duration = Duration::from_secs(3600);

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
