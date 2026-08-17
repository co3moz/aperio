//! Per-route rate limits: which rule a request matches, how burst and rps default
//! against each other, that a method filter scopes a rule and gives it its own
//! bucket, and that an unusable rule is dropped rather than taking the section
//! down.

use super::*;

/// Serializes tests that touch the process-global config document / default
/// `aperio-server.yaml`. Loads `yaml` as the default document, runs `f`.
fn with_config(yaml: &str, f: impl FnOnce()) {
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
    }
  }
  let _cleanup = Cleanup;
  std::fs::write("aperio-server.yaml", yaml).unwrap();
  crate::config_file::reload().unwrap();
  f();
}

fn rules_from(yaml: &str) -> RouteLimits {
  let raw: Vec<RateLimitRuleRaw> = serde_yaml::from_str(yaml).unwrap();
  RouteLimits {
    rules: compile(raw),
  }
}

#[test]
fn matches_first_rule_by_host_and_path() {
  let limits = rules_from(
    "- hostname: app.example.com\n  path: /login\n  rps: 5\n- path: /export\n  rps: 1\n",
  );
  // Host + path specific rule.
  let r = limits
    .matched(Some("app.example.com"), "/login", None)
    .unwrap();
  assert_eq!(r.rps, 5.0);
  assert_eq!(r.burst, 5.0);
  assert_eq!(r.key, "app.example.com|/login|*");
  // Path-only rule matches any host on a segment boundary.
  assert!(
    limits
      .matched(Some("other.com"), "/export/data", None)
      .is_some()
  );
  // A host-specific rule cannot match when the request carries no host.
  assert!(limits.matched(None, "/login", None).is_none());
  // No rule for an unrelated path.
  assert!(
    limits
      .matched(Some("app.example.com"), "/other", None)
      .is_none()
  );
  // Host-specific rule does not fire for a different host.
  assert!(limits.matched(Some("nope.com"), "/login", None).is_none());
  assert!(!limits.is_empty());
}

#[test]
fn any_host_any_path_rule_matches_everything() {
  // Neither hostname nor path set → matches any request.
  let limits = rules_from("- rps: 2\n");
  let r = limits.matched(None, "/whatever", None).unwrap();
  assert_eq!(r.key, "*|*|*");
  assert!(limits.matched(Some("x.com"), "/", None).is_some());
}

#[test]
fn burst_defaults_to_rps_and_invalid_rules_dropped() {
  let limits = rules_from("- path: /a\n  rps: 3\n- path: /b\n  rps: 0\n");
  assert_eq!(limits.matched(None, "/a", None).unwrap().burst, 3.0);
  // rps 0 rule is dropped.
  assert!(limits.matched(None, "/b", None).is_none());
}

#[test]
fn nan_rps_is_dropped_and_explicit_burst_kept() {
  // NaN rps is rejected; an explicit positive burst is honored.
  let limits = rules_from("- path: /nan\n  rps: .nan\n- path: /b\n  rps: 4\n  burst: 9\n");
  assert!(limits.matched(None, "/nan", None).is_none());
  assert_eq!(limits.matched(None, "/b", None).unwrap().burst, 9.0);
}

#[test]
fn sub_one_burst_is_floored_to_one() {
  // A sub-1.0 explicit burst would 429 every request, so it floors to 1.
  let limits = rules_from("- path: /c\n  rps: 10\n  burst: 0.25\n");
  assert_eq!(limits.matched(None, "/c", None).unwrap().burst, 1.0);
  // A zero burst falls back to rps.
  let z = rules_from("- path: /d\n  rps: 7\n  burst: 0\n");
  assert_eq!(z.matched(None, "/d", None).unwrap().burst, 7.0);
}

#[test]
fn from_config_file_absent_section_is_default() {
  with_config("other: 1\n", || {
    assert!(from_config_file().is_empty());
  });
}

#[test]
fn from_config_file_parses_and_compiles() {
  with_config(
    "rate_limits:\n  - hostname: app.example.com\n    path: /login\n    rps: 5\n",
    || {
      let limits = from_config_file();
      assert_eq!(
        limits
          .matched(Some("app.example.com"), "/login", None)
          .unwrap()
          .rps,
        5.0
      );
    },
  );
}

#[test]
fn from_config_file_malformed_section_disables_feature() {
  with_config("rate_limits: nope\n", || {
    assert!(from_config_file().is_empty());
  });
}

// --- method filter (planned_features #26) -----------------------------------

#[test]
fn a_method_filter_scopes_the_rule_to_those_verbs() {
  let limits = super::RouteLimits {
    rules: super::compile(
      serde_yaml::from_str(
        r#"
- path: /api
  rps: 5
  methods: [post, PUT]
"#,
      )
      .unwrap(),
    ),
  };
  assert!(
    limits.matched(None, "/api/x", Some("POST")).is_some(),
    "a listed method is limited"
  );
  assert!(
    limits.matched(None, "/api/x", Some("put")).is_some(),
    "the comparison ignores case in both directions"
  );
  assert!(
    limits.matched(None, "/api/x", Some("GET")).is_none(),
    "an unlisted method is not limited by this rule"
  );
  assert!(
    limits.matched(None, "/api/x", None).is_some(),
    "asking about the route rather than a request ignores the filter"
  );
}

#[test]
fn rules_differing_only_by_method_get_separate_buckets() {
  let rules = super::compile(
    serde_yaml::from_str(
      r#"
- path: /api
  rps: 1
  methods: [POST]
- path: /api
  rps: 100
"#,
    )
    .unwrap(),
  );
  assert_ne!(
    rules[0].key, rules[1].key,
    "a write limit must not drain the read limit's bucket"
  );
}

#[test]
fn an_empty_method_list_means_every_method() {
  let rules =
    super::compile(serde_yaml::from_str("- path: /api\n  rps: 5\n  methods: []\n").unwrap());
  assert!(rules[0].methods.is_none());
}
