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

  // Without a value (yaml section drives it) — the id resolves to "".
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

  let s = resolve_settings(&cli, &home, &local);
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
