//! What a token may do and what a connection is: permission gating and the
//! organization fence over it, the request timeline and its per-stage statistics,
//! and the bounded structures that keep a long-lived server from growing without
//! end.

use super::*;
use crate::store::tokens::TokenSpec;

fn perms(hostnames: &[&str], paths: &[&str]) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: hostnames.iter().map(|s| s.to_string()).collect(),
    paths: paths.iter().map(|s| s.to_string()).collect(),
    token_name: Some("t".to_string()),
    token_id: Some("id".to_string()),
    allow_public: false,
    allow_bind: false,
    allow_otel: false,
    topics: Vec::new(),
    org_id: None,
    org_hostnames: Vec::new(),
    max_connections: None,
  }
}

#[test]
fn master_perms_allow_everything() {
  let m = ClientPerms::master();
  assert!(m.master);
  assert!(m.allow_public);
  assert!(m.hostname_allowed("anything.example.com"));
  assert!(m.path_allowed("/whatever"));
}

#[test]
fn empty_lists_are_unrestricted() {
  let p = perms(&[], &[]);
  assert!(p.hostname_allowed("a.example.com"));
  assert!(p.path_allowed("/api"));
}

#[test]
fn wildcard_entry_is_unrestricted() {
  let p = perms(&["*"], &["*"]);
  assert!(p.hostname_allowed("a.example.com"));
  assert!(p.path_allowed("/anything"));
}

/// Perms carrying an organization hostname allowlist.
fn fenced_perms(hostnames: &[&str], org_hostnames: &[&str]) -> ClientPerms {
  let mut p = perms(hostnames, &[]);
  p.org_hostnames = org_hostnames.iter().map(|s| s.to_string()).collect();
  p
}

#[test]
fn org_allowlist_fences_binds_the_token_would_otherwise_permit() {
  // A wildcard token inside a fenced org may only bind within the fence.
  let p = fenced_perms(&["*"], &["*.acme.com"]);
  assert!(p.hostname_allowed("app.acme.com"));
  assert!(!p.hostname_allowed("evil.example.com"));
  // The parent domain is not a subdomain of itself.
  assert!(!p.hostname_allowed("acme.com"));

  // An unrestricted token (empty list) is fenced just the same.
  let p = fenced_perms(&[], &["acme.com"]);
  assert!(p.hostname_allowed("acme.com"));
  assert!(!p.hostname_allowed("other.com"));

  // Both fences must admit the bind: inside the org, but not on the token.
  let p = fenced_perms(&["app.acme.com"], &["*.acme.com"]);
  assert!(p.hostname_allowed("app.acme.com"));
  assert!(!p.hostname_allowed("other.acme.com"));
}

#[test]
fn org_allowlist_never_fences_the_master_token() {
  let mut m = ClientPerms::master();
  m.org_hostnames = vec!["acme.com".to_string()];
  assert!(m.hostname_allowed("anything.example.com"));
}

#[test]
fn granted_hostnames_drop_entries_outside_the_org_fence() {
  // A token minted before the org was fenced still carries the old hostname;
  // it must not be auto-bound on connect.
  let p = fenced_perms(
    &["app.acme.com", "legacy.example.com", "*"],
    &["*.acme.com"],
  );
  assert_eq!(p.granted_hostnames(), vec!["app.acme.com".to_string()]);

  // Without a fence every specific grant is kept.
  let p = perms(&["app.acme.com", "legacy.example.com"], &[]);
  assert_eq!(
    p.granted_hostnames(),
    vec!["app.acme.com".to_string(), "legacy.example.com".to_string()]
  );
}

#[test]
fn specific_entries_gate_exact_values() {
  let p = perms(&["a.example.com"], &["/api"]);
  assert!(p.hostname_allowed("a.example.com"));
  assert!(!p.hostname_allowed("b.example.com"));
  assert!(p.path_allowed("/api"));
  assert!(!p.path_allowed("/other"));
}

#[test]
fn granted_hostnames_excludes_wildcard() {
  let p = perms(&["a.example.com", "*", "b.example.com"], &[]);
  assert_eq!(
    p.granted_hostnames(),
    vec!["a.example.com".to_string(), "b.example.com".to_string()]
  );
}

#[test]
fn granted_path_is_first_specific() {
  let p = perms(&[], &["*", "/api", "/v2"]);
  assert_eq!(p.granted_path(), Some("/api".to_string()));

  // Only a wildcard → no specific grant.
  let wild = perms(&[], &["*"]);
  assert_eq!(wild.granted_path(), None);
}

#[test]
fn test_request_timeline_assembly() {
  use crate::protocol::ClientTimings;
  use crate::state::RequestTimeline;

  // Server measured: dispatched at +100µs, response back at +10_000µs,
  // finished at +10_200µs. Client spent 8_000µs total, so 1_900µs of
  // transit splits into 950µs per direction.
  let t = RequestTimeline::assemble(
    100,
    10_000,
    10_200,
    Some(ClientTimings {
      backend_sent_us: 500,
      backend_first_byte_us: 6_000,
      backend_done_us: Some(7_500),
      respond_us: 8_000,
    }),
  );
  assert_eq!(t.dispatched_us, 100);
  assert_eq!(t.client_received_us, Some(100 + 950));
  assert_eq!(t.backend_sent_us, Some(1_050 + 500));
  assert_eq!(t.backend_first_byte_us, Some(1_050 + 6_000));
  assert_eq!(t.backend_done_us, Some(1_050 + 7_500));
  assert_eq!(t.client_responded_us, Some(1_050 + 8_000));
  assert_eq!(t.response_received_us, 10_000);
  assert_eq!(t.finished_us, 10_200);
  assert!(t.estimated_anchor);

  // Monotonic ordering of every present stage.
  let stages = [
    Some(0),
    Some(t.dispatched_us),
    t.client_received_us,
    t.backend_sent_us,
    t.backend_first_byte_us,
    t.backend_done_us,
    t.client_responded_us,
    Some(t.response_received_us),
    Some(t.finished_us),
  ];
  let present: Vec<u64> = stages.into_iter().flatten().collect();
  assert!(present.windows(2).all(|w| w[0] <= w[1]), "{present:?}");

  // Without client timings only the server stages exist.
  let t = RequestTimeline::assemble(100, 10_000, 10_200, None);
  assert!(t.client_received_us.is_none());
  assert!(!t.estimated_anchor);

  // A client that reports more time than the round trip (clock weirdness)
  // must not panic or go backwards.
  let t = RequestTimeline::assemble(
    100,
    5_000,
    5_100,
    Some(ClientTimings {
      backend_sent_us: 1,
      backend_first_byte_us: 2,
      backend_done_us: Some(3),
      respond_us: 60_000,
    }),
  );
  assert_eq!(t.client_received_us, Some(100));
}

#[test]
fn test_stage_window_stats_and_anomaly() {
  use crate::state::{RequestTimeline, StageStats};

  let tl = |queue: u64, backend: u64| {
    RequestTimeline::assemble(
      queue,
      queue + 2_000 + backend,
      queue + 2_100 + backend,
      Some(crate::protocol::ClientTimings {
        backend_sent_us: 100,
        backend_first_byte_us: 100 + backend,
        backend_done_us: Some(150 + backend),
        respond_us: 200 + backend,
      }),
    )
  };

  let mut stats = StageStats::default();
  // A steady baseline: 30 requests with ~identical stage durations.
  for _ in 0..30 {
    stats.record(Some("app.local"), None, &tl(100, 5_000));
  }
  let window = stats.routes.get("app.local").expect("route window");
  let rows = window.stats();
  let backend_wait = rows.iter().find(|r| r.stage == "backend_wait").unwrap();
  assert_eq!(backend_wait.count, 30);
  assert!(
    (backend_wait.mean - 5_000.0).abs() < 1.0,
    "mean {}",
    backend_wait.mean
  );
  assert!(
    !backend_wait.anomalous,
    "steady traffic must not be anomalous"
  );

  // One wild outlier in backend_wait flips only that stage's verdict.
  stats.record(Some("app.local"), None, &tl(100, 80_000));
  let rows = stats.routes.get("app.local").unwrap().stats();
  let backend_wait = rows.iter().find(|r| r.stage == "backend_wait").unwrap();
  assert!(backend_wait.anomalous, "outlier must be flagged");
  let queue = rows.iter().find(|r| r.stage == "queue").unwrap();
  assert!(!queue.anomalous, "an unrelated stage must stay quiet");
}

#[test]
fn test_token_map_gc() {
  use crate::state::{RateLimitState, gc_token_daily_bytes, gc_token_rate};
  use std::collections::HashMap;
  use std::time::{Duration, Instant};

  let now = Instant::now();

  // token_rate: below the threshold nothing is dropped, even stale buckets.
  let mut rate: HashMap<String, RateLimitState> = HashMap::new();
  rate.insert(
    "stale".to_string(),
    RateLimitState {
      tokens: 1.0,
      last_updated: now - Duration::from_secs(3600),
    },
  );
  gc_token_rate(&mut rate, now);
  assert_eq!(rate.len(), 1, "small maps are left alone");

  // Past the threshold, idle buckets are evicted and fresh ones kept.
  for i in 0..1200 {
    let age = if i % 2 == 0 { 3600 } else { 0 };
    rate.insert(
      format!("t{i}"),
      RateLimitState {
        tokens: 1.0,
        last_updated: now - Duration::from_secs(age),
      },
    );
  }
  gc_token_rate(&mut rate, now);
  assert!(rate.contains_key("t1"), "fresh bucket survives");
  assert!(!rate.contains_key("t0"), "idle bucket evicted");
  assert!(
    !rate.contains_key("stale"),
    "the old stale bucket evicted too"
  );

  // token_daily_bytes: past the threshold, non-today entries are dropped.
  let mut daily: HashMap<String, (String, u64)> = HashMap::new();
  for i in 0..1200 {
    let day = if i % 2 == 0 {
      "2020-01-01"
    } else {
      "2026-07-19"
    };
    daily.insert(format!("t{i}"), (day.to_string(), 100));
  }
  gc_token_daily_bytes(&mut daily, "2026-07-19");
  assert!(daily.contains_key("t1"), "today's entry survives");
  assert!(!daily.contains_key("t0"), "yesterday's entry dropped");
  assert!(daily.values().all(|(d, _)| d == "2026-07-19"));
}

