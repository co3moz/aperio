//! Unit tests for `--check-config`.
//!
//! The bulk of the module lives in [`run`], the CLI entrypoint that reads the
//! layered configuration (env vars + `aperio-server.yaml`), runs every
//! validator and returns an exit code. These tests drive it end to end against
//! throwaway temp config files and a controlled environment, plus unit-test the
//! private helpers directly.
//!
//! `run` and the config loader both touch process-global state (the `APERIO_*`
//! environment and the retained config document), so every test that mutates
//! them takes the one process-wide config lock (`test_support::config_lock`,
//! shared with every other config-touching test module), and restores the
//! environment on drop.

use super::*;
use crate::static_routes::RouteRule;

// --------------------------------------------------------------------------
// Environment isolation
// --------------------------------------------------------------------------

/// Every `APERIO_*` (and bare) env var `run` reads. Cleared at guard
/// construction and restored on drop so tests never leak into one another.
const KEYS: &[&str] = &[
  "APERIO_SERVER_CONFIG",
  "APERIO_SERVER_TOKEN",
  "PORT",
  "APERIO_GATEWAY_TIMEOUT",
  "APERIO_GATEWAY_RESPONSE_TIMEOUT",
  "APERIO_MAX_BODY_SIZE",
  "APERIO_MAX_CONCURRENT_REQUESTS",
  "APERIO_MAX_TUNNELS",
  "APERIO_IP_LIMIT_MAX",
  "APERIO_IP_LIMIT_REFILL",
  "APERIO_LOGIN_LOCKOUT_THRESHOLD",
  "APERIO_LOGIN_LOCKOUT_SECS",
  "APERIO_CLIENT_DOWN_THRESHOLD",
  "APERIO_FAILOVER_MAX_JUMPS",
  "APERIO_FAILOVER_WINDOW",
  "APERIO_CACHE_MAX_BYTES",
  "APERIO_CACHE_MAX_STALE",
  "APERIO_AUDIT_MAX_SIZE",
  "APERIO_AUDIT_MAX_FILES",
  "APERIO_TOKEN_EXPIRY_WARNING",
  "APERIO_ALERT_ERROR_RATE",
  "APERIO_ALERT_WINDOW",
  "APERIO_ALERT_MIN_REQUESTS",
  "APERIO_ALERT_CLIENT_DOWN",
  "APERIO_RETENTION_CAPTURES",
  "APERIO_RETENTION_ACCESS_LOG",
  "APERIO_RETENTION_AUDIT",
  "APERIO_RETENTION_STATS",
  "APERIO_DB_MAX_BYTES",
  "APERIO_LB_STRATEGY",
  "APERIO_FAILOVER",
  "APERIO_RANDOM_SUBDOMAIN",
  "APERIO_TRUSTED_PROXIES",
  "APERIO_SERVER_AUTH",
  "APERIO_UI_LANGUAGE",
  "APERIO_504_PAGE",
  "APERIO_503_PAGE",
  "APERIO_OIDC_ISSUER",
  "APERIO_OIDC_CLIENT_ID",
  "APERIO_DATA_DIR",
];

/// Holds the process-wide config lock (the same one every config-touching
/// test module takes, so none of them races the global document or
/// `APERIO_SERVER_CONFIG`), snapshots + clears every relevant env var, and
/// restores everything on drop.
struct EnvGuard {
  /// Held for the length of the test: everything that touches the global
  /// config document or the `APERIO_*` environment takes the same lock.
  _lock: std::sync::MutexGuard<'static, ()>,
  saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
  fn acquire() -> Self {
    let lock = crate::test_support::config_lock();
    let saved = KEYS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
    for k in KEYS {
      unsafe { std::env::remove_var(k) };
    }
    EnvGuard { _lock: lock, saved }
  }
}

impl Drop for EnvGuard {
  fn drop(&mut self) {
    for (k, v) in &self.saved {
      match v {
        Some(val) => unsafe { std::env::set_var(k, val) },
        None => unsafe { std::env::remove_var(k) },
      }
    }
    let _ = std::fs::remove_file("aperio-server.yaml");
  }
}

fn set(k: &str, v: &str) {
  // Only keys the guard knows to clear and restore. A key outside `KEYS` set
  // by one test would still be set for the next one, which is the other half
  // of how these tests used to interfere with each other.
  assert!(
    KEYS.contains(&k),
    "{k} is set by a test but not in KEYS, so nothing would ever clear it"
  );
  unsafe { std::env::set_var(k, v) };
}

