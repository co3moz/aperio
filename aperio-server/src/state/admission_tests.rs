//! What the server lets in, and on whose budget.
//!
//! This module had no sibling test file: its coverage came from `state_tests.rs`,
//! which is the crate's file rather than this one's, and it showed. A mutation
//! run over 107 mutants left 30 alive here, and they fell into two groups.
//!
//! The first is arithmetic that no test ever exercised, because every existing
//! test calls a limiter twice in a row. Back to back, `elapsed` is nearly zero,
//! so `tokens + elapsed * rate` is `tokens + 0` and the whole refill term can
//! be deleted, inverted or divided without changing an answer. These tests let
//! real time pass, which is the only way to make that term mean anything.
//!
//! The second is enforcement that was only ever checked in its permissive
//! direction: a quota with nothing to count, an org with no clients. Replacing
//! the whole of `check_org_client_quota` with `Ok(())` survived.

use super::*;
use crate::store::tokens::TokenSpec;

/// A token with the given spec, and its id.
async fn token(state: &AppState, spec: TokenSpec) -> String {
  let mut store = state.token_store.lock().await;
  store
    .create(spec)
    .expect("the test store can be written to")
    .0
    .id
}

// ----- token limits -----

/// `max_rps: 0` means no limit, not a limit of zero.
///
/// The filter is `*v > 0.0`, and `>= 0.0` survived: under it a token that set
/// no rate limit gets a bucket with a burst of one, so the second request in
/// any given second from a token with no rate limit is refused. Every token
/// created without `max_rps` is affected, which is most of them.
#[tokio::test]
async fn a_token_with_no_rate_limit_is_not_given_one() {
  let state = crate::test_support::test_state();
  let id = token(
    &state,
    TokenSpec {
      name: "unlimited".to_string(),
      max_rps: Some(0.0),
      ..Default::default()
    },
  )
  .await;
  for i in 0..5 {
    assert!(
      state.check_token_limits(Some(&id)).await.is_ok(),
      "request {i} was refused, but this token declared no rate limit"
    );
  }
}

/// The token bucket refills as time passes.
///
/// `tokens + elapsed * rps` had both its operators mutated and both survived,
/// because the tests spend the burst in one breath and never come back. The
/// sleep is what gives `elapsed` a value: without it the refill term is zero
/// and any arithmetic over it is indistinguishable from any other.
#[tokio::test]
async fn a_spent_token_bucket_refills_with_time() {
  let state = crate::test_support::test_state();
  let id = token(
    &state,
    TokenSpec {
      name: "ten".to_string(),
      max_rps: Some(10.0),
      ..Default::default()
    },
  )
  .await;
  // Burst is the rate, so ten go through and the eleventh does not.
  for _ in 0..10 {
    assert!(state.check_token_limits(Some(&id)).await.is_ok());
  }
  assert!(
    state.check_token_limits(Some(&id)).await.is_err(),
    "the burst is spent"
  );

  // At ten a second, 200ms is two tokens back.
  tokio::time::sleep(std::time::Duration::from_millis(200)).await;
  assert!(
    state.check_token_limits(Some(&id)).await.is_ok(),
    "time passing must put tokens back; if this fails the refill term is not \
     adding elapsed * rps to the bucket"
  );
}

/// The daily byte counter starts again on a new day rather than carrying over.
///
/// `entry.0 != today` inverted survived. Under it a request on the same day
/// resets the counter to that one request's bytes, so the daily quota is never
/// reached however much is transferred, and a request on a *new* day adds to
/// yesterday's total instead of starting fresh.
#[tokio::test]
async fn the_daily_byte_counter_rolls_over_rather_than_accumulating() {
  let state = crate::test_support::test_state();
  let id = token(
    &state,
    TokenSpec {
      name: "quota".to_string(),
      daily_max_bytes: Some(100),
      ..Default::default()
    },
  )
  .await;

  // Two charges on the same day add up and cross the quota.
  state.add_token_bytes(Some(&id), 60).await;
  assert!(
    state.check_token_limits(Some(&id)).await.is_ok(),
    "60 of 100"
  );
  state.add_token_bytes(Some(&id), 60).await;
  assert!(
    state.check_token_limits(Some(&id)).await.is_err(),
    "120 of 100: the two charges must add, not replace"
  );

  // Backdate the stored day: the next charge is a new day and starts over.
  {
    let mut usage = state.token_daily_bytes.lock().await;
    let entry = usage.get_mut(&id).expect("the token has usage");
    entry.0 = "1999-01-01".to_string();
  }
  state.add_token_bytes(Some(&id), 10).await;
  assert!(
    state.check_token_limits(Some(&id)).await.is_ok(),
    "a new day starts from that day's bytes, not yesterday's total"
  );
}

// ----- organization quotas -----

