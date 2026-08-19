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

// ----- the token buckets' arithmetic and boundaries (planned_features #146)
// ---------------------------------------------------------------------------
//
// A mutation sweep left twenty survivors in this module, and they were all one
// shape: every comparison and every operator in the two remaining buckets, the
// per-IP one and the per-route one, could be changed without a test noticing.
// A token bucket is arithmetic and boundaries and nothing else, so the suite
// was pinning that the limiter exists and refuses eventually, not what it
// computes. An off-by-one here admits one request more than the config says,
// or refuses one it promised, and neither is visible to a test that only spends
// a burst and waits for a 429.

/// A state whose per-IP bucket holds `max` and refills at `refill` per second.
fn ip_state(max: f64, refill: f64) -> AppState {
  let mut config = crate::test_support::test_config();
  config.ip_limit_max = max;
  config.ip_limit_refill = refill;
  crate::test_support::test_state_with(config)
}

/// A state carrying one `rate_limits:` rule over every host and path.
fn route_state(rps: f64, burst: f64) -> AppState {
  let mut config = crate::test_support::test_config();
  config.route_limits = crate::route_limits::RouteLimits {
    rules: vec![crate::route_limits::RateLimitRule {
      hostname: None,
      path: None,
      rps,
      burst,
      methods: None,
      key: "test".to_string(),
    }],
  };
  crate::test_support::test_state_with(config)
}

const IP: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 9));

/// The per-IP bucket admits exactly its capacity, and the next one is refused.
///
/// `state.tokens >= cost` is the whole enforcement, and the tests around it
/// spent a burst and asserted that *something* was eventually refused, which
/// holds for a bucket one too large or one too small. This counts.
#[tokio::test]
async fn the_ip_bucket_admits_its_capacity_and_no_more() {
  // No refill, so what is measured is the capacity rather than the clock.
  let state = ip_state(5.0, 0.0);
  for i in 0..5 {
    assert!(
      state.check_rate_limit(IP).await,
      "request {i} is within a capacity of five"
    );
  }
  assert!(
    !state.check_rate_limit(IP).await,
    "the sixth request is one over a capacity of five"
  );
}

/// The per-IP bucket puts back exactly `elapsed * refill`, and no other
/// arithmetic over those two terms produces the same answer.
///
/// `tokens + elapsed * refill_rate` had `+` mutated to `-` and `*` to `+`, and
/// both survived: back-to-back calls leave `elapsed` at nearly zero, where
/// `t + 0*r`, `t - 0*r` and `t + (0+r)` are indistinguishable. Letting real
/// time pass is what separates them, and the assertions are two-sided so that
/// "more came back than should have" fails as loudly as "none did".
#[tokio::test]
async fn the_ip_bucket_refills_by_elapsed_times_rate() {
  // Ten a second, capacity ten: 300ms is three tokens, not two and not four.
  let state = ip_state(10.0, 10.0);
  for _ in 0..10 {
    assert!(state.check_rate_limit(IP).await);
  }
  assert!(!state.check_rate_limit(IP).await, "the bucket is spent");

  tokio::time::sleep(std::time::Duration::from_millis(300)).await;
  // Three are owed. Two are claimed here, well clear of the timing slack at
  // either end, and the third is left as headroom for a slow machine.
  for i in 0..2 {
    assert!(
      state.check_rate_limit(IP).await,
      "token {i} of the three that 300ms at ten a second owes; if this fails \
       the refill term is not adding elapsed * rate"
    );
  }

  // And the other side: a spent bucket that has *not* waited stays spent. If
  // `*` became `+` the refill would be `elapsed + rate`, which hands over a
  // whole rate's worth on any call however little time has passed.
  let quick = ip_state(1.0, 10.0);
  assert!(quick.check_rate_limit(IP).await, "the one token it holds");
  assert!(
    !quick.check_rate_limit(IP).await,
    "immediately after, nothing is owed yet; if this fails the refill is not \
     scaled by how little time passed"
  );
}

/// The per-route bucket refills the same way, and `*` is not `/`.
///
/// `bucket.tokens + elapsed * rps` mutated to `elapsed / rps` survived. At one
/// request a second the two agree, which is why this uses four: `0.5 * 4` is
/// two tokens back and `0.5 / 4` is an eighth of one.
#[tokio::test]
async fn the_route_bucket_refills_by_elapsed_times_rate() {
  let state = route_state(4.0, 4.0);
  for _ in 0..4 {
    assert!(state.check_route_rate_limit(None, "/", "GET").await);
  }
  assert!(
    !state.check_route_rate_limit(None, "/", "GET").await,
    "the burst of four is spent"
  );

  tokio::time::sleep(std::time::Duration::from_millis(500)).await;
  assert!(
    state.check_route_rate_limit(None, "/", "GET").await,
    "500ms at four a second owes two tokens; if this fails the refill term is \
     dividing by the rate rather than multiplying by it"
  );
}

