use super::*;

#[test]
fn test_parse_bandwidth() {
  assert_eq!(parse_bandwidth("8mbit"), Some(1_000_000));
  assert_eq!(parse_bandwidth("1gbit"), Some(125_000_000));
  assert_eq!(parse_bandwidth("500kbit"), Some(62_500));
  assert_eq!(parse_bandwidth("2MB"), Some(2_000_000));
  assert_eq!(parse_bandwidth("100kb"), Some(100_000));
  assert_eq!(parse_bandwidth("1.5mbit"), Some(187_500));
  assert_eq!(parse_bandwidth("125000"), Some(125_000));
  assert_eq!(parse_bandwidth("8 Mbit"), Some(1_000_000));
  assert_eq!(parse_bandwidth("0"), None);
  assert_eq!(parse_bandwidth("-5mbit"), None);
  assert_eq!(parse_bandwidth("fast"), None);
}

#[test]
fn test_normalize_target() {
  // A bare port exposes localhost.
  assert_eq!(normalize_target("3000"), "http://localhost:3000");
  assert_eq!(normalize_target(" 8080 "), "http://localhost:8080");
  // A bare hostname gets an http scheme.
  assert_eq!(normalize_target("example.com"), "http://example.com");
  assert_eq!(
    normalize_target("example.com:9000"),
    "http://example.com:9000"
  );
  // Full URLs pass through untouched.
  assert_eq!(
    normalize_target("https://example.com"),
    "https://example.com"
  );
}

#[test]
fn test_file_config_server_forms() {
  // Canonical nested form.
  let nested: FileConfig = serde_yaml::from_str(
    "server:\n  url: https://tunnel.example.com\n  token: apr_nested\ntarget: http://localhost:3000\n",
  )
  .unwrap();
  assert_eq!(
    nested.server_url().as_deref(),
    Some("https://tunnel.example.com")
  );
  assert_eq!(nested.server_token().as_deref(), Some("apr_nested"));

  // Legacy flat form keeps working.
  let flat: FileConfig = serde_yaml::from_str(
    "server: https://tunnel.example.com\ntoken: apr_flat\ntarget: http://localhost:3000\n",
  )
  .unwrap();
  assert_eq!(
    flat.server_url().as_deref(),
    Some("https://tunnel.example.com")
  );
  assert_eq!(flat.server_token().as_deref(), Some("apr_flat"));

  // Nested url with legacy top-level token.
  let mixed: FileConfig =
    serde_yaml::from_str("server:\n  url: https://t.example.com\ntoken: apr_mixed\n").unwrap();
  assert_eq!(mixed.server_token().as_deref(), Some("apr_mixed"));
}

#[test]
fn test_health_group_folds_into_the_flat_fields() {
  // The grouped form is what a new config writes.
  let mut nested: FileConfig = serde_yaml::from_str(
    "target: http://localhost:3000\nhealth:\n  endpoint: /healthz\n  interval: 7\n  timeout: 3\n  threshold: 4\n  wait_for_backend: true\n",
  )
  .unwrap();
  let deprecated = nested.fold_groups();
  assert!(deprecated.is_empty(), "nothing deprecated in the new form");
  assert_eq!(nested.target_health.as_deref(), Some("/healthz"));
  assert_eq!(nested.health_interval, Some(7));
  assert_eq!(nested.health_timeout, Some(3));
  assert_eq!(nested.health_threshold, Some(4));
  assert_eq!(nested.wait_for_backend, Some(true));

  // The flat spelling still works, and is reported so the operator can move.
  let mut flat: FileConfig = serde_yaml::from_str(
    "target: http://localhost:3000\ntarget_health: /health\nhealth_interval: 9\n",
  )
  .unwrap();
  let deprecated = flat.fold_groups();
  assert_eq!(flat.target_health.as_deref(), Some("/health"));
  assert_eq!(flat.health_interval, Some(9));
  let old: Vec<&str> = deprecated.iter().map(|k| k.old).collect();
  assert_eq!(old, vec!["target_health", "health_interval"]);
  assert_eq!(deprecated[1].new, "health.interval");

  // Both spellings at once: the block wins, field by field, and the flat key
  // left over is still reported.
  let mut mixed: FileConfig = serde_yaml::from_str(
    "target: http://localhost:3000\nhealth_interval: 9\nhealth_timeout: 2\nhealth:\n  interval: 30\n",
  )
  .unwrap();
  let deprecated = mixed.fold_groups();
  assert_eq!(mixed.health_interval, Some(30), "the block wins");
  assert_eq!(mixed.health_timeout, Some(2), "the flat key still applies");
  assert_eq!(deprecated.len(), 2);
}

