//! Unit tests for the retention pruner: the pure helpers, one `run_cycle`
//! over every configured surface, and the disk-usage guard. The background
//! `spawn` loop is exercised for a single immediate cycle only (its interval
//! ticks once right away, then blocks for an hour).

use super::*;
use crate::test_support::*;

use std::sync::atomic::Ordering;
use std::time::Duration;

/// `DISK_WARNED` is process-global too (one guard loop per process), so the
/// disk-guard tests, each of which seeds it and asserts what a cycle did to
/// it, corrupt each other when they interleave: one test's cycle stores
/// `false` between another's cycle and its assertion. Reproduced at 21
/// failures in 40 runs of the three tests alone. A tokio mutex because the
/// guard is held across the cycle's awaits, and an async lock neither trips
/// the held-across-await lint nor can be poisoned by a failing test.
///
/// **Every test that can drive a cycle takes it, which is more than the tests
/// with `disk_guard` in the name** (`planned_features.md` #112). A `spawn`
/// test that sets `retention.db_max_size` starts the real pruner, whose first
/// tick fires immediately and calls the same `disk_guard_cycle` against the
/// same global, on its own runtime, on another thread, in parallel with
/// whatever else `cargo test` is running. Those two were once holding only
/// an environment lock, so a guard cycle on an almost-empty directory could store
/// `false` in the middle of the warn test and fail it about one full run in
/// twenty: rare enough to read as noise, which is what makes it worth
/// naming rather than rerunning.
static DISK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Puts a `retention` section in force on this thread, as `(field, value)`
/// pairs; none at all is the defaults.
fn retention(pairs: &[(&str, &str)]) {
  if pairs.is_empty() {
    crate::live::reset();
    return;
  }
  let mut yaml = String::from("retention:\n");
  for (field, value) in pairs {
    yaml.push_str(&format!("  {field}: {value}\n"));
  }
  crate::live::set_yaml(&yaml);
}

fn now() -> u64 {
  crate::store::tokens::now_secs()
}