/// The client quota is enforced, and counts only that organization's clients.
///
/// Two mutants survived here and they are the two halves of the same
/// sentence: replacing the whole function with `Ok(())` (the quota is never
/// enforced), and flipping `==` to `!=` (it is enforced against everybody
/// else's clients, so an org alone on a server is refused at once while one
/// sharing with a busy neighbour is refused early).
#[tokio::test]
async fn the_org_client_quota_counts_that_orgs_clients_and_stops_at_the_cap() {
  let state = crate::test_support::test_state();
  let org = {
    let mut orgs = state.org_store.lock().await;
    let org = orgs.create("acme", Vec::new(), None).expect("org");
    orgs
      .set_quota(&org.id, Some(Some(2)), None, None, None)
      .expect("quota set");
    org.id
  };
  let other = {
    let mut orgs = state.org_store.lock().await;
    orgs.create("other", Vec::new(), None).expect("org").id
  };

  let connect = async |id: &str, owner: Option<&str>| {
    let mut c = crate::test_support::mock_client(None, None, None, None);
    c.perms.org_id = owner.map(str::to_string);
    state.clients.write().await.insert(id.to_string(), c);
  };

  assert!(
    state.check_org_client_quota(Some(&org)).await.is_ok(),
    "none yet"
  );

  // Somebody else's clients do not spend this org's quota.
  connect("x1", Some(&other)).await;
  connect("x2", Some(&other)).await;
  connect("x3", Some(&other)).await;
  assert!(
    state.check_org_client_quota(Some(&org)).await.is_ok(),
    "another organization's clients are not counted against this one"
  );

  connect("a1", Some(&org)).await;
  assert!(
    state.check_org_client_quota(Some(&org)).await.is_ok(),
    "1 of 2"
  );
  connect("a2", Some(&org)).await;
  assert!(
    state.check_org_client_quota(Some(&org)).await.is_err(),
    "at the cap the quota refuses; a function that always says Ok is not a quota"
  );
}

/// The monthly byte quota counts what was sent *and* what was received.
///
/// `bytes_sent + bytes_received` becoming `*` survived, and a product is zero
/// whenever either direction is, so a service that only uploads or only
/// downloads would never reach its quota at all.
#[tokio::test]
async fn the_monthly_quota_adds_both_directions() {
  let state = crate::test_support::test_state();
  let org = {
    let mut orgs = state.org_store.lock().await;
    let org = orgs.create("acme", Vec::new(), None).expect("org");
    orgs
      .set_quota(&org.id, None, None, None, Some(Some(100)))
      .expect("quota set");
    org.id
  };
  assert!(
    !state.org_over_month_bytes(Some(&org)).await,
    "nothing used yet"
  );

  // The numbers are chosen so the two operators disagree: 99 + 1 is a hundred
  // and over the quota, while 99 * 1 is ninety-nine and under it. Sixty each
  // way would not have separated them, since 3600 is over the quota too, and
  // a first version of this test used exactly that and let the mutant live.
  {
    let mut stats = state.persistent_stats.lock().await;
    stats.record_request(true, 99, 1, 1, Some(&org));
  }
  assert!(
    state.org_over_month_bytes(Some(&org)).await,
    "99 one way and 1 the other is 100, which reaches the quota; a product \
     would be 99 and let it through"
  );
}

// ----- maintenance windows -----

/// A scheduled window in the file is consulted.
///
/// The guard is `!cfg.maintenance_windows.is_empty()`, and deleting the `!`
/// survived: under it the window list is only read when it is *empty*, so
/// every scheduled window ever written is silently ignored. Nothing else
/// notices, because the runtime flag path still works and that is what the
/// dashboard uses; the file-configured windows just quietly stop existing.
// Held across awaits on purpose: the lock exists to serialize the tests that
// write `aperio-server.yaml`, so releasing it at the first await would defeat
// it. Same reason `oidc_tests.rs` allows this.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_scheduled_window_in_the_file_puts_the_host_into_maintenance() {
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
    }
  }
  let _cleanup = Cleanup;
  // A window covering the whole day, every day, so the test does not depend
  // on when it runs.
  std::fs::write(
    "aperio-server.yaml",
    "maintenance_windows:\n  - hostname: 'app.example.com'\n    from: '00:00'\n    to: '23:59'\n    reason: 'scheduled'\n",
  )
  .unwrap();
  crate::config_file::reload().unwrap();

  let mut cfg = crate::test_support::test_config();
  cfg.maintenance_windows = crate::maintenance_windows::from_config_file();
  assert!(
    !cfg.maintenance_windows.is_empty(),
    "the fixture must actually parse, or this test proves nothing"
  );
  let state = crate::test_support::test_state_with(cfg);

  let flag = state
    .maintenance_for(Some("app.example.com"))
    .await
    .expect("the window covers this host right now");
  assert_eq!(flag.reason.as_deref(), Some("scheduled"));
  assert!(
    state
      .maintenance_for(Some("other.example.com"))
      .await
      .is_none(),
    "a window naming one host does not cover another"
  );
}
