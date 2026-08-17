//! Narrowing a pool to the service that serves a request: hostname before
//! path, ejection and health, the load-balancing strategies, sticky affinity,
//! and the per-candidate IP filter, all keyed on the service rather than the
//! connection carrying it.

use super::super::tests::*;
use super::*;
use crate::routing::{apply_lb_strategy, find_affinity_match, select_client_pool};
use crate::settings::LbStrategy;
use crate::state::AppState;
use crate::tests::{TEST_THRESHOLD, mock_client};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use std::time::Instant;

// --- select_client_pool -----------------------------------------------------

#[test]
fn pool_prefers_host_matched_clients() {
  let mut bound = base_handle();
  bound.sole_mut().assigned_hostnames = vec!["a.example.com".to_string()];
  let unbound = base_handle();

  let clients = pool_of(vec![("bound", bound), ("unbound", unbound)]);
  let (pool, (host_key, path_key)) =
    select_client_pool(&clients, "/", Some("a.example.com"), false, HEALTHY).unwrap();
  assert_eq!(ids(&pool), vec!["bound".to_string()]);
  assert_eq!(host_key, Some("a.example.com".to_string()));
  assert_eq!(path_key, None);
}

#[test]
fn pool_falls_back_to_unbound_when_not_strict() {
  let mut bound = base_handle();
  bound.sole_mut().assigned_hostnames = vec!["a.example.com".to_string()];
  let unbound = base_handle();

  let clients = pool_of(vec![("bound", bound), ("unbound", unbound)]);
  // Request host matches nobody → unbound pool answers when not strict.
  let (pool, (host_key, _)) =
    select_client_pool(&clients, "/", Some("other.example.com"), false, HEALTHY).unwrap();
  assert_eq!(ids(&pool), vec!["unbound".to_string()]);
  assert_eq!(host_key, None);
}

#[test]
fn ejected_client_removed_but_pool_fails_open_when_all_ejected() {
  let future = std::time::Instant::now() + Duration::from_secs(60);

  // One healthy, one ejected → only the healthy client is routed.
  let mut ejected = base_handle();
  ejected.sole_mut().ejected_until = Some(future);
  let healthy = base_handle();
  let clients = pool_of(vec![("ejected", ejected), ("healthy", healthy)]);
  let (pool, _) = select_client_pool(&clients, "/", None, false, HEALTHY).unwrap();
  assert_eq!(ids(&pool), vec!["healthy".to_string()]);

  // Every candidate ejected → fail open (better a struggling client than none).
  let mut a = base_handle();
  a.sole_mut().ejected_until = Some(future);
  let mut b = base_handle();
  b.sole_mut().ejected_until = Some(future);
  let all = pool_of(vec![("a", a), ("b", b)]);
  let (pool, _) = select_client_pool(&all, "/", None, false, HEALTHY).unwrap();
  assert_eq!(pool.len(), 2);
}

#[test]
fn record_failure_ejects_after_threshold() {
  let mut h = base_handle();
  let now = std::time::Instant::now();
  let window = Duration::from_secs(30);
  let eject = Duration::from_secs(30);
  // Below threshold: not yet ejected.
  assert!(!h.sole_mut().record_failure(now, window, 3, eject));
  assert!(!h.sole_mut().record_failure(now, window, 3, eject));
  assert!(!h.sole().is_ejected(now));
  // Third failure crosses the threshold and ejects.
  assert!(h.sole_mut().record_failure(now, window, 3, eject));
  assert!(h.sole().is_ejected(now));
  // Still ejected before the window, clear after it.
  assert!(h.sole().is_ejected(now + Duration::from_secs(29)));
  assert!(!h.sole().is_ejected(now + Duration::from_secs(31)));
}

#[test]
fn pool_strict_mode_rejects_unbound() {
  let unbound = base_handle();
  let clients = pool_of(vec![("unbound", unbound)]);
  // require_hostname_bind = true and no host match → no route.
  assert!(select_client_pool(&clients, "/", Some("x.example.com"), true, HEALTHY).is_none());
}