/// RFC3339 timestamp string for the current instant (parseable, recent).
fn ts_now() -> String {
  chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// A long-expired RFC3339 timestamp string.
fn ts_old() -> String {
  "2000-01-01T00:00:00+00:00".to_string()
}

fn capture_with(id: &str, timestamp: String) -> crate::state::CapturedRequest {
  crate::state::CapturedRequest {
    id: id.to_string(),
    timestamp,
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
    duration_ms: 1,
    timeline: None,
    client_id: "c1".to_string(),
    client_name: None,
    org_id: None,
  }
}

fn inbox_entry_with(id: &str, timestamp: String) -> crate::store::inbox::InboxEntry {
  crate::store::inbox::InboxEntry {
    id: id.to_string(),
    timestamp,
    method: "POST".to_string(),
    uri: "/hook".to_string(),
    host: None,
    headers: Vec::new(),
    body: None,
    body_truncated: false,
    status: 200,
    service: None,
    org_id: None,
  }
}

fn delivery_with(id: &str, created_at: u64) -> crate::store::webhooks::Delivery {
  crate::store::webhooks::Delivery {
    id: id.to_string(),
    webhook_id: "w".to_string(),
    webhook_name: "w".to_string(),
    org_id: None,
    event: "e".to_string(),
    timestamp: ts_now(),
    success: true,
    status: Some(200),
    error: None,
    attempts: 1,
    duration_ms: 1,
    body: String::new(),
    created_at,
  }
}

// --- Pure helpers -----------------------------------------------------------

#[test]
fn a_retention_setting_counts_whole_days() {
  // Unset.
  retention(&[]);
  assert_eq!(Surface::Captures.days(), None);
  // Zero, and anything under a day, is "keep forever".
  retention(&[("captures", "0s")]);
  assert_eq!(Surface::Captures.days(), None);
  retention(&[("captures", "12h")]);
  assert_eq!(Surface::Captures.days(), None);
  retention(&[("captures", "7d"), ("stats", "36h")]);
  assert_eq!(Surface::Captures.days(), Some(7));
  assert_eq!(Surface::Stats.days(), Some(1), "a part day rounds down");
  assert_eq!(Surface::Audit.days(), None, "each surface is its own");
}

#[test]
fn cutoff_ts_subtracts_and_saturates() {
  let today = now();
  let one_day = cutoff_ts(1);
  assert!(one_day <= today);
  assert!(today - one_day >= 24 * 3600 - 5);
  // A wildly large TTL saturates to zero rather than underflowing.
  assert_eq!(cutoff_ts(u64::MAX), 0);
}

#[test]
fn report_total_sums_every_surface() {
  let report = RetentionReport {
    captures: 1,
    inbox: 2,
    access_log_lines: 3,
    audit_events: 4,
    stats_buckets: 5,
  };
  assert_eq!(report.total(), 15);
  assert_eq!(RetentionReport::default().total(), 0);
}

// --- run_cycle --------------------------------------------------------------

#[tokio::test]
async fn run_cycle_is_a_noop_when_unconfigured() {
  retention(&[]);
  let state = std::sync::Arc::new(test_state());
  state
    .captured_requests
    .lock()
    .await
    .push_back(capture_with("c1", ts_old()));

  let report = run_cycle(&state).await;

  assert_eq!(report.total(), 0);
  // Nothing configured, so the old capture survives.
  assert_eq!(state.captured_requests.lock().await.len(), 1);
}

#[tokio::test]
async fn run_cycle_prunes_captures_and_inbox() {
  retention(&[("captures", "1d")]);
  let state = std::sync::Arc::new(test_state());
  {
    let mut caps = state.captured_requests.lock().await;
    caps.push_back(capture_with("old", ts_old()));
    caps.push_back(capture_with("new", ts_now()));
  }
  {
    let mut inbox = state.inbox_store.lock().await;
    inbox.insert(inbox_entry_with("old", ts_old()));
    inbox.insert(inbox_entry_with("new", ts_now()));
  }

  let report = run_cycle(&state).await;

  assert_eq!(report.captures, 1);
  assert_eq!(report.inbox, 1);
  let caps = state.captured_requests.lock().await;
  assert_eq!(caps.len(), 1);
  assert_eq!(caps[0].id, "new");
  assert_eq!(state.inbox_store.lock().await.list(&None).len(), 1);
}

#[tokio::test]
async fn run_cycle_prunes_access_log() {
  retention(&[("access_log", "1d")]);
  let dir =
    crate::test_support::test_temp_root().join(format!("accesslog-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let path = dir.join("access.log");
  let old_line = format!("{{\"ts\":\"{}\",\"path\":\"/old\"}}", ts_old());
  let new_line = format!("{{\"ts\":\"{}\",\"path\":\"/new\"}}", ts_now());
  // A line with no `ts` field is retained (never dropped on a parse quirk).
  let junk_line = "{\"path\":\"/junk\"}".to_string();
  std::fs::write(&path, format!("{old_line}\n{new_line}\n{junk_line}\n")).unwrap();

  let mut state = test_state();
  let handle = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(&path)
    .unwrap();
  state.access_log = Some(crate::access_log::spawn_writer(
    path.to_string_lossy().into_owned(),
    handle,
  ));
  let state = std::sync::Arc::new(state);

  let report = run_cycle(&state).await;

  assert_eq!(report.access_log_lines, 1);
  let rewritten = std::fs::read_to_string(&path).unwrap();
  assert!(!rewritten.contains("/old"));
  assert!(rewritten.contains("/new"));
  assert!(rewritten.contains("/junk"));
}

#[tokio::test]
async fn run_cycle_access_log_keeps_everything_when_recent() {
  retention(&[("access_log", "1d")]);
  let dir =
    crate::test_support::test_temp_root().join(format!("accesslog2-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let path = dir.join("access.log");
  std::fs::write(&path, format!("{{\"ts\":\"{}\"}}\n", ts_now())).unwrap();

  let mut state = test_state();
  let handle = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(&path)
    .unwrap();
  state.access_log = Some(crate::access_log::spawn_writer(
    path.to_string_lossy().into_owned(),
    handle,
  ));
  let state = std::sync::Arc::new(state);

  let report = run_cycle(&state).await;
  assert_eq!(report.access_log_lines, 0);
}

#[tokio::test]
async fn prune_access_log_returns_zero_without_a_configured_file() {
  // A default state has no access-log writer: the early-return arm.
  let state = test_state();
  assert_eq!(prune_access_log(&state, now()).await, 0);

  // Writer present but the file is missing on disk: the read fails inside
  // the writer's rewrite, which answers 0 rather than erroring.
  let dir = crate::test_support::test_temp_root();
  let handle_path = dir.join(format!("al-handle-{}", uuid::Uuid::new_v4()));
  let missing_path = dir.join(format!("missing-{}.log", uuid::Uuid::new_v4()));
  let mut state2 = test_state();
  state2.access_log = Some(crate::access_log::spawn_writer(
    missing_path.to_string_lossy().into_owned(),
    std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(&handle_path)
      .unwrap(),
  ));
  assert_eq!(prune_access_log(&state2, now()).await, 0);
}

#[tokio::test]
async fn run_cycle_prunes_audit() {
  retention(&[("audit", "1d")]);
  let dir = crate::test_support::test_temp_root().join(format!("audit-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let n = now();
  let old1 = format!(
    "{{\"ts\":100,\"timestamp\":\"{}\",\"event\":\"old\",\"actor\":\"system\",\"actor_ip\":\"-\",\"details\":\"d\",\"prev\":\"\"}}",
    ts_old()
  );
  let old2 = format!(
    "{{\"ts\":200,\"timestamp\":\"{}\",\"event\":\"old\",\"actor\":\"system\",\"actor_ip\":\"-\",\"details\":\"d\",\"prev\":\"\"}}",
    ts_old()
  );
  let fresh = format!(
    "{{\"ts\":{},\"timestamp\":\"{}\",\"event\":\"new\",\"actor\":\"system\",\"actor_ip\":\"-\",\"details\":\"d\",\"prev\":\"\"}}",
    n,
    ts_now()
  );
  std::fs::write(
    dir.join("audit.jsonl"),
    format!("{old1}\n{old2}\n{fresh}\n"),
  )
  .unwrap();

  let mut state = test_state();
  state.audit = tokio::sync::Mutex::new(crate::store::audit::AuditLog::load(
    &dir.to_string_lossy(),
    10 * 1024 * 1024,
    3,
  ));
  let state = std::sync::Arc::new(state);

  let report = run_cycle(&state).await;
  assert_eq!(report.audit_events, 2);
}

#[tokio::test]
async fn run_cycle_stats_line_runs_and_keeps_current_bucket() {
  retention(&[("stats", "1d")]);
  let state = std::sync::Arc::new(test_state());
  // Record a request so a current-day bucket exists; a 1-day TTL keeps it.
  state
    .persistent_stats
    .lock()
    .await
    .record_request(true, 10, 20, 5, None);

  let report = run_cycle(&state).await;
  assert_eq!(report.stats_buckets, 0);
}

// --- Disk-usage guard helpers ----------------------------------------------

#[test]
fn db_max_bytes_reads_the_cap_and_zero_is_none() {
  retention(&[]);
  assert_eq!(db_max_bytes(), None);
  retention(&[("db_max_size", "0")]);
  assert_eq!(db_max_bytes(), None);
  retention(&[("db_max_size", "4096")]);
  assert_eq!(db_max_bytes(), Some(4096));
  retention(&[("db_max_size", "1GB")]);
  assert_eq!(db_max_bytes(), Some(1024 * 1024 * 1024));
}

#[test]
fn db_size_sums_the_sqlite_sidecars() {
  let dir = crate::test_support::test_temp_root().join(format!("dbsize-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  assert_eq!(db_size_bytes(&dir), 0);
  std::fs::write(dir.join("aperio.db"), vec![0u8; 100]).unwrap();
  std::fs::write(dir.join("aperio.db-wal"), vec![0u8; 30]).unwrap();
  std::fs::write(dir.join("aperio.db-shm"), vec![0u8; 20]).unwrap();
  // An unrelated file is ignored.
  std::fs::write(dir.join("other"), vec![0u8; 999]).unwrap();
  assert_eq!(db_size_bytes(&dir), 150);
}

// --- disk_guard_cycle -------------------------------------------------------

#[tokio::test]
async fn disk_guard_below_reset_ratio_clears_warning_and_returns() {
  let _serial = DISK_LOCK.lock().await;
  DISK_WARNED.store(true, Ordering::SeqCst);
  let dir = crate::test_support::test_temp_root().join(format!("dg-low-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::write(dir.join("aperio.db"), vec![0u8; 10]).unwrap();
  let state = std::sync::Arc::new(test_state());

  // size 10, cap 1000 → well under the 0.8 reset ratio.
  disk_guard_cycle(&state, 1000, &dir).await;

  assert!(!DISK_WARNED.load(Ordering::SeqCst));
  // No pruning below the cap.
  assert!(
    state
      .audit
      .lock()
      .await
      .recent()
      .iter()
      .all(|e| e.event != "disk_pruned")
  );
}

#[tokio::test]
async fn disk_guard_warns_once_near_the_cap() {
  let _serial = DISK_LOCK.lock().await;
  DISK_WARNED.store(false, Ordering::SeqCst);
  let dir = crate::test_support::test_temp_root().join(format!("dg-warn-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  // 950 of a 1000 cap → past the 0.9 warn ratio but still under the cap.
  std::fs::write(dir.join("aperio.db"), vec![0u8; 950]).unwrap();
  let state = std::sync::Arc::new(test_state());

  disk_guard_cycle(&state, 1000, &dir).await;

  assert!(DISK_WARNED.load(Ordering::SeqCst));
  let recent = state.audit.lock().await.recent();
  assert!(recent.iter().any(|e| e.event == "disk_usage_warning"));
  // Still under the cap, so no pruning happened.
  assert!(recent.iter().all(|e| e.event != "disk_pruned"));

  // A second cycle at the same size does not warn again (one per episode).
  // Counted by event rather than by the log's total length: a length is only
  // stable while nothing else can append, which is a promise about the whole
  // process rather than about this test, and it is not what this is checking.
  disk_guard_cycle(&state, 1000, &dir).await;
  let warnings = state
    .audit
    .lock()
    .await
    .recent()
    .iter()
    .filter(|e| e.event == "disk_usage_warning")
    .count();
  assert_eq!(warnings, 1, "one warning per episode, not one per cycle");
}

#[tokio::test]
async fn disk_guard_prunes_over_the_cap() {
  let _serial = DISK_LOCK.lock().await;
  DISK_WARNED.store(false, Ordering::SeqCst);
  let dir = crate::test_support::test_temp_root().join(format!("dg-over-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::write(dir.join("aperio.db"), vec![0u8; 500]).unwrap();
  let state = std::sync::Arc::new(test_state());

  // Seed more than the retained caps so the truncations actually remove rows.
  {
    let mut inbox = state.inbox_store.lock().await;
    for i in 0..120 {
      inbox.insert(inbox_entry_with(&format!("i{i}"), ts_now()));
    }
  }
  {
    let mut deliveries = state.webhook_deliveries.lock().await;
    for i in 0..120 {
      deliveries.record(delivery_with(&format!("d{i}"), now() + i));
    }
  }

  // cap 100 < size 500 → over the cap, pruning path runs.
  disk_guard_cycle(&state, 100, &dir).await;

  // Truncations keep at most 100 of each.
  assert_eq!(state.inbox_store.lock().await.list(&None).len(), 100);
  assert_eq!(
    state.webhook_deliveries.lock().await.list(None, 1000).len(),
    100
  );
  let recent = state.audit.lock().await.recent();
  assert!(recent.iter().any(|e| e.event == "disk_pruned"));
}

// --- spawn ------------------------------------------------------------------

#[tokio::test]
async fn spawn_is_inert_when_nothing_is_configured() {
  retention(&[]);
  let state = std::sync::Arc::new(test_state());
  // The pruner runs, so retention can be configured later without a
  // restart, but a cycle with nothing configured removes nothing.
  spawn(state.clone());
  state
    .captured_requests
    .lock()
    .await
    .push_back(capture_with("c", ts_old()));
  tokio::time::sleep(Duration::from_millis(30)).await;
  // Nothing configured, so the capture is untouched.
  assert_eq!(state.captured_requests.lock().await.len(), 1);
}

#[tokio::test]
async fn spawn_runs_one_cycle_and_audits_the_prune() {
  // Sets a disk cap, so the pruner it starts runs a guard cycle against the
  // process-global `DISK_WARNED` on its first (immediate) tick.
  let _serial = DISK_LOCK.lock().await;
  retention(&[("captures", "1d"), ("db_max_size", "1000000000")]);
  let state = std::sync::Arc::new(test_state());
  {
    let mut caps = state.captured_requests.lock().await;
    caps.push_back(capture_with("old", ts_old()));
    caps.push_back(capture_with("new", ts_now()));
  }

  spawn(state.clone());
  // The interval's first tick fires immediately, so one cycle runs as soon as
  // the executor gets a chance.
  tokio::time::sleep(Duration::from_millis(80)).await;

  assert_eq!(state.captured_requests.lock().await.len(), 1);
  let recent = state.audit.lock().await.recent();
  assert!(recent.iter().any(|e| e.event == "retention_pruned"));
}

#[tokio::test]
async fn spawn_enabled_by_disk_cap_alone() {
  // The disk cap is the whole point of this one, so it certainly runs a
  // guard cycle: same global, same lock.
  let _serial = DISK_LOCK.lock().await;
  retention(&[("db_max_size", "1000000000")]);
  let state = std::sync::Arc::new(test_state());
  // Only the disk cap is set (no retention TTLs).
  spawn(state.clone());
  tokio::time::sleep(Duration::from_millis(30)).await;
  // Nothing to assert beyond it starting cleanly; the db is empty/tiny.
}