/// A route bucket admits its burst exactly, not its burst plus one.
#[tokio::test]
async fn the_route_bucket_admits_its_burst_and_no_more() {
  // A rate low enough that nothing meaningful refills while the loop runs.
  let state = route_state(0.001, 3.0);
  for i in 0..3 {
    assert!(
      state.check_route_rate_limit(None, "/", "GET").await,
      "request {i} is within a burst of three"
    );
  }
  assert!(
    !state.check_route_rate_limit(None, "/", "GET").await,
    "the fourth request is one over a burst of three"
  );
}

/// A zero burst never reaches the limiter, because the config refuses it
/// first.
///
/// This is what the twentieth survivor turned out to be rather than a gap.
/// `rl.burst.filter(|b| *b > 0.0)` in `check_route_rate_limit` mutated to
/// `>= 0.0` survives, and no test can kill it through a configured route:
/// `StaticRoutes::compile` rejects a non-positive burst before the limiter is
/// ever asked, so the filter is a second line that the first line makes
/// unreachable. The check that *does* have teeth is this one, and it lives
/// here so the next sweep's reader finds the answer next to the question.
#[test]
fn a_zero_burst_is_refused_by_the_config_before_the_limiter_sees_it() {
  use crate::static_routes::{RouteRateLimit, RouteRule, StaticRoutes};
  let err = StaticRoutes::compile(vec![RouteRule {
    path: Some("/api".to_string()),
    rate_limit: Some(RouteRateLimit {
      rps: 5.0,
      burst: Some(0.0),
      methods: None,
    }),
    ..Default::default()
  }])
  .err()
  .expect("a zero burst is not a burst");
  assert!(
    err.contains("`rate_limit.burst` must be positive"),
    "the refusal should name the key the operator wrote: {err}"
  );
}

// ----- the sweeps -----

/// A stale entry is dropped and a fresh one is kept, in both maps the beat
/// sweeps.
///
/// `duration_since(last_updated) < 600` had `<` mutated to `>` and to `==`,
/// and both survived because no test ever looked at what the beat left behind.
/// Inverted, the sweep keeps precisely the entries it exists to drop and drops
/// every live one, which is a limiter that forgets an attacker's bucket on
/// every beat.
#[tokio::test]
async fn the_beat_drops_stale_buckets_and_keeps_live_ones() {
  let state = crate::test_support::test_state();
  let now = std::time::Instant::now();
  let stale = now - std::time::Duration::from_secs(3600);

  {
    let mut ip = state.rate_limiter.lock().await;
    ip.insert(
      "198.51.100.1".parse().unwrap(),
      RateLimitState {
        tokens: 1.0,
        last_updated: stale,
      },
    );
    ip.insert(
      "198.51.100.2".parse().unwrap(),
      RateLimitState {
        tokens: 1.0,
        last_updated: now,
      },
    );
    let mut route = state.route_rate.lock().await;
    route.insert(
      "old".to_string(),
      RateLimitState {
        tokens: 1.0,
        last_updated: stale,
      },
    );
    route.insert(
      "new".to_string(),
      RateLimitState {
        tokens: 1.0,
        last_updated: now,
      },
    );
  }

  state.gc_tick_once(now).await;

  let ip = state.rate_limiter.lock().await;
  assert!(
    !ip.contains_key(&"198.51.100.1".parse().unwrap()),
    "an hour-old bucket is stale and must be swept"
  );
  assert!(
    ip.contains_key(&"198.51.100.2".parse().unwrap()),
    "a bucket touched just now must survive the beat; if this fails the sweep \
     is keeping what it should drop"
  );
  let route = state.route_rate.lock().await;
  assert!(!route.contains_key("old"), "the same, for the route map");
  assert!(route.contains_key("new"), "the same, for the route map");
}