#[test]
fn pool_longest_path_bind_wins() {
  let mut api = base_handle();
  api.sole_mut().declared_path = Some("/api".to_string());
  let mut apiv1 = base_handle();
  apiv1.sole_mut().declared_path = Some("/api/v1".to_string());
  let unbound = base_handle();

  let clients = pool_of(vec![("api", api), ("apiv1", apiv1), ("unbound", unbound)]);
  let (pool, (_, path_key)) =
    select_client_pool(&clients, "/api/v1/users", None, false, HEALTHY).unwrap();
  assert_eq!(ids(&pool), vec!["apiv1".to_string()]);
  assert_eq!(path_key, Some("/api/v1".to_string()));
}

#[test]
fn pool_ineligible_clients_excluded() {
  let mut draining = base_handle();
  draining.draining = true;
  let mut disabled = base_handle();
  disabled.sole_mut().admin_enabled = false;
  let mut unhealthy_backend = base_handle();
  unhealthy_backend.sole_mut().backend_healthy = false;

  let clients = pool_of(vec![
    ("draining", draining),
    ("disabled", disabled),
    ("unhealthy", unhealthy_backend),
  ]);
  assert!(select_client_pool(&clients, "/", None, false, HEALTHY).is_none());
}

#[test]
fn pool_excludes_stale_clients() {
  let healthy = base_handle();
  let clients = pool_of(vec![("healthy", healthy)]);
  // A zero threshold makes even a just-connected client stale.
  assert!(select_client_pool(&clients, "/", None, false, Duration::ZERO).is_none());
}

// --- apply_lb_strategy ------------------------------------------------------

#[test]
fn lb_round_robin_and_sticky_keep_pool() {
  let clients = pool_of(vec![("a", base_handle()), ("b", base_handle())]);
  let pool = refs(&["a", "b"]);
  assert_eq!(
    ids(&apply_lb_strategy(
      pool.clone(),
      &clients,
      LbStrategy::RoundRobin
    )),
    ids(&pool)
  );
  assert_eq!(
    ids(&apply_lb_strategy(
      pool.clone(),
      &clients,
      LbStrategy::Sticky
    )),
    ids(&pool)
  );
}

#[test]
fn lb_primary_standby_keeps_lowest_priority() {
  let mut primary = base_handle();
  primary.sole_mut().priority = 0;
  let mut standby = base_handle();
  standby.sole_mut().priority = 5;
  let clients = pool_of(vec![("primary", primary), ("standby", standby)]);

  let pool = refs(&["primary", "standby"]);
  let narrowed = apply_lb_strategy(pool, &clients, LbStrategy::PrimaryStandby);
  assert_eq!(ids(&narrowed), vec!["primary".to_string()]);
}

// --- find_affinity_match ----------------------------------------------------

#[test]
fn affinity_matches_instance_id_then_connection_id() {
  let mut with_instance = base_handle();
  with_instance.reported_instance_id = Some("inst-1".to_string());
  let plain = base_handle();

  let clients = pool_of(vec![("conn-a", with_instance), ("conn-b", plain)]);
  let pool = refs(&["conn-a", "conn-b"]);

  // Reported instance id wins.
  assert_eq!(
    find_affinity_match(&pool, &clients, "inst-1").map(|r| r.client),
    Some("conn-a".to_string())
  );
  // Falls back to the connection id.
  assert_eq!(
    find_affinity_match(&pool, &clients, "conn-b").map(|r| r.client),
    Some("conn-b".to_string())
  );
  // Unknown affinity value.
  assert_eq!(find_affinity_match(&pool, &clients, "nope"), None);
}

// --- ClientHandle routing accessors -----------------------------------------

#[test]
fn effective_path_bind_precedence() {
  let mut h = base_handle();
  h.sole_mut().assigned_path = Some("/granted".to_string());
  assert_eq!(
    h.sole().effective_path_bind(),
    Some(&"/granted".to_string())
  );

  // Declared wins over assigned.
  h.sole_mut().declared_path = Some("/declared".to_string());
  assert_eq!(
    h.sole().effective_path_bind(),
    Some(&"/declared".to_string())
  );

  // Dashboard override wins over everything.
  h.sole_mut().override_path_bind = Some("/override".to_string());
  assert_eq!(
    h.sole().effective_path_bind(),
    Some(&"/override".to_string())
  );
}

