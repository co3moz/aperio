//! Tests for the threshold-based alerting module.
//!
//! `test-util` is not enabled for `tokio`, so virtual time is unavailable and
//! the background ticker's 15 s cadence has to be driven with real sleeps. One
//! comprehensive test therefore drives a live [`spawn`] task across three ticks
//! (~48 s) to exercise every fire/resolve branch of both rules plus window
//! eviction; the rest of the coverage comes from the pure `from_env` parser and
//! the `emit` side-effect helper, which need no timing.

use super::*;
use crate::store::webhooks::WebhookFormat;
use crate::test_support::*;

use std::sync::Mutex as StdMutex;

/// Serializes the tests that mutate process-global `APERIO_ALERT_*` env vars.
static ENV_LOCK: StdMutex<()> = StdMutex::new(());

const ALERT_KEYS: &[&str] = &[
  "APERIO_ALERT_ERROR_RATE",
  "APERIO_ALERT_CLIENT_DOWN",
  "APERIO_ALERT_WINDOW",
  "APERIO_ALERT_MIN_REQUESTS",
];

fn clear_alert_env() {
  for k in ALERT_KEYS {
    unsafe { std::env::remove_var(k) };
  }
}

fn set_env(k: &str, v: &str) {
  unsafe { std::env::set_var(k, v) };
}

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
// AlertConfig::from_env
// ---------------------------------------------------------------------------

#[test]
fn from_env_all_unset_is_none() {
  let _g = ENV_LOCK.lock().unwrap();
  clear_alert_env();
  assert!(AlertConfig::from_env().is_none());
}

#[test]
fn from_env_zero_and_invalid_values_are_off() {
  let _g = ENV_LOCK.lock().unwrap();
  clear_alert_env();
  // Zero, negative and unparsable values are all filtered out -> both off.
  set_env("APERIO_ALERT_ERROR_RATE", "0");
  set_env("APERIO_ALERT_CLIENT_DOWN", "-5");
  assert!(AlertConfig::from_env().is_none());

  set_env("APERIO_ALERT_ERROR_RATE", "not-a-number");
  clear_env_single("APERIO_ALERT_CLIENT_DOWN");
  assert!(AlertConfig::from_env().is_none());
  clear_alert_env();
}

#[test]
fn from_env_error_rate_only_uses_defaults() {
  let _g = ENV_LOCK.lock().unwrap();
  clear_alert_env();
  set_env("APERIO_ALERT_ERROR_RATE", "12.5");
  let cfg = AlertConfig::from_env().expect("some");
  assert_eq!(cfg.error_rate_pct, 12.5);
  assert_eq!(cfg.window, Duration::from_secs(300));
  assert_eq!(cfg.min_requests, 20);
  assert_eq!(cfg.client_down, Duration::ZERO);
  clear_alert_env();
}

#[test]
fn from_env_client_down_only() {
  let _g = ENV_LOCK.lock().unwrap();
  clear_alert_env();
  set_env("APERIO_ALERT_CLIENT_DOWN", "45");
  let cfg = AlertConfig::from_env().expect("some");
  assert_eq!(cfg.error_rate_pct, 0.0);
  assert_eq!(cfg.client_down, Duration::from_secs(45));
  clear_alert_env();
}

#[test]
fn from_env_full_custom_config() {
  let _g = ENV_LOCK.lock().unwrap();
  clear_alert_env();
  set_env("APERIO_ALERT_ERROR_RATE", "5");
  set_env("APERIO_ALERT_CLIENT_DOWN", "30");
  set_env("APERIO_ALERT_WINDOW", "120");
  set_env("APERIO_ALERT_MIN_REQUESTS", "50");
  let cfg = AlertConfig::from_env().expect("some");
  assert_eq!(cfg.error_rate_pct, 5.0);
  assert_eq!(cfg.client_down, Duration::from_secs(30));
  assert_eq!(cfg.window, Duration::from_secs(120));
  assert_eq!(cfg.min_requests, 50);
  clear_alert_env();
}

#[test]
fn from_env_invalid_window_and_min_fall_back_to_defaults() {
  let _g = ENV_LOCK.lock().unwrap();
  clear_alert_env();
  set_env("APERIO_ALERT_ERROR_RATE", "10");
  set_env("APERIO_ALERT_WINDOW", "garbage");
  set_env("APERIO_ALERT_MIN_REQUESTS", "0");
  let cfg = AlertConfig::from_env().expect("some");
  assert_eq!(cfg.window, Duration::from_secs(300));
  assert_eq!(cfg.min_requests, 20);
  clear_alert_env();
}

fn clear_env_single(k: &str) {
  unsafe { std::env::remove_var(k) };
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
  let cfg = AlertConfig {
    error_rate_pct: 10.0,
    window: Duration::from_secs(300),
    min_requests: 20,
    client_down: Duration::from_secs(60),
  };
  spawn(state, cfg);
  // Let the task reach its first `sleep().await` (variable setup executes),
  // then let the test end so the runtime aborts it before any tick.
  tokio::task::yield_now().await;
}

#[tokio::test]
async fn spawn_logs_both_rules_off() {
  let state = Arc::new(test_state());
  let cfg = AlertConfig {
    error_rate_pct: 0.0,
    window: Duration::from_secs(300),
    min_requests: 20,
    client_down: Duration::ZERO,
  };
  spawn(state, cfg);
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
    .lock()
    .await
    .insert("svc1".to_string(), mock_client(None, None, None, None));

  let cfg = AlertConfig {
    error_rate_pct: 10.0,
    window: Duration::from_secs(25),
    min_requests: 20,
    client_down: Duration::from_secs(1),
  };
  spawn(state.clone(), cfg);

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
