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

fn rules_from(yaml: &str) -> WafRules {
  let raw: Vec<WafRuleRaw> = serde_yaml::from_str(yaml).unwrap();
  WafRules {
    rules: compile(raw),
  }
}

#[test]
fn deny_matches_path_method_and_header() {
  let waf = rules_from(
    "- path: \"^/admin\"\n  methods: [POST]\n- header:\n    name: user-agent\n    regex: \"(?i)sqlmap\"\n",
  );
  assert!(!waf.is_empty());
  let mut h = HeaderMap::new();
  // Path+method deny.
  assert!(waf.deny_reason("POST", "/admin/x", &h).is_some());
  // Wrong method → no match on the first rule.
  assert!(waf.deny_reason("GET", "/admin/x", &h).is_none());
  // Path present but no header on this request → header rule does not match.
  assert!(waf.deny_reason("GET", "/", &h).is_none());
  // Header rule with a non-matching value.
  h.insert("user-agent", "curl/8".parse().unwrap());
  assert!(waf.deny_reason("GET", "/", &h).is_none());
  // Header rule.
  h.insert("user-agent", "sqlMAP/1.0".parse().unwrap());
  let reason = waf.deny_reason("GET", "/", &h).unwrap();
  assert!(reason.contains("header=user-agent"));
}

#[test]
fn non_utf8_header_value_does_not_match() {
  // A header whose bytes are not valid UTF-8 cannot satisfy a regex condition.
  let waf = rules_from("- header:\n    name: x-test\n    regex: \".\"\n");
  let mut h = HeaderMap::new();
  h.insert(
    "x-test",
    axum::http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
  );
  assert!(waf.deny_reason("GET", "/", &h).is_none());
}

#[test]
fn body_rule_only_trips_over_limit() {
  let waf = rules_from("- path: \"^/upload\"\n  max_body: 100\n");
  let h = HeaderMap::new();
  // A body rule is not a deny rule.
  assert!(waf.deny_reason("POST", "/upload", &h).is_none());
  // Under the limit is fine; over trips 413.
  assert!(waf.body_reason("POST", "/upload", &h, 50).is_none());
  let reason = waf.body_reason("POST", "/upload", &h, 500).unwrap();
  assert!(reason.contains("max_body=100"));
  // Different path is unaffected.
  assert!(waf.body_reason("POST", "/other", &h, 500).is_none());
}

#[test]
fn methods_are_upcased_and_described() {
  // Lowercase methods in config are normalized to uppercase for matching.
  let waf = rules_from("- methods: [post, delete]\n");
  let h = HeaderMap::new();
  assert!(waf.deny_reason("POST", "/x", &h).is_some());
  assert!(waf.deny_reason("DELETE", "/x", &h).is_some());
  assert!(waf.deny_reason("GET", "/x", &h).is_none());
}

#[test]
fn invalid_regex_rule_is_dropped() {
  let (rules, dropped) =
    compile_reported(serde_yaml::from_str("- path: \"(unclosed\"\n- path: \"^/ok\"\n").unwrap());
  assert_eq!(rules.len(), 1);
  assert_eq!(dropped, 1);
}

#[test]
fn empty_and_bad_header_rules_are_dropped() {
  // An entry with no conditions at all is ignored.
  let (rules, dropped) = compile_reported(serde_yaml::from_str("- {}\n").unwrap());
  assert_eq!(rules.len(), 0);
  assert_eq!(dropped, 1);
  // An invalid header regex drops the whole rule.
  let (rules, dropped) = compile_reported(
    serde_yaml::from_str("- header:\n    name: x\n    regex: \"(bad\"\n").unwrap(),
  );
  assert_eq!(rules.len(), 0);
  assert_eq!(dropped, 1);
}

#[test]
fn count_dropped_reports_the_number_of_bad_rules() {
  let raw: Vec<WafRuleRaw> =
    serde_yaml::from_str("- path: \"(bad\"\n- {}\n- path: \"^/ok\"\n").unwrap();
  assert_eq!(count_dropped(raw), 2);
}

#[test]
fn from_config_file_absent_section_is_default() {
  with_config("other: 1\n", || {
    assert!(from_config_file().is_empty());
  });
}

#[test]
fn from_config_file_parses_and_compiles() {
  with_config("waf:\n  - path: \"^/\\\\.git\"\n", || {
    let waf = from_config_file();
    let h = HeaderMap::new();
    assert!(waf.deny_reason("GET", "/.git/config", &h).is_some());
  });
}

#[test]
fn from_config_file_malformed_section_disables_feature() {
  with_config("waf: nope\n", || {
    assert!(from_config_file().is_empty());
  });
}