/// Ten minutes is the cutoff and it is exclusive: an entry exactly that old is
/// swept.
///
/// This is the one boundary the beat can be asked about exactly, because
/// `gc_tick_once` takes `now` as an argument rather than reading the clock.
/// `<` mutated to `<=` survived, and it is the difference between a map that
/// bounds itself at ten minutes and one that bounds itself at ten minutes plus
/// however long an entry sits on the boundary.
#[tokio::test]
async fn the_beats_cutoff_is_exclusive_at_ten_minutes() {
  let state = crate::test_support::test_state();
  let now = std::time::Instant::now();
  let exactly_ten = now - std::time::Duration::from_secs(600);
  let just_inside = now - std::time::Duration::from_secs(599);

  {
    let mut ip = state.rate_limiter.lock().await;
    ip.insert(
      "198.51.100.3".parse().unwrap(),
      RateLimitState {
        tokens: 1.0,
        last_updated: exactly_ten,
      },
    );
    ip.insert(
      "198.51.100.4".parse().unwrap(),
      RateLimitState {
        tokens: 1.0,
        last_updated: just_inside,
      },
    );
    // The beat sweeps two maps with the same cutoff written twice, so the
    // boundary is asserted on both: with only the first, the route map's copy
    // of the comparison could be widened to `<=` and nothing would fail.
    let mut route = state.route_rate.lock().await;
    route.insert(
      "at-the-cutoff".to_string(),
      RateLimitState {
        tokens: 1.0,
        last_updated: exactly_ten,
      },
    );
    route.insert(
      "inside".to_string(),
      RateLimitState {
        tokens: 1.0,
        last_updated: just_inside,
      },
    );
  }

  state.gc_tick_once(now).await;

  let ip = state.rate_limiter.lock().await;
  assert!(
    !ip.contains_key(&"198.51.100.3".parse().unwrap()),
    "exactly ten minutes is not less than ten minutes, so it is swept"
  );
  assert!(
    ip.contains_key(&"198.51.100.4".parse().unwrap()),
    "one second inside the cutoff survives"
  );
  let route = state.route_rate.lock().await;
  assert!(
    !route.contains_key("at-the-cutoff"),
    "the route map's cutoff is the same one, and exclusive the same way"
  );
  assert!(route.contains_key("inside"), "one second inside survives");
}

/// The inline failsafe sweeps when the map is *over* the threshold, and not
/// before, and not at it.
///
/// `len() > 1000` had `>` mutated to `>=`, to `==` and to `<`, and all three
/// survived. It takes three map sizes to separate them, because each mutant
/// differs from the original at exactly one of them: at the threshold `>=`
/// sweeps and `>` does not; over it `==` stops sweeping; under it `<` starts.
/// A test at one size kills one mutant and reports the others as unreachable,
/// which is how this line came to have three survivors on it at once.
///
/// The `<` case is the one worth naming: under it the failsafe runs on every
/// request while the map is small and stands down once it is large, which is
/// the sweep doing its work exactly when it is not needed and refusing it when
/// it is.
async fn failsafe_case(fill: usize, stale_survives: bool, label: &str) {
  let state = ip_state(1000.0, 0.0);
  let stale = std::time::Instant::now() - std::time::Duration::from_secs(3600);
  let marker: std::net::IpAddr = "198.51.100.5".parse().unwrap();
  let live: std::net::IpAddr = "198.51.100.6".parse().unwrap();

  {
    let mut map = state.rate_limiter.lock().await;
    map.insert(
      marker,
      RateLimitState {
        tokens: 1.0,
        last_updated: stale,
      },
    );
    // Filled to `fill` counting the marker and the live entry, since the
    // length is read before the request's own entry is inserted.
    for i in 0..(fill - 2) as u32 {
      map.insert(
        std::net::IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + i)),
        RateLimitState {
          tokens: 1.0,
          last_updated: stale,
        },
      );
    }
    // A live entry alongside the stale one. When the failsafe does fire it
    // must drop the stale and keep this, which is what separates the real
    // comparison from one that keeps only entries *at* the cutoff.
    map.insert(
      live,
      RateLimitState {
        tokens: 1.0,
        last_updated: std::time::Instant::now(),
      },
    );
    assert_eq!(map.len(), fill, "the fixture holds what the case describes");
  }

  state.check_rate_limit(IP).await;

  let map = state.rate_limiter.lock().await;
  assert_eq!(map.contains_key(&marker), stale_survives, "{label}");
  assert!(
    map.contains_key(&live),
    "a bucket touched just now survives every sweep: {label}"
  );
}

#[tokio::test]
async fn the_ip_failsafe_sweeps_over_the_threshold_and_not_at_or_under_it() {
  failsafe_case(
    2,
    true,
    "a small map is left alone between beats: the failsafe is not the routine \
     sweep and must not run on every request",
  )
  .await;
  failsafe_case(
    1000,
    true,
    "exactly at the threshold is not over it, so the map is left alone",
  )
  .await;
  failsafe_case(
    1100,
    false,
    "over the threshold the failsafe sweeps, and an hour-old entry goes",
  )
  .await;
}

