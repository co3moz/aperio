//! What a token may do and what a connection is: permission gating and the
//! organization fence over it, the request timeline and its per-stage statistics,
//! and the bounded structures that keep a long-lived server from growing without
//! end.

use super::*;
use crate::store::tokens::TokenSpec;

fn perms(hostnames: &[&str], paths: &[&str]) -> ClientPerms {
  ClientPerms {
    allow_server_side: false,
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
    allow_server_side: false,
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

/// changed underneath it.
/// A connection holding `filters` under a token, ready to have its grant
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

/// A server-side WebSocket upgrade spends one slot, not two.
///
/// `handle_ws_proxy` takes a slot before any of the work, and the server-side
/// branch used to take a second one of its own. Both were held at once, so
/// with the cap one away from full the upgrade spent the last slot at the
/// caller and was then refused by its own acquisition, answering 503 with a
/// slot free. The caller hands its slot over now, which this pins from the
/// only side a test can see: the count never exceeds one per upgrade.
#[tokio::test]
async fn one_websocket_upgrade_spends_one_slot() {
  use std::sync::atomic::Ordering;
  let mut cfg = crate::test_support::test_config();
  cfg.max_ws_connections = 1;
  let state = crate::test_support::test_state_with(cfg);

  let first = state.try_acquire_ws_slot().expect("the only slot");
  assert_eq!(state.active_ws_connections.load(Ordering::SeqCst), 1);
  // A second acquisition is what the old code did while still holding the
  // first, and it is what made the refusal spurious.
  assert!(
    state.try_acquire_ws_slot().is_none(),
    "the cap is one, so a second slot must not be available; an upgrade that \
     needs two cannot run at the cap"
  );
  drop(first);
  assert_eq!(state.active_ws_connections.load(Ordering::SeqCst), 0);
  assert!(
    state.try_acquire_ws_slot().is_some(),
    "the slot returns when the upgrade ends"
  );
}