#[test]
fn matches_host_uses_override_then_union() {
  let mut h = base_handle();
  h.sole_mut().assigned_hostnames = vec!["a.example.com".to_string()];
  h.sole_mut().declared_hostname = Some("b.example.com".to_string());
  assert!(h.sole().has_hostname_bind());
  assert!(h.sole().matches_host("a.example.com"));
  assert!(h.sole().matches_host("b.example.com"));
  assert!(!h.sole().matches_host("c.example.com"));

  // An override replaces the whole set.
  h.sole_mut().override_hostname_binds = vec!["c.example.com".to_string()];
  assert!(h.sole().matches_host("c.example.com"));
  assert!(!h.sole().matches_host("a.example.com"));

  // Several overridden names all route: an operator retargeting the hostname
  // the client declared can keep the random subdomain alive alongside it.
  h.sole_mut().override_hostname_binds =
    vec!["c.example.com".to_string(), "a.example.com".to_string()];
  assert!(h.sole().matches_host("c.example.com"));
  assert!(h.sole().matches_host("a.example.com"));
  assert!(!h.sole().matches_host("b.example.com"));
}

#[test]
fn is_healthy_threshold() {
  let h = base_handle();
  // Just connected: healthy under any positive threshold.
  assert!(h.is_healthy(Duration::from_secs(60)));
  // A zero threshold is never satisfied (elapsed is never < 0).
  assert!(!h.is_healthy(Duration::ZERO));
}

// --- filter_pool_by_ip (per-candidate allowed_ips, #123) ---------------------

#[test]
fn test_filter_pool_by_ip_union_semantics() {
  let visitor: IpAddr = "127.0.0.1".parse().unwrap();

  let mut restricted = base_handle();
  restricted.sole_mut().allowed_ips = vec!["203.0.113.7".to_string()];
  let open = base_handle();
  let clients = pool_of(vec![("restricted", restricted), ("open", open)]);

  // Union semantics: the unrestricted candidate admits the visitor even
  // though the restricted one rejects them (fail-open by design).
  match filter_pool_by_ip(refs(&["restricted", "open"]), &clients, Some(visitor)) {
    IpFilterOutcome::Allowed(pool) => assert_eq!(ids(&pool), vec!["open".to_string()]),
    IpFilterOutcome::Denied(_) => panic!("open candidate must admit the visitor"),
  }

  // No visitor IP (internal callers): the pool passes through untouched.
  match filter_pool_by_ip(refs(&["restricted"]), &clients, None) {
    IpFilterOutcome::Allowed(pool) => assert_eq!(ids(&pool), vec!["restricted".to_string()]),
    IpFilterOutcome::Denied(_) => panic!("no-ip filtering must not deny"),
  }
}

#[test]
fn test_filter_pool_by_ip_denied_picks_most_primary_redirect() {
  let visitor: IpAddr = "127.0.0.1".parse().unwrap();

  // Two rejecting candidates: the standby declares a redirect, the primary
  // does too, the most-primary (lowest tier) declaring entry wins.
  let mut primary = base_handle();
  primary.sole_mut().allowed_ips = vec!["203.0.113.7".to_string()];
  primary.sole_mut().priority = 0;
  primary.sole_mut().denied = Some("https://primary.example.com/denied".to_string());
  let mut standby = base_handle();
  standby.sole_mut().allowed_ips = vec!["203.0.113.8".to_string()];
  standby.sole_mut().priority = 5;
  standby.sole_mut().denied = Some("https://standby.example.com/denied".to_string());
  let clients = pool_of(vec![("p", primary), ("s", standby)]);

  match filter_pool_by_ip(refs(&["p", "s"]), &clients, Some(visitor)) {
    IpFilterOutcome::Denied(redirect) => {
      assert_eq!(
        redirect.as_deref(),
        Some("https://primary.example.com/denied")
      );
    }
    IpFilterOutcome::Allowed(_) => panic!("both candidates must reject the visitor"),
  }
}

#[test]
fn test_filter_pool_by_ip_denied_without_redirect_is_stealth() {
  let visitor: IpAddr = "127.0.0.1".parse().unwrap();
  let mut restricted = base_handle();
  restricted.sole_mut().allowed_ips = vec!["203.0.113.7".to_string()];
  let clients = pool_of(vec![("r", restricted)]);

  match filter_pool_by_ip(refs(&["r"]), &clients, Some(visitor)) {
    IpFilterOutcome::Denied(redirect) => assert!(redirect.is_none()),
    IpFilterOutcome::Allowed(_) => panic!("the only candidate must reject the visitor"),
  }
}

