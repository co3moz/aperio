use super::*;

/// A fully-populated baseline config (there is no `Default` for `ServerConfig`).
fn base_config() -> ServerConfig {
  ServerConfig {
    token: "test".to_string(),
    gateway_timeout: Duration::from_secs(30),
    gateway_response_timeout: Duration::from_secs(30),
    max_body_size: 1024,
    max_tunnels: 10,
    ip_limit_max: 100.0,
    ip_limit_refill: 10.0,
    auth_credentials: None,
    trust_proxy: false,
    ignore_client_auth: false,
    real_ip_header: None,
    trusted_proxies: Vec::new(),
    admin_allowed_ips: Vec::new(),
    secure_cookies: false,
    require_hostname_bind: false,
    metrics_token: None,
    random_subdomain_suffix: None,
    client_down_threshold: Duration::from_secs(3600),
    tunnel_compression: false,
    custom_504_page: None,
    custom_503_page: None,
    lb_strategy: LbStrategy::RoundRobin,
    failover_mode: FailoverMode::Fail,
    failover_max_jumps: 2,
    failover_window: Duration::from_secs(15),
    failover_all_methods: false,
    retry_on_5xx: false,
    retry_statuses: Vec::new(),
    outlier_ejection: false,
    outlier_max_failures: 5,
    outlier_window: Duration::from_secs(30),
    outlier_eject: Duration::from_secs(30),
    cache_enabled: false,
    max_concurrent_requests: 100,
    max_ws_connections: 10_000,
    login_lockout_threshold: 5,
    login_lockout_secs: 60,
    audit_max_size: 10 * 1024 * 1024,
    audit_max_files: 3,
    ui_language: "en".to_string(),
    header_rules: Default::default(),
    static_routes: Default::default(),
    error_pages: Default::default(),
    route_limits: Default::default(),
    fallbacks: Default::default(),
    waf: Default::default(),
    token_pinning: false,
    preview_noindex: false,
    cache_max_bytes: 64 * 1024 * 1024,
    cache_max_stale: 3600,
  }
}

#[test]
fn lb_strategy_parsing() {
  assert!(matches!(
    parse_lb_strategy(""),
    Some(LbStrategy::RoundRobin)
  ));
  assert!(matches!(
    parse_lb_strategy("round_robin"),
    Some(LbStrategy::RoundRobin)
  ));
  assert!(matches!(
    parse_lb_strategy("Round-Robin"),
    Some(LbStrategy::RoundRobin)
  ));
  assert!(matches!(
    parse_lb_strategy("primary-standby"),
    Some(LbStrategy::PrimaryStandby)
  ));
  assert!(matches!(
    parse_lb_strategy("failover"),
    Some(LbStrategy::PrimaryStandby)
  ));
  assert!(matches!(
    parse_lb_strategy("sticky"),
    Some(LbStrategy::Sticky)
  ));
  assert!(parse_lb_strategy("nonsense").is_none());
}

#[test]
fn failover_mode_parsing() {
  assert!(matches!(parse_failover_mode(""), Some(FailoverMode::Fail)));
  assert!(matches!(
    parse_failover_mode("fail"),
    Some(FailoverMode::Fail)
  ));
  assert!(matches!(
    parse_failover_mode("retry"),
    Some(FailoverMode::Retry)
  ));
  assert!(matches!(
    parse_failover_mode("wait"),
    Some(FailoverMode::Wait)
  ));
  assert!(matches!(
    parse_failover_mode("retry_wait"),
    Some(FailoverMode::RetryWait)
  ));
  assert!(parse_failover_mode("bogus").is_none());
}

#[test]
fn override_keys_lists_only_set_fields() {
  let empty = SettingsOverrides::default();
  assert!(override_keys(&empty).is_empty());

  let o = SettingsOverrides {
    lb_strategy: Some("sticky".to_string()),
    max_body_size: Some(1024),
    ..Default::default()
  };
  let keys = override_keys(&o);
  assert_eq!(keys.len(), 2);
  assert!(keys.contains(&"lb_strategy".to_string()));
  assert!(keys.contains(&"max_body_size".to_string()));
}

#[test]
fn config_reload_diff_reports_changes_and_masks_secrets() {
  let old = base_config();
  let mut new = base_config();
  new.max_tunnels = 20;
  new.lb_strategy = LbStrategy::Sticky;
  new.auth_credentials = Some("alice:hunter2".to_string());

  let diff = config_reload_diff(&old, &new);
  let joined = diff.join(" | ");
  assert!(joined.contains("max_tunnels: 10→20"), "{joined}");
  assert!(
    joined.contains("lb_strategy: round-robin→sticky"),
    "{joined}"
  );
  // The secret value must never appear verbatim; only the masked placeholder.
  assert!(joined.contains("auth_credentials:"), "{joined}");
  assert!(!joined.contains("hunter2"), "{joined}");

  // No changes → empty diff.
  assert!(config_reload_diff(&old, &base_config()).is_empty());
}

