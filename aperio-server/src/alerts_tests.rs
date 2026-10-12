//! Tests for the threshold-based alerting module.
//!
//! `test-util` is not enabled for `tokio`, so virtual time is unavailable and
//! the background ticker's 15 s cadence has to be driven with real sleeps. One
//! comprehensive test therefore drives a live [`spawn`] task across three ticks
//! (~48 s) to exercise every fire/resolve branch of both rules plus window
//! eviction; the rest of the coverage comes from the pure `from_settings` reader and
//! the `emit` side-effect helper, which need no timing.

use super::*;
use crate::store::webhooks::WebhookFormat;
use crate::test_support::*;

/// Snapshot of `(event, details)` for every audit row written so far.
async fn audit_rows(state: &AppState) -> Vec<(String, String)> {
  state
    .audit
    .lock()
    .await
    .recent()
    .into_iter()
    .map(|e| (e.event, e.details))
    .collect()
}

/// Number of audit rows whose event equals `event` and whose details contain
/// the given `kind` marker.
fn count_alert(rows: &[(String, String)], event: &str, kind: &str) -> usize {
  rows
    .iter()
    .filter(|(e, d)| e == event && d.contains(&format!("\"kind\":\"{kind}\"")))
    .count()
}

// ---------------------------------------------------------------------------
// AlertConfig::from_settings
// ---------------------------------------------------------------------------

#[test]
fn from_settings_with_nothing_set_is_none() {
  crate::live::reset();
  assert!(AlertConfig::from_settings().is_none());
}

#[test]
fn from_settings_zero_values_are_off() {
  crate::live::set_yaml(
    "alerts:
  error_rate: 0
  client_down: 0s
",
  );
  assert!(AlertConfig::from_settings().is_none());
}

#[test]
fn from_settings_error_rate_only_uses_defaults() {
  crate::live::set_yaml(
    "alerts:
  error_rate: 0.125
",
  );
  let cfg = AlertConfig::from_settings().expect("some");
  assert_eq!(cfg.error_rate_pct, 12.5);
  assert_eq!(cfg.window, Duration::from_secs(300));
  assert_eq!(cfg.min_requests, 20);
  assert_eq!(cfg.client_down, Duration::ZERO);
}

#[test]
fn from_settings_client_down_only() {
  crate::live::set_yaml(
    "alerts:
  client_down: 45s
",
  );
  let cfg = AlertConfig::from_settings().expect("some");
  assert_eq!(cfg.error_rate_pct, 0.0);
  assert_eq!(cfg.client_down, Duration::from_secs(45));
}

#[test]
fn from_settings_full_custom_config() {
  crate::live::set_yaml(
    "alerts:
  error_rate: 0.05
  client_down: 30s
  window: 2m
  min_requests: 50
",
  );
  let cfg = AlertConfig::from_settings().expect("some");
  assert!((cfg.error_rate_pct - 5.0).abs() < 1e-9);
  assert_eq!(cfg.client_down, Duration::from_secs(30));
  assert_eq!(cfg.window, Duration::from_secs(120));
  assert_eq!(cfg.min_requests, 50);
}

#[test]
fn a_typo_in_the_alerts_section_is_refused_not_read_as_off() {
  assert!(
    aperio_config::server_settings::ServerSettings::from_yaml(
      "alerts:
  window: soon
"
    )
    .is_err()
  );
}

// ---------------------------------------------------------------------------
// emit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn emit_audits_and_dispatches_to_subscribers() {
  let state = Arc::new(test_state());
  // A webhook subscribed to the event makes emit_event's subscriber path
  // non-empty so the filter/dispatch branch runs.
  state.webhook_store.lock().await.create(
    "pager".to_string(),
    "http://127.0.0.1:1/never".to_string(),
    vec!["alert_triggered".to_string()],
    None,
    WebhookFormat::Generic,
    None,
  );

  emit(
    &state,
    "alert_triggered",
    serde_json::json!({"kind": "error_rate", "rate_pct": 42.0}),
  )
  .await;

  let rows = audit_rows(&state).await;
  assert_eq!(count_alert(&rows, "alert_triggered", "error_rate"), 1);
  // The audited details are the compact JSON payload.
  let (_, details) = rows
    .iter()
    .find(|(e, _)| e == "alert_triggered")
    .expect("row");
  assert!(details.contains("\"rate_pct\":42.0"));
}