/// The same three cases for the per-route map, which has the same failsafe
/// over its own threshold constant and had none of it exercised.
async fn route_failsafe_case(fill: usize, stale_survives: bool, label: &str) {
  let state = route_state(1000.0, 1000.0);
  let stale = std::time::Instant::now() - std::time::Duration::from_secs(3600);

  {
    let mut map = state.route_rate.lock().await;
    map.insert(
      "marker".to_string(),
      RateLimitState {
        tokens: 1.0,
        last_updated: stale,
      },
    );
    for i in 0..(fill - 2) {
      map.insert(
        format!("filler-{i}"),
        RateLimitState {
          tokens: 1.0,
          last_updated: stale,
        },
      );
    }
    map.insert(
      "live".to_string(),
      RateLimitState {
        tokens: 1.0,
        last_updated: std::time::Instant::now(),
      },
    );
    assert_eq!(map.len(), fill, "the fixture holds what the case describes");
  }

  state.check_route_rate_limit(None, "/", "GET").await;

  let map = state.route_rate.lock().await;
  assert_eq!(map.contains_key("marker"), stale_survives, "{label}");
  assert!(
    map.contains_key("live"),
    "a bucket touched just now survives every sweep: {label}"
  );
}

#[tokio::test]
async fn the_route_failsafe_sweeps_over_the_threshold_and_not_at_or_under_it() {
  route_failsafe_case(2, true, "a small route map is left alone between beats").await;
  route_failsafe_case(1000, true, "exactly at the threshold is not over it").await;
  route_failsafe_case(1100, false, "over the threshold the stale bucket goes").await;
}

/// The beat drops a session the moment it expires, not a second later.
///
/// `info.expires_at > now_secs` mutated to `>=` survived. The difference is one
/// second on a session that has just run out, which sounds like nothing until
/// it is the second somebody spends it in.
#[tokio::test]
async fn the_beat_drops_a_session_at_the_instant_it_expires() {
  use crate::store::sessions::{Plane, SessionInfo};
  use crate::store::users::Role;
  let state = crate::test_support::test_state();
  let now_secs = crate::store::sessions::now_secs();
  let session = |expires_at: u64| SessionInfo {
    plane: Plane::Admin,
    expires_at,
    created_at: 0,
    ip: None,
    user_agent: None,
    scope_host: None,
    username: Some("op".to_string()),
    role: Role::Admin,
    selected_org: None,
    bound_org: None,
  };

  {
    let mut sessions = state.sessions.lock().await;
    // Expiring exactly now, which is expired: the test is `expires_at > now`.
    sessions.insert("spent", session(now_secs));
    sessions.insert("live", session(now_secs + 3600));
  }

  state.gc_tick_once(std::time::Instant::now()).await;

  let sessions = state.sessions.lock().await;
  assert!(
    sessions.get("spent").is_none(),
    "a session whose expiry has arrived is over; `>=` would give it one more \
     second, and the whole of the second it is checked in"
  );
  assert!(
    sessions.get("live").is_some(),
    "an hour of validity left is not expired"
  );
}

/// `daily_max_bytes: 0` means no quota, in the same way `max_rps: 0` does.
///
/// `daily_max_bytes.filter(|v| *v > 0)` mutated to `>= 0` survived. Under it a
/// zero is a real quota of zero bytes, and since `used >= quota` is then true
/// of any usage at all, the first request after a single byte has moved is
/// refused for exceeding a limit the operator never set.
#[tokio::test]
async fn a_token_with_no_daily_quota_is_not_given_one_of_zero() {
  let state = crate::test_support::test_state();
  let id = token(
    &state,
    TokenSpec {
      name: "unquotaed".to_string(),
      daily_max_bytes: Some(0),
      ..Default::default()
    },
  )
  .await;
  // Usage on the books, so the quota comparison is actually reached.
  state.add_token_bytes(Some(&id), 4096).await;
  assert!(
    state.check_token_limits(Some(&id)).await.is_ok(),
    "a written zero is the absence of a quota, not a quota of nothing"
  );
}

// Two mutants on the inline failsafes' cutoff, `< Duration::from_secs(600)`
// widened to `<=` at both call sites, are left alive on purpose.
//
// They cannot be killed from here and would not be worth killing. The beat's
// copy of that comparison takes `now` as an argument, which is why
// `the_beats_cutoff_is_exclusive_at_ten_minutes` can put an entry exactly on
// the boundary and assert what happens to it. The failsafes read
// `Instant::now()` inside the function, so a test cannot place an entry at
// exactly six hundred seconds, and neither can a running server: the case the
// two spellings disagree about has measure zero on a real clock, and an entry
// that survives one sweep is swept by the next. Making it reachable would mean
// threading a clock through two hot paths to pin a difference nobody can
// observe.
//
// The other two from that sweep are also not gaps, and are recorded where the
// question comes up rather than here: the burst filter is unreachable behind
// `compile`'s validation (see the zero-burst test above), and the
// `route_limits.is_empty()` match guard is a fast path whose absence changes
// nothing, since `matched()` over an empty rule list returns `None` anyway.
