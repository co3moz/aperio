//! The `sites` setting as an operator writes it: which site a request falls
//! under, the one precedence rule (most specific wins) and the three settings
//! that add up instead, the answers a site gives without a client, and every
//! site that cannot work being refused by name rather than stored.

use super::*;

fn sites(yaml: &str) -> Sites {
  let raw: Vec<aperio_config::Site> = serde_yaml::from_str(yaml).unwrap();
  Sites::compile(&raw).unwrap()
}

fn refused(yaml: &str) -> String {
  let raw: Vec<aperio_config::Site> = serde_yaml::from_str(yaml).unwrap();
  match Sites::compile(&raw) {
    Ok(_) => panic!("expected the sites to be refused"),
    Err(e) => e,
  }
}

fn h(list: &[(&str, &str)]) -> Vec<(String, String)> {
  list
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

// --- matching ----------------------------------------------------------------

#[test]
fn a_hostname_matches_exactly_by_pattern_or_everywhere() {
  let s = sites(
    "- hostname: app.example.com\n  timeout: 10s\n\
     - hostname: \"*.example.com\"\n  timeout: 20s\n\
     - hostname: \"*-pi.example.com\"\n  timeout: 30s\n\
     - hostname: \"*\"\n  timeout: 40s\n",
  );
  assert_eq!(s.timeout_for(Some("app.example.com"), "/"), Some(10));
  assert_eq!(
    s.timeout_for(Some("APP.Example.com"), "/"),
    Some(10),
    "case does not matter"
  );
  assert_eq!(s.timeout_for(Some("kitchen-pi.example.com"), "/"), Some(30));
  assert_eq!(s.timeout_for(Some("other.example.com"), "/"), Some(20));
  assert_eq!(s.timeout_for(Some("elsewhere.test"), "/"), Some(40));
  assert_eq!(
    s.timeout_for(None, "/"),
    Some(40),
    "`*` covers a request with no Host"
  );
}

#[test]
fn a_path_is_a_prefix_on_a_segment_boundary() {
  let s = sites("- hostname: \"*\"\n  path: /api\n  timeout: 5s\n");
  assert_eq!(s.timeout_for(Some("a.test"), "/api"), Some(5));
  assert_eq!(s.timeout_for(Some("a.test"), "/api/v1"), Some(5));
  assert_eq!(s.timeout_for(Some("a.test"), "/apix"), None);
  assert_eq!(s.timeout_for(Some("a.test"), "/"), None);
}

#[test]
fn each_setting_comes_from_the_most_specific_site_that_writes_it() {
  // Written broadest first on purpose: the order a list is written in only
  // breaks ties, it does not decide which site is narrower.
  let s = sites(
    "- hostname: \"*\"\n  timeout: 30s\n  rate_limit: {rps: 100}\n\
     - hostname: app.example.com\n  timeout: 60s\n\
     - hostname: app.example.com\n  path: /upload\n  timeout: 10m\n",
  );
  assert_eq!(
    s.timeout_for(Some("app.example.com"), "/upload/a"),
    Some(600)
  );
  assert_eq!(s.timeout_for(Some("app.example.com"), "/"), Some(60));
  assert_eq!(s.timeout_for(Some("other.test"), "/"), Some(30));
  // Nothing narrower writes a rate limit, so `*`'s still counts here.
  assert_eq!(
    s.rate_limit_for(Some("app.example.com"), "/upload/a", None)
      .map(|r| r.rps),
    Some(100.0)
  );
}

#[test]
fn two_sites_of_the_same_rank_keep_the_order_they_were_written_in() {
  let s = sites(
    "- hostname: \"*\"\n  path: /a\n  timeout: 1s\n\
     - hostname: \"*\"\n  path: /b\n  timeout: 2s\n",
  );
  let order: Vec<String> = s.all().iter().map(Site::label).collect();
  assert_eq!(order, ["*/a", "*/b"]);
}

// --- what adds up ----------------------------------------------------------------

#[test]
fn headers_layer_broadest_first_so_the_narrowest_has_the_last_word() {
  let s = sites(
    "- hostname: \"*\"\n  headers:\n    response:\n      add: {x-served-by: aperio, cache-control: no-store}\n      remove: [x-powered-by]\n\
     - hostname: app.example.com\n  path: /static\n  headers:\n    response:\n      add: {cache-control: \"public, max-age=3600\"}\n",
  );
  let out = s.response_headers(
    Some("app.example.com"),
    "/static/app.js",
    h(&[("x-powered-by", "php"), ("cache-control", "max-age=0")]),
  );
  assert!(
    out.iter().any(|(k, v)| k == "x-served-by" && v == "aperio"),
    "{out:?}"
  );
  assert!(!out.iter().any(|(k, _)| k == "x-powered-by"), "{out:?}");
  let cache: Vec<&str> = out
    .iter()
    .filter(|(k, _)| k.eq_ignore_ascii_case("cache-control"))
    .map(|(_, v)| v.as_str())
    .collect();
  assert_eq!(cache, ["public, max-age=3600"], "the narrower site wins");

  let elsewhere = s.response_headers(Some("other.test"), "/", h(&[]));
  assert!(
    elsewhere
      .iter()
      .any(|(k, v)| k == "cache-control" && v == "no-store")
  );
}

#[test]
fn request_headers_layer_the_same_way() {
  let s = sites(
    "- hostname: \"*\"\n  headers:\n    request:\n      add: {x-env: prod}\n\
     - hostname: app.example.com\n  headers:\n    request:\n      remove: [x-env]\n",
  );
  assert!(
    s.request_headers(Some("app.example.com"), "/", h(&[]))
      .is_empty()
  );
  assert_eq!(
    s.request_headers(Some("b.test"), "/", h(&[])),
    h(&[("x-env", "prod")])
  );
}

#[test]
fn a_hostname_with_a_rule_of_its_own_still_gets_the_rules_everybody_gets() {
  // "Most specific wins" would let this hostname's one rule switch off the
  // scanner block, which is a firewall that fails open.
  let s = sites(
    "- hostname: \"*\"\n  waf:\n    - path: \"^/\\\\.git\"\n\
     - hostname: app.example.com\n  waf:\n    - path: \"^/admin\"\n",
  );
  let none = HeaderMap::new();
  assert!(
    s.waf_deny(Some("app.example.com"), "GET", "/admin", &none)
      .is_some()
  );
  assert!(
    s.waf_deny(Some("app.example.com"), "GET", "/.git/config", &none)
      .is_some()
  );
  assert!(
    s.waf_deny(Some("other.test"), "GET", "/admin", &none)
      .is_none()
  );
  assert!(s.has_waf());
}

#[test]
fn a_firewall_rule_says_which_site_it_came_from() {
  let s = sites("- hostname: app.example.com\n  waf:\n    - path: \"^/admin\"\n");
  let reason = s
    .waf_deny(Some("app.example.com"), "GET", "/admin", &HeaderMap::new())
    .unwrap();
  assert!(reason.starts_with("app.example.com waf[0]"), "{reason}");
}

#[test]
fn a_size_rule_is_only_checked_against_a_body() {
  let s = sites("- hostname: \"*\"\n  path: /upload\n  waf:\n    - max_body: 100\n");
  let none = HeaderMap::new();
  assert!(s.waf_deny(None, "POST", "/upload", &none).is_none());
  assert!(s.waf_body(None, "POST", "/upload", &none, 50).is_none());
  assert!(s.waf_body(None, "POST", "/upload", &none, 500).is_some());
  assert!(s.waf_body(None, "POST", "/elsewhere", &none, 500).is_none());
}

#[test]
fn every_matching_sites_maintenance_window_applies() {
  let utc = |d: u32, hh: u32, mm: u32| {
    chrono::NaiveDate::from_ymd_opt(2026, 8, d)
      .unwrap()
      .and_hms_opt(hh, mm, 0)
      .unwrap()
      .and_utc()
      .timestamp() as u64
  };
  let s = sites(
    "- hostname: \"*\"\n  maintenance:\n    - {from: \"02:00\", to: \"04:00\", reason: everyone}\n\
     - hostname: app.example.com\n  maintenance:\n    - {from: \"12:00\", to: \"13:00\", reason: app}\n",
  );
  let night = utc(5, 3, 0);
  let noon = utc(5, 12, 30);
  let (w, _) = s.maintenance_for(Some("app.example.com"), night).unwrap();
  assert_eq!(
    w.reason.as_deref(),
    Some("everyone"),
    "the `*` window is the app's too"
  );
  let (w, _) = s.maintenance_for(Some("app.example.com"), noon).unwrap();
  assert_eq!(w.reason.as_deref(), Some("app"));
  assert!(s.maintenance_for(Some("other.test"), noon).is_none());
  assert!(s.has_maintenance());
}

// --- answers ----------------------------------------------------------------------

#[test]
fn a_redirect_answers_and_preserves_the_path_when_asked() {
  let s = sites(
    "- hostname: old.example.com\n  redirect: {url: \"https://new.example.com\", preserve_path: true}\n\
     - hostname: gone.example.com\n  redirect: {url: \"https://example.com\", permanent: true}\n",
  );
  let res = s
    .answer(Some("old.example.com"), "/docs/intro", Some("a=1"))
    .unwrap();
  assert_eq!(res.status(), 302);
  assert_eq!(
    res.headers().get("location").unwrap(),
    "https://new.example.com/docs/intro?a=1"
  );
  let res = s.answer(Some("gone.example.com"), "/", None).unwrap();
  assert_eq!(res.status(), 301);
  assert_eq!(
    res.headers().get("location").unwrap(),
    "https://example.com"
  );
  assert!(s.answer(Some("other.example.com"), "/", None).is_none());
  assert!(s.answer(None, "/", None).is_none());
}

#[test]
fn a_fixed_response_answers_on_a_path_under_every_hostname() {
  let s = sites(
    "- hostname: \"*\"\n  path: /robots.txt\n  respond: {content_type: text/plain, body: \"User-agent: *\"}\n\
     - hostname: soon.example.com\n  respond: {status: 503, body: \"<h1>Coming soon</h1>\"}\n",
  );
  let res = s.answer(Some("app.test"), "/robots.txt", None).unwrap();
  assert_eq!(res.status(), 200);
  assert_eq!(res.headers().get("content-type").unwrap(), "text/plain");
  let res = s.answer(Some("soon.example.com"), "/x", None).unwrap();
  assert_eq!(res.status(), 503);
  assert_eq!(
    res.headers().get("content-type").unwrap(),
    "text/html; charset=utf-8",
    "html unless it says otherwise"
  );
  assert!(s.answer(Some("app.test"), "/", None).is_none());
}

#[test]
fn a_site_with_only_policy_never_answers() {
  let s = sites(
    "- hostname: \"*\"\n  path: /api\n  timeout: 90s\n\
     - hostname: \"*\"\n  canary: {service: web-v2, weight: 20}\n",
  );
  assert!(s.answer(Some("a.test"), "/api/v1", None).is_none());
  assert_eq!(
    s.canary_for(Some("a.test"), "/").map(|c| c.weight),
    Some(20)
  );
}

// --- what a hostname says when nothing serves it -----------------------------------

#[test]
fn a_fallback_comes_from_the_hostnames_site_before_the_catch_all() {
  let s = sites(
    "- hostname: app.example.com\n  fallback: {url: \"https://status.example.com\"}\n\
     - hostname: \"*\"\n  fallback: {url: \"https://www.example.com\", preserve_path: true, permanent: true}\n",
  );
  assert_eq!(
    s.fallback_for(Some("app.example.com")).unwrap().url,
    "https://status.example.com"
  );
  let any = s.fallback_for(Some("other.example.com")).unwrap();
  assert_eq!(any.url, "https://www.example.com");
  assert_eq!(any.status(), StatusCode::MOVED_PERMANENTLY);
  assert_eq!(
    any.location("/a/b", Some("q=1")),
    "https://www.example.com/a/b?q=1"
  );
  assert_eq!(s.fallback_for(None).unwrap().url, "https://www.example.com");
}

#[test]
fn pages_come_from_the_hostnames_site_and_then_from_star() {
  let s = sites(
    "- hostname: \"*\"\n  pages: {unavailable: \"<h1>down</h1>\", maintenance: \"<h1>later</h1>\"}\n\
     - hostname: app.example.com\n  pages: {unavailable: \"<h1>app down</h1>\"}\n",
  );
  assert_eq!(
    s.page_504(Some("APP.example.com")),
    Some("<h1>app down</h1>")
  );
  assert_eq!(
    s.page_503(Some("app.example.com")),
    Some("<h1>later</h1>"),
    "the broader one fills in"
  );
  assert_eq!(s.page_504(Some("other.test")), Some("<h1>down</h1>"));
  assert_eq!(s.page_504(None), Some("<h1>down</h1>"));
  assert_eq!(Sites::default().page_504(Some("a.test")), None);
}

// --- rate limits -------------------------------------------------------------------

#[test]
fn a_rate_limit_counts_its_methods_and_gets_a_bucket_of_its_own() {
  let s = sites(
    "- hostname: \"*\"\n  path: /a\n  rate_limit: {rps: 5, methods: [post, PUT]}\n\
     - hostname: \"*\"\n  path: /b\n  rate_limit: {rps: 0.5}\n",
  );
  let a = s.rate_limit_for(None, "/a", Some("POST")).unwrap();
  assert_eq!(
    a.methods.as_deref(),
    Some(&["POST".to_string(), "PUT".to_string()][..])
  );
  assert!(
    s.rate_limit_for(None, "/a", Some("put")).is_some(),
    "case does not matter"
  );
  assert!(
    s.rate_limit_for(None, "/a", Some("GET")).is_none(),
    "an unlisted verb is not counted"
  );
  assert!(
    s.rate_limit_for(None, "/a", None).is_some(),
    "asking about the route ignores the filter"
  );
  let b = s.rate_limit_for(None, "/b", None).unwrap();
  assert_eq!(
    b.burst, 1.0,
    "a burst below one token could never pay for a request"
  );
  assert_ne!(a.key, b.key, "two sites never share a bucket");
  assert_eq!(a.burst, 5.0, "one second's worth by default");
}

// --- refusals ----------------------------------------------------------------------

#[test]
fn a_site_that_cannot_work_is_refused_naming_it() {
  let e = refused("- hostname: app.example.com\n");
  assert!(
    e.starts_with("sites[0] (app.example.com): sets nothing"),
    "{e}"
  );
  assert!(refused("- hostname: \"bad host!\"\n  timeout: 1s\n").contains("is not a hostname"));
  assert!(refused("- hostname: \"a.*.com\"\n  timeout: 1s\n").contains("is not a hostname"));
  assert!(
    refused("- hostname: a.test\n  path: /../x\n  timeout: 1s\n").contains("is not a path prefix")
  );
}

#[test]
fn an_answer_with_policy_beside_it_is_refused() {
  // A static answer never reaches a client, so a client-side timeout on it is
  // a setting that cannot mean anything; saying so beats ignoring it.
  let e = refused(
    "- hostname: a.test\n  redirect: {url: \"https://b.test\"}\n  timeout: 30s\n  canary: {service: v2}\n",
  );
  assert!(e.contains("`timeout`, `canary`"), "{e}");
  assert!(e.contains("answers by itself with `redirect`"), "{e}");
  assert!(
    refused("- hostname: a.test\n  redirect: {url: \"https://b.test\"}\n  respond: {body: hi}\n")
      .contains("two answers")
  );
}

#[test]
fn hostname_settings_on_a_path_are_refused() {
  let e = refused(
    "- hostname: a.test\n  path: /x\n  fallback: {url: \"https://b.test\"}\n  pages: {unavailable: down}\n",
  );
  assert!(
    e.contains("`fallback`, `pages` are about the whole hostname"),
    "{e}"
  );
}

#[test]
fn bad_values_are_refused() {
  assert!(refused("- hostname: a.test\n  timeout: 0s\n").contains("at least 1s"));
  assert!(refused("- hostname: a.test\n  rate_limit: {rps: 0}\n").contains("positive"));
  assert!(refused("- hostname: a.test\n  rate_limit: {rps: 1, burst: 0}\n").contains("positive"));
  assert!(
    refused("- hostname: a.test\n  rate_limit: {rps: 1, methods: []}\n").contains("count nothing")
  );
  assert!(refused("- hostname: a.test\n  canary: {service: \"\"}\n").contains("canary.service"));
  assert!(
    refused("- hostname: a.test\n  canary: {service: v2, weight: 200}\n").contains("percentage")
  );
  assert!(refused("- hostname: a.test\n  fallback: {url: \"ftp://x\"}\n").contains("fallback.url"));
  assert!(
    refused("- hostname: a.test\n  redirect: {url: \"/relative\"}\n").contains("redirect.url")
  );
  assert!(
    refused("- hostname: a.test\n  respond: {status: 1000}\n").contains("not an HTTP status")
  );
  let big = "x".repeat(MAX_PAGE_BYTES + 1);
  assert!(
    refused(&format!(
      "- hostname: a.test\n  pages: {{unavailable: \"{big}\"}}\n"
    ))
    .contains("pages.unavailable: exceeds 512KB")
  );
}

#[test]
fn a_component_error_is_named_inside_its_site() {
  let e = refused("- hostname: a.test\n  waf:\n    - path: \"(unclosed\"\n");
  assert!(e.starts_with("sites[0] (a.test): waf[0].path"), "{e}");
  let e = refused("- hostname: a.test\n  maintenance:\n    - {from: \"2 am\", to: \"04:00\"}\n");
  assert!(e.starts_with("sites[0] (a.test): maintenance[0]"), "{e}");
}
