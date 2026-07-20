use super::*;

/// Serializes tests that touch the process-global config document / default
/// `aperio-server.yaml`. Loads `yaml` as the default document, runs `f`.
fn with_config(yaml: &str, f: impl FnOnce()) {
  let lock = std::env::temp_dir().join("aperio-cfgfile-test.lock");
  let start = std::time::Instant::now();
  loop {
    match std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&lock)
    {
      Ok(_) => break,
      Err(_) => {
        if let Ok(md) = std::fs::metadata(&lock)
          && md
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|e| e.as_secs() > 30)
        {
          let _ = std::fs::remove_file(&lock);
        }
        assert!(
          start.elapsed().as_secs() < 120,
          "config-file test lock timeout"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
      }
    }
  }
  struct Cleanup(std::path::PathBuf);
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
      let _ = std::fs::remove_file(&self.0);
    }
  }
  let _cleanup = Cleanup(lock);
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
  let r = limits.matched(Some("app.example.com"), "/login").unwrap();
  assert_eq!(r.rps, 5.0);
  assert_eq!(r.burst, 5.0);
  assert_eq!(r.key, "app.example.com|/login");
  // Path-only rule matches any host on a segment boundary.
  assert!(limits.matched(Some("other.com"), "/export/data").is_some());
  // A host-specific rule cannot match when the request carries no host.
  assert!(limits.matched(None, "/login").is_none());
  // No rule for an unrelated path.
  assert!(limits.matched(Some("app.example.com"), "/other").is_none());
  // Host-specific rule does not fire for a different host.
  assert!(limits.matched(Some("nope.com"), "/login").is_none());
  assert!(!limits.is_empty());
}

#[test]
fn any_host_any_path_rule_matches_everything() {
  // Neither hostname nor path set → matches any request.
  let limits = rules_from("- rps: 2\n");
  let r = limits.matched(None, "/whatever").unwrap();
  assert_eq!(r.key, "*|*");
  assert!(limits.matched(Some("x.com"), "/").is_some());
}

#[test]
fn burst_defaults_to_rps_and_invalid_rules_dropped() {
  let limits = rules_from("- path: /a\n  rps: 3\n- path: /b\n  rps: 0\n");
  assert_eq!(limits.matched(None, "/a").unwrap().burst, 3.0);
  // rps 0 rule is dropped.
  assert!(limits.matched(None, "/b").is_none());
}

#[test]
fn nan_rps_is_dropped_and_explicit_burst_kept() {
  // NaN rps is rejected; an explicit positive burst is honored.
  let limits = rules_from("- path: /nan\n  rps: .nan\n- path: /b\n  rps: 4\n  burst: 9\n");
  assert!(limits.matched(None, "/nan").is_none());
  assert_eq!(limits.matched(None, "/b").unwrap().burst, 9.0);
}

#[test]
fn sub_one_burst_is_floored_to_one() {
  // A sub-1.0 explicit burst would 429 every request, so it floors to 1.
  let limits = rules_from("- path: /c\n  rps: 10\n  burst: 0.25\n");
  assert_eq!(limits.matched(None, "/c").unwrap().burst, 1.0);
  // A zero burst falls back to rps.
  let z = rules_from("- path: /d\n  rps: 7\n  burst: 0\n");
  assert_eq!(z.matched(None, "/d").unwrap().burst, 7.0);
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
          .matched(Some("app.example.com"), "/login")
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
