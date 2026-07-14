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
    cache_enabled: false,
    max_concurrent_requests: 100,
    login_lockout_threshold: 5,
    login_lockout_secs: 60,
    audit_max_size: 10 * 1024 * 1024,
    audit_max_files: 3,
    ui_language: "en".to_string(),
    header_rules: Default::default(),
    static_routes: Default::default(),
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
