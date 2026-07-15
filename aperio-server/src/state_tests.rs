use super::*;

fn perms(hostnames: &[&str], paths: &[&str]) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: hostnames.iter().map(|s| s.to_string()).collect(),
    paths: paths.iter().map(|s| s.to_string()).collect(),
    token_name: Some("t".to_string()),
    token_id: Some("id".to_string()),
    allow_public: false,
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
    stats.record(Some("app.local"), &tl(100, 5_000));
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
  stats.record(Some("app.local"), &tl(100, 80_000));
  let rows = stats.routes.get("app.local").unwrap().stats();
  let backend_wait = rows.iter().find(|r| r.stage == "backend_wait").unwrap();
  assert!(backend_wait.anomalous, "outlier must be flagged");
  let queue = rows.iter().find(|r| r.stage == "queue").unwrap();
  assert!(!queue.anomalous, "an unrelated stage must stay quiet");
}
