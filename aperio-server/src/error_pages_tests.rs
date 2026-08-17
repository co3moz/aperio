//! The per-hostname error pages: lookup, and every way a section can be wrong
//! without costing the operator the feature or the start.

use super::*;

fn pages_with(rules: Vec<CompiledRule>) -> ErrorPages {
  ErrorPages { rules }
}

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

/// Writes an HTML file into a fresh temp path and returns it.
fn tmp_html(contents: &str) -> std::path::PathBuf {
  let p =
    crate::test_support::test_temp_root().join(format!("errpage-{}.html", uuid::Uuid::new_v4()));
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
