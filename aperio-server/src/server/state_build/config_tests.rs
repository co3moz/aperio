//! What this pins down: the defaults build a working config, each setting
//! lands on the field the request path reads, and a value the server cannot
//! honour is refused with the setting's own path in the message rather than
//! silently replaced.

use super::*;

fn dir() -> String {
  let dir = crate::test_support::test_temp_root().join(format!("cfg-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().into_owned()
}

#[test]
fn the_defaults_build() {
  let c = build_config(&ServerSettings::default(), "tok", &dir()).expect("defaults build");
  assert_eq!(c.token, "tok");
  assert_eq!(c.max_body_size, 10 * 1024 * 1024);
  assert_eq!(c.gateway_response_timeout, Duration::from_secs(30));
  assert_eq!(c.max_tunnels, 64);
  assert!(c.inspector && c.access_events && c.request_id_enabled);
  assert_eq!(c.request_id_header, "x-request-id");
  assert_eq!(c.default_access, crate::settings::DefaultAccess::Deny);
  assert!(!c.trust_proxy && !c.secure_cookies);
  assert_eq!(c.metrics_token, None, "no token without metrics");
  assert_eq!(c.max_ws_connections, 10_000);
  assert_eq!(c.shutdown_drain, Some(0));
}

#[test]
fn settings_land_where_the_request_path_reads_them() {
  let s = ServerSettings::from_yaml(
    "limits:\n  request_body: 2MB\n  websockets: 0\nproxy:\n  trusted: [10.0.0.0/8]\n  real_ip_header: CF-Connecting-IP\ncache:\n  enabled: true\n  size: 8MB\nrouting:\n  lb_strategy: sticky\n  random_subdomain: example.com\nshutdown:\n  drain: auto\n",
  )
  .unwrap();
  let c = build_config(&s, "tok", &dir()).unwrap();
  assert_eq!(c.max_body_size, 2 * 1024 * 1024);
  assert_eq!(c.max_ws_connections, usize::MAX, "0 is no cap");
  assert!(c.trust_proxy, "a trusted range implies trust");
  assert!(
    c.secure_cookies,
    "secure cookies follow proxy trust when unset"
  );
  assert_eq!(c.real_ip_header.as_deref(), Some("cf-connecting-ip"));
  assert!(c.cache_enabled);
  assert_eq!(c.cache_max_bytes, 8 * 1024 * 1024);
  assert_eq!(c.lb_strategy, crate::settings::LbStrategy::Sticky);
  assert_eq!(c.random_subdomain_suffix.as_deref(), Some("*.example.com"));
  assert!(c.shutdown_drain_auto);
}

#[test]
fn a_value_the_server_cannot_honour_names_its_setting() {
  for (doc, field) in [
    ("proxy:\n  trusted: [not-an-address]\n", "proxy.trusted"),
    (
      "security:\n  admin_allowed_ips: [10.0.0.0/99]\n",
      "security.admin_allowed_ips",
    ),
    ("security:\n  denied_ips: [bogus]\n", "security.denied_ips"),
    (
      "routing:\n  random_subdomain: \"a.*.b\"\n",
      "routing.random_subdomain",
    ),
    ("dashboard:\n  ui_language: xx\n", "dashboard.ui_language"),
    (
      "request_id:\n  header: \"bad header\"\n",
      "request_id.header",
    ),
    ("logging:\n  sample_rate: 2.0\n", "logging.sample_rate"),
    ("cache:\n  size: 0\n", "cache.size"),
    ("outbound:\n  proxy: \"https://p:1\"\n", "outbound.proxy"),
  ] {
    let s = ServerSettings::from_yaml(doc).unwrap_or_else(|e| panic!("{doc}: {e}"));
    let Err(err) = build_config(&s, "tok", &dir()) else {
      panic!("{doc} should be refused");
    };
    assert!(err.starts_with(field), "{doc}: {err}");
  }
}

#[test]
fn metrics_without_a_token_get_one_kept_in_the_data_directory() {
  let d = dir();
  let s = ServerSettings::from_yaml("metrics:\n  enabled: true\n").unwrap();
  let first = build_config(&s, "tok", &d).unwrap().metrics_token.unwrap();
  let again = build_config(&s, "tok", &d).unwrap().metrics_token.unwrap();
  assert_eq!(first, again, "the same token every time");
  let s = ServerSettings::from_yaml("metrics:\n  enabled: true\n  token: mine\n").unwrap();
  assert_eq!(
    build_config(&s, "tok", &d)
      .unwrap()
      .metrics_token
      .as_deref(),
    Some("mine")
  );
}

#[test]
fn a_rule_that_leaves_its_optional_fields_out_builds() {
  // The rules travel from the document to their modules through yaml, where
  // an unset optional field would arrive as `null` and be refused as the
  // wrong type. Each list here leaves most of its fields out on purpose.
  let s = ServerSettings::from_yaml(concat!(
    "routes:\n",
    "  - hostname: old.example.com\n",
    "    redirect: https://new.example.com\n",
    "  - path: /robots.txt\n",
    "    respond:\n",
    "      body: \"User-agent: *\"\n",
    "rate_limits:\n",
    "  - path: /login\n",
    "    rps: 5\n",
    "fallbacks:\n",
    "  - hostname: app.example.com\n",
    "    url: https://status.example.com\n",
    "waf:\n",
    "  - path: ^/wp-admin\n",
    "maintenance_windows:\n",
    "  - hostname: app.example.com\n",
    "    from: '02:00'\n",
    "    to: '04:00'\n",
    "alerts:\n",
    "  rules:\n",
    "    - name: disk\n",
    "      metric: store_bytes\n",
    "      above: 1000\n",
    "headers:\n",
    "  response:\n",
    "    remove: [Server]\n",
  ))
  .unwrap();
  let c = build_config(&s, "tok", &dir()).unwrap_or_else(|e| panic!("{e}"));
  assert!(!c.static_routes.is_empty());
  assert_eq!(c.route_limits.rules.len(), 1);
  assert!(!c.waf.is_empty());
  assert!(!c.maintenance_windows.is_empty());
  assert_eq!(c.alert_rules.rules().len(), 1);
}