#[test]
fn test_stage_stats_route_cap_evicts_lru() {
  use crate::state::{RequestTimeline, STAGE_ROUTE_CAP, StageStats};

  let tl = RequestTimeline::assemble(100, 10_000, 10_200, None);
  let cap = STAGE_ROUTE_CAP;
  let mut stats = StageStats::default();

  // Fill exactly to the cap with distinct hostnames, oldest first.
  for i in 0..cap {
    stats.record(Some(&format!("h{i}.local")), None, &tl);
  }
  assert_eq!(stats.routes.len(), cap);
  assert!(stats.routes.contains_key("h0.local"));

  // Touch h0 so it is no longer the least-recently-used route.
  stats.record(Some("h0.local"), None, &tl);

  // A brand-new route past the cap evicts the LRU route (h1, the oldest
  // untouched one), never growing beyond the cap.
  stats.record(Some("new.local"), None, &tl);
  assert_eq!(stats.routes.len(), cap);
  assert!(stats.routes.contains_key("new.local"));
  assert!(
    stats.routes.contains_key("h0.local"),
    "recently-touched route survives"
  );
  assert!(
    !stats.routes.contains_key("h1.local"),
    "the LRU route was evicted"
  );
}

// ----- DurationHistogram -----

#[test]
fn test_duration_histogram_observe_and_render() {
  let h = DurationHistogram::default();
  h.observe(Duration::from_millis(3)); // <= 0.005 → every bucket
  h.observe(Duration::from_millis(300)); // between 0.25 and 0.5
  h.observe(Duration::from_secs(60)); // beyond the last finite bound (+Inf only)

  let mut out = String::new();
  h.render(&mut out);
  assert!(out.contains("# TYPE aperio_request_duration_seconds histogram"));
  // The 3ms sample lands in the smallest (0.005) bucket.
  assert!(out.contains("le=\"0.005\"} 1"), "{out}");
  // All three samples fall under +Inf.
  assert!(out.contains("le=\"+Inf\"} 3"), "{out}");
  assert!(
    out.contains("aperio_request_duration_seconds_count 3"),
    "{out}"
  );
  // Sum reflects the observed micros (~60.303s).
  assert!(
    out.contains("aperio_request_duration_seconds_sum "),
    "{out}"
  );
}

// ----- EndpointStats / EndpointWindow -----

#[test]
fn test_endpoint_stats_record_summary_and_overflow() {
  use crate::state::{ENDPOINT_MIN_SAMPLES, EndpointStats};
  let mut stats = EndpointStats::default();
  // A spread of durations plus one 5xx to bump the error counter.
  for ms in [10u64, 20, 30, 40, 500] {
    let status = if ms == 500 { 503 } else { 200 };
    stats.record(Some("a.local"), "/api", status, ms, None);
  }
  let w = stats.endpoints.get("a.local|/api").expect("endpoint");
  assert_eq!(w.count, 5);
  assert_eq!(w.errors, 1);
  assert!(w.samples() >= ENDPOINT_MIN_SAMPLES.min(5));
  let (avg, p50, p95, max) = w.summary();
  assert!(avg > 0.0);
  assert_eq!(max, 500);
  assert!(p50 <= p95 && p95 <= max);

  // An empty window summarizes to zeros.
  let empty = EndpointStats::default();
  assert!(empty.endpoints.is_empty());
}

#[test]
fn test_endpoint_stats_key_cap_folds_into_other() {
  use crate::state::EndpointStats;
  let mut stats = EndpointStats::default();
  // Overflow the distinct-endpoint cap; extra keys fold into __other.
  for i in 0..400 {
    stats.record(Some(&format!("h{i}.local")), "/p", 200, 5, None);
  }
  assert!(
    stats.endpoints.contains_key("__other|__other"),
    "overflow endpoint folds into __other"
  );
}

// ----- RouteTrends / RouteTrend -----

#[test]
fn test_route_trends_record_and_series() {
  let mut trends = RouteTrends::default();
  let now = 1_000_000u64; // seconds
  let minute = now / 60;
  // One of each status class into the same minute bucket.
  trends.record(Some("app.local"), 204, None, now);
  trends.record(Some("app.local"), 301, None, now);
  trends.record(Some("app.local"), 404, None, now);
  trends.record(Some("app.local"), 500, None, now);
  // A later minute rolls a new bucket.
  trends.record(Some("app.local"), 200, None, now + 60);

  let series = trends
    .routes
    .get("app.local")
    .unwrap()
    .series(3, minute + 1);
  assert_eq!(series.len(), 3);
  // The first minute holds the four class counts.
  let first = series.iter().find(|b| b.minute == minute).unwrap();
  assert_eq!(first.total, 4);
  assert_eq!(first.s2xx, 1);
  assert_eq!(first.s3xx, 1);
  assert_eq!(first.s4xx, 1);
  assert_eq!(first.s5xx, 1);
  // The next minute holds the single 2xx.
  let second = series.iter().find(|b| b.minute == minute + 1).unwrap();
  assert_eq!(second.total, 1);
  assert_eq!(second.s2xx, 1);
}

#[test]
fn test_route_trends_cap_ignores_overflow() {
  let mut trends = RouteTrends::default();
  for i in 0..100 {
    trends.record(Some(&format!("h{i}.local")), 200, None, 0);
  }
  let len = trends.routes.len();
  // A brand-new route past the cap is simply not trended.
  trends.record(Some("overflow.local"), 200, None, 0);
  assert_eq!(trends.routes.len(), len);
  assert!(!trends.routes.contains_key("overflow.local"));
}

// ----- ClientHandle routing / health helpers -----

#[test]
fn test_client_effective_binds_precedence() {
  use crate::test_support::mock_client;
  // declared path only.
  let c = mock_client(Some("a.local"), Some("/api"), None, None);
  assert_eq!(c.sole().effective_path_bind(), Some(&"/api".to_string()));
  assert!(c.sole().matches_host("a.local"));
  assert!(c.sole().has_hostname_bind());

  // override path wins over declared.
  let c = mock_client(Some("a.local"), Some("/api"), None, Some("/ovr"));
  assert_eq!(c.sole().effective_path_bind(), Some(&"/ovr".to_string()));

  // assigned path used when nothing declared/overridden.
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().assigned_path = Some("/assigned".to_string());
  assert_eq!(
    c.sole().effective_path_bind(),
    Some(&"/assigned".to_string())
  );

  // hostname override replaces the whole set.
  let c = mock_client(Some("a.local"), None, Some("override.local"), None);
  assert_eq!(c.effective_hostnames(), vec![&"override.local".to_string()]);
  assert!(c.sole().matches_host("override.local"));
  assert!(!c.sole().matches_host("a.local"));

  // union of assigned + declared + extra declared hostnames, de-duplicated.
  let mut c = mock_client(Some("declared.local"), None, None, None);
  c.sole_mut().assigned_hostnames =
    vec!["assigned.local".to_string(), "declared.local".to_string()];
  c.sole_mut().declared_hostnames = vec!["extra.local".to_string(), "assigned.local".to_string()];
  let hosts = c.effective_hostnames();
  assert!(hosts.contains(&&"assigned.local".to_string()));
  assert!(hosts.contains(&&"declared.local".to_string()));
  assert!(hosts.contains(&&"extra.local".to_string()));
  assert_eq!(hosts.len(), 3, "duplicates collapse");

  // no binds at all.
  let c = mock_client(None, None, None, None);
  assert!(!c.sole().has_hostname_bind());
  assert!(c.sole().effective_path_bind().is_none());
}

#[test]
fn test_client_health_and_ejection() {
  use crate::test_support::mock_client;
  let now = Instant::now();
  let mut c = mock_client(None, None, None, None);

  // Fresh connection is healthy within the threshold.
  assert!(c.is_healthy(Duration::from_secs(3600)));
  // A zero threshold makes even a just-connected client stale.
  assert!(!c.is_healthy(Duration::from_nanos(0)));

  // Not ejected initially.
  assert!(!c.sole().is_ejected(now));
  // Below the failure threshold: no ejection.
  let window = Duration::from_secs(30);
  let eject_for = Duration::from_secs(30);
  assert!(!c.sole_mut().record_failure(now, window, 3, eject_for));
  assert!(!c.sole_mut().record_failure(now, window, 3, eject_for));
  // The third failure inside the window trips the ejection.
  assert!(c.sole_mut().record_failure(now, window, 3, eject_for));
  assert!(c.sole().is_ejected(now));
  // Failures are cleared once ejected; a repeat call while ejected is a no-op.
  assert!(!c.sole_mut().record_failure(now, window, 3, eject_for));

  // Stale failures outside the window are pruned before counting.
  let mut c2 = mock_client(None, None, None, None);
  let old = now - Duration::from_secs(120);
  c2.sole_mut().recent_failures.push_back(old);
  c2.sole_mut().recent_failures.push_back(old);
  assert!(!c2.sole_mut().record_failure(now, window, 3, eject_for));
  assert_eq!(c2.sole().recent_failures.len(), 1, "old failures pruned");
}

// ----- AppState: config, request slots -----

#[tokio::test]
async fn test_config_snapshot_and_request_slots() {
  let mut cfg = crate::test_support::test_config();
  cfg.max_concurrent_requests = 2;
  let state = crate::test_support::test_state_with(cfg);

  assert_eq!(state.config().max_concurrent_requests, 2);

  let s1 = state.try_acquire_request_slot().expect("slot 1");
  let s2 = state.try_acquire_request_slot().expect("slot 2");
  assert!(state.try_acquire_request_slot().is_none(), "at capacity");
  drop(s1);
  // Dropping a slot frees capacity for the next request.
  let s3 = state.try_acquire_request_slot().expect("slot after drop");
  drop(s2);
  drop(s3);
}

#[tokio::test]
async fn ws_slots_respect_the_live_websocket_limit() {
  let mut cfg = crate::test_support::test_config();
  cfg.max_ws_connections = 2;
  let state = crate::test_support::test_state_with(cfg);

  let a = state.try_acquire_ws_slot().expect("ws slot 1");
  let b = state.try_acquire_ws_slot().expect("ws slot 2");
  assert!(state.try_acquire_ws_slot().is_none(), "at the WS cap");
  drop(a);
  // Dropping a live WebSocket frees a slot for the next upgrade.
  let c = state.try_acquire_ws_slot().expect("ws slot after drop");
  drop(b);
  drop(c);
}

