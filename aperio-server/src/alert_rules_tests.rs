//! What a rule has to get right: the bound it fires on, the sustain window in
//! both directions, and every malformed rule being refused by name rather
//! than sitting quietly armed and never firing.

use super::*;

fn compile(yaml: &str) -> AlertRules {
  let raw: Vec<RuleRaw> = serde_yaml::from_str(yaml).unwrap();
  AlertRules::compile(raw).unwrap()
}

fn err(yaml: &str) -> String {
  let raw: Vec<RuleRaw> = serde_yaml::from_str(yaml).unwrap();
  match AlertRules::compile(raw) {
    Ok(_) => panic!("expected the section to be refused"),
    Err(e) => e,
  }
}

#[test]
fn above_and_below_fire_on_their_own_side_of_the_bound() {
  let rules = compile(
    "- name: mem\n  metric: rss_bytes\n  above: 100\n- name: quiet\n  metric: connected_clients\n  below: 2\n",
  );
  let (mem, quiet) = (&rules.rules()[0], &rules.rules()[1]);
  assert!(mem.breached(101.0));
  assert!(!mem.breached(100.0), "the bound itself is not a breach");
  assert!(!mem.breached(99.0));
  assert!(quiet.breached(1.0));
  assert!(!quiet.breached(2.0));
  assert!(!quiet.breached(3.0));
}

#[test]
fn a_rule_without_a_sustain_window_fires_on_the_first_observation() {
  let rules = compile("- name: mem\n  metric: rss_bytes\n  above: 100\n");
  let rule = &rules.rules()[0];
  let mut tracker = RuleTracker::default();
  let now = Instant::now();
  assert!(matches!(
    tracker.observe(rule, 150.0, now),
    Some(Transition::Fired)
  ));
  // Still breached: no second alert for the same episode.
  assert!(tracker.observe(rule, 200.0, now).is_none());
  assert!(matches!(
    tracker.observe(rule, 50.0, now),
    Some(Transition::Resolved)
  ));
}

#[test]
fn the_sustain_window_applies_to_firing_and_to_resolving() {
  let rules = compile("- name: mem\n  metric: rss_bytes\n  above: 100\n  for: 60\n");
  let rule = &rules.rules()[0];
  let mut tracker = RuleTracker::default();
  let t0 = Instant::now();

  // Breached, but not for long enough yet.
  assert!(tracker.observe(rule, 150.0, t0).is_none());
  assert!(
    tracker
      .observe(rule, 150.0, t0 + Duration::from_secs(30))
      .is_none()
  );
  assert!(matches!(
    tracker.observe(rule, 150.0, t0 + Duration::from_secs(60)),
    Some(Transition::Fired)
  ));

  // Clearing takes the same window, so a value sitting on the threshold does
  // not alert and resolve on alternating ticks.
  assert!(
    tracker
      .observe(rule, 50.0, t0 + Duration::from_secs(70))
      .is_none()
  );
  assert!(matches!(
    tracker.observe(rule, 50.0, t0 + Duration::from_secs(130)),
    Some(Transition::Resolved)
  ));
}

#[test]
fn a_spike_that_does_not_last_never_fires() {
  let rules = compile("- name: mem\n  metric: rss_bytes\n  above: 100\n  for: 60\n");
  let rule = &rules.rules()[0];
  let mut tracker = RuleTracker::default();
  let t0 = Instant::now();
  assert!(tracker.observe(rule, 150.0, t0).is_none());
  // Back under before the window elapses: the clock restarts, so a later
  // breach has to serve its own full window.
  assert!(
    tracker
      .observe(rule, 10.0, t0 + Duration::from_secs(30))
      .is_none()
  );
  assert!(
    tracker
      .observe(rule, 150.0, t0 + Duration::from_secs(40))
      .is_none()
  );
  assert!(
    tracker
      .observe(rule, 150.0, t0 + Duration::from_secs(80))
      .is_none(),
    "40s into the second breach is not yet 60"
  );
  assert!(matches!(
    tracker.observe(rule, 150.0, t0 + Duration::from_secs(101)),
    Some(Transition::Fired)
  ));
}

#[test]
fn metric_names_accept_their_aliases_and_reject_the_unknown() {
  let rules = compile(
    "- name: a\n  metric: CLIENTS\n  below: 1\n- name: b\n  metric: disk_bytes\n  above: 1\n",
  );
  assert_eq!(rules.rules()[0].metric, Metric::ConnectedClients);
  assert_eq!(rules.rules()[1].metric, Metric::StoreBytes);
  assert!(err("- name: a\n  metric: temperature\n  above: 1\n").contains("is not a metric"));
}

#[test]
fn malformed_rules_are_refused_by_name() {
  assert!(err("- name: a\n  metric: rss_bytes\n").contains("needs `above` or `below`"));
  assert!(err("- name: a\n  metric: rss_bytes\n  above: 1\n  below: 2\n").contains("not both"));
  assert!(
    err(
      "- name: dup\n  metric: rss_bytes\n  above: 1\n- name: dup\n  metric: rss_bytes\n  above: 2\n"
    )
    .contains("already has this name")
  );
  assert!(err("- name: \"\"\n  metric: rss_bytes\n  above: 1\n").contains("`name` is required"));
}

#[test]
fn a_metric_that_cannot_be_read_here_says_so() {
  // The operator is told at startup rather than wondering why a rule is
  // quiet; on Linux every metric is readable.
  assert!(Metric::ConnectedClients.readable_here());
  assert_eq!(Metric::RssBytes.readable_here(), cfg!(target_os = "linux"));
}