#[test]
fn apply_settings_overrides_covers_every_field() {
  let base = base_config();
  let o = SettingsOverrides {
    gateway_timeout_secs: Some(100),
    gateway_response_timeout_secs: Some(101),
    max_body_size: Some(2 * 1024 * 1024),
    max_tunnels: Some(42),
    require_hostname_bind: Some(true),
    lb_strategy: Some("primary-standby".to_string()),
    failover_mode: Some("retry-wait".to_string()),
    failover_max_jumps: Some(9),
    failover_window_secs: Some(77),
    failover_all_methods: Some(true),
    client_down_threshold_secs: Some(88),
    ip_limit_max: Some(555.0),
    ip_limit_refill: Some(66.0),
    tunnel_compression: Some(true),
    random_subdomain_suffix: Some("*.preview.example.com".to_string()),
    custom_504_page: Some("<h1>504</h1>".to_string()),
    custom_503_page: Some("<h1>503</h1>".to_string()),
    auth_credentials: Some("bob:s3cret".to_string()),
    cache_enabled: Some(true),
    cache_max_bytes: Some(128 * 1024 * 1024),
    cache_max_stale: Some(7200),
    max_concurrent_requests: Some(250),
    login_lockout_threshold: Some(11),
    login_lockout_secs: Some(120),
    audit_max_size: Some(1234),
    audit_max_files: Some(9),
    ui_language: Some("de".to_string()),
    preview_noindex: Some(true),
  };
  let c = apply_settings_overrides(&base, &o);
  assert_eq!(c.gateway_timeout.as_secs(), 100);
  assert_eq!(c.gateway_response_timeout.as_secs(), 101);
  assert_eq!(c.max_body_size, 2 * 1024 * 1024);
  assert_eq!(c.max_tunnels, 42);
  assert!(c.require_hostname_bind);
  assert!(matches!(c.lb_strategy, LbStrategy::PrimaryStandby));
  assert!(matches!(c.failover_mode, FailoverMode::RetryWait));
  assert_eq!(c.failover_max_jumps, 9);
  assert_eq!(c.failover_window.as_secs(), 77);
  assert!(c.failover_all_methods);
  assert_eq!(c.client_down_threshold.as_secs(), 88);
  assert_eq!(c.ip_limit_max, 555.0);
  assert_eq!(c.ip_limit_refill, 66.0);
  assert!(c.tunnel_compression);
  assert_eq!(
    c.random_subdomain_suffix.as_deref(),
    Some("*.preview.example.com")
  );
  assert_eq!(c.custom_504_page.as_deref(), Some("<h1>504</h1>"));
  assert_eq!(c.custom_503_page.as_deref(), Some("<h1>503</h1>"));
  assert_eq!(c.auth_credentials.as_deref(), Some("bob:s3cret"));
  assert!(c.cache_enabled);
  assert_eq!(c.cache_max_bytes, 128 * 1024 * 1024);
  assert_eq!(c.cache_max_stale, 7200);
  assert_eq!(c.max_concurrent_requests, 250);
  assert_eq!(c.login_lockout_threshold, 11);
  assert_eq!(c.login_lockout_secs, 120);
  assert_eq!(c.audit_max_size, 1234);
  assert_eq!(c.audit_max_files, 9);
  assert_eq!(c.ui_language, "de");
  assert!(c.preview_noindex);
}

#[test]
fn apply_settings_overrides_clamps_below_minimums() {
  let base = base_config();
  let o = SettingsOverrides {
    gateway_timeout_secs: Some(0),
    gateway_response_timeout_secs: Some(0),
    max_body_size: Some(1),
    max_tunnels: Some(0),
    failover_window_secs: Some(0),
    client_down_threshold_secs: Some(0),
    max_concurrent_requests: Some(0),
    login_lockout_threshold: Some(0),
    login_lockout_secs: Some(0),
    ..Default::default()
  };
  let c = apply_settings_overrides(&base, &o);
  assert_eq!(c.gateway_timeout.as_secs(), 1);
  assert_eq!(c.gateway_response_timeout.as_secs(), 1);
  assert_eq!(c.max_body_size, 1024);
  assert_eq!(c.max_tunnels, 1);
  assert_eq!(c.failover_window.as_secs(), 1);
  assert_eq!(c.client_down_threshold.as_secs(), 1);
  assert_eq!(c.max_concurrent_requests, 1);
  assert_eq!(c.login_lockout_threshold, 1);
  assert_eq!(c.login_lockout_secs, 1);
}