// ----- AppState: token limits & byte accounting -----

#[tokio::test]
async fn test_check_token_limits_rps_and_quota() {
  let state = crate::test_support::test_state();

  // Master traffic (no token) is never limited.
  assert!(state.check_token_limits(None).await.is_ok());
  // Unknown token id: nothing to enforce.
  assert!(state.check_token_limits(Some("nope")).await.is_ok());

  // A token with a 1 rps limit allows one request, then rejects.
  let rps_id = {
    let mut store = state.token_store.lock().await;
    let (tok, _secret) = store
      .create(TokenSpec {
        name: "rps".to_string(),
        max_rps: Some(1.0),
        ..Default::default()
      })
      .expect("the test store can be written to");
    tok.id
  };
  assert!(state.check_token_limits(Some(&rps_id)).await.is_ok());
  assert!(
    state.check_token_limits(Some(&rps_id)).await.is_err(),
    "burst exhausted"
  );

  // A token with a daily byte quota rejects once usage reaches it.
  let quota_id = {
    let mut store = state.token_store.lock().await;
    let (tok, _secret) = store
      .create(TokenSpec {
        name: "quota".to_string(),
        daily_max_bytes: Some(100),
        ..Default::default()
      })
      .expect("the test store can be written to");
    tok.id
  };
  // Under quota: allowed. Zero bytes is a no-op.
  state.add_token_bytes(Some(&quota_id), 0).await;
  assert!(state.check_token_limits(Some(&quota_id)).await.is_ok());
  // Reaching the quota flips it to rejected.
  state.add_token_bytes(Some(&quota_id), 150).await;
  assert!(state.check_token_limits(Some(&quota_id)).await.is_err());
  // Master byte accounting is ignored.
  state.add_token_bytes(None, 10).await;
}

// ----- AppState: org quotas -----

#[tokio::test]
async fn test_org_quotas() {
  let state = crate::test_support::test_state();

  // No org / no quota → permissive.
  assert!(state.org_quota(None).await.is_none());
  assert!(state.check_org_client_quota(None).await.is_ok());
  assert!(!state.org_over_month_bytes(None).await);

  let org_id = {
    let mut orgs = state.org_store.lock().await;
    let org = orgs.create("acme", Vec::new(), None).expect("org");
    orgs
      .set_quota(
        &org.id,
        Some(Some(1)), // max_clients
        Some(Some(1)), // max_tokens
        Some(Some(1)), // max_users
        Some(Some(50)),
      )
      .expect("the test store can be written to");
    org.id
  };
  assert!(state.org_quota(Some(&org_id)).await.is_some());

  // Under the caps the client quota is allowed.
  assert!(state.check_org_client_quota(Some(&org_id)).await.is_ok());

  // Month-bytes quota is not exceeded with no traffic recorded.
  assert!(!state.org_over_month_bytes(Some(&org_id)).await);
  // Unknown org id → no quota.
  assert!(!state.org_over_month_bytes(Some("missing")).await);
}

// ----- AppState: rate limiting -----

#[tokio::test]
async fn test_ip_rate_limit_exhausts() {
  let mut cfg = crate::test_support::test_config();
  cfg.ip_limit_max = 2.0;
  cfg.ip_limit_refill = 0.0; // no refill so the bucket empties for good
  let state = crate::test_support::test_state_with(cfg);
  let ip: IpAddr = "203.0.113.7".parse().unwrap();
  assert!(state.check_rate_limit(ip).await);
  assert!(state.check_rate_limit(ip).await);
  assert!(!state.check_rate_limit(ip).await, "bucket drained");
}

#[tokio::test]
async fn test_route_rate_limit_default_allows() {
  let state = crate::test_support::test_state();
  // No `rate_limits:` rules configured → always allowed.
  assert!(
    state
      .check_route_rate_limit(Some("a.local"), "/x", "GET")
      .await
  );
}

// ----- AppState: disconnect token clients -----

#[tokio::test]
async fn test_disconnect_token_clients() {
  use crate::test_support::mock_client;
  let state = crate::test_support::test_state();

  let mut c = mock_client(Some("a.local"), None, None, None);
  c.perms = ClientPerms {
    master: false,
    hostnames: vec![],
    paths: vec![],
    token_name: Some("t".to_string()),
    token_id: Some("tok-1".to_string()),
    allow_public: false,
    allow_bind: false,
    allow_otel: false,
    topics: Vec::new(),
    org_id: None,
    org_hostnames: Vec::new(),
    max_connections: None,
  };
  state.clients.write().await.insert("c1".to_string(), c);
  state
    .token_seen_ips
    .lock()
    .await
    .insert("tok-1".to_string(), std::collections::HashSet::new());

  let dropped = state.disconnect_token_clients("tok-1").await;
  assert_eq!(dropped, 1);
  assert!(
    !state.token_seen_ips.lock().await.contains_key("tok-1"),
    "seen-ip tracking dropped with the token"
  );
  // A token nobody uses drops nothing.
  assert_eq!(state.disconnect_token_clients("tok-x").await, 0);
}

// ----- AppState: audit, session actor, events, reload -----

#[tokio::test]
async fn test_audit_events_and_session_actor() {
  use axum::http::HeaderMap;
  let state = crate::test_support::test_state();

  // Global + org-scoped audit records land in the log without panicking.
  state.audit("evt", "actor", "127.0.0.1", "details").await;
  state
    .audit_in("evt2", "actor", "127.0.0.1", Some("org".to_string()), "d")
    .await;

  // No session → the actor resolves to "-".
  let empty = HeaderMap::new();
  assert_eq!(state.session_actor(&empty).await, "-");
  state.audit_session("evt3", &empty, "127.0.0.1", "d").await;

  // An admin session resolves to the built-in "aperio" actor.
  let headers = crate::test_support::admin_headers(&state).await;
  assert_eq!(state.session_actor(&headers).await, "aperio");

  // Emitting an event with no subscribers is a no-op.
  state
    .emit_event("nothing", serde_json::json!({"k": 1}))
    .await;
}

#[tokio::test]
async fn test_reload_from_file_returns_diff() {
  let state = std::sync::Arc::new(crate::test_support::test_state());
  // With no dashboard overrides and no file layer, the effective config is
  // unchanged, so the diff is empty. Exercises the reload plumbing.
  let diff = state.reload_from_file().await;
  assert!(diff.is_empty(), "no changes: {diff:?}");
}

#[test]
fn request_timeline_assemble_anchors_client_offsets() {
  use crate::protocol::ClientTimings;
  // The three real boundaries pass through verbatim; the client offsets are
  // anchored onto the tunnel round-trip by an even transit split.
  let t = RequestTimeline::assemble(
    100,
    10_000,
    10_200,
    Some(ClientTimings {
      backend_sent_us: 500,
      backend_first_byte_us: 6_000,
      backend_done_us: Some(7_500),
      respond_us: 8_000,
    }),
  );
  // Measured, verbatim.
  assert_eq!(t.dispatched_us, 100);
  assert_eq!(t.response_received_us, 10_000);
  assert_eq!(t.finished_us, 10_200);
  assert!(t.estimated_anchor);
  // Transit = round_trip(9_900) - respond(8_000) = 1_900; anchor = 100 + 950.
  let anchor = 100 + (10_000 - 100 - 8_000) / 2;
  assert_eq!(t.client_received_us, Some(anchor));
  assert_eq!(t.backend_sent_us, Some(anchor + 500));
  assert_eq!(t.backend_first_byte_us, Some(anchor + 6_000));
  assert_eq!(t.backend_done_us, Some(anchor + 7_500));
  assert_eq!(t.client_responded_us, Some(anchor + 8_000));
  // Client stages stay within the measured tunnel round-trip [dispatched, received].
  assert!(t.client_received_us.unwrap() >= t.dispatched_us);
  assert!(t.client_responded_us.unwrap() <= t.response_received_us);

  // Without client timings: only the three measured boundaries, no anchor.
  let t = RequestTimeline::assemble(100, 10_000, 10_200, None);
  assert_eq!(
    (t.dispatched_us, t.response_received_us, t.finished_us),
    (100, 10_000, 10_200)
  );
  assert!(!t.estimated_anchor);
  assert_eq!(t.client_received_us, None);
  assert_eq!(t.backend_sent_us, None);
}

#[test]
fn the_head_of_a_streamed_response_carries_every_stage_but_the_body_end() {
  use crate::protocol::ClientTimings;
  use crate::state::RequestTimeline;

  // A streamed response reports at the head, so `backend_done_us` has not
  // happened yet. Before this the client sent nothing at all for a stream and
  // the whole waterfall collapsed to one round-trip row, which meant the
  // largest responses, the ones actually worth profiling, were the ones with
  // no detail.
  let t = RequestTimeline::assemble(
    100,
    10_000,
    10_200,
    Some(ClientTimings {
      backend_sent_us: 500,
      backend_first_byte_us: 6_000,
      backend_done_us: None,
      respond_us: 8_000,
    }),
  );
  let anchor = 100 + (10_000 - 100 - 8_000) / 2;
  assert!(t.estimated_anchor);
  assert_eq!(t.client_received_us, Some(anchor));
  assert_eq!(t.backend_sent_us, Some(anchor + 500));
  assert_eq!(t.backend_first_byte_us, Some(anchor + 6_000));
  assert_eq!(t.client_responded_us, Some(anchor + 8_000));
  // The one hole, and it is the honest one: the body was still arriving.
  assert_eq!(t.backend_done_us, None);
  // Still inside the measured round trip, same as the buffered case.
  assert!(t.client_responded_us.unwrap() <= t.response_received_us);
}

