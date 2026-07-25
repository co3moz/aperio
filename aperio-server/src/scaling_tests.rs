//! Tests for the autoscaling runtime: the single-flight gate, the cooldown and
//! breaker, saturation windowing, capacity measurement, and the SSRF fence.

use super::*;
use crate::store::scaling::{
  DEFAULT_COLD_START_SECS, DEFAULT_COOLDOWN_SECS, DEFAULT_TARGET_UTILIZATION, DEFAULT_WINDOW_SECS,
};
use crate::test_support::{mock_client, test_state};

fn record(hostname: &str) -> ScalingRecord {
  ScalingRecord {
    id: ScalingRecord::key(None, hostname, None),
    org_id: None,
    hostname: hostname.to_string(),
    path: None,
    url: "https://api.example/scale".to_string(),
    secret: None,
    min: 0,
    max: 4,
    cold_start_secs: DEFAULT_COLD_START_SECS,
    target_utilization: DEFAULT_TARGET_UTILIZATION,
    window_secs: DEFAULT_WINDOW_SECS,
    cooldown_secs: DEFAULT_COOLDOWN_SECS,
    owners: Vec::new(),
    config_hash: String::new(),
    created_at: 0,
    last_seen: 0,
  }
}

#[test]
fn single_flight_lets_exactly_one_caller_fire() {
  let mut runtime = ScalingRuntime::default();
  let record = record("app.example.com");
  let now = Instant::now();

  // The burst behind the first request must not produce a second call.
  assert!(matches!(runtime.begin(&record, now), Begin::Fire));
  for _ in 0..50 {
    assert!(matches!(runtime.begin(&record, now), Begin::AlreadyWaking));
  }

  // Once the call completes the gate closes for the cooldown.
  runtime.finish(&record.id, true, Duration::from_secs(60), now);
  assert!(matches!(runtime.begin(&record, now), Begin::Skip));
  // ... and reopens after it.
  let later = now + Duration::from_secs(61);
  assert!(matches!(runtime.begin(&record, later), Begin::Fire));
}

#[test]
fn failures_back_off_exponentially_and_trip_the_breaker() {
  let mut runtime = ScalingRuntime::default();
  let record = record("app.example.com");
  let mut now = Instant::now();

  for attempt in 1..=4 {
    assert!(
      matches!(runtime.begin(&record, now), Begin::Fire),
      "attempt {attempt}"
    );
    runtime.finish(&record.id, false, Duration::from_secs(10), now);
    assert!(!runtime.is_disarmed(&record.id));
    // Each failure roughly doubles the wait: 20s, 40s, 80s, 160s.
    let backoff = 10 * (1 << attempt);
    assert!(
      matches!(
        runtime.begin(&record, now + Duration::from_secs(backoff - 1)),
        Begin::Skip
      ),
      "attempt {attempt} reopened too early"
    );
    now += Duration::from_secs(backoff);
  }

  // The fifth consecutive failure disarms the record entirely.
  assert!(matches!(runtime.begin(&record, now), Begin::Fire));
  runtime.finish(&record.id, false, Duration::from_secs(10), now);
  assert!(runtime.is_disarmed(&record.id));
  assert!(matches!(
    runtime.begin(&record, now + Duration::from_secs(86_400)),
    Begin::Skip
  ));

  // Re-announcing (or editing) the record re-arms it.
  runtime.rearm(&record.id);
  assert!(!runtime.is_disarmed(&record.id));
  assert!(matches!(runtime.begin(&record, now), Begin::Fire));
}

#[test]
fn a_success_clears_the_failure_count() {
  let mut runtime = ScalingRuntime::default();
  let record = record("app.example.com");
  let mut now = Instant::now();
  for _ in 0..3 {
    runtime.begin(&record, now);
    runtime.finish(&record.id, false, Duration::from_secs(1), now);
    now += Duration::from_secs(600);
  }
  runtime.begin(&record, now);
  runtime.finish(&record.id, true, Duration::from_secs(1), now);
  // Back to a plain cooldown, not an escalated backoff.
  assert!(matches!(
    runtime.begin(&record, now + Duration::from_secs(2)),
    Begin::Fire
  ));
  assert!(!runtime.is_disarmed(&record.id));
}

#[test]
fn saturation_must_persist_for_the_whole_window() {
  let mut runtime = ScalingRuntime::default();
  let mut record = record("app.example.com");
  record.window_secs = 15;
  let start = Instant::now();

  // A single spike is not a reason to scale out.
  assert!(!runtime.saturation_reached(&record, 0.95, start));
  assert!(!runtime.saturation_reached(&record, 0.95, start + Duration::from_secs(14)));
  assert!(runtime.saturation_reached(&record, 0.95, start + Duration::from_secs(15)));

  // Dropping below the target resets the clock.
  assert!(!runtime.saturation_reached(&record, 0.1, start + Duration::from_secs(16)));
  assert!(!runtime.saturation_reached(&record, 0.95, start + Duration::from_secs(17)));
  assert!(!runtime.saturation_reached(&record, 0.95, start + Duration::from_secs(31)));
  assert!(runtime.saturation_reached(&record, 0.95, start + Duration::from_secs(32)));
}

