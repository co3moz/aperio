//! The background loops, one beat at a time. Each has a `*_tick_once` so a
//! test can drive a single iteration instead of waiting on a timer.

use crate::store::tokens::TokenSpec;
use crate::test_support::*;
use std::time::Duration;

// ---------------------------------------------------------------------------
// The background loops, one beat at a time.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_uptime_tick_observes_and_accrues() {
  use crate::{flush_stats_once, uptime_tick_once};
  let state = std::sync::Arc::new(test_state());
  state.clients.write().await.insert(
    "c1".to_string(),
    crate::test_support::mock_client(Some("up.example.com"), None, None, None),
  );
  uptime_tick_once(&state).await;
  let snap = state.uptime.lock().await.snapshot();
  // The entity key is the service name / instance id / connection id, in
  // that order of preference; the mock declares none, so its connection id.
  let entity = snap.get("c1").expect("the tick recorded the live service");
  assert_eq!(entity.status, crate::store::uptime::Availability::Up);
  // And the flush beat writes it out without complaint.
  flush_stats_once(&state).await;
}

#[tokio::test]
async fn one_expiry_tick_warns_once_and_rearms_on_refresh() {
  use crate::token_expiry_tick_once;
  let state = std::sync::Arc::new(test_state());
  let now = crate::store::tokens::now_secs();
  let (expiring_soon, _) = {
    let mut store = state.token_store.lock().await;
    store
      .create(TokenSpec {
        name: "expiring".into(),
        // Expires in half an hour, inside the 24h window.
        ttl_seconds: Some(1800),
        ..Default::default()
      })
      .expect("the test store can be written to")
  };
  {
    let mut store = state.token_store.lock().await;
    store
      .create(TokenSpec {
        name: "fresh".into(),
        // A week out, outside the window.
        ttl_seconds: Some(7 * 24 * 3600),
        ..Default::default()
      })
      .expect("the test store can be written to");
  }

  let mut warned = std::collections::HashSet::new();
  token_expiry_tick_once(&state, 24 * 3600, now, &mut warned).await;
  assert_eq!(warned.len(), 1, "only the token inside the window");

  let events = state.audit.lock().await.recent();
  let expiring_events = events
    .iter()
    .filter(|e| e.event == "token_expiring")
    .count();
  assert_eq!(expiring_events, 1);
  assert!(
    events
      .iter()
      .any(|e| e.event == "token_expiring" && e.details.contains("name=expiring")),
    "the warning names the token"
  );

  // A second beat with the same set warns nobody again.
  token_expiry_tick_once(&state, 24 * 3600, now, &mut warned).await;
  let events = state.audit.lock().await.recent();
  assert_eq!(
    events
      .iter()
      .filter(|e| e.event == "token_expiring")
      .count(),
    1,
    "once per token per expiry"
  );

  // A refresh moves expires_at, which re-arms the warning: the old entry is
  // swept the beat after the recorded expiry passes.
  let past_old_expiry = now + 3600;
  token_expiry_tick_once(&state, 24 * 3600, past_old_expiry, &mut warned).await;
  assert!(
    warned.is_empty(),
    "a passed expiry is forgotten so a refreshed token can warn again"
  );
  let _ = expiring_soon;
}

#[test]
fn one_hot_reload_tick_applies_a_changed_file_and_audits_it() {
  use crate::hot_reload_tick_once;
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
      let _ = crate::config_file::reload();
    }
  }
  let _cleanup = Cleanup;
  let file =
    crate::test_support::test_temp_root().join(format!("hotreload-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(&file, "gateway_timeout: 10\n").unwrap();
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", file.to_str().unwrap()) };
  crate::config_file::load();

  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let state = std::sync::Arc::new(test_state());
    let mtime = std::fs::metadata(&file)
      .ok()
      .and_then(|m| m.modified().ok());

    // Nothing moved: the beat is a no-op and keeps the mtime.
    let same = hot_reload_tick_once(&state, &file, mtime).await;
    assert_eq!(same, mtime);
    assert!(
      state
        .audit
        .lock()
        .await
        .recent()
        .iter()
        .all(|e| e.event != "config_reloaded"),
      "an unchanged file reloads nothing"
    );

    // The file changes: the beat re-applies it, the live setting moves, and
    // the audit trail says which key. `None` as the remembered mtime stands
    // in for "it moved": filesystem mtime granularity is up to a second, and
    // a test must not sleep its way across it.
    std::fs::write(&file, "gateway_timeout: 42\n").unwrap();
    let next = hot_reload_tick_once(&state, &file, None).await;
    assert!(
      next.is_some(),
      "the new mtime is what the next beat compares to"
    );
    assert_eq!(state.config().gateway_timeout, Duration::from_secs(42));
    let events = state.audit.lock().await.recent();
    let entry = events
      .iter()
      .find(|e| e.event == "config_reloaded")
      .expect("the reload is audited");
    assert!(
      entry.details.contains("gateway_timeout"),
      "{}",
      entry.details
    );
  });
}

#[tokio::test]
async fn bind_listener_binds_plain_reuseport_and_reports_a_taken_port() {
  use crate::bind_listener;
  // Plain bind on an ephemeral port.
  let plain = bind_listener("127.0.0.1", 0, false).await.unwrap();
  let taken = plain.local_addr().unwrap().port();

  // The SO_REUSEPORT path builds its socket by hand; prove it yields a
  // working listener too.
  let shared = bind_listener("127.0.0.1", 0, true).await.unwrap();
  assert!(shared.local_addr().unwrap().port() > 0);

  // A port someone plainly holds is refused for a plain second bind: this is
  // the branch serve_until_shutdown turns into its startup error.
  assert!(bind_listener("127.0.0.1", taken, false).await.is_err());

  // And a hostname that resolves to nothing is an error, not a hang.
  assert!(
    bind_listener("definitely-not-a-host.invalid", 0, true)
      .await
      .is_err()
  );
}
