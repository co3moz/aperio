use super::*;

fn perms(hostnames: &[&str], paths: &[&str]) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: hostnames.iter().map(|s| s.to_string()).collect(),
    paths: paths.iter().map(|s| s.to_string()).collect(),
    token_name: Some("t".to_string()),
    token_id: Some("id".to_string()),
    allow_public: false,
    org_id: None,
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
      backend_done_us: 7_500,
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
      backend_done_us: 3,
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
        backend_done_us: 150 + backend,
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
  assert_eq!(c.effective_path_bind(), Some(&"/api".to_string()));
  assert!(c.matches_host("a.local"));
  assert!(c.has_hostname_bind());

  // override path wins over declared.
  let c = mock_client(Some("a.local"), Some("/api"), None, Some("/ovr"));
  assert_eq!(c.effective_path_bind(), Some(&"/ovr".to_string()));

  // assigned path used when nothing declared/overridden.
  let mut c = mock_client(None, None, None, None);
  c.assigned_path = Some("/assigned".to_string());
  assert_eq!(c.effective_path_bind(), Some(&"/assigned".to_string()));

  // hostname override replaces the whole set.
  let c = mock_client(Some("a.local"), None, Some("override.local"), None);
  assert_eq!(c.effective_hostnames(), vec![&"override.local".to_string()]);
  assert!(c.matches_host("override.local"));
  assert!(!c.matches_host("a.local"));

  // union of assigned + declared + extra declared hostnames, de-duplicated.
  let mut c = mock_client(Some("declared.local"), None, None, None);
  c.assigned_hostnames = vec!["assigned.local".to_string(), "declared.local".to_string()];
  c.declared_hostnames = vec!["extra.local".to_string(), "assigned.local".to_string()];
  let hosts = c.effective_hostnames();
  assert!(hosts.contains(&&"assigned.local".to_string()));
  assert!(hosts.contains(&&"declared.local".to_string()));
  assert!(hosts.contains(&&"extra.local".to_string()));
  assert_eq!(hosts.len(), 3, "duplicates collapse");

  // no binds at all.
  let c = mock_client(None, None, None, None);
  assert!(!c.has_hostname_bind());
  assert!(c.effective_path_bind().is_none());
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
  assert!(!c.is_ejected(now));
  // Below the failure threshold: no ejection.
  let window = Duration::from_secs(30);
  let eject_for = Duration::from_secs(30);
  assert!(!c.record_failure(now, window, 3, eject_for));
  assert!(!c.record_failure(now, window, 3, eject_for));
  // The third failure inside the window trips the ejection.
  assert!(c.record_failure(now, window, 3, eject_for));
  assert!(c.is_ejected(now));
  // Failures are cleared once ejected; a repeat call while ejected is a no-op.
  assert!(!c.record_failure(now, window, 3, eject_for));

  // Stale failures outside the window are pruned before counting.
  let mut c2 = mock_client(None, None, None, None);
  let old = now - Duration::from_secs(120);
  c2.recent_failures.push_back(old);
  c2.recent_failures.push_back(old);
  assert!(!c2.record_failure(now, window, 3, eject_for));
  assert_eq!(c2.recent_failures.len(), 1, "old failures pruned");
}

// ----- AppState: config, request slots -----

#[tokio::test]
async fn test_config_snapshot_and_request_slots() {
  use crate::test_support::test_config;
  let mut cfg = test_config();
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
    let (tok, _secret) = store.create(
      "rps".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      Some(1.0),
      None,
      false,
      false,
      None,
    );
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
    let (tok, _secret) = store.create(
      "quota".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      None,
      Some(100),
      false,
      false,
      None,
    );
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
  assert!(state.check_org_token_quota(None).await.is_ok());
  assert!(state.check_org_user_quota(None).await.is_ok());
  assert!(state.check_org_client_quota(None).await.is_ok());
  assert!(!state.org_over_month_bytes(None).await);

  let org_id = {
    let mut orgs = state.org_store.lock().await;
    let org = orgs.create("acme").expect("org");
    orgs.set_quota(
      &org.id,
      Some(Some(1)), // max_clients
      Some(Some(1)), // max_tokens
      Some(Some(1)), // max_users
      Some(Some(50)),
    );
    org.id
  };
  assert!(state.org_quota(Some(&org_id)).await.is_some());

  // Under the caps everything is allowed.
  assert!(state.check_org_token_quota(Some(&org_id)).await.is_ok());
  assert!(state.check_org_user_quota(Some(&org_id)).await.is_ok());
  assert!(state.check_org_client_quota(Some(&org_id)).await.is_ok());

  // Add a token in the org → the token cap of 1 is now reached.
  {
    let mut store = state.token_store.lock().await;
    store.create(
      "t".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      None,
      None,
      false,
      false,
      Some(org_id.clone()),
    );
  }
  assert!(
    state.check_org_token_quota(Some(&org_id)).await.is_err(),
    "token quota reached"
  );

  // Month-bytes quota is not exceeded with no traffic recorded.
  assert!(!state.org_over_month_bytes(Some(&org_id)).await);
  // Unknown org id → no quota.
  assert!(!state.org_over_month_bytes(Some("missing")).await);
}

// ----- AppState: rate limiting -----

#[tokio::test]
async fn test_ip_rate_limit_exhausts() {
  use crate::test_support::test_config;
  let mut cfg = test_config();
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
  assert!(state.check_route_rate_limit(Some("a.local"), "/x").await);
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
    org_id: None,
  };
  state.clients.lock().await.insert("c1".to_string(), c);
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
