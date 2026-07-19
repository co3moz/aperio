use super::*;

fn pages_with(rules: Vec<CompiledRule>) -> ErrorPages {
  ErrorPages { rules }
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