#[test]
fn internal_destinations_are_refused() {
  // The declaration comes from a client, so these are the addresses an
  // attacker would aim the server at.
  for ip in [
    "127.0.0.1",
    "10.0.0.5",
    "192.168.1.1",
    "172.16.0.1",
    "169.254.169.254",
    "100.64.0.1",
    "0.0.0.0",
  ] {
    assert!(is_internal(ip.parse().unwrap()), "{ip} must be refused");
  }
  for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34"] {
    assert!(!is_internal(ip.parse().unwrap()), "{ip} must be allowed");
  }
  // IPv6, including the mapped form of a private v4 address.
  assert!(is_internal("::1".parse().unwrap()));
  assert!(is_internal("fc00::1".parse().unwrap()));
  assert!(is_internal("fe80::1".parse().unwrap()));
  assert!(is_internal("::ffff:127.0.0.1".parse().unwrap()));
  assert!(!is_internal("2606:4700:4700::1111".parse().unwrap()));
}

#[tokio::test]
async fn destination_scheme_and_address_are_both_checked() {
  // Plain http is refused unless the operator opts in.
  let http = url::Url::parse("http://example.com/scale").unwrap();
  assert!(destination_allowed(&http, false, false).await.is_err());
  // Loopback stays refused even then, whatever the scheme.
  let local = url::Url::parse("http://127.0.0.1:9000/scale").unwrap();
  assert!(destination_allowed(&local, true, false).await.is_err());
  let local_name = url::Url::parse("https://localhost/scale").unwrap();
  assert!(
    destination_allowed(&local_name, false, false)
      .await
      .is_err()
  );
  // An operator whose provider API genuinely lives on the internal network
  // can opt back in, which is the only way a private address is ever called.
  assert!(destination_allowed(&local, true, true).await.is_ok());
}

#[tokio::test]
async fn measure_reports_pool_capacity_and_utilization() {
  let state = test_state();
  // Two clients on the bind, one with a concurrency limit half consumed.
  let mut a = mock_client(Some("app.example.com"), None, None, None);
  a.max_concurrent = Some(10);
  a.inflight_limiter = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(10)));
  let permits = a
    .inflight_limiter
    .as_ref()
    .unwrap()
    .clone()
    .try_acquire_many_owned(4)
    .unwrap();
  let mut b = mock_client(Some("app.example.com"), None, None, None);
  b.max_concurrent = Some(10);
  b.inflight_limiter = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(10)));
  // A client on a different hostname must not count.
  let other = mock_client(Some("other.example.com"), None, None, None);
  {
    let mut clients = state.clients.lock().await;
    clients.insert("a".into(), a);
    clients.insert("b".into(), b);
    clients.insert("other".into(), other);
  }

  let capacity = measure(&state, "app.example.com", None).await;
  assert_eq!(capacity.instances, 2);
  assert_eq!(capacity.capacity, 20);
  assert_eq!(capacity.inflight, 4);
  assert!((capacity.utilization - 0.2).abs() < 1e-9);
  drop(permits);

  // Nothing serving the bind at all.
  let empty = measure(&state, "nobody.example.com", None).await;
  assert_eq!(empty.instances, 0);
  assert_eq!(empty.utilization, 0.0);
}

#[tokio::test]
async fn measure_excludes_clients_that_cannot_take_a_request() {
  let state = test_state();
  let mut draining = mock_client(Some("app.example.com"), None, None, None);
  draining.draining = true;
  let mut unhealthy = mock_client(Some("app.example.com"), None, None, None);
  unhealthy.backend_healthy = false;
  let mut disabled = mock_client(Some("app.example.com"), None, None, None);
  disabled.admin_enabled = false;
  // A standby tier exists to be idle; counting it would mask saturation of
  // the primaries under primary-standby.
  let mut standby = mock_client(Some("app.example.com"), None, None, None);
  standby.priority = 1;
  {
    let mut clients = state.clients.lock().await;
    clients.insert("d".into(), draining);
    clients.insert("u".into(), unhealthy);
    clients.insert("x".into(), disabled);
    clients.insert("s".into(), standby);
  }
  assert_eq!(measure(&state, "app.example.com", None).await.instances, 0);
}

#[test]
fn reason_is_reported_verbatim_to_the_endpoint() {
  assert_eq!(Reason::ColdStart.as_str(), "cold_start");
  assert_eq!(Reason::ScaleOut.as_str(), "scale_out");
}

#[test]
fn forget_drops_all_state_for_a_bind() {
  let mut runtime = ScalingRuntime::default();
  let record = record("app.example.com");
  let now = Instant::now();
  runtime.begin(&record, now);
  runtime.finish(&record.id, false, Duration::from_secs(600), now);
  assert!(matches!(runtime.begin(&record, now), Begin::Skip));
  runtime.forget(&record.id);
  // A fresh record starts clean, with no inherited cooldown.
  assert!(matches!(runtime.begin(&record, now), Begin::Fire));
}