#[test]
fn test_ip_in_ranges_matches_plain_and_cidr() {
  let ranges = parse_trusted_proxies("203.0.113.7, 10.0.0.0/8").unwrap();
  assert!(ip_in_ranges("203.0.113.7".parse().unwrap(), &ranges));
  assert!(ip_in_ranges("10.4.5.6".parse().unwrap(), &ranges));
  assert!(!ip_in_ranges("192.168.1.1".parse().unwrap(), &ranges));
  // An empty allowlist matches nothing (callers treat empty as "no fence").
  assert!(!ip_in_ranges("10.4.5.6".parse().unwrap(), &[]));
}

// --- pick_proxy_client / route_exists ---------------------------------------

/// Registers two healthy clients bound to the same hostname and returns the
/// state serving that route.
async fn two_client_pool() -> std::sync::Arc<AppState> {
  let state = std::sync::Arc::new(crate::test_support::test_state());
  for id in ["a", "b"] {
    let mut c = crate::test_support::mock_client(Some("app.example.com"), None, None, None);
    c.reported_instance_id = Some(id.to_string());
    state.clients.write().await.insert(id.to_string(), c);
  }
  state
}

async fn pick_one(state: &AppState) -> String {
  match pick_proxy_client(state, "/", Some("app.example.com"), None, None, None, None).await {
    PickOutcome::Selected(c) => c.id,
    other => panic!(
      "expected a selection, got {:?}",
      std::mem::discriminant(&other)
    ),
  }
}

#[tokio::test]
async fn round_robin_alternates_across_the_pool() {
  let state = two_client_pool().await;
  let first = pick_one(&state).await;
  let second = pick_one(&state).await;
  assert_ne!(
    first, second,
    "consecutive requests must go to different pool members"
  );
}

#[tokio::test]
async fn route_exists_does_not_rotate_the_pool() {
  let state = two_client_pool().await;
  // The cold-start probe runs before every pick when autoscaling is on. Doing
  // it with pick_proxy_client burned a rotation step, so each request consumed
  // two and a two-client pool always landed on the same member.
  let mut chosen = Vec::new();
  for _ in 0..4 {
    assert!(route_exists(&state, "/", Some("app.example.com"), None).await);
    chosen.push(pick_one(&state).await);
  }
  assert_eq!(chosen[0], chosen[2]);
  assert_eq!(chosen[1], chosen[3]);
  assert_ne!(chosen[0], chosen[1], "the pool must still alternate");
}

#[tokio::test]
async fn route_exists_is_false_without_a_serving_client() {
  let state = std::sync::Arc::new(crate::test_support::test_state());
  assert!(!route_exists(&state, "/", Some("app.example.com"), None).await);
}

#[tokio::test]
async fn a_selection_carries_both_names_a_client_can_be_shown_under() {
  // The capture, and everything else that shows a client to a person, reads
  // these off the selection rather than taking the clients lock again.
  let state = std::sync::Arc::new(crate::test_support::test_state());
  let mut c = crate::test_support::mock_client(Some("app.example.com"), None, None, None);
  c.sole_mut().service_name = Some("web".to_string());
  c.sole_mut().service_custom_name = Some("web (blue)".to_string());
  state.clients.write().await.insert("a".to_string(), c);

  match pick_proxy_client(&state, "/", Some("app.example.com"), None, None, None, None).await {
    PickOutcome::Selected(c) => {
      assert_eq!(c.service_name.as_deref(), Some("web"));
      assert_eq!(c.service_custom_name.as_deref(), Some("web (blue)"));
    }
    other => panic!(
      "expected a selection, got {:?}",
      std::mem::discriminant(&other)
    ),
  }
}

/// A routed pool as connection ids, which is what these assertions were
/// written against and still the readable thing to compare. The pool itself
/// is `(connection, service)` pairs now.
fn ids(pool: &[crate::routing::ServiceRef]) -> Vec<String> {
  pool.iter().map(|r| r.client.clone()).collect()
}