#[test]
fn test_health_group_folds_per_service_entry() {
  let mut cfg: FileConfig = serde_yaml::from_str(
    "services:\n  - name: api\n    target: http://localhost:4000\n    health:\n      endpoint: /up\n      threshold: 5\n  - name: web\n    target: http://localhost:3000\n    health_timeout: 8\n",
  )
  .unwrap();
  let deprecated = cfg.fold_groups();
  let services = cfg.services.unwrap();
  assert_eq!(services[0].target_health.as_deref(), Some("/up"));
  assert_eq!(services[0].health_threshold, Some(5));
  assert_eq!(services[1].health_timeout, Some(8));
  assert_eq!(
    deprecated.iter().map(|k| k.old).collect::<Vec<_>>(),
    vec!["health_timeout"],
    "only the entry using the old spelling is reported"
  );
}

/// Writes files into a fresh temp directory and returns it.
fn config_dir(files: &[(&str, &str)]) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-cfg-{}", uuid::Uuid::new_v4()));
  for (name, body) in files {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
  }
  dir
}

#[test]
fn test_parse_config_tree_folds_groups() {
  // The reload path must apply the same folding pass as the initial load: a
  // file written in the grouped `health:` form used to lose the whole block
  // on reload, silently turning health probing off.
  let dir = config_dir(&[(
    "aperio.yaml",
    "target: http://localhost:3000\nhealth:\n  endpoint: /healthz\n  interval: 7\n",
  )]);
  let (cfg, files) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  assert_eq!(cfg.target_health.as_deref(), Some("/healthz"));
  assert_eq!(cfg.health_interval, Some(7));
  assert_eq!(files.len(), 1, "one file contributed");

  // Unparseable input reports the error instead of panicking, so the
  // supervisor keeps the previous configuration.
  let bad = config_dir(&[("aperio.yaml", ": not yaml")]);
  assert!(parse_config_tree(&bad.join("aperio.yaml")).is_err());
  let _ = std::fs::remove_dir_all(&dir);
  let _ = std::fs::remove_dir_all(&bad);
}

// --- include: (planned_features #41) ----------------------------------------