#[test]
fn stream_limits_sanitized_repairs_inconsistent_trios() {
  use crate::state::{STREAM_BACKLOG_LIMIT, STREAM_PAUSE_BYTES, STREAM_RESUME_BYTES, StreamLimits};

  // The defaults pass through untouched.
  assert_eq!(
    StreamLimits::sanitized(
      STREAM_PAUSE_BYTES,
      STREAM_RESUME_BYTES,
      STREAM_BACKLOG_LIMIT,
      0,
    ),
    StreamLimits::default()
  );

  // A resume mark at or above the pause mark would flap; it is pulled back.
  let l = StreamLimits::sanitized(1024 * 1024, 2 * 1024 * 1024, 64 * 1024 * 1024, 0);
  assert_eq!(l.pause_bytes, 1024 * 1024);
  assert_eq!(l.resume_bytes, 256 * 1024);

  // A cap below the pause mark would cut every stream before it could be
  // paused; it is raised to twice the pause mark.
  let l = StreamLimits::sanitized(8 * 1024 * 1024, 1024, 1024, 0);
  assert_eq!(l.backlog_limit, 16 * 1024 * 1024);

  // A pause mark of essentially zero would pause on the first chunk forever;
  // it gets a sane floor.
  let l = StreamLimits::sanitized(1, 0, 1, 0);
  assert_eq!(l.pause_bytes, 64 * 1024);
  assert_eq!(l.resume_bytes, 0);
  assert_eq!(l.backlog_limit, 128 * 1024);
}

#[test]
fn a_token_may_lower_the_connection_ceiling_but_never_raise_it() {
  // The server's number is policy: a token asking for more is not an error,
  // it simply does not get it. Otherwise minting a token would be a way to
  // spend more of the server's resources than the operator allowed.
  let server_max = 16;
  let mut perms = ClientPerms::master();

  perms.max_connections = None;
  assert_eq!(
    perms.connection_ceiling(server_max),
    16,
    "unset = the server's"
  );

  perms.max_connections = Some(4);
  assert_eq!(
    perms.connection_ceiling(server_max),
    4,
    "a token may ask for less"
  );

  perms.max_connections = Some(64);
  assert_eq!(
    perms.connection_ceiling(server_max),
    16,
    "a token asking for more gets the server's number"
  );

  // A server that allows nothing still leaves one connection: a service with
  // zero connections is a service that cannot exist, and refusing every
  // client is not what setting a small number means.
  perms.max_connections = Some(0);
  assert_eq!(perms.connection_ceiling(server_max), 1);
  perms.max_connections = None;
  assert_eq!(perms.connection_ceiling(0), 1);
}

// --- Which hostnames an organization may act on ---

/// A state whose org store holds one org, returning its id.
async fn state_with_org(hostnames: &[&str]) -> (AppState, String) {
  let state = crate::test_support::test_state();
  let id = state
    .org_store
    .lock()
    .await
    .create(
      "acme",
      hostnames.iter().map(|h| h.to_string()).collect(),
      None,
    )
    .unwrap()
    .id;
  (state, id)
}

#[tokio::test]
async fn a_fenced_org_may_act_on_its_hostnames_with_no_client_connected() {
  // The bug this covers: maintenance mode and share links asked whether one
  // of the org's clients was serving the hostname *right now*, so an org
  // fenced to x.com could not 503 x.com until a client for it was up, which
  // is exactly when nobody's client is up.
  let (state, id) = state_with_org(&["x.com", "*.x.com"]).await;
  assert!(state.org_may_claim_hostname(Some(&id), "x.com").await);
  assert!(state.org_may_claim_hostname(Some(&id), "app.x.com").await);
  assert!(state.org_may_claim_hostname(Some(&id), "a.b.x.com").await);
  // Another tenant's hostname stays refused, which is the point of the fence.
  assert!(!state.org_may_claim_hostname(Some(&id), "y.com").await);
}

#[tokio::test]
async fn a_wildcard_alone_does_not_carry_the_bare_domain() {
  // `*.x.com` is a subdomain wildcard, TLS-style: an operator who wants the
  // apex lists it too. Asserted here because it is the difference between
  // the two entries in the allowlist above.
  let (state, id) = state_with_org(&["*.x.com"]).await;
  assert!(state.org_may_claim_hostname(Some(&id), "app.x.com").await);
  assert!(!state.org_may_claim_hostname(Some(&id), "x.com").await);
}

#[tokio::test]
async fn master_is_never_fenced() {
  let (state, _) = state_with_org(&["x.com"]).await;
  assert!(
    state
      .org_may_claim_hostname(None, "anything.example.com")
      .await
  );
}

#[tokio::test]
async fn an_unfenced_org_still_needs_a_client_serving_the_hostname() {
  // With no allowlist there is no boundary to read, so the older test is all
  // that is left: an org with no fence cannot claim a hostname out of thin
  // air just because nobody drew one.
  let (state, id) = state_with_org(&[]).await;
  assert!(!state.org_may_claim_hostname(Some(&id), "x.com").await);

  let mut handle = crate::test_support::mock_client(Some("x.com"), None, None, None);
  handle.perms = ClientPerms {
    org_id: Some(id.clone()),
    ..ClientPerms::master()
  };
  state.clients.write().await.insert("c1".to_string(), handle);
  assert!(state.org_may_claim_hostname(Some(&id), "x.com").await);
  assert!(!state.org_may_claim_hostname(Some(&id), "y.com").await);
}

#[tokio::test]
async fn a_dropped_handler_takes_its_pending_entry_with_it() {
  use crate::state::{PendingGuard, PendingMap, PendingRequest};

  // A visitor that hangs up mid-request leaves axum dropping the handler
  // future. Every removal in the proxy is on a path that is then no longer
  // running, and the only sweep that reached the entry ran when the *serving
  // client* disconnected, so under a long-lived client the map grew with
  // every abandoned request. It is also an alert metric, so the leak
  // reported itself as load.
  let state = crate::test_support::test_state();
  let state = std::sync::Arc::new(state);

  {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    state.pending_requests.lock().await.insert(
      "abandoned".to_string(),
      PendingRequest {
        tx,
        client_id: "c1".to_string(),
      },
    );
    let _guard = PendingGuard::new(state.clone(), PendingMap::Requests, "abandoned".to_string());
    assert_eq!(state.pending_requests.lock().await.len(), 1);
  }
  // Dropped with the scope, exactly as the handler future is.
  assert!(
    state.pending_requests.lock().await.is_empty(),
    "the entry outlived the handler that registered it"
  );

  // The ordinary path removes the entry first; the guard must then be a
  // lookup that finds nothing rather than something that can go wrong.
  {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    state.pending_upgrades.lock().await.insert(
      "answered".to_string(),
      PendingRequest {
        tx,
        client_id: "c1".to_string(),
      },
    );
    let _guard = PendingGuard::new(state.clone(), PendingMap::Upgrades, "answered".to_string());
    state.pending_upgrades.lock().await.remove("answered");
  }
  assert!(state.pending_upgrades.lock().await.is_empty());
}

// --- The activity ring behind the chart's long view ---

#[test]
fn activity_buckets_by_five_seconds_and_keeps_quiet_slices() {
  let mut activity = Activity::default();
  // Three requests inside one slice, one of them a failure.
  activity.record(None, false, 1000);
  activity.record(None, true, 1002);
  activity.record(None, false, 1004);
  // A slice later, after a gap of two silent slices.
  activity.record(None, false, 1020);

  let series = activity.series(None, ActivityRange::Quarter, 1020);
  assert_eq!(series.len(), ACTIVITY_BUCKETS);
  let series = &series[series.len() - 6..];
  // Oldest first, and the silent slices are present rather than skipped: a
  // quiet minute is an answer, and omitting it draws the traffic on either
  // side as if it were adjacent.
  let totals: Vec<u32> = series.iter().map(|b| b.total).collect();
  assert_eq!(totals, vec![0, 3, 0, 0, 0, 1]);
  assert_eq!(series[1].failed, 1);
  // Every bucket is stamped, five seconds apart, aligned to the slice.
  for pair in series.windows(2) {
    assert_eq!(pair[1].at - pair[0].at, ACTIVITY_BUCKET_SECS);
  }
  assert_eq!(series.last().unwrap().at, 1020);
}

#[test]
fn activity_is_per_organization_and_bounded() {
  let mut activity = Activity::default();
  activity.record(None, false, 100);
  activity.record(Some("acme"), false, 100);
  activity.record(Some("acme"), false, 100);

  let newest = |a: &Activity, org: Option<&str>| {
    *a.series(org, ActivityRange::Quarter, 100)
      .last()
      .expect("the series is never empty")
  };
  assert_eq!(newest(&activity, None).total, 1);
  assert_eq!(newest(&activity, Some("acme")).total, 2);
  // An org that never served anything reads as silence, not as someone else's
  // traffic.
  assert_eq!(newest(&activity, Some("other")).total, 0);

  // The ring is bounded: fifteen minutes of slices, then the oldest goes.
  let mut long = Activity::default();
  for i in 0..(ACTIVITY_BUCKETS as u64 + 50) {
    long.record(None, false, i * ACTIVITY_BUCKET_SECS);
  }
  let last = (ACTIVITY_BUCKETS as u64 + 49) * ACTIVITY_BUCKET_SECS;
  let series = long.series(None, ActivityRange::Quarter, last);
  assert_eq!(series.len(), ACTIVITY_BUCKETS);
  assert!(
    series.iter().all(|b| b.total == 1),
    "the window is full of the most recent slices"
  );
}

#[test]
fn every_range_holds_about_sixty_cells_and_counts_the_same_request() {
  let mut activity = Activity::default();
  // A real wall-clock second: the day ring reaches back 24 hours, and a `now`
  // smaller than that would clamp its oldest buckets to zero.
  let now = 1_700_000_000;
  activity.record(None, true, now);

  // One request, counted once in each resolution: the rings are three views
  // of the same traffic, not three samples of it.
  for range in [
    ActivityRange::Quarter,
    ActivityRange::TwoHours,
    ActivityRange::Day,
  ] {
    let series = activity.series(None, range, now);
    assert_eq!(series.len(), range.buckets());
    assert_eq!(series.last().unwrap().total, 1);
    assert_eq!(series.last().unwrap().failed, 1);
    for pair in series.windows(2) {
      assert_eq!(pair[1].at - pair[0].at, range.width_secs());
    }
    // Roughly sixty cells whatever the span: that is what keeps the chart
    // readable and the payload small.
    assert!((60..=96).contains(&series.len()) || range == ActivityRange::Quarter);
  }

  // The span each range actually covers.
  let span = |r: ActivityRange| r.width_secs() * r.buckets() as u64;
  assert_eq!(span(ActivityRange::Quarter), 15 * 60);
  assert_eq!(span(ActivityRange::TwoHours), 2 * 60 * 60);
  assert_eq!(span(ActivityRange::Day), 24 * 60 * 60);
}