/// A pool built from connection ids, every one of them the connection's only
/// service.
fn refs(ids: &[&str]) -> Vec<crate::routing::ServiceRef> {
  ids
    .iter()
    .map(|id| crate::routing::ServiceRef {
      client: (*id).to_string(),
      index: 0,
    })
    .collect()
}

// --- Which service, not just which connection --------------------------------

/// The pool names the service that matched, not merely the connection.
///
/// This is what the whole `(connection, service)` change is for, and it is
/// the one thing a single-service fixture cannot show: with one service the
/// index is always zero and any implementation looks right. Two services on
/// one connection, bound to different hostnames, is the smallest case where
/// a wrong answer is a wrong answer.
///
/// What it would mean to get this wrong is worth stating, because it is not
/// an error anyone would see: the request would be dispatched to the other
/// service's backend, which is connected, healthy, and will answer.
#[test]
fn the_pool_names_which_service_of_the_connection_matched() {
  let mut handle = base_handle();
  handle.sole_mut().declared_hostname = Some("first.example".to_string());
  let mut extra = base_handle();
  extra.sole_mut().declared_hostname = Some("second.example".to_string());
  handle.services.extend(extra.services);
  let clients = pool_of(vec![("conn", handle)]);

  let (pool, _) =
    select_client_pool(&clients, "/", Some("second.example"), false, HEALTHY).expect("routed");
  assert_eq!(pool.len(), 1, "only one service binds that hostname");
  assert_eq!(pool[0].client, "conn");
  assert_eq!(
    pool[0].index, 1,
    "the second service is the one that matched"
  );

  let (pool, _) =
    select_client_pool(&clients, "/", Some("first.example"), false, HEALTHY).expect("routed");
  assert_eq!(pool[0].index, 0, "and the first for its own hostname");
}

/// A path bind is the service's too, so two services of one connection can
/// bind different prefixes and each gets its own traffic.
#[test]
fn two_services_of_one_connection_can_bind_different_paths() {
  let mut handle = base_handle();
  handle.sole_mut().declared_path = Some("/alpha".to_string());
  let mut extra = base_handle();
  extra.sole_mut().declared_path = Some("/beta".to_string());
  handle.services.extend(extra.services);
  let clients = pool_of(vec![("conn", handle)]);

  let (pool, key) = select_client_pool(&clients, "/beta/x", None, false, HEALTHY).expect("routed");
  assert_eq!(pool.len(), 1);
  assert_eq!(pool[0].index, 1);
  assert_eq!(key.1.as_deref(), Some("/beta"), "the winning bind is /beta");
}

/// Ejecting a failing service leaves the other one on the same connection
/// serving, which is the reason ejection had to stop being per connection.
#[test]
fn ejecting_one_service_does_not_take_its_neighbour_out_of_routing() {
  let now = std::time::Instant::now();
  let mut handle = base_handle();
  handle.sole_mut().declared_hostname = Some("alpha.example".to_string());
  let mut extra = base_handle();
  extra.sole_mut().declared_hostname = Some("beta.example".to_string());
  handle.services.extend(extra.services);
  handle.services[1].ejected_until = Some(now + Duration::from_secs(30));
  let clients = pool_of(vec![("conn", handle)]);

  // The healthy neighbour still routes.
  let (pool, _) =
    select_client_pool(&clients, "/", Some("alpha.example"), false, HEALTHY).expect("routed");
  assert_eq!(pool[0].index, 0);

  // And the ejected one still routes to itself rather than to its neighbour,
  // because ejection fails open when it is the route's only candidate.
  let (pool, _) =
    select_client_pool(&clients, "/", Some("beta.example"), false, HEALTHY).expect("routed");
  assert_eq!(
    pool[0].index, 1,
    "an ejected sole candidate is served, never silently swapped for another service"
  );
}

