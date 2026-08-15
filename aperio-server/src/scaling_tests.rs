//! Tests for the autoscaling runtime: the single-flight gate, the cooldown and
//! breaker, saturation windowing, capacity measurement, and the SSRF fence.

use super::*;

use crate::outbound::is_internal;
use crate::store::scaling::{
  DEFAULT_COLD_START_SECS, DEFAULT_COOLDOWN_SECS, DEFAULT_TARGET_UTILIZATION, DEFAULT_WINDOW_SECS,
};
use crate::store::tokens::TokenSpec;
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
    let mut clients = state.clients.write().await;
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
    let mut clients = state.clients.write().await;
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

/// A local endpoint answering one canned status per accepted connection,
/// forever, so cooldown-spanning tests can call it repeatedly.
async fn canned_endpoint(status: u16) -> std::net::SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    loop {
      let Ok((mut socket, _)) = listener.accept().await else {
        return;
      };
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 2048];
        let _ = socket.read(&mut buf).await;
        let response =
          format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let _ = socket.write_all(response.as_bytes()).await;
      });
    }
  });
  addr
}

/// A test config that lets a scaling call reach a local plain-http endpoint.
fn local_call_config() -> crate::settings::ServerConfig {
  let mut cfg = crate::test_support::test_config();
  cfg.scaling_enabled = true;
  cfg.scaling_allow_http = true;
  cfg.scaling_allow_private = true;
  cfg
}

#[tokio::test]
async fn a_successful_call_holds_the_visitor_and_lands_on_the_audit_trail() {
  let addr = canned_endpoint(200).await;
  let state = std::sync::Arc::new(crate::test_support::test_state_with(local_call_config()));
  let mut rec = record("cold.example.com");
  rec.url = format!("http://{addr}/scale");
  rec.secret = Some("hook-secret".to_string());

  let ask = request_capacity(&state, &rec, Reason::ColdStart, 0).await;
  assert_eq!(ask, Ask::Hold);
  let events = state.audit.lock().await.recent();
  let entry = events
    .iter()
    .find(|e| e.event == "scaling_requested")
    .expect("the ask is audited");
  assert!(entry.details.contains("desired=1"), "{}", entry.details);
  assert!(
    !entry.details.contains("hook-secret"),
    "the secret never reaches the log: {}",
    entry.details
  );

  // Asking again inside the cooldown is a skip that still holds: an
  // instance is plausibly on its way.
  let ask = request_capacity(&state, &rec, Reason::ColdStart, 0).await;
  assert_eq!(ask, Ask::Hold);
}

#[tokio::test]
async fn a_failing_endpoint_does_not_hold_and_the_last_failure_disarms() {
  let addr = canned_endpoint(500).await;
  let state = std::sync::Arc::new(crate::test_support::test_state_with(local_call_config()));
  let mut rec = record("failing.example.com");
  rec.url = format!("http://{addr}/scale");
  rec.cooldown_secs = 0;

  // The first failures, recorded directly and dated in the past so the
  // backoff they set has already expired when the real call happens.
  let past = Instant::now() - Duration::from_secs(600);
  {
    let mut runtime = state.scaling_runtime.lock().await;
    for _ in 0..BREAKER_THRESHOLD - 1 {
      runtime.finish(&rec.id, false, Duration::ZERO, past);
    }
  }

  // The one real call is the last straw: it fails, does not hold the
  // visitor, and trips the breaker.
  let ask = request_capacity(&state, &rec, Reason::ScaleOut, 2).await;
  assert_eq!(ask, Ask::DoNotHold);
  assert!(state.scaling_runtime.lock().await.is_disarmed(&rec.id));
  let events = state.audit.lock().await.recent();
  assert!(
    events.iter().any(|e| e.event == "scaling_failed"),
    "a failed ask is audited as a failure"
  );

  // A disarmed record is skipped without holding: nothing is coming.
  let ask = request_capacity(&state, &rec, Reason::ScaleOut, 2).await;
  assert_eq!(ask, Ask::DoNotHold);
}