#[test]
fn an_unknown_range_is_the_quarter_hour_that_the_endpoint_always_returned() {
  // The parameter was added after the endpoint shipped, so a caller that does
  // not send it, or sends something this build does not know, gets exactly
  // what it used to get rather than an error or a day of history.
  assert_eq!(ActivityRange::parse(None), ActivityRange::Quarter);
  assert_eq!(ActivityRange::parse(Some("")), ActivityRange::Quarter);
  assert_eq!(ActivityRange::parse(Some("5m")), ActivityRange::Quarter);
  assert_eq!(ActivityRange::parse(Some("2h")), ActivityRange::TwoHours);
  assert_eq!(ActivityRange::parse(Some("1d")), ActivityRange::Day);
}

#[test]
fn the_long_ranges_survive_a_restart_and_the_fine_one_does_not() {
  let dir = crate::test_support::test_temp_root()
    .join(format!("activity-restart-{}", uuid::Uuid::new_v4()));
  let path = dir.to_string_lossy().to_string();
  let now = 1_700_000_000;

  let mut activity = Activity::load(&path, now);
  activity.record(None, false, now);
  activity.record(Some("acme"), true, now);
  activity.save_if_dirty();
  drop(activity);

  let restarted = Activity::load(&path, now + 60);
  // A view covering a day that empties on every deploy answers "what happened
  // overnight" with a shrug, which is worse than not offering the range.
  for range in [ActivityRange::TwoHours, ActivityRange::Day] {
    let series = restarted.series(None, range, now + 60);
    assert_eq!(
      series.iter().map(|b| b.total).sum::<u32>(),
      1,
      "the coarse rings come back"
    );
    assert_eq!(
      restarted
        .series(Some("acme"), range, now + 60)
        .iter()
        .map(|b| b.failed)
        .sum::<u32>(),
      1,
      "and they come back per organization"
    );
  }
  // The fine ring is deliberately not restored: fifteen minutes of five-second
  // slices is the view of "right now", and a restart is exactly the moment
  // when "right now" changed.
  assert_eq!(
    restarted
      .series(None, ActivityRange::Quarter, now + 60)
      .iter()
      .map(|b| b.total)
      .sum::<u32>(),
    0
  );

  // Aged out: a file written a day ago must not redraw yesterday as today.
  let stale = Activity::load(&path, now + 2 * 24 * 60 * 60);
  assert_eq!(
    stale
      .series(None, ActivityRange::Day, now + 2 * 24 * 60 * 60)
      .iter()
      .map(|b| b.total)
      .sum::<u32>(),
    0
  );
  let _ = std::fs::remove_dir_all(&dir);
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn a_route_rate_limit_spends_its_burst_and_then_refuses() {
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
    }
  }
  let _cleanup = Cleanup;
  std::fs::write(
    "aperio-server.yaml",
    "rate_limits:\n  - hostname: api.example.com\n    path: /login\n    rps: 1\n    burst: 2\n",
  )
  .unwrap();
  crate::config_file::reload().unwrap();

  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let mut cfg = crate::test_support::test_config();
    cfg.route_limits = crate::route_limits::from_config_file();
    let state = crate::test_support::test_state_with(cfg);

    // A path outside every rule never pays.
    assert!(
      state
        .check_route_rate_limit(Some("api.example.com"), "/free", "GET")
        .await
    );
    // The burst is two; the third request inside the same instant is refused.
    assert!(
      state
        .check_route_rate_limit(Some("api.example.com"), "/login", "GET")
        .await
    );
    assert!(
      state
        .check_route_rate_limit(Some("api.example.com"), "/login", "GET")
        .await
    );
    assert!(
      !state
        .check_route_rate_limit(Some("api.example.com"), "/login", "GET")
        .await
    );
  });
}

#[tokio::test]
async fn one_gc_beat_sweeps_stale_buckets_and_expired_sessions() {
  let state = crate::test_support::test_state_with(crate::test_support::test_config());
  // A stale and a fresh rate bucket, dated by hand.
  let old = Instant::now() - Duration::from_secs(700);
  state.rate_limiter.lock().await.insert(
    "203.0.113.9".parse().unwrap(),
    RateLimitState {
      tokens: 1.0,
      last_updated: old,
    },
  );
  assert!(
    state
      .check_rate_limit("198.51.100.7".parse().unwrap())
      .await
  );
  state.route_rate.lock().await.insert(
    "stale-route".to_string(),
    RateLimitState {
      tokens: 1.0,
      last_updated: old,
    },
  );
  let session = |expires_at: u64| crate::store::sessions::SessionInfo {
    plane: crate::store::sessions::Plane::Admin,
    expires_at,
    created_at: 0,
    ip: None,
    user_agent: None,
    scope_host: None,
    username: None,
    role: crate::store::users::Role::Admin,
    selected_org: None,
    bound_org: None,
  };
  {
    let mut sessions = state.sessions.lock().await;
    sessions.insert("expired", session(1));
    sessions.insert("live", session(crate::store::tokens::now_secs() + 3600));
  }

  state.gc_tick_once(Instant::now()).await;

  let rates = state.rate_limiter.lock().await;
  assert!(
    !rates.contains_key(&"203.0.113.9".parse().unwrap()),
    "stale IP swept"
  );
  assert!(
    rates.contains_key(&"198.51.100.7".parse().unwrap()),
    "fresh IP kept"
  );
  drop(rates);
  assert!(
    state.route_rate.lock().await.is_empty(),
    "stale route swept"
  );
  let sessions = state.sessions.lock().await;
  assert!(sessions.get("expired").is_none());
  assert!(sessions.get("live").is_some());
}

// --- inline route rate limits (planned_features #26) ------------------------

#[tokio::test]
async fn an_inline_route_rate_limit_applies_and_respects_its_method_filter() {
  let mut cfg = crate::test_support::test_config();
  let rules: Vec<crate::static_routes::RouteRule> = serde_yaml::from_str(
    r#"
- path: /upload
  rate_limit:
    rps: 1
    burst: 2
    methods: [POST]
"#,
  )
  .unwrap();
  cfg.static_routes = crate::static_routes::StaticRoutes::compile(rules).unwrap();
  let state = crate::test_support::test_state_with(cfg);

  // Two POSTs fit the burst, the third does not.
  assert!(state.check_route_rate_limit(None, "/upload", "POST").await);
  assert!(state.check_route_rate_limit(None, "/upload", "POST").await);
  assert!(!state.check_route_rate_limit(None, "/upload", "POST").await);
  // A GET on the same path is outside the filter, so it is never charged and
  // never refused, even though the POST bucket is empty.
  assert!(state.check_route_rate_limit(None, "/upload", "GET").await);
  assert!(state.check_route_rate_limit(None, "/upload", "GET").await);
}

#[tokio::test]
async fn an_inline_route_limit_wins_over_a_rate_limits_entry() {
  let mut cfg = crate::test_support::test_config();
  let rules: Vec<crate::static_routes::RouteRule> =
    serde_yaml::from_str("- path: /x\n  rate_limit:\n    rps: 1\n    burst: 1\n").unwrap();
  cfg.static_routes = crate::static_routes::StaticRoutes::compile(rules).unwrap();
  cfg.route_limits = crate::route_limits::RouteLimits {
    rules: crate::route_limits::compile(
      serde_yaml::from_str("- path: /x\n  rps: 1000\n  burst: 1000\n").unwrap(),
    ),
  };
  let state = crate::test_support::test_state_with(cfg);

  // The generous `rate_limits:` entry matches too, but the route's own limit
  // is the one written next to the route, so it is the one that applies.
  assert!(state.check_route_rate_limit(None, "/x", "GET").await);
  assert!(!state.check_route_rate_limit(None, "/x", "GET").await);
}

// ---------------------------------------------------------------------------
// Per-visitor streamed-response ceiling (planned_features #20)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_stream_ceiling_is_off_by_default() {
  let state = crate::test_support::test_state();
  // A NAT or a CGNAT puts many real people behind one address, so a default
  // here would be a guess with a queue of users behind it.
  assert_eq!(state.config().max_streams_per_ip, 0);
  assert!(
    state
      .try_acquire_stream_slot("203.0.113.7".parse().unwrap())
      .is_none(),
    "off means the caller takes the ungated path, not that a slot is handed out"
  );
}

#[tokio::test]
async fn a_visitor_holds_at_most_its_share_and_gets_it_back() {
  let mut config = crate::test_support::test_config();
  config.max_streams_per_ip = 2;
  let state = crate::test_support::test_state_with(config);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();

  let first = state.try_acquire_stream_slot(ip).expect("one");
  let second = state.try_acquire_stream_slot(ip).expect("two");
  assert!(
    state.try_acquire_stream_slot(ip).is_none(),
    "the third is refused"
  );

  // A concurrency limit, not a rate limit: closing a stream frees its slot at
  // once, so a visitor that opens and closes as fast as it likes never trips
  // it and one that holds them open does.
  drop(second);
  let third = state.try_acquire_stream_slot(ip).expect("a freed slot");
  drop(first);
  drop(third);

  // Nothing left behind: the map holds only the addresses currently
  // streaming, or it grows one entry per stranger for the life of the process.
  assert!(
    state.stream_counts.lock().unwrap().get(&ip).is_none(),
    "the entry is removed at zero, not left at zero"
  );
}

#[tokio::test]
async fn one_visitor_at_its_ceiling_does_not_block_another() {
  let mut config = crate::test_support::test_config();
  config.max_streams_per_ip = 1;
  let state = crate::test_support::test_state_with(config);
  let noisy: std::net::IpAddr = "203.0.113.7".parse().unwrap();
  let quiet: std::net::IpAddr = "198.51.100.4".parse().unwrap();

  let _held = state.try_acquire_stream_slot(noisy).expect("one");
  assert!(state.try_acquire_stream_slot(noisy).is_none());
  // The whole point: saturating the service now takes a botnet rather than
  // one host.
  assert!(state.try_acquire_stream_slot(quiet).is_some());
}