#[test]
fn an_include_contributes_its_settings_and_the_including_file_wins() {
  let dir = config_dir(&[
    (
      "aperio.yaml",
      "include: [shared.yaml]\ntimeout: 99\nservices:\n  - name: web\n    target: http://localhost:1\n",
    ),
    (
      "shared.yaml",
      "timeout: 5\nmax_redirects: 3\nservices:\n  - name: api\n    target: http://localhost:2\n",
    ),
  ]);
  let (cfg, files) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  // A key only the include sets is used.
  assert_eq!(cfg.max_redirects, Some(3));
  // A key both set belongs to the including file.
  assert_eq!(cfg.timeout, Some(99));
  // Services concatenate, includes first, so a fragment adds rather than
  // replaces.
  let names: Vec<_> = cfg
    .services
    .as_ref()
    .expect("both files contributed services")
    .iter()
    .map(|s| s.name.clone().unwrap_or_default())
    .collect();
  assert_eq!(names, vec!["api", "web"]);
  assert_eq!(files.len(), 2, "both files are reported for watching");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn includes_resolve_relative_to_the_file_that_wrote_them() {
  // Not to the working directory: a fragment has to mean the same thing
  // whichever directory the client is started from.
  let dir = config_dir(&[
    ("aperio.yaml", "include: [conf/base.yaml]\n"),
    ("conf/base.yaml", "include: [nested.yaml]\ntimeout: 11\n"),
    ("conf/nested.yaml", "max_redirects: 4\n"),
  ]);
  let (cfg, files) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  assert_eq!(cfg.timeout, Some(11));
  assert_eq!(
    cfg.max_redirects,
    Some(4),
    "the nested include resolved next to its parent"
  );
  assert_eq!(files.len(), 3);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn later_includes_win_over_earlier_ones() {
  let dir = config_dir(&[
    ("aperio.yaml", "include: [a.yaml, b.yaml]\n"),
    ("a.yaml", "timeout: 1\n"),
    ("b.yaml", "timeout: 2\n"),
  ]);
  let (cfg, _) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  assert_eq!(cfg.timeout, Some(2));
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cycle_is_reported_rather_than_followed() {
  let dir = config_dir(&[
    ("aperio.yaml", "include: [loop.yaml]\n"),
    ("loop.yaml", "include: [aperio.yaml]\n"),
  ]);
  let Err(err) = parse_config_tree(&dir.join("aperio.yaml")) else {
    panic!("expected an error")
  };
  assert!(err.contains("cycle"), "got {err}");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_or_malformed_include_is_an_error_naming_the_file() {
  let dir = config_dir(&[("aperio.yaml", "include: [nope.yaml]\n")]);
  let Err(err) = parse_config_tree(&dir.join("aperio.yaml")) else {
    panic!("expected an error")
  };
  assert!(
    err.contains("nope.yaml"),
    "the message names the file: {err}"
  );

  let bad = config_dir(&[("aperio.yaml", "include: 42\n")]);
  let Err(err) = parse_config_tree(&bad.join("aperio.yaml")) else {
    panic!("expected an error")
  };
  assert!(err.contains("`include:`"), "got {err}");
  let _ = std::fs::remove_dir_all(&dir);
  let _ = std::fs::remove_dir_all(&bad);
}

#[test]
fn test_target_flag_accepted_by_subcommands() {
  // `check` (and every mode) accepts --target as an alternative to the
  // positional argument, with the same normalization.
  let cli = Cli::try_parse_from([
    "aperio-client",
    "check",
    "--target",
    "https://rep.example.com",
  ])
  .unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(args.mode, CliMode::Check));
  assert_eq!(args.target.as_deref(), Some("https://rep.example.com"));

  // In run mode the positional wins over --target.
  let cli = Cli::try_parse_from(["aperio-client", "3000", "--target", "4000"]).unwrap();
  let args = cli_to_args(cli);
  assert_eq!(args.target.as_deref(), Some("http://localhost:3000"));

  // --target alone works in run mode and is normalized like the positional.
  let cli = Cli::try_parse_from(["aperio-client", "--target", "3000"]).unwrap();
  let args = cli_to_args(cli);
  assert_eq!(args.target.as_deref(), Some("http://localhost:3000"));
}

#[test]
fn test_bind_tunnels_flag_parsing() {
  // With an explicit client id.
  let cli = Cli::try_parse_from(["aperio-client", "--bind-tunnels", "client-1"]).unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(args.mode, CliMode::BindTunnels(ref id) if id == "client-1"));

  // Without a value (yaml section drives it), the id resolves to "".
  let cli = Cli::try_parse_from(["aperio-client", "--bind-tunnels"]).unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(args.mode, CliMode::BindTunnels(ref id) if id.is_empty()));

  // A following flag is not swallowed as the value.
  let cli = Cli::try_parse_from(["aperio-client", "--bind-tunnels", "--config", "x.yaml"]).unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(args.mode, CliMode::BindTunnels(ref id) if id.is_empty()));
  assert_eq!(args.opts.config.as_deref(), Some("x.yaml"));

  // Conflicts with a positional target.
  assert!(Cli::try_parse_from(["aperio-client", "3000", "--bind-tunnels", "c"]).is_err());
}