/// The chosen service's name travels with the dispatch.
///
/// A client carrying several services receives every request over one socket,
/// so the frame has to say which of its targets the request is for. Without
/// it the client would have to guess from the path, which is the server's job
/// and which it has already done: the pool matched a service, and this is
/// that answer being carried rather than thrown away.
#[test]
fn the_selected_service_is_the_one_named_for_the_client() {
  let mut handle = base_handle();
  handle.sole_mut().service_name = Some("api".to_string());
  handle.sole_mut().declared_path = Some("/api".to_string());
  let mut extra = base_handle();
  extra.sole_mut().service_name = Some("web".to_string());
  extra.sole_mut().declared_path = Some("/web".to_string());
  handle.services.extend(extra.services);
  let clients = pool_of(vec![("conn", handle)]);

  let (pool, _) = select_client_pool(&clients, "/web/x", None, false, HEALTHY).expect("routed");
  let chosen = &pool[0];
  assert_eq!(
    chosen.get(&clients).and_then(|s| s.service_name.clone()),
    Some("web".to_string()),
    "the name the dispatch carries is the matched service's, not the connection's first"
  );
}

/// The traversal gate asks each service, not the connection.
///
/// This is the *entire* gate for a path containing `.` or `..`, so a false
/// "ungated" here serves `/./admin` with no credential on a route whose
/// `/admin` answers 401. Reading the gate off one service and the hostname
/// off another produces exactly that: with two services on one connection,
/// the gated one's declaration was paired with its own hostname only by
/// accident of being first.
#[tokio::test]
async fn the_traversal_gate_pairs_each_gate_with_its_own_hostname() {
  let state = std::sync::Arc::new(crate::test_support::test_state());
  let mut handle = base_handle();
  handle.sole_mut().declared_hostname = Some("first.example".to_string());
  handle.sole_mut().visitor_auth = Some("u:p".to_string());
  let mut second = base_handle();
  second.sole_mut().declared_hostname = Some("second.example".to_string());
  second.sole_mut().visitor_auth = Some("u:p".to_string());
  handle.services.extend(second.services);
  state
    .clients
    .write()
    .await
    .insert("conn".to_string(), handle);

  assert!(
    host_has_visitor_auth(&state, Some("first.example")).await,
    "the first service's gate covers its own hostname"
  );
  assert!(
    host_has_visitor_auth(&state, Some("second.example")).await,
    "and so does the second's, which is the reading that was wrong"
  );
  assert!(
    !host_has_visitor_auth(&state, Some("unrelated.example")).await,
    "a hostname nothing serves is still ungated"
  );
}

#[test]
pub(crate) fn test_find_affinity_match() {
  let mut clients = HashMap::new();
  let mut a = mock_client(None, None, None, None);
  a.reported_instance_id = Some("instance-a".to_string());
  let b = mock_client(None, None, None, None);
  clients.insert("conn-a".to_string(), a);
  clients.insert("conn-b".to_string(), b);
  let pool = refs(&["conn-a", "conn-b"]);

  // Matches by instance ID (survives reconnects) and by connection ID.
  assert_eq!(
    find_affinity_match(&pool, &clients, "instance-a").map(|r| r.client),
    Some("conn-a".to_string())
  );
  assert_eq!(
    find_affinity_match(&pool, &clients, "conn-b").map(|r| r.client),
    Some("conn-b".to_string())
  );
  // Unknown affinity falls back to rotation (None).
  assert_eq!(find_affinity_match(&pool, &clients, "gone"), None);
  // A client that left the pool no longer matches.
  assert_eq!(
    find_affinity_match(&refs(&["conn-b"]), &clients, "instance-a"),
    None
  );
}

#[test]
pub(crate) fn test_apply_lb_strategy_primary_standby() {
  let mut clients = HashMap::new();
  let primary = mock_client(None, None, None, None);
  let mut standby = mock_client(None, None, None, None);
  standby.sole_mut().priority = 1;
  clients.insert("primary".to_string(), primary);
  clients.insert("standby".to_string(), standby);

  let pool = refs(&["primary", "standby"]);
  // Round-robin keeps the whole pool.
  assert_eq!(
    apply_lb_strategy(pool.clone(), &clients, LbStrategy::RoundRobin).len(),
    2
  );
  // Primary-standby narrows to the lowest priority tier.
  assert_eq!(
    ids(&apply_lb_strategy(
      pool,
      &clients,
      LbStrategy::PrimaryStandby
    )),
    vec!["primary".to_string()]
  );
  // Once the primary is out of the pool, the standby takes over.
  assert_eq!(
    ids(&apply_lb_strategy(
      refs(&["standby"]),
      &clients,
      LbStrategy::PrimaryStandby
    )),
    vec!["standby".to_string()]
  );
}