// ---------------------------------------------------------------------------
// Minimum-throughput guard for streamed responses (planned_features #17)
// ---------------------------------------------------------------------------

#[test]
fn the_throughput_floor_is_off_unless_asked_for() {
  let mut guard = super::ThroughputGuard::new(0);
  let start = Instant::now();
  // A stream that takes nothing at all, for far longer than the window.
  assert!(!guard.record(
    0,
    Duration::from_secs(600),
    start + Duration::from_secs(600)
  ));
}

#[test]
fn a_reader_that_keeps_data_waiting_and_takes_too_little_is_ended() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  // Thirty seconds of the consumer holding data up, and 100 bytes taken.
  // This is the hole the per-item stall timeout leaves: it accepted chunks,
  // so it never timed out, it just accepted them impossibly slowly.
  assert!(guard.record(
    100,
    Duration::from_secs(30),
    start + super::MIN_THROUGHPUT_WINDOW
  ));
}

#[test]
fn a_reader_that_keeps_up_is_left_alone() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  assert!(!guard.record(
    1024 * 60,
    Duration::from_secs(30),
    start + super::MIN_THROUGHPUT_WINDOW
  ));
}

#[test]
fn a_quiet_backend_never_costs_the_consumer_its_stream() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  // An hour of wall clock, one byte delivered, but the consumer only ever
  // kept data waiting for a millisecond: server-sent events and long polling
  // look exactly like this, and measuring wall-clock throughput would end
  // them. What is measured is "data was ready and you did not take it".
  assert!(!guard.record(
    1,
    Duration::from_millis(1),
    start + Duration::from_secs(3600)
  ));
}

#[test]
fn the_verdict_is_taken_once_per_window_and_the_counters_reset() {
  let mut guard = super::ThroughputGuard::new(1024);
  let start = Instant::now();
  // Inside the window nothing is decided, however bad it looks.
  assert!(!guard.record(0, Duration::from_secs(29), start + Duration::from_secs(29)));
  // The window closes and this one fails.
  let closed = start + super::MIN_THROUGHPUT_WINDOW + Duration::from_secs(1);
  assert!(guard.record(0, Duration::from_secs(2), closed));
  // A fresh window starts clean: the previous window's starvation must not
  // be held against a consumer that has started keeping up.
  assert!(!guard.record(
    1024 * 60,
    Duration::from_secs(30),
    closed + super::MIN_THROUGHPUT_WINDOW
  ));
}

// ---------------------------------------------------------------------------
// Differentiated rate budgets (planned_features #64)
// ---------------------------------------------------------------------------

/// A state whose bucket holds exactly `tokens` and never refills, so what a
/// test measures is the price and not the clock.
fn state_with_budget(tokens: f64) -> AppState {
  let mut config = crate::test_support::test_config();
  config.ip_limit_max = tokens;
  config.ip_limit_refill = 0.0;
  crate::test_support::test_state_with(config)
}

#[test]
fn the_three_prices_are_ordered_and_separated() {
  // This, not the specific numbers, is what the design claims. The magnitudes
  // are a judgement about how much pressure a shared bucket can take, and
  // they have moved once already.
  assert!(RateCost::Cheap.tokens() < RateCost::Guessable.tokens());
  assert!(RateCost::Guessable.tokens() < RateCost::Expensive.tokens());
}

#[tokio::test]
async fn a_credential_attempt_costs_more_than_a_read() {
  let budget = 20.0;
  let state = state_with_budget(budget);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();

  let reads = (budget / RateCost::Cheap.tokens()) as usize;
  let guesses = (budget / RateCost::Guessable.tokens()) as usize;
  assert!(guesses < reads, "a login has to be dearer than a page view");

  for i in 0..guesses {
    assert!(
      state.check_rate_limit_cost(ip, RateCost::Guessable).await,
      "attempt {i} fits"
    );
  }
  assert!(
    !state.check_rate_limit_cost(ip, RateCost::Guessable).await,
    "and the next does not"
  );
}

#[tokio::test]
async fn the_budget_is_shared_between_the_classes() {
  let state = state_with_budget(RateCost::Guessable.tokens() * 2.0);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
  // Two login attempts empty a bucket that holds exactly two of them, and
  // nothing is left for a read. One bucket at different prices, not a bucket
  // per class: separate buckets would let an attacker spend a full allowance
  // on each.
  assert!(state.check_rate_limit_cost(ip, RateCost::Guessable).await);
  assert!(state.check_rate_limit_cost(ip, RateCost::Guessable).await);
  assert!(!state.check_rate_limit(ip).await, "nothing is left");
}

#[tokio::test]
async fn a_refused_call_is_not_charged() {
  // Just under the price of the expensive call, so it is refused.
  let state = state_with_budget(RateCost::Expensive.tokens() - 0.5);
  let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
  assert!(!state.check_rate_limit_cost(ip, RateCost::Expensive).await);
  // A call that was turned away has not been served, so it should not have
  // been paid for either: the cheap budget is untouched.
  assert!(state.check_rate_limit(ip).await);
}

// ---------------------------------------------------------------------------
// Fair-share capture eviction (planned_features #69)
// ---------------------------------------------------------------------------

/// A capture belonging to `org`, with `id` so eviction order is observable.
fn capture_of(org: Option<&str>, id: &str) -> CapturedRequest {
  CapturedRequest {
    id: id.to_string(),
    timestamp: String::new(),
    method: "GET".to_string(),
    uri: "/".to_string(),
    req_headers: Vec::new(),
    req_body: None,
    req_body_truncated: false,
    status: 200,
    resp_headers: Vec::new(),
    resp_body: None,
    resp_body_truncated: false,
    resp_streamed: false,
    duration_ms: 0,
    timeline: None,
    client_id: "c1".to_string(),
    client_name: None,
    org_id: org.map(str::to_string),
  }
}

fn orgs_in(captured: &VecDeque<CapturedRequest>) -> Vec<Option<String>> {
  captured.iter().map(|c| c.org_id.clone()).collect()
}

#[test]
fn a_noisy_organization_evicts_itself_not_a_quiet_one() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  // The quiet tenant's single capture arrived first, so front-eviction would
  // take it. That is the bug: a tenant investigating one request an hour
  // could never find it.
  captured.push_back(capture_of(Some("quiet"), "q1"));
  for i in 0..9 {
    captured.push_back(capture_of(Some("noisy"), &format!("n{i}")));
  }
  evict_for_fairness(&mut captured);
  assert!(
    orgs_in(&captured).contains(&Some("quiet".to_string())),
    "the quiet org's capture survived"
  );
  assert_eq!(captured.front().unwrap().id, "q1");
  assert_eq!(captured.len(), 9);
}

#[test]
fn eviction_takes_the_oldest_of_the_largest_holder() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  for i in 0..3 {
    captured.push_back(capture_of(Some("big"), &format!("b{i}")));
  }
  captured.push_back(capture_of(Some("small"), "s0"));
  evict_for_fairness(&mut captured);
  // Within the org being trimmed, the oldest goes: the front-eviction rule
  // applied inside the tenant rather than across tenants.
  assert_eq!(
    captured.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    vec!["b1", "b2", "s0"]
  );
}

#[test]
fn repeated_eviction_converges_on_an_even_split() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  for i in 0..10 {
    captured.push_back(capture_of(Some("a"), &format!("a{i}")));
  }
  // `b` arrives late and keeps inserting; each insert trims whoever holds
  // most, so `b` grows at `a`'s expense until they are even, without anyone
  // having chosen a per-org number.
  for i in 0..5 {
    evict_for_fairness(&mut captured);
    captured.push_back(capture_of(Some("b"), &format!("b{i}")));
  }
  let a = captured
    .iter()
    .filter(|c| c.org_id.as_deref() == Some("a"))
    .count();
  let b = captured
    .iter()
    .filter(|c| c.org_id.as_deref() == Some("b"))
    .count();
  assert_eq!((a, b), (5, 5));
}

#[test]
fn one_organization_alone_may_use_the_whole_buffer() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  for i in 0..5 {
    captured.push_back(capture_of(None, &format!("m{i}")));
  }
  evict_for_fairness(&mut captured);
  // A fixed per-org ceiling would have wasted the rest of the buffer here.
  assert_eq!(captured.len(), 4);
  assert_eq!(captured.front().unwrap().id, "m1");
}

#[test]
fn an_empty_buffer_is_left_alone() {
  let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
  evict_for_fairness(&mut captured);
  assert!(captured.is_empty());
}

#[test]
fn a_tie_is_broken_by_age_and_not_by_hash_order() {
  // Two tenants holding the same number: the one whose oldest capture is
  // oldest gives it up. Taking the maximum out of a HashMap would break this
  // differently on every call, so two equally busy tenants would take turns at
  // random and neither could predict what it kept.
  for _ in 0..20 {
    let mut captured: VecDeque<CapturedRequest> = VecDeque::new();
    captured.push_back(capture_of(Some("first"), "f0"));
    captured.push_back(capture_of(Some("second"), "s0"));
    captured.push_back(capture_of(Some("first"), "f1"));
    captured.push_back(capture_of(Some("second"), "s1"));
    evict_for_fairness(&mut captured);
    assert_eq!(captured.front().unwrap().id, "s0");
  }
}

/// A connection holding `filters` under a token, ready to have its grant
/// changed underneath it.
async fn subscribed(
  state: &std::sync::Arc<AppState>,
  connection_id: &str,
  token_id: &str,
  granted: &[&str],
  filters: &[&str],
) -> tokio::sync::mpsc::Receiver<axum::extract::ws::Message> {
  use crate::test_support::mock_client;
  let (tx, rx) = tokio::sync::mpsc::channel::<axum::extract::ws::Message>(16);
  let mut handle = mock_client(None, None, None, None);
  handle.tx = tx;
  handle.instance_group = Some(connection_id.to_string());
  handle.perms = ClientPerms {
    master: false,
    token_id: Some(token_id.to_string()),
    topics: granted.iter().map(|s| s.to_string()).collect(),
    ..ClientPerms::master()
  };
  state
    .clients
    .write()
    .await
    .insert(connection_id.to_string(), handle);
  let refused = crate::tunnel::pubsub::set_subscriptions(
    state,
    connection_id,
    filters.iter().map(|s| s.to_string()).collect(),
    true,
  )
  .await;
  assert!(refused.is_empty(), "unexpected refusals: {refused:?}");
  rx
}