/// Writes `yaml` to a fresh temp file and points `APERIO_SERVER_CONFIG` at it,
/// then loads it so `structured(...)`/`watched_path()` see it. Returns the path.
fn load_config(yaml: &str) -> std::path::PathBuf {
  let file =
    crate::test_support::test_temp_root().join(format!("checkcfg-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(&file, yaml).unwrap();
  set("APERIO_SERVER_CONFIG", file.to_str().unwrap());
  crate::config_file::load();
  file
}

fn write_temp_page() -> std::path::PathBuf {
  let file =
    crate::test_support::test_temp_root().join(format!("page-{}.html", uuid::Uuid::new_v4()));
  std::fs::write(&file, b"<html>maintenance</html>").unwrap();
  file
}

// --------------------------------------------------------------------------
// run(): the CLI entrypoint
// --------------------------------------------------------------------------

#[test]
fn run_all_valid_returns_zero() {
  let _g = EnvGuard::acquire();
  let data_dir =
    crate::test_support::test_temp_root().join(format!("datadir-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&data_dir).unwrap();
  let page = write_temp_page();

  // Every scalar valid; structured sections all parse and compile; OIDC pair
  // complete; data dir exists. No shadowing, so zero warnings, zero errors.
  load_config(&format!(
    concat!(
      "server_token: 0123456789abcdef\n",
      "port: 8080\n",
      "routes:\n",
      "  - hostname: a.example.com\n    path: /api\n    redirect: https://x.example.com\n",
      "  - hostname: b.example.com\n    path: /web\n    redirect: https://y.example.com\n",
      "expose:\n",
      "  - port: 5000\n    key: longenoughkey\n",
      "rate_limits:\n",
      "  - path: /api\n    rps: 10\n",
      "fallbacks:\n",
      "  - hostname: a.example.com\n    url: https://up.example.com\n",
      "waf:\n",
      "  - path: /admin\n",
      "headers:\n  request:\n    add:\n      X-A: b\n",
      "error_pages:\n  - hostname: a.example.com\n    504_page: {page}\n",
    ),
    page = page.display()
  ));

  set("APERIO_GATEWAY_TIMEOUT", "30");
  set("APERIO_MAX_BODY_SIZE", "1048576");
  set("APERIO_IP_LIMIT_MAX", "100.0");
  set("APERIO_LB_STRATEGY", "round-robin");
  set("APERIO_FAILOVER", "retry");
  set("APERIO_RANDOM_SUBDOMAIN", "*.example.com");
  set("APERIO_TRUSTED_PROXIES", "10.0.0.0/8");
  set("APERIO_SERVER_AUTH", "user:secret");
  set("APERIO_UI_LANGUAGE", "en");
  set("APERIO_504_PAGE", page.to_str().unwrap());
  set("APERIO_503_PAGE", page.to_str().unwrap());
  set("APERIO_OIDC_ISSUER", "https://issuer.example.com");
  set("APERIO_OIDC_CLIENT_ID", "client-abc");
  set("APERIO_DATA_DIR", data_dir.to_str().unwrap());

  assert_eq!(run(), 0);

  let _ = std::fs::remove_dir_all(&data_dir);
  let _ = std::fs::remove_file(&page);
}

#[test]
fn run_short_token_and_shadowing_warn_but_still_zero() {
  let _g = EnvGuard::acquire();
  // Warnings only: short token + a broad route shadowing a narrow one + a
  // not-yet-created data dir. No errors -> exit 0.
  load_config(concat!(
    "server_token: short\n",
    "routes:\n",
    "  - hostname: a.example.com\n    redirect: https://x\n",
    "  - hostname: a.example.com\n    path: /api\n    redirect: https://y\n",
  ));
  set("APERIO_DATA_DIR", "/nonexistent/aperio-data-dir-xyz");
  assert_eq!(run(), 0);
}

#[test]
fn run_missing_token_fails() {
  let _g = EnvGuard::acquire();
  // No token at all is a hard error.
  load_config("port: 8080\n");
  assert_eq!(run(), 1);
}

#[test]
fn run_invalid_scalars_and_enums_fail() {
  let _g = EnvGuard::acquire();
  load_config("server_token: 0123456789abcdef\n");
  // Unparseable numerics.
  set("PORT", "not-a-port");
  set("APERIO_MAX_BODY_SIZE", "huge");
  set("APERIO_IP_LIMIT_MAX", "abc");
  // Unknown enum / malformed structured scalars.
  set("APERIO_LB_STRATEGY", "bogus");
  set("APERIO_FAILOVER", "bogus");
  set("APERIO_RANDOM_SUBDOMAIN", "not a pattern");
  set("APERIO_TRUSTED_PROXIES", "not-a-cidr!!");
  set("APERIO_SERVER_AUTH", "no-colon-here");
  set("APERIO_UI_LANGUAGE", "xx");
  // A page path that does not resolve.
  set("APERIO_504_PAGE", "/no/such/page.html");
  // OIDC issuer without a client id.
  set("APERIO_OIDC_ISSUER", "https://issuer.example.com");
  assert_eq!(run(), 1);
}

#[test]
fn the_config_lock_holds_however_long_a_test_takes() {
  // The property the old lock did not have. It was a file whose age decided
  // whether it counted, and a holder that had not finished within thirty
  // seconds had its lock taken away — so under a fully parallel run two tests
  // could be inside at once, and one of them failed for something the other
  // one did to the environment. Held for a long moment here on purpose: it is
  // the *duration* that used to break it.
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  let inside = Arc::new(AtomicUsize::new(0));
  let overlaps = Arc::new(AtomicUsize::new(0));
  let mut threads = Vec::new();
  for _ in 0..8 {
    let inside = inside.clone();
    let overlaps = overlaps.clone();
    threads.push(std::thread::spawn(move || {
      for _ in 0..20 {
        let _guard = crate::test_support::config_lock();
        if inside.fetch_add(1, Ordering::SeqCst) != 0 {
          overlaps.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        inside.fetch_sub(1, Ordering::SeqCst);
      }
    }));
  }
  for t in threads {
    t.join().unwrap();
  }
  assert_eq!(
    overlaps.load(Ordering::SeqCst),
    0,
    "two holders were inside the config lock at once"
  );
}

#[test]
fn a_panicking_holder_does_not_take_the_next_test_with_it() {
  // Poisoning is recovered rather than propagated: a test that panics while
  // holding the lock should fail alone, not turn every later config test into
  // a second failure with a message about someone else's panic.
  let panicked = std::thread::spawn(|| {
    let _guard = crate::test_support::config_lock();
    panic!("a test failed while holding the lock");
  })
  .join();
  assert!(panicked.is_err());
  let _guard = crate::test_support::config_lock();
}

#[test]
fn run_oidc_client_without_issuer_fails() {
  let _g = EnvGuard::acquire();
  load_config("server_token: 0123456789abcdef\n");
  set("APERIO_OIDC_CLIENT_ID", "client-abc");
  assert_eq!(run(), 1);
}

#[test]
fn run_malformed_and_duplicate_sections_fail() {
  let _g = EnvGuard::acquire();
  // headers section that fails to deserialize, an expose section that trips
  // every validation (bad protocol, short key, duplicate port), fallbacks with
  // a non-http url, and a waf rule with no conditions.
  load_config(concat!(
    "server_token: 0123456789abcdef\n",
    "headers:\n  request: not-a-mapping\n",
    "expose:\n",
    "  - port: 5000\n    key: short\n    protocol: udp\n",
    "  - port: 5000\n    key: alsolongkey\n",
    "fallbacks:\n  - hostname: a.example.com\n    url: ftp://nope\n",
    "waf:\n  - {}\n",
  ));
  assert_eq!(run(), 1);
}

#[test]
fn run_routes_that_parse_but_fail_to_compile() {
  let _g = EnvGuard::acquire();
  // A route with neither `redirect` nor `respond` parses as a RouteRule but
  // fails `StaticRoutes::compile`.
  load_config(concat!(
    "server_token: 0123456789abcdef\n",
    "routes:\n  - hostname: a.example.com\n",
  ));
  assert_eq!(run(), 1);
}

#[test]
fn run_invalid_routes_section_fails() {
  let _g = EnvGuard::acquire();
  // A routes entry whose hostname is a sequence cannot deserialize into
  // RouteRule -> `check_section` reports the section invalid.
  load_config(concat!(
    "server_token: 0123456789abcdef\n",
    "routes:\n  - hostname:\n      - 1\n      - 2\n",
  ));
  assert_eq!(run(), 1);
}

#[test]
fn run_without_config_file_reports_environment_only() {
  let _g = EnvGuard::acquire();
  // No APERIO_SERVER_CONFIG and no default file present -> the "environment
  // only" header branch; token missing still fails.
  let _ = std::fs::remove_file("aperio-server.yaml");
  // Reset the retained document so no earlier structured section lingers.
  let _ = crate::config_file::reload();
  assert_eq!(run(), 1);
}

// --------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------

#[test]
fn env_filters_blank_values() {
  let _g = EnvGuard::acquire();
  assert_eq!(env("APERIO_UI_LANGUAGE"), None);
  set("APERIO_UI_LANGUAGE", "   ");
  assert_eq!(env("APERIO_UI_LANGUAGE"), None);
  set("APERIO_UI_LANGUAGE", " en ");
  assert_eq!(env("APERIO_UI_LANGUAGE").as_deref(), Some(" en "));
}

#[test]
fn check_parse_ok_and_fail() {
  let _g = EnvGuard::acquire();
  let mut r = Report::default();
  set("PORT", "8080");
  check_parse::<u16>(&mut r, "PORT", "port number");
  assert_eq!((r.errors, r.warnings), (0, 0));

  set("PORT", "70000"); // out of u16 range
  check_parse::<u16>(&mut r, "PORT", "port number");
  assert_eq!(r.errors, 1);

  // Unset var is silently skipped.
  let mut r2 = Report::default();
  check_parse::<u16>(&mut r2, "APERIO_MAX_TUNNELS", "count");
  assert_eq!((r2.errors, r2.warnings), (0, 0));
}

#[test]
fn check_page_readable_and_missing() {
  let _g = EnvGuard::acquire();
  let page = write_temp_page();
  let mut r = Report::default();
  set("APERIO_504_PAGE", page.to_str().unwrap());
  check_page(&mut r, "APERIO_504_PAGE");
  assert_eq!(r.errors, 0);

  set("APERIO_504_PAGE", "/definitely/not/here.html");
  check_page(&mut r, "APERIO_504_PAGE");
  assert_eq!(r.errors, 1);
  let _ = std::fs::remove_file(&page);
}

#[test]
fn check_section_parses_and_reports_invalid() {
  let _g = EnvGuard::acquire();
  load_config(concat!(
    "headers:\n  request:\n    add:\n      X-A: b\n",
    "routes:\n  - hostname: a.example.com\n",
  ));
  let mut r = Report::default();
  // A well-formed section parses.
  let parsed = check_section::<crate::headers::HeaderRules>(&mut r, "headers");
  assert!(parsed.is_some());
  assert_eq!(r.errors, 0);
  // An absent section returns None without recording anything.
  assert!(check_section::<crate::headers::HeaderRules>(&mut r, "missing").is_none());
}

#[test]
fn report_counts_findings() {
  let mut r = Report::default();
  r.ok("fine");
  r.warn("careful");
  r.fail("broken");
  assert_eq!((r.errors, r.warnings), (1, 1));
}

// --------------------------------------------------------------------------
// lint_route_shadowing (moved from the inline module)
// --------------------------------------------------------------------------

fn rule(host: Option<&str>, path: Option<&str>) -> RouteRule {
  RouteRule {
    hostname: host.map(|s| s.to_string()),
    path: path.map(|s| s.to_string()),
    redirect: Some("https://example.com".to_string()),
    permanent: false,
    preserve_path: false,
    respond: None,
  }
}

#[test]
fn test_shadowing_flags_broad_rule_hiding_narrow_one() {
  let mut r = Report::default();
  // A catch-all path on a host precedes a specific path on the same host.
  let routes = vec![rule(Some("a.com"), None), rule(Some("a.com"), Some("/api"))];
  lint_route_shadowing(&mut r, &routes);
  assert_eq!(r.warnings, 1);
}

#[test]
fn test_shadowing_flags_exact_duplicate_bind() {
  let mut r = Report::default();
  let routes = vec![rule(None, Some("/api")), rule(None, Some("/api"))];
  lint_route_shadowing(&mut r, &routes);
  assert_eq!(r.warnings, 1);
}

#[test]
fn test_no_shadowing_for_distinct_routes() {
  let mut r = Report::default();
  let routes = vec![
    rule(Some("a.com"), Some("/api")),
    rule(Some("b.com"), Some("/api")),
    rule(Some("a.com"), Some("/web")),
    rule(Some("a.com"), Some("/api/v1")), // narrower than /api, but /api is earlier → shadowed
  ];
  lint_route_shadowing(&mut r, &routes);
  // Only /api/v1 (index 3) is shadowed by /api (index 0).
  assert_eq!(r.warnings, 1);
}