#[tokio::test]
async fn an_absent_endpoint_is_a_failure_not_a_hang() {
  let state = std::sync::Arc::new(crate::test_support::test_state_with(local_call_config()));
  let mut rec = record("gone.example.com");
  rec.url = "http://127.0.0.1:9/scale".to_string();
  let ask = request_capacity(&state, &rec, Reason::ColdStart, 0).await;
  assert_eq!(ask, Ask::DoNotHold);
}

#[tokio::test(start_paused = true)]
async fn cold_start_wait_holds_until_a_routable_client_appears() {
  let addr = canned_endpoint(200).await;
  let state = std::sync::Arc::new(crate::test_support::test_state_with(local_call_config()));
  let mut rec = record("cold.example.com");
  rec.url = format!("http://{addr}/scale");
  rec.min = 0;
  rec.cold_start_secs = 30;
  state
    .scaling_store
    .lock()
    .await
    .upsert(rec, None, crate::store::tokens::now_secs());

  // A second, path-scoped record proves the more specific one wins the
  // lookup; give it a dead endpoint so a wrong pick would fail the test.
  let mut scoped = record("cold.example.com");
  scoped.id = ScalingRecord::key(None, "cold.example.com", Some("/api"));
  scoped.path = Some("/api".to_string());
  scoped.url = "http://127.0.0.1:9/never".to_string();
  state
    .scaling_store
    .lock()
    .await
    .upsert(scoped, None, crate::store::tokens::now_secs());

  let waiter = {
    let state = state.clone();
    tokio::spawn(async move {
      cold_start_wait(
        &state,
        Some("cold.example.com"),
        "/",
        "203.0.113.9".parse().unwrap(),
      )
      .await;
    })
  };
  // Let the ask land, then connect the instance it was waiting for.
  tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  assert!(
    !waiter.is_finished(),
    "still holding, nothing serves it yet"
  );
  {
    let mut clients = state.clients.write().await;
    let mut c = mock_client(Some("cold.example.com"), None, None, None);
    c.max_concurrent = Some(4);
    clients.insert("c-new".to_string(), c);
  }
  let _ = state.client_connected.send(true);
  tokio::time::timeout(std::time::Duration::from_secs(40), waiter)
    .await
    .expect("released once a routable client appeared")
    .unwrap();
}

#[tokio::test]
async fn cold_start_wait_never_wakes_what_should_stay_down() {
  let state = std::sync::Arc::new(crate::test_support::test_state_with(local_call_config()));
  // No hostname at all: nothing to look up.
  cold_start_wait(&state, None, "/", "203.0.113.9".parse().unwrap()).await;

  // A flagged hostname is meant to be down; returning immediately is the
  // whole assertion, a call would hang on the dead endpoint's timeout.
  let mut rec = record("flagged.example.com");
  rec.url = "http://127.0.0.1:9/never".to_string();
  state
    .scaling_store
    .lock()
    .await
    .upsert(rec, None, crate::store::tokens::now_secs());
  state.maintenance.lock().await.insert(
    "flagged.example.com".to_string(),
    crate::state::MaintenanceFlag::default(),
  );
  cold_start_wait(
    &state,
    Some("flagged.example.com"),
    "/",
    "203.0.113.9".parse().unwrap(),
  )
  .await;

  // A record without scale-to-zero (min > 0) is not a cold-start trap.
  let mut warm = record("warm.example.com");
  warm.min = 1;
  warm.url = "http://127.0.0.1:9/never".to_string();
  state
    .scaling_store
    .lock()
    .await
    .upsert(warm, None, crate::store::tokens::now_secs());
  cold_start_wait(
    &state,
    Some("warm.example.com"),
    "/",
    "203.0.113.9".parse().unwrap(),
  )
  .await;
}

