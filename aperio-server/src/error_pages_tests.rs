use super::*;

fn pages_with(rules: Vec<CompiledRule>) -> ErrorPages {
  ErrorPages { rules }
}

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

/// Writes an HTML file into a fresh temp path and returns it.
fn tmp_html(contents: &str) -> std::path::PathBuf {
  let p = std::env::temp_dir().join(format!("aperio-errpage-{}.html", uuid::Uuid::new_v4()));
  std::fs::write(&p, contents).unwrap();
  p
}

#[test]
fn test_error_pages_lookup() {
  let pages = pages_with(vec![CompiledRule {
    hostname: "app.example.com".to_string(),
    html_504: Some("<h1>app 504</h1>".to_string()),
    html_503: None,
  }]);

  // Exact hostname match, case-insensitive on the request side.
  assert_eq!(
    pages.page_504(Some("app.example.com")),
    Some("<h1>app 504</h1>")
  );
  assert_eq!(
    pages.page_504(Some("APP.Example.COM")),
    Some("<h1>app 504</h1>")
  );

  // Unknown hostnames and missing hosts fall back to the global page.
  assert_eq!(pages.page_504(Some("other.example.com")), None);
  assert_eq!(pages.page_504(None), None);

  // A rule without a 503 page keeps the global maintenance page.
  assert_eq!(pages.page_503(Some("app.example.com")), None);
}

#[test]
fn test_error_pages_default_is_empty() {
  let pages = ErrorPages::default();
  assert_eq!(pages.page_504(Some("app.example.com")), None);
  assert_eq!(pages.page_503(Some("app.example.com")), None);
}

#[test]
fn from_config_file_absent_section_is_default() {
  with_config("other: 1\n", || {
    let pages = from_config_file();
    assert_eq!(pages.page_504(Some("app.example.com")), None);
  });
}

#[test]
fn from_config_file_loads_pages_and_falls_back_on_unreadable() {
  let p504 = tmp_html("<h1>custom 504</h1>");
  // A 503 path that does not exist keeps the global maintenance page.
  let missing = std::env::temp_dir().join("aperio-errpage-does-not-exist.html");
  let yaml = format!(
    "error_pages:\n  - hostname: APP.Example.com\n    504_page: {}\n    503_page: {}\n",
    p504.display(),
    missing.display()
  );
  with_config(&yaml, || {
    let pages = from_config_file();
    // Hostname is lowercased on load; matched case-insensitively.
    assert_eq!(
      pages.page_504(Some("app.example.com")),
      Some("<h1>custom 504</h1>")
    );
    // The unreadable 503 page falls back to the global page (None here).
    assert_eq!(pages.page_503(Some("app.example.com")), None);
  });
  let _ = std::fs::remove_file(&p504);
}

#[test]
fn from_config_file_skips_entries_without_a_hostname_or_pages() {
  let p = tmp_html("<h1>x</h1>");
  // Entry #1 has no hostname (ignored); entry #2 has a hostname but both page
  // paths are blank (skipped); entry #3 is valid.
  let yaml = format!(
    "error_pages:\n  - hostname: \"  \"\n    504_page: {p}\n  - hostname: b.example.com\n    504_page: \"  \"\n    503_page: \"\"\n  - hostname: c.example.com\n    503_page: {p}\n",
    p = p.display()
  );
  with_config(&yaml, || {
    let pages = from_config_file();
    // Only the third entry compiled.
    assert_eq!(pages.page_503(Some("c.example.com")), Some("<h1>x</h1>"));
    assert_eq!(pages.page_504(Some("b.example.com")), None);
  });
  let _ = std::fs::remove_file(&p);
}

#[test]
fn from_config_file_malformed_section_is_default() {
  with_config("error_pages: not-a-list\n", || {
    let pages = from_config_file();
    assert_eq!(pages.page_504(Some("a.example.com")), None);
  });
}