#[test]
fn apply_settings_overrides_skips_out_of_range_and_clears_empties() {
  let mut base = base_config();
  base.random_subdomain_suffix = Some("*.old.example.com".to_string());
  base.custom_504_page = Some("<old504>".to_string());
  base.custom_503_page = Some("<old503>".to_string());
  base.auth_credentials = Some("old:creds".to_string());

  let o = SettingsOverrides {
    // Non-positive rate values are rejected, keeping the base.
    ip_limit_max: Some(0.0),
    ip_limit_refill: Some(-1.0),
    // Zero cache budget is rejected (keeps base); stale of 0 is accepted.
    cache_max_bytes: Some(0),
    cache_max_stale: Some(0),
    // Unshipped language is ignored.
    ui_language: Some("xx".to_string()),
    // An invalid random pattern (two wildcards) keeps the previous value.
    random_subdomain_suffix: Some("*.*.bad.example.com".to_string()),
    // Empty strings clear the optional page/cred values.
    custom_504_page: Some(String::new()),
    custom_503_page: Some(String::new()),
    auth_credentials: Some(String::new()),
    ..Default::default()
  };
  let c = apply_settings_overrides(&base, &o);
  assert_eq!(c.ip_limit_max, base.ip_limit_max);
  assert_eq!(c.ip_limit_refill, base.ip_limit_refill);
  assert_eq!(c.cache_max_bytes, base.cache_max_bytes);
  assert_eq!(c.cache_max_stale, 0);
  assert_eq!(c.ui_language, "en");
  // Invalid pattern falls back to the previous suffix.
  assert_eq!(
    c.random_subdomain_suffix.as_deref(),
    Some("*.old.example.com")
  );
  assert!(c.custom_504_page.is_none());
  assert!(c.custom_503_page.is_none());
  assert!(c.auth_credentials.is_none());
}

#[test]
fn apply_settings_overrides_clears_empty_random_suffix() {
  let mut base = base_config();
  base.random_subdomain_suffix = Some("*.old.example.com".to_string());
  let o = SettingsOverrides {
    random_subdomain_suffix: Some("   ".to_string()),
    ..Default::default()
  };
  let c = apply_settings_overrides(&base, &o);
  assert!(c.random_subdomain_suffix.is_none());
}

#[test]
fn config_reload_diff_renders_unset_and_long_values() {
  let old = base_config();
  let mut new = base_config();
  // A long custom page value is summarized by length, not shown verbatim.
  let long = "x".repeat(120);
  new.custom_504_page = Some(long.clone());
  // Clearing a value renders the new side as "unset".
  let mut old2 = base_config();
  old2.custom_503_page = Some("something".to_string());

  let diff = config_reload_diff(&old, &new);
  let joined = diff.join(" | ");
  assert!(joined.contains("custom_504_page:"), "{joined}");
  assert!(joined.contains("<120 chars>"), "{joined}");
  assert!(!joined.contains(&long), "the raw page must not appear");

  let diff2 = config_reload_diff(&old2, &base_config());
  let joined2 = diff2.join(" | ");
  assert!(joined2.contains("custom_503_page:"), "{joined2}");
  assert!(joined2.contains("→unset"), "{joined2}");
}

#[test]
fn settings_view_round_trips_every_strategy_and_mode() {
  for (strat, name) in [
    (LbStrategy::RoundRobin, "round-robin"),
    (LbStrategy::PrimaryStandby, "primary-standby"),
    (LbStrategy::Sticky, "sticky"),
  ] {
    let mut c = base_config();
    c.lb_strategy = strat;
    assert_eq!(settings_view(&c)["lb_strategy"], name);
  }
  for (mode, name) in [
    (FailoverMode::Fail, "fail"),
    (FailoverMode::Retry, "retry"),
    (FailoverMode::Wait, "wait"),
    (FailoverMode::RetryWait, "retry-wait"),
  ] {
    let mut c = base_config();
    c.failover_mode = mode;
    assert_eq!(settings_view(&c)["failover_mode"], name);
  }
}

#[test]
fn file_overrides_default_when_no_document() {
  // With no aperio-server.yaml document loaded, the file layer is empty.
  // (The Some-branch is exercised by config_file's own tests.)
  if crate::config_file::document().is_none() {
    assert!(override_keys(&file_overrides()).is_empty());
  }
}

#[test]
fn apply_settings_overrides_updates_valid_and_skips_invalid() {
  let base = base_config();
  let o = SettingsOverrides {
    gateway_timeout_secs: Some(base.gateway_timeout.as_secs() + 7),
    lb_strategy: Some("sticky".to_string()),
    failover_mode: Some("not-a-mode".to_string()),
    require_hostname_bind: Some(!base.require_hostname_bind),
    ..Default::default()
  };
  let updated = apply_settings_overrides(&base, &o);

  assert_eq!(
    updated.gateway_timeout.as_secs(),
    base.gateway_timeout.as_secs() + 7
  );
  assert!(matches!(updated.lb_strategy, LbStrategy::Sticky));
  // An unparseable failover mode is ignored, leaving the base value.
  assert_eq!(updated.failover_mode, base.failover_mode);
  assert_eq!(updated.require_hostname_bind, !base.require_hostname_bind);
}