// ---------------------------------------------------------------------------
// spawn: startup logging branches (no ticks driven)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_logs_both_rules_on() {
  let state = Arc::new(test_state());
  crate::live::set_yaml(
    "alerts:
  error_rate: 0.1
  client_down: 1m
",
  );
  spawn(state);
  // Let the task reach its first `sleep().await` (variable setup executes),
  // then let the test end so the runtime aborts it before any tick.
  tokio::task::yield_now().await;
}

#[tokio::test]
async fn spawn_logs_both_rules_off() {
  let state = Arc::new(test_state());
  crate::live::reset();
  spawn(state);
  tokio::task::yield_now().await;
}

// ---------------------------------------------------------------------------
// Live ticker: error-rate and client-down fire then resolve (real time).
// ---------------------------------------------------------------------------

/// Drives a real `spawn` task across three 15 s ticks. Between ticks the test
/// mutates `stats` (to move the error rate) and the client-down threshold (to
/// flip a service up/down), asserting both rules fire on tick 2 and resolve on
/// tick 3. A 25 s window makes the oldest sample expire by tick 3, exercising
/// the sliding-window eviction path too.
#[tokio::test(flavor = "current_thread")]
async fn ticker_fires_and_resolves_both_rules() {
  // Down-threshold 0 => any client reads as Down until we raise it later.
  let mut base = test_config();
  base.client_down_threshold = Duration::ZERO;
  let state = Arc::new(test_state_with(base));

  // One service entity, initially Down (threshold is zero).
  state
    .clients
    .write()
    .await
    .insert("svc1".to_string(), mock_client(None, None));

  // Read by the ticker on this thread: the runtime is current-thread.
  crate::live::set_yaml(
    "alerts:
  error_rate: 0.1
  window: 25s
  min_requests: 20
  client_down: 1s
",
  );
  spawn(state.clone());

  // Tick 1 (~16 s): first error sample (0/0) and first sight of svc1 down.
  tokio::time::sleep(Duration::from_secs(16)).await;
  {
    let rows = audit_rows(&state).await;
    assert_eq!(count_alert(&rows, "alert_triggered", "error_rate"), 0);
    assert_eq!(count_alert(&rows, "alert_triggered", "client_down"), 0);
  }

  // Raise the error rate to 50% before tick 2 (50 ok + 50 failed = 100 total).
  {
    let mut s = state.stats.lock().await;
    s.successful_requests = 50;
    s.failed_requests = 50;
  }

  // Tick 2 (~32 s): error rate 50% >= 10% and svc1 down > 1 s -> both fire.
  tokio::time::sleep(Duration::from_secs(16)).await;
  {
    let rows = audit_rows(&state).await;
    assert_eq!(
      count_alert(&rows, "alert_triggered", "error_rate"),
      1,
      "error-rate alert should fire once"
    );
    assert_eq!(
      count_alert(&rows, "alert_triggered", "client_down"),
      1,
      "client-down alert should fire once"
    );
  }

  // Before tick 3: flood with successes so the recent rate drops far below the
  // resolve threshold (80% of 10% = 8%), and raise the down-threshold so svc1
  // now reads healthy.
  {
    let mut s = state.stats.lock().await;
    s.successful_requests = 950;
    s.failed_requests = 50;
  }
  {
    let mut cfg2 = test_config();
    cfg2.client_down_threshold = Duration::from_secs(3600);
    *state.config_store.write().unwrap() = Arc::new(cfg2);
  }

  // Tick 3 (~48 s): oldest sample (age ~32 s) is evicted from the 25 s window;
  // recent rate ~0% resolves the error alert, and svc1 healthy resolves the
  // client-down alert.
  tokio::time::sleep(Duration::from_secs(16)).await;
  {
    let rows = audit_rows(&state).await;
    assert_eq!(
      count_alert(&rows, "alert_resolved", "error_rate"),
      1,
      "error-rate alert should resolve once"
    );
    assert_eq!(
      count_alert(&rows, "alert_resolved", "client_down"),
      1,
      "client-down alert should resolve once"
    );
    // No spurious re-fires.
    assert_eq!(count_alert(&rows, "alert_triggered", "error_rate"), 1);
    assert_eq!(count_alert(&rows, "alert_triggered", "client_down"), 1);
  }
}
