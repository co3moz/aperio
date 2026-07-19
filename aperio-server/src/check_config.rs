//! `aperio-server --check-config`: lint / dry-run of the server
//! configuration.
//!
//! Runs after `config_file::load()` materialized `aperio-server.yaml` into
//! the environment, so the exact same layered view the server would boot
//! with is validated — file keys, environment variables, and the structured
//! sections — without binding a port or touching the data directory.
//! Prints one line per finding and exits 0 (valid) or 1 (errors found).

/// Local mirror of an `error_pages:` entry for lint-time validation (the
/// runtime type in `error_pages.rs` is private and compiles files eagerly).
#[derive(serde::Deserialize)]
struct ErrorPageRuleLint {
  hostname: String,
  #[serde(rename = "504_page")]
  page_504: Option<String>,
  #[serde(rename = "503_page")]
  page_503: Option<String>,
}

/// One accumulated lint report.
#[derive(Default)]
struct Report {
  errors: usize,
  warnings: usize,
}

impl Report {
  fn ok(&self, what: &str) {
    println!("  ok    {what}");
  }
  fn warn(&mut self, what: &str) {
    self.warnings += 1;
    println!("  warn  {what}");
  }
  fn fail(&mut self, what: &str) {
    self.errors += 1;
    println!("  FAIL  {what}");
  }
}

