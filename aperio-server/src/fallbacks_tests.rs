use super::*;

/// Serializes tests that touch the process-global config document / default
/// `aperio-server.yaml` so the parallel test runner can't let them clobber one
/// another. Loads `yaml` as the default document, runs `f`, and cleans up.
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

fn rules(yaml: &str) -> Fallbacks {
  Fallbacks {
    rules: compile(serde_yaml::from_str(yaml).unwrap()),
  }
}

#[test]
fn exact_match_wins_over_catch_all() {
  let f = rules(
    "- hostname: app.example.com\n  url: https://status.example.com\n- hostname: \"*\"\n  url: https://www.example.com\n",
  );
  assert_eq!(
    f.matched(Some("app.example.com")).unwrap().url,
    "https://status.example.com"
  );
  // Any other host falls to the catch-all.
  assert_eq!(
    f.matched(Some("other.example.com")).unwrap().url,
    "https://www.example.com"
  );
  // A None host with a catch-all present still resolves to the catch-all.
  assert_eq!(f.matched(None).unwrap().url, "https://www.example.com");
}

#[test]
fn no_catch_all_means_no_match() {
  let f = rules("- hostname: app.example.com\n  url: https://s.example.com\n");
  assert!(f.matched(Some("nope.com")).is_none());
  assert!(f.matched(None).is_none());
  // Invalid (non-http) URLs are dropped at compile.
  let bad = rules("- hostname: a.com\n  url: ftp://x\n");
  assert!(bad.is_empty());
}

#[test]
fn invalid_hostname_entry_is_dropped() {
  // A blatantly invalid hostname is rejected by normalize_hostname_bind, so the
  // entry is skipped while the valid catch-all survives.
  let f = rules(
    "- hostname: \"bad host!!\"\n  url: https://x.example.com\n- hostname: \"*\"\n  url: https://ok.example.com\n",
  );
  assert_eq!(
    f.matched(Some("bad host!!")).unwrap().url,
    "https://ok.example.com"
  );
  // Whitespace-trimmed URL still compiles.
  let trimmed = rules("- hostname: a.com\n  url: \"  https://a.example.com  \"\n");
  assert_eq!(
    trimmed.matched(Some("a.com")).unwrap().url,
    "https://a.example.com"
  );
}

#[test]
fn permanent_and_flags_round_trip() {
  let f = rules(
    "- hostname: a.com\n  url: https://a.example.com\n  permanent: true\n  preserve_path: true\n",
  );
  let r = f.matched(Some("a.com")).unwrap();
  assert!(r.permanent);
  assert!(r.preserve_path);
  assert!(!f.is_empty());
}

#[test]
fn redirect_location_preserves_path_when_asked() {
  let rule = FallbackRule {
    hostname: "*".into(),
    url: "https://origin.example.com/".into(),
    permanent: false,
    preserve_path: true,
  };
  assert_eq!(
    redirect_location(&rule, "/a/b", Some("q=1")),
    "https://origin.example.com/a/b?q=1"
  );
  // No query preserves just the path.
  assert_eq!(
    redirect_location(&rule, "/a/b", None),
    "https://origin.example.com/a/b"
  );
  let plain = FallbackRule {
    preserve_path: false,
    ..rule
  };
  assert_eq!(redirect_location(&plain, "/a/b", Some("q=1")), plain.url);
}

#[test]
fn from_config_file_absent_section_is_default() {
  with_config("other: 1\n", || {
    // No `fallbacks:` key → the feature is off.
    assert!(from_config_file().is_empty());
  });
}

#[test]
fn from_config_file_parses_and_compiles() {
  with_config(
    "fallbacks:\n  - hostname: app.example.com\n    url: https://status.example.com\n",
    || {
      let f = from_config_file();
      assert_eq!(
        f.matched(Some("app.example.com")).unwrap().url,
        "https://status.example.com"
      );
    },
  );
}

#[test]
fn from_config_file_malformed_section_disables_feature() {
  // A scalar where a sequence is expected fails to deserialize → default (off).
  with_config("fallbacks: not-a-list\n", || {
    assert!(from_config_file().is_empty());
  });
}