#[test]
fn test_resolve_settings_reports_invalid_idle_timeout() {
  let cli = CliArgs {
    mode: CliMode::Run,
    target: None,
    local_port: None,
    opts: Default::default(),
  };
  let local: FileConfig = serde_yaml::from_str("idle_timeout: not-a-duration\n").unwrap();
  // Must be reported, never fatal: this runs on hot-reload too, where a typo
  // saved into aperio.yaml used to take down a client that was serving traffic.
  let Err(err) = resolve_settings(&cli, &Default::default(), &local) else {
    panic!("an unparsable idle_timeout must be reported as an error");
  };
  assert!(err.contains("idle_timeout"), "got: {err}");

  // A valid value still resolves, and 0 means "no idle shutdown".
  let local: FileConfig = serde_yaml::from_str("idle_timeout: 90s\n").unwrap();
  let s = resolve_settings(&cli, &Default::default(), &local).unwrap();
  assert_eq!(s.idle_timeout, Some(90));
  let local: FileConfig = serde_yaml::from_str("idle_timeout: 0\n").unwrap();
  let s = resolve_settings(&cli, &Default::default(), &local).unwrap();
  assert_eq!(s.idle_timeout, None);
}

#[test]
fn test_resolve_settings_layering() {
  // CLI beats the local file; the local file beats the home file.
  let cli = CliArgs {
    mode: CliMode::Run,
    target: Some("http://localhost:9999".to_string()),
    local_port: None,
    opts: CommonOpts {
      server_token: Some("apr_cli".to_string()),
      ..Default::default()
    },
  };
  let home: FileConfig = serde_yaml::from_str(
    "server:\n  url: https://home.example.com\n  token: apr_home\nhostname: home.example.com\npriority: 3\n",
  )
  .unwrap();
  let local: FileConfig =
    serde_yaml::from_str("server:\n  url: https://local.example.com\ntarget: http://localhost:1\n")
      .unwrap();

  let s = resolve_settings(&cli, &home, &local).unwrap();
  assert_eq!(s.token.as_deref(), Some("apr_cli")); // CLI wins
  assert_eq!(s.server.as_deref(), Some("https://local.example.com")); // local file beats home
  assert_eq!(s.target.as_deref(), Some("http://localhost:9999")); // positional beats local
  assert_eq!(s.hostnames, vec!["home.example.com".to_string()]); // home fills the gaps
  assert_eq!(s.priority, 3);
  // Defaults apply when no layer sets a value.
  assert_eq!(s.timeout_secs, 30);
  assert_eq!(s.max_redirects, 5);
  assert_eq!(s.max_response_body, 50 * 1024 * 1024);
}

#[test]
fn test_build_ws_url() {
  assert_eq!(
    build_ws_url("http://localhost:8080").unwrap(),
    "ws://localhost:8080/aperio/ws"
  );
  assert_eq!(
    build_ws_url("https://example.com").unwrap(),
    "wss://example.com/aperio/ws"
  );
  assert_eq!(
    build_ws_url("ws://localhost:8080").unwrap(),
    "ws://localhost:8080/aperio/ws"
  );
  assert_eq!(
    build_ws_url("localhost:8080").unwrap(),
    "ws://localhost:8080/aperio/ws"
  );
  assert!(build_ws_url("ftp://localhost").is_err());
}

#[test]
fn test_split_ip_list() {
  assert_eq!(
    // Deliberately messy: a space before a comma and two empty entries, which
    // is what this function exists to survive. An earlier sweep tidied the
    // input and quietly took the coverage with it.
    split_ip_list(" 203.0.113.7, 10.0.0.0/8 ,,"),
    vec!["203.0.113.7".to_string(), "10.0.0.0/8".to_string()]
  );
  assert!(split_ip_list("").is_empty());
}

#[test]
fn test_valid_ip_entry() {
  assert!(valid_ip_entry("203.0.113.7"));
  assert!(valid_ip_entry("10.0.0.0/8"));
  assert!(valid_ip_entry("2001:db8::/32"));
  assert!(valid_ip_entry("2001:db8::1"));
  assert!(valid_ip_entry("*"));
  assert!(valid_ip_entry(" 127.0.0.1 "));
  assert!(!valid_ip_entry("10.0.0.0/33"));
  assert!(!valid_ip_entry("2001:db8::/129"));
  assert!(!valid_ip_entry("not-an-ip"));
  assert!(!valid_ip_entry("10.0.0/8"));
  assert!(!valid_ip_entry(""));
}