#[test]
pub(crate) fn test_select_client_pool_excludes_unhealthy() {
  let mut clients = HashMap::new();
  let mut stale = mock_client(None, None, None, None);
  // Last heartbeat far in the past -> down
  stale.last_ping_at = Some(Instant::now() - Duration::from_secs(120));
  clients.insert("stale".to_string(), stale);

  // Only client is unhealthy -> nothing selectable
  assert!(select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).is_none());

  // A fresh client joins -> traffic goes only to it
  let mut fresh = mock_client(None, None, None, None);
  fresh.last_ping_at = Some(Instant::now());
  clients.insert("fresh".to_string(), fresh);
  let (pool, _) = select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
  assert_eq!(ids(&pool), vec!["fresh".to_string()]);

  // The stale client recovers with a new ping -> back in the pool
  clients.get_mut("stale").unwrap().last_ping_at = Some(Instant::now());
  let (pool, _) = select_client_pool(&clients, "/", None, false, Duration::from_secs(15)).unwrap();
  assert_eq!(pool.len(), 2);
}

#[test]
pub(crate) fn test_select_client_pool_hostname_routing() {
  let mut clients = HashMap::new();
  clients.insert(
    "a".to_string(),
    mock_client(Some("a.example.com"), None, None, None),
  );
  clients.insert(
    "b".to_string(),
    mock_client(Some("b.example.com"), None, None, None),
  );
  clients.insert("unbound".to_string(), mock_client(None, None, None, None));

  // Host matches a.example.com → only client "a"
  let (pool, key) =
    select_client_pool(&clients, "/", Some("a.example.com"), false, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["a".to_string()]);
  assert_eq!(key, (Some("a.example.com".to_string()), None));

  // Unknown host → falls back to unbound client
  let (pool, key) =
    select_client_pool(&clients, "/", Some("c.example.com"), false, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["unbound".to_string()]);
  assert_eq!(key, (None, None));

  // Strict mode: unknown host → no client at all
  assert!(select_client_pool(&clients, "/", Some("c.example.com"), true, TEST_THRESHOLD).is_none());
  // Strict mode: matching host still works
  let (pool, _) =
    select_client_pool(&clients, "/", Some("b.example.com"), true, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["b".to_string()]);
  // Strict mode: no Host header → no client
  assert!(select_client_pool(&clients, "/", None, true, TEST_THRESHOLD).is_none());
}

#[test]
pub(crate) fn test_select_client_pool_hostname_and_path_combined() {
  let mut clients = HashMap::new();
  clients.insert(
    "host-api".to_string(),
    mock_client(Some("a.example.com"), Some("/api"), None, None),
  );
  clients.insert(
    "host-root".to_string(),
    mock_client(Some("a.example.com"), None, None, None),
  );

  // Path under /api on the bound host → path-bound client wins
  let (pool, key) = select_client_pool(
    &clients,
    "/api/users",
    Some("a.example.com"),
    false,
    TEST_THRESHOLD,
  )
  .unwrap();
  assert_eq!(ids(&pool), vec!["host-api".to_string()]);
  assert_eq!(
    key,
    (Some("a.example.com".to_string()), Some("/api".to_string()))
  );

  // Other paths on the bound host → unbound-path client
  let (pool, _) = select_client_pool(
    &clients,
    "/other",
    Some("a.example.com"),
    false,
    TEST_THRESHOLD,
  )
  .unwrap();
  assert_eq!(ids(&pool), vec!["host-root".to_string()]);
}

#[test]
pub(crate) fn test_select_client_pool_override_wins() {
  let mut clients = HashMap::new();
  // Client reported no hostname, dashboard overruled it to a.example.com
  clients.insert(
    "overruled".to_string(),
    mock_client(None, None, Some("a.example.com"), None),
  );

  let (pool, _) =
    select_client_pool(&clients, "/", Some("a.example.com"), true, TEST_THRESHOLD).unwrap();
  assert_eq!(ids(&pool), vec!["overruled".to_string()]);

  // With the override active, the client is no longer an unbound fallback
  assert!(
    select_client_pool(&clients, "/", Some("x.example.com"), false, TEST_THRESHOLD).is_none()
  );
}