/// The filters a connection is currently subscribed to.
async fn subscriptions_of(state: &AppState, connection_id: &str) -> Vec<String> {
  state.clients.read().await[connection_id]
    .subscriptions
    .clone()
}

#[tokio::test]
async fn narrowing_a_tokens_topics_withdraws_the_subscriptions_it_no_longer_covers() {
  // ClientPerms is a snapshot taken at connect. Without this, an edit in the
  // dashboard only took effect the next time the client happened to
  // reconnect, and a topic just taken away kept being delivered for as long
  // as the process stayed up.
  let state = std::sync::Arc::new(crate::test_support::test_state());
  let mut rx = subscribed(
    &state,
    "c",
    "tok",
    &["deploy/#", "metrics/#"],
    &["deploy/web", "metrics/cpu"],
  )
  .await;

  let withdrawn = state
    .apply_token_topics("tok", &["deploy/#".to_string()])
    .await;
  assert_eq!(withdrawn, 1);
  assert_eq!(subscriptions_of(&state, "c").await, vec!["deploy/web"]);

  // Told, not silently dropped: the client already logs this frame by name,
  // so the withdrawal shows up where someone is looking.
  let Ok(axum::extract::ws::Message::Text(text)) = rx.try_recv() else {
    panic!("the client was not told which filter was withdrawn");
  };
  let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
  assert_eq!(parsed["type"], "SubscribeRefused");
  assert_eq!(parsed["topic"], "metrics/cpu");
  assert!(
    rx.try_recv().is_err(),
    "only the withdrawn filter is reported"
  );

  // The cached grant moved too, so the next Subscribe is judged by the new
  // list rather than by the one the connection was born with.
  let refused =
    crate::tunnel::pubsub::set_subscriptions(&state, "c", vec!["metrics/cpu".to_string()], true)
      .await;
  assert_eq!(
    refused.len(),
    1,
    "re-subscribing under the old grant worked"
  );
}

#[tokio::test]
async fn a_widened_grant_keeps_everything_and_leaves_other_tokens_alone() {
  let state = std::sync::Arc::new(crate::test_support::test_state());
  let mut mine = subscribed(&state, "c", "tok", &["deploy/#"], &["deploy/web"]).await;
  let mut theirs = subscribed(&state, "other", "tok-2", &["deploy/#"], &["deploy/web"]).await;

  let withdrawn = state
    .apply_token_topics("tok", &["#".to_string(), "$aperio/client/#".to_string()])
    .await;
  assert_eq!(withdrawn, 0, "a widening withdraws nothing");
  assert_eq!(subscriptions_of(&state, "c").await, vec!["deploy/web"]);
  assert!(mine.try_recv().is_err());

  // Another token's connection is not touched, however similar it looks.
  assert_eq!(subscriptions_of(&state, "other").await, vec!["deploy/web"]);
  assert!(theirs.try_recv().is_err());
}

/// Every field of the wire's `ServiceDecl` is accounted for on the handle.
///
/// The table above `ClientHandle` is the only written record of which of its
/// fields are service-scoped and therefore become many when #46 splits
/// identity into `(connection, service)`. A record like that is worth exactly
/// as much as the thing that stops it going stale: a field added to the wire
/// without a line here would be a service setting nobody classified, and the
/// split would silently leave it on the connection, which is the same as
/// giving every service the last one's value.
#[test]
fn the_wire_says_what_a_service_is_and_the_handle_accounts_for_all_of_it() {
  let declared = struct_fields(include_str!("protocol.rs"), "ServiceDecl");
  assert!(
    !declared.is_empty(),
    "the protocol still declares ServiceDecl"
  );

  let mapped: Vec<&str> = SERVICE_DECL_IN_SERVICE_STATE
    .iter()
    .map(|(w, _)| *w)
    .collect();

  let unclassified: Vec<&String> = declared
    .iter()
    .filter(|f| !mapped.contains(&f.as_str()))
    .collect();
  assert!(
    unclassified.is_empty(),
    "ServiceDecl gained {unclassified:?} and SERVICE_DECL_IN_SERVICE_STATE does not say where \
     it lands. Add a line, with None if it does not reach the handle at all."
  );

  let invented: Vec<&&str> = mapped
    .iter()
    .filter(|w| !declared.contains(&w.to_string()))
    .collect();
  assert!(
    invented.is_empty(),
    "SERVICE_DECL_IN_SERVICE_STATE names {invented:?}, which the wire no longer has."
  );

  let mut seen = std::collections::HashSet::new();
  for w in &mapped {
    assert!(seen.insert(*w), "{w} is listed twice");
  }
}

/// And every field the table points at actually exists, in `ServiceState`.
///
/// The other direction of the same drift: a rename would leave the table
/// pointing at nothing, and it would still read as authority.
#[test]
fn every_field_the_table_points_at_is_a_field_the_service_has() {
  let service = struct_fields(include_str!("state.rs"), "ServiceState");
  assert!(!service.is_empty(), "ServiceState is still a struct");

  let dangling: Vec<&str> = SERVICE_DECL_IN_SERVICE_STATE
    .iter()
    .filter_map(|(_, h)| *h)
    .filter(|h| !service.contains(&h.to_string()))
    .collect();
  assert!(
    dangling.is_empty(),
    "SERVICE_DECL_IN_SERVICE_STATE points at {dangling:?}, which ServiceState does not have. \
     A rename has to be made in both places."
  );
}

/// The two structs divide the fields the way the three lists say they should.
///
/// The compiler already stops a service field from being read off a
/// connection, which is the half a type can do. It cannot say the division is
/// the right one: a field put in the wrong struct compiles, and the mistake
/// only shows later as one value shared by services that should each have had
/// their own, or as a warn-once flag that silences the second service because
/// the first already warned. Neither is a compile error and neither fails any
/// other test, so this is the only thing standing between the seam and a
/// quiet drift back across it.
#[test]
fn the_two_structs_divide_the_fields_the_way_the_seam_says() {
  let src = include_str!("state.rs");
  let handle = struct_fields(src, "ClientHandle");
  let service = struct_fields(src, "ServiceState");
  assert!(!handle.is_empty() && !service.is_empty());

  let mut want_service: Vec<&str> = SERVICE_DECL_IN_SERVICE_STATE
    .iter()
    .filter_map(|(_, h)| *h)
    .collect();
  want_service.extend(SERVICE_SCOPED_DERIVED.iter().copied());

  let mut seen = std::collections::HashSet::new();
  for f in &want_service {
    assert!(seen.insert(*f), "{f} is claimed twice by the service side");
  }

  let mut stray: Vec<&String> = service
    .iter()
    .filter(|f| !want_service.contains(&f.as_str()))
    .collect();
  assert!(
    stray.is_empty(),
    "ServiceState carries {stray:?}, which the seam does not call service-scoped. \
     Either it belongs on ClientHandle, or a list has to say why it is here."
  );

  let missing: Vec<&&str> = want_service
    .iter()
    .filter(|f| !service.contains(&f.to_string()))
    .collect();
  assert!(
    missing.is_empty(),
    "the seam calls {missing:?} service-scoped, but they are not in ServiceState."
  );

  // The connection side, and the one field that joins the two.
  let mut want_handle: Vec<&str> = CONNECTION_SCOPED.to_vec();
  want_handle.push("services");
  stray = handle
    .iter()
    .filter(|f| !want_handle.contains(&f.as_str()))
    .collect();
  assert!(
    stray.is_empty(),
    "ClientHandle gained {stray:?} and nothing says whether it belongs to the connection \
     or to the service. Put it in CONNECTION_SCOPED, or in ServiceState."
  );
  let missing: Vec<&&str> = want_handle
    .iter()
    .filter(|f| !handle.contains(&f.to_string()))
    .collect();
  assert!(
    missing.is_empty(),
    "the seam calls {missing:?} connection-scoped, but ClientHandle does not have them."
  );
}

/// Field names of a struct, read from source. Reading them avoids the one
/// alternative, a second hand-written list, which is the thing being guarded
/// against in the first place.
#[cfg(test)]
fn struct_fields(source: &str, name: &str) -> Vec<String> {
  let Some(start) = source.find(&format!("struct {name} {{")) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for line in source[start..].lines().skip(1) {
    let line = line.trim();
    if line == "}" {
      break;
    }
    let Some(rest) = line
      .strip_prefix("pub(crate) ")
      .or_else(|| line.strip_prefix("pub "))
    else {
      continue;
    };
    if let Some((field, _)) = rest.split_once(':')
      && field.chars().all(|c| c.is_ascii_lowercase() || c == '_')
      && !field.is_empty()
    {
      out.push(field.to_string());
    }
  }
  out
}

/// `sole` and `sole_mut` address the same service.
///
/// They are two methods over a list, and nothing but this says they agree.
/// If one ever reached for the first entry and the other for the last, every
/// call site would still compile and every test would still pass while the
/// length is one, which it is everywhere today. The bug would appear on the
/// day a second service arrives, in the form of writes landing somewhere the
/// reads do not look, and it would appear in four hundred places at once.
///
/// So the list is given a second entry here, which is the only place in the
/// tree that does it, precisely because that is the condition under which
/// the two could disagree.
#[test]
fn the_one_service_written_to_is_the_one_read_back() {
  let mut handle = crate::test_support::mock_client(Some("first.example"), None, None, None);
  let second = crate::test_support::mock_client(Some("second.example"), None, None, None);
  handle.services.extend(second.services);
  assert_eq!(handle.services.len(), 2, "the case worth testing");

  handle.sole_mut().response_timeout = Some(77);
  assert_eq!(
    handle.sole().response_timeout,
    Some(77),
    "a write through sole_mut is visible through sole"
  );
  assert_eq!(
    handle.services[1].response_timeout, None,
    "and it went to one service, not to every service"
  );
}