#[test]
fn test_merge_security_headers() {
  use aperio_config::{SecurityHeaderOptions, SecurityHeaders};

  // No preset: rules pass through untouched.
  assert!(merge_security_headers(None, None).is_none());

  // Flag preset injects the standard set.
  let rules = merge_security_headers(None, Some(&SecurityHeaders::Flag(true))).unwrap();
  let add = rules.response.unwrap().add;
  assert_eq!(
    add.get("Strict-Transport-Security").map(String::as_str),
    Some("max-age=63072000")
  );
  assert_eq!(add.get("X-Frame-Options").map(String::as_str), Some("DENY"));
  assert_eq!(
    add.get("X-Content-Type-Options").map(String::as_str),
    Some("nosniff")
  );
  assert_eq!(
    add.get("Referrer-Policy").map(String::as_str),
    Some("strict-origin-when-cross-origin")
  );
  assert!(!add.contains_key("Content-Security-Policy"));

  // `false` injects nothing (a service can opt out of a top-level preset).
  assert!(merge_security_headers(None, Some(&SecurityHeaders::Flag(false))).is_none());

  // Detailed selection injects only what is set.
  let detailed = SecurityHeaders::Detailed(SecurityHeaderOptions {
    hsts: Some(true),
    hsts_max_age: Some(60),
    csp: Some("default-src 'self'".to_string()),
    ..Default::default()
  });
  let rules = merge_security_headers(None, Some(&detailed)).unwrap();
  let add = rules.response.unwrap().add;
  assert_eq!(
    add.get("Strict-Transport-Security").map(String::as_str),
    Some("max-age=60")
  );
  assert_eq!(
    add.get("Content-Security-Policy").map(String::as_str),
    Some("default-src 'self'")
  );
  assert!(!add.contains_key("X-Frame-Options"));

  // Explicit headers: rules win over the preset (case-insensitively).
  let mut user = aperio_config::HeaderRules::default();
  let mut dir = aperio_config::HeaderDirectives::default();
  dir
    .add
    .insert("x-frame-options".to_string(), "SAMEORIGIN".to_string());
  dir.remove.push("Referrer-Policy".to_string());
  user.response = Some(dir);
  let rules = merge_security_headers(Some(user), Some(&SecurityHeaders::Flag(true))).unwrap();
  let resp = rules.response.unwrap();
  assert_eq!(
    resp.add.get("x-frame-options").map(String::as_str),
    Some("SAMEORIGIN")
  );
  assert!(!resp.add.contains_key("X-Frame-Options"));
  assert!(!resp.add.contains_key("Referrer-Policy"));
  assert!(resp.add.contains_key("Strict-Transport-Security"));
}

#[test]
fn test_home_config_supplies_the_list_sections() {
  // `services:`, `tunnels:` and `bind-tunnels:` used to be read from the
  // local file only, so declaring them in ~/.aperio.yaml did nothing at all
  // while every neighbouring key layered normally.
  let cli = CliArgs {
    mode: CliMode::Run,
    target: None,
    local_port: None,
    opts: CommonOpts::default(),
  };
  let home: FileConfig = serde_yaml::from_str(
    "server:\n  url: https://home.example.com\n  token: apr_home\n\
     services:\n  - name: web\n    target: http://localhost:3000\n\
     tunnels:\n  - target: 127.0.0.1:27017\n",
  )
  .unwrap();

  // Home alone: its sections are used.
  let s = resolve_settings(&cli, &home, &FileConfig::default()).unwrap();
  assert_eq!(s.services.len(), 1);
  assert_eq!(s.services[0].name.as_deref(), Some("web"));
  assert_eq!(s.tunnels.len(), 1);

  // A local file that declares its own replaces the home one wholesale,
  // rather than merging entry by entry.
  let local: FileConfig = serde_yaml::from_str(
    "services:\n  - name: api\n    target: http://localhost:4000\n  - name: docs\n    target: http://localhost:5000\n",
  )
  .unwrap();
  let s = resolve_settings(&cli, &home, &local).unwrap();
  assert_eq!(s.services.len(), 2);
  assert_eq!(s.services[0].name.as_deref(), Some("api"));
  // A section the local file does not mention still comes from home.
  assert_eq!(s.tunnels.len(), 1);
}