/// Non-empty environment lookup.
fn env(name: &str) -> Option<String> {
  std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Checks that an env var, when set, parses as the given numeric type.
fn check_parse<T: std::str::FromStr>(r: &mut Report, name: &str, kind: &str) {
  if let Some(raw) = env(name) {
    if raw.trim().parse::<T>().is_ok() {
      r.ok(&format!("{name} = {}", raw.trim()));
    } else {
      r.fail(&format!(
        "{name} '{}' is not a valid {kind}; the server would fall back to its default",
        raw.trim()
      ));
    }
  }
}

/// Checks that a page-path env var, when set, points at a readable file.
fn check_page(r: &mut Report, name: &str) {
  if let Some(path) = env(name) {
    match std::fs::read_to_string(path.trim()) {
      Ok(_) => r.ok(&format!("{name} file is readable ({})", path.trim())),
      Err(e) => r.fail(&format!("{name} {} is not readable: {e}", path.trim())),
    }
  }
}

/// Validates one structured yaml section against its schema type.
fn check_section<T: serde::de::DeserializeOwned>(r: &mut Report, key: &str) -> Option<T> {
  let section = crate::config_file::structured(key)?;
  match serde_yaml::from_value::<T>(section) {
    Ok(parsed) => {
      r.ok(&format!("`{key}:` section parses"));
      Some(parsed)
    }
    Err(e) => {
      r.fail(&format!("`{key}:` section is invalid: {e}"));
      None
    }
  }
}

/// Runs the full lint. Returns the process exit code.
pub(crate) fn run() -> i32 {
  let mut r = Report::default();

  match crate::config_file::watched_path() {
    Some(path) => println!("Checking configuration ({})", path.display()),
    None => println!("Checking configuration (no aperio-server.yaml — environment only)"),
  }

  // --- Core requirements ---
  match env("APERIO_SERVER_TOKEN") {
    Some(t) if t.trim().len() >= 16 => r.ok("APERIO_SERVER_TOKEN is set"),
    Some(_) => {
      r.warn("APERIO_SERVER_TOKEN is set but short (< 16 chars) — consider a longer secret")
    }
    None => r.fail("APERIO_SERVER_TOKEN is not set (the server refuses to start without it)"),
  }

  // --- Numeric scalars: set-but-unparsable values silently fall back to
  // defaults at startup; the lint surfaces them as errors. ---
  check_parse::<u16>(&mut r, "PORT", "port number");
  check_parse::<u64>(&mut r, "APERIO_SERVER_GATEWAY_TIMEOUT", "number of seconds");
  check_parse::<u64>(
    &mut r,
    "APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT",
    "number of seconds",
  );
  check_parse::<usize>(&mut r, "APERIO_MAX_BODY_SIZE", "byte count");
  check_parse::<usize>(&mut r, "APERIO_MAX_CONCURRENT_REQUESTS", "count");
  check_parse::<usize>(&mut r, "APERIO_MAX_TUNNELS", "count");
  check_parse::<f64>(&mut r, "APERIO_IP_LIMIT_MAX", "number");
  check_parse::<f64>(&mut r, "APERIO_IP_LIMIT_REFILL", "number");
  check_parse::<u32>(&mut r, "APERIO_LOGIN_LOCKOUT_THRESHOLD", "count");
  check_parse::<u64>(&mut r, "APERIO_LOGIN_LOCKOUT_SECS", "number of seconds");
  check_parse::<u64>(&mut r, "APERIO_CLIENT_DOWN_THRESHOLD", "number of seconds");
  check_parse::<u32>(&mut r, "APERIO_FAILOVER_MAX_JUMPS", "count");
  check_parse::<u64>(&mut r, "APERIO_FAILOVER_WINDOW", "number of seconds");
  check_parse::<u64>(&mut r, "APERIO_CACHE_MAX_BYTES", "byte count");
  check_parse::<u64>(&mut r, "APERIO_CACHE_MAX_STALE", "number of seconds");
  check_parse::<u64>(&mut r, "APERIO_AUDIT_MAX_SIZE", "byte count");
  check_parse::<usize>(&mut r, "APERIO_AUDIT_MAX_FILES", "count");
  check_parse::<u64>(&mut r, "APERIO_TOKEN_EXPIRY_WARNING", "number of days");
  check_parse::<f64>(&mut r, "APERIO_ALERT_ERROR_RATE", "ratio");
  check_parse::<u64>(&mut r, "APERIO_ALERT_WINDOW", "number of seconds");
  check_parse::<u64>(&mut r, "APERIO_ALERT_MIN_REQUESTS", "count");
  check_parse::<u64>(&mut r, "APERIO_ALERT_CLIENT_DOWN", "count");
  check_parse::<u64>(&mut r, "APERIO_RETENTION_CAPTURES", "number of days");
  check_parse::<u64>(&mut r, "APERIO_RETENTION_ACCESS_LOG", "number of days");
  check_parse::<u64>(&mut r, "APERIO_RETENTION_AUDIT", "number of days");
  check_parse::<u64>(&mut r, "APERIO_RETENTION_STATS", "number of days");
  check_parse::<u64>(&mut r, "APERIO_DB_MAX_BYTES", "byte count");

  // --- Enumerated / structured scalars ---
  if let Some(raw) = env("APERIO_LB_STRATEGY") {
    match crate::settings::parse_lb_strategy(&raw) {
      Some(_) => r.ok(&format!("APERIO_LB_STRATEGY = {}", raw.trim())),
      None => r.fail(&format!(
        "APERIO_LB_STRATEGY '{}' is unknown (expected round-robin, primary-standby or sticky)",
        raw.trim()
      )),
    }
  }
  if let Some(raw) = env("APERIO_FAILOVER") {
    match crate::settings::parse_failover_mode(&raw) {
      Some(_) => r.ok(&format!("APERIO_FAILOVER = {}", raw.trim())),
      None => r.fail(&format!(
        "APERIO_FAILOVER '{}' is unknown (expected fail, retry, wait or retry-wait)",
        raw.trim()
      )),
    }
  }
  if let Some(raw) = env("APERIO_RANDOM_SUBDOMAIN") {
    match crate::routing::normalize_random_subdomain_pattern(&raw) {
      Some(p) => r.ok(&format!("APERIO_RANDOM_SUBDOMAIN pattern {p}")),
      None => r.fail(&format!(
        "APERIO_RANDOM_SUBDOMAIN '{}' is not a valid pattern (expected e.g. *.example.com)",
        raw.trim()
      )),
    }
  }
  if let Some(raw) = env("APERIO_TRUSTED_PROXIES") {
    match crate::routing::parse_trusted_proxies(&raw) {
      Ok(list) => r.ok(&format!("APERIO_TRUSTED_PROXIES ({} entries)", list.len())),
      Err(e) => r.fail(&format!("APERIO_TRUSTED_PROXIES is invalid: {e}")),
    }
  }
  if let Some(raw) = env("APERIO_SERVER_AUTH") {
    if crate::routing::valid_visitor_creds(&raw) {
      r.ok("APERIO_SERVER_AUTH is well-formed (user:password)");
    } else {
      r.fail("APERIO_SERVER_AUTH must be in user:password form (both parts non-empty)");
    }
  }
  if let Some(raw) = env("APERIO_UI_LANGUAGE") {
    let lang = raw.trim().to_ascii_lowercase();
    if crate::settings::UI_LANGUAGES.contains(&lang.as_str()) {
      r.ok(&format!("APERIO_UI_LANGUAGE = {lang}"));
    } else {
      r.fail(&format!(
        "APERIO_UI_LANGUAGE '{lang}' is not shipped (available: {})",
        crate::settings::UI_LANGUAGES.join(", ")
      ));
    }
  }

  // --- Error/maintenance pages ---
  check_page(&mut r, "APERIO_504_PAGE");
  check_page(&mut r, "APERIO_503_PAGE");

  // --- Structured sections ---
  check_section::<crate::headers::HeaderRules>(&mut r, "headers");
  if let Some(routes) = check_section::<Vec<crate::static_routes::RouteRule>>(&mut r, "routes")
    && let Err(e) = crate::static_routes::StaticRoutes::compile(routes)
  {
    r.fail(&format!("`routes:` section does not compile: {e}"));
  }
  if let Some(expose) = check_section::<Vec<crate::expose::ExposeRule>>(&mut r, "expose") {
    let mut ports = std::collections::HashSet::new();
    for (i, rule) in expose.iter().enumerate() {
      if rule.protocol != "tcp" {
        r.fail(&format!(
          "`expose:` entry #{}: protocol '{}' is not supported (TCP only)",
          i + 1,
          rule.protocol
        ));
      }
      if rule.key.trim().len() < 8 {
        r.fail(&format!(
          "`expose:` entry #{}: the key must be at least 8 characters",
          i + 1
        ));
      }
      if !ports.insert(rule.port) {
        r.fail(&format!(
          "`expose:` entry #{}: port {} is declared twice",
          i + 1,
          rule.port
        ));
      }
    }
  }
  if let Some(pages) = check_section::<Vec<ErrorPageRuleLint>>(&mut r, "error_pages") {
    for rule in &pages {
      for (which, path) in [("504_page", &rule.page_504), ("503_page", &rule.page_503)] {
        if let Some(path) = path.as_ref().map(|p| p.trim()).filter(|p| !p.is_empty())
          && let Err(e) = std::fs::read_to_string(path)
        {
          r.fail(&format!(
            "`error_pages:` {which} for {} is not readable ({path}): {e}",
            rule.hostname
          ));
        }
      }
    }
  }

  // --- OIDC coherence ---
  let oidc_issuer = env("APERIO_OIDC_ISSUER");
  let oidc_client = env("APERIO_OIDC_CLIENT_ID");
  match (&oidc_issuer, &oidc_client) {
    (Some(_), Some(_)) => r.ok("OIDC issuer and client id are both set"),
    (Some(_), None) => r.fail("APERIO_OIDC_ISSUER is set but APERIO_OIDC_CLIENT_ID is missing"),
    (None, Some(_)) => r.fail("APERIO_OIDC_CLIENT_ID is set but APERIO_OIDC_ISSUER is missing"),
    (None, None) => {}
  }

  // --- Data dir ---
  if let Some(dir) = env("APERIO_DATA_DIR") {
    let path = std::path::Path::new(dir.trim());
    if path.is_dir() {
      r.ok(&format!("APERIO_DATA_DIR exists ({})", dir.trim()));
    } else {
      r.warn(&format!(
        "APERIO_DATA_DIR {} does not exist yet (it is created at startup)",
        dir.trim()
      ));
    }
  }

  println!();
  if r.errors > 0 {
    println!(
      "Configuration check FAILED: {} error(s), {} warning(s)",
      r.errors, r.warnings
    );
    1
  } else {
    println!("Configuration OK ({} warning(s))", r.warnings);
    0
  }
}