#[tokio::test]
async fn a_visitor_the_owning_tokens_would_reject_cannot_bill_a_cold_start() {
  let state = std::sync::Arc::new(crate::test_support::test_state_with(local_call_config()));
  let token_id = {
    let mut tokens = state.token_store.lock().await;
    tokens
      .create(TokenSpec {
        name: "fenced".into(),
        hostnames: vec!["cold.example.com".into()],
        allowed_ips: vec!["10.0.0.0/8".into()],
        ..Default::default()
      })
      .expect("the test store can be written to")
      .0
      .id
      .clone()
  };
  let mut rec = record("cold.example.com");
  rec.owners = vec![token_id];
  rec.url = "http://127.0.0.1:9/never".to_string();

  // Outside the owners' allowed_ips: refused before any endpoint is called,
  // which is why the dead URL never matters.
  assert!(!visitor_allowed(&state, &rec, "203.0.113.9".parse().unwrap()).await);
  cold_start_wait(
    &state,
    Some("cold.example.com"),
    "/",
    "203.0.113.9".parse().unwrap(),
  )
  .await;
  // Inside them: admitted.
  assert!(visitor_allowed(&state, &rec, "10.1.2.3".parse().unwrap()).await);
  // An owner with no restriction admits everyone.
  rec.owners = vec!["unknown-token".to_string()];
  assert!(visitor_allowed(&state, &rec, "203.0.113.9".parse().unwrap()).await);
}

// ---------------------------------------------------------------------------
// Scale in (planned_features #68)
// ---------------------------------------------------------------------------

#[test]
fn scale_in_needs_several_windows_of_idleness() {
  let mut runtime = ScalingRuntime::default();
  let mut record = record("app.example.com");
  record.window_secs = 10;
  record.target_utilization = 0.8;
  let start = Instant::now();

  // Well under the target, but not yet for long enough.
  assert!(!runtime.idle_reached(&record, 0.1, start));
  assert!(!runtime.idle_reached(&record, 0.1, start + Duration::from_secs(35)));
  // Four windows, deliberately asymmetric with the single window scale-out
  // needs: being an instance short costs latency on live traffic, being one
  // over costs money.
  assert!(runtime.idle_reached(&record, 0.1, start + Duration::from_secs(41)));
}

#[test]
fn a_pool_at_its_target_is_neither_saturated_nor_idle() {
  let mut runtime = ScalingRuntime::default();
  let mut record = record("app.example.com");
  record.window_secs = 10;
  record.target_utilization = 0.8;
  let start = Instant::now();
  // 0.6 is under the target and over half of it: the gap between the two
  // thresholds is what stops a pool hovering at its target from scaling out
  // and in on alternating samples.
  assert!(!runtime.saturation_reached(&record, 0.6, start));
  assert!(!runtime.idle_reached(&record, 0.6, start));
  assert!(!runtime.idle_reached(&record, 0.6, start + Duration::from_secs(600)));
}

#[test]
fn traffic_returning_cancels_a_pending_scale_in() {
  let mut runtime = ScalingRuntime::default();
  let mut record = record("app.example.com");
  record.window_secs = 10;
  record.target_utilization = 0.8;
  let start = Instant::now();
  assert!(!runtime.idle_reached(&record, 0.1, start));
  // One busy sample resets the clock: the idle stretch has to be continuous,
  // or a service quiet at night would give up an instance mid-morning.
  assert!(!runtime.saturation_reached(&record, 0.9, start + Duration::from_secs(20)));
  assert!(!runtime.idle_reached(&record, 0.1, start + Duration::from_secs(30)));
  assert!(!runtime.idle_reached(&record, 0.1, start + Duration::from_secs(60)));
  assert!(runtime.idle_reached(&record, 0.1, start + Duration::from_secs(71)));
}

#[test]
fn the_scale_in_reason_has_its_own_name_in_the_payload() {
  // A receiver switches on this string, and a scale-in arriving labelled
  // scale_out would be acted on backwards.
  assert_eq!(Reason::ScaleIn.as_str(), "scale_in");
}