/// A handle carries at least one service, which is what lets `sole` return a
/// reference instead of an `Option`.
///
/// Pinned at the constructor the tests themselves use, because an invariant
/// that only holds in production is an invariant the tests will break first.
#[test]
fn a_handle_is_never_built_without_a_service() {
  let handle = crate::test_support::mock_client(None, None, None, None);
  assert!(!handle.services.is_empty());
}

/// A routing predicate answers for the service it is called on.
///
/// This is the whole point of moving them off `ClientHandle`. There they read
/// `sole()`, so on a connection carrying two services both would have
/// answered for the first, and routing would have sent every request for the
/// second service to the first one's backend. The methods look identical
/// either way and nothing else in the tree can tell the difference yet,
/// because nothing else builds a two-service handle.
#[test]
fn each_service_answers_the_routing_questions_for_itself() {
  let mut handle = crate::test_support::mock_client(Some("first.example"), Some("/a"), None, None);
  let second = crate::test_support::mock_client(Some("second.example"), Some("/b"), None, None);
  handle.services.extend(second.services);

  assert!(handle.services[0].matches_host("first.example"));
  assert!(!handle.services[0].matches_host("second.example"));
  assert!(handle.services[1].matches_host("second.example"));
  assert!(!handle.services[1].matches_host("first.example"));

  assert_eq!(
    handle.services[0].effective_path_bind().map(String::as_str),
    Some("/a")
  );
  assert_eq!(
    handle.services[1].effective_path_bind().map(String::as_str),
    Some("/b")
  );

  // And the connection's own view is still the first, which is what every
  // caller that has not been taught to pick a service still gets.
  assert!(handle.sole().matches_host("first.example"));
}

// ----- match_declarations: which service a Ping entry updates ---------------

/// Builds a connection's service list from names, `None` for a nameless one.
fn services_named(names: &[Option<&str>]) -> Vec<ServiceState> {
  names
    .iter()
    .map(|n| {
      let mut s = crate::test_support::mock_client(None, None, None, None)
        .services
        .remove(0);
      s.service_name = n.map(str::to_string);
      s
    })
    .collect()
}

fn names(v: &[Option<&str>]) -> Vec<Option<String>> {
  v.iter().map(|n| n.map(str::to_string)).collect()
}

#[test]
fn a_named_declaration_finds_its_own_service_however_the_list_is_ordered() {
  // The case position-matching gets wrong, and the reason this function
  // exists: the client reordered its `services:` block. Nothing about the
  // services changed, so nothing may move between them.
  let existing = services_named(&[Some("api"), Some("web")]);
  let got = match_declarations(&existing, &names(&[Some("web"), Some("api")])).unwrap();
  assert_eq!(got, vec![Some(1), Some(0)]);
}

#[test]
fn a_service_this_connection_does_not_carry_yet_is_reported_as_new() {
  let existing = services_named(&[Some("api")]);
  let got = match_declarations(&existing, &names(&[Some("api"), Some("jobs")])).unwrap();
  assert_eq!(got, vec![Some(0), None]);
}

#[test]
fn nameless_declarations_match_nameless_services_in_order() {
  // A client that names nothing is every client before #46, so this path has
  // to keep behaving exactly like the single-service one it replaces.
  let existing = services_named(&[None, None]);
  let got = match_declarations(&existing, &names(&[None, None])).unwrap();
  assert_eq!(got, vec![Some(0), Some(1)]);
}

#[test]
fn a_nameless_declaration_never_claims_a_named_service() {
  // Otherwise adding a name to one entry of a two-service config would hand
  // the other entry that service's history.
  let existing = services_named(&[Some("api"), None]);
  let got = match_declarations(&existing, &names(&[None])).unwrap();
  assert_eq!(got, vec![Some(1)]);
}

#[test]
fn a_named_declaration_adopts_a_service_that_has_no_name_yet() {
  // Not the mirror of the rule above, and the first draft of this had it
  // backwards. A connection is created carrying one nameless placeholder,
  // and it is the first Ping that names it. Refusing the adoption would mean
  // every client that names its service gets a second one appended beside
  // the empty one it meant to fill, on its very first heartbeat.
  //
  // It is also the kinder answer for a client that adds a `name:` to a
  // service it had been running without one: same service, new label, and no
  // reason to lose its counters over it. The named-first pass means this can
  // only fire when no service of that name exists, so it never steals one.
  let existing = services_named(&[None]);
  let got = match_declarations(&existing, &names(&[Some("api")])).unwrap();
  assert_eq!(got, vec![Some(0)]);
}

#[test]
fn no_two_declarations_land_on_the_same_service() {
  // Two nameless entries against one nameless service: the second is new,
  // not a second writer of the first one's state.
  let existing = services_named(&[None]);
  let got = match_declarations(&existing, &names(&[None, None])).unwrap();
  assert_eq!(got, vec![Some(0), None]);
}

#[test]
fn a_repeated_name_is_refused_rather_than_resolved() {
  // Either answer is wrong. Taking the first silently drops the second
  // service; taking the last silently drops the first. Both leave a client
  // serving less than its config says with nothing to read about it.
  let existing = services_named(&[Some("api")]);
  let err = match_declarations(&existing, &names(&[Some("api"), Some("api")])).unwrap_err();
  assert_eq!(err, "api");
}

#[test]
fn a_service_that_stopped_being_declared_is_simply_unclaimed() {
  // Nothing here removes it; the caller does. What this has to get right is
  // that its absence does not shift the others onto each other.
  let existing = services_named(&[Some("api"), Some("web"), Some("jobs")]);
  let got = match_declarations(&existing, &names(&[Some("jobs"), Some("api")])).unwrap();
  assert_eq!(got, vec![Some(2), Some(0)]);
}

/// A connection's hostnames are every service's, not the first one's.
///
/// The organization fence asks this question to decide whether one org may
/// mint a share link for, or act on, a hostname another org is currently
/// serving. Answering from the first service only leaves a hostname served by
/// the second invisible to the fence, which is a tenant boundary with a hole
/// in it rather than a display bug.
#[test]
fn a_connection_reports_the_hostnames_of_all_its_services() {
  let mut handle = crate::test_support::mock_client(Some("first.example"), None, None, None);
  let second = crate::test_support::mock_client(Some("second.example"), None, None, None);
  handle.services.extend(second.services);

  let hosts: Vec<&str> = handle
    .effective_hostnames()
    .into_iter()
    .map(String::as_str)
    .collect();
  assert!(hosts.contains(&"first.example"));
  assert!(
    hosts.contains(&"second.example"),
    "the second service's hostname is served, so the fence has to see it"
  );
}

// ---------------------------------------------------------------------------
// A connection carrying several services is asked about all of them (#122)
// ---------------------------------------------------------------------------

/// A two-service handle: the first serves `first`, the second `second`.
fn multiplexed_handle(first: &str, second: &str) -> ClientHandle {
  let mut handle = crate::test_support::mock_client(Some(first), None, None, None);
  handle.services[0].declared_hostnames = vec![first.to_string()];
  // Built the way `on_ping` builds one: a fresh service sharing the
  // connection's pacer cell, then given its own binds.
  let pacer = handle.services[0].bandwidth_bps.clone();
  let mut extra = crate::state::ServiceState::newly_declared(pacer);
  extra.declared_hostname = Some(second.to_string());
  extra.declared_hostnames = vec![second.to_string()];
  handle.services.push(extra);
  handle
}

#[tokio::test]
async fn the_org_fence_sees_a_hostname_held_by_a_later_service() {
  // The fence is a tenant boundary: narrowing an organization's allowlist has
  // to drop any connection still serving a name that left it. It asked the
  // first service only, so a multiplexed connection whose *second* service
  // held the revoked hostname passed the check and went on serving it, which
  // is the same hole `effective_hostnames` was fixed for and reachable the
  // moment a client could carry two services.
  let state = crate::test_support::test_state();
  let mut handle = multiplexed_handle("kept.example.com", "revoked.example.com");
  handle.perms.org_id = Some("acme".to_string());
  state.clients.write().await.insert("c1".to_string(), handle);

  let dropped = state
    .apply_org_hostnames("acme", &["kept.example.com".to_string()])
    .await;
  assert_eq!(
    dropped, 1,
    "the connection serves a name outside the allowlist and has to go"
  );
}

#[tokio::test]
async fn a_connection_serving_only_permitted_names_is_left_alone() {
  // The other half of the same check: iterating every service must not turn
  // the fence into something that drops connections it has no quarrel with.
  let state = crate::test_support::test_state();
  let mut handle = multiplexed_handle("a.example.com", "b.example.com");
  handle.perms.org_id = Some("acme".to_string());
  state.clients.write().await.insert("c1".to_string(), handle);

  let dropped = state
    .apply_org_hostnames(
      "acme",
      &["a.example.com".to_string(), "b.example.com".to_string()],
    )
    .await;
  assert_eq!(dropped, 0);
}

#[test]
fn process_scoped_answers_are_about_the_process_not_its_first_service() {
  let threshold = std::time::Duration::from_secs(30);
  let mut handle = multiplexed_handle("a.example.com", "b.example.com");
  assert!(handle.serves_process_scoped(threshold));

  // A raw `tunnels:` open and an `expose` lookup are about the client process.
  // Reading the first service's kill switch meant disabling `a` from the
  // dashboard silently took away a tunnel the process declared and served
  // just as well through `b`.
  handle.services[0].admin_enabled = false;
  assert!(
    handle.serves_process_scoped(threshold),
    "one disabled service does not take the process's tunnels away"
  );
  handle.services[1].admin_enabled = false;
  assert!(
    !handle.serves_process_scoped(threshold),
    "with nothing enabled there is no process left to serve them"
  );
}

#[test]
fn a_process_is_named_by_every_service_it_carries() {
  let mut handle = multiplexed_handle("a.example.com", "b.example.com");
  handle.services[0].service_name = Some("web".to_string());
  handle.services[1].service_name = Some("api".to_string());
  assert_eq!(handle.process_name().as_deref(), Some("web, api"));

  // One service reads exactly as it did, which is every deployment before
  // multiplexing.
  handle.services.truncate(1);
  assert_eq!(handle.process_name().as_deref(), Some("web"));
}
