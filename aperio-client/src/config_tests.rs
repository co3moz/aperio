//! What the layered configuration resolves to: the value spellings people write
//! (bandwidth, targets), the order the layers win in, `include:` (precedence,
//! relative paths, cycles), and the command line's own shape.

use super::*;
use clap::Parser;
use serde_yaml::Mapping;

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

/// The command line with nothing on it.
fn no_cli() -> CliArgs {
  CliArgs {
    mode: CliMode::Run,
    target: None,
    local_port: None,
    opts: CommonOpts::default(),
  }
}

fn yaml(text: &str) -> Mapping {
  aperio_config::layers::parse_document(text).unwrap()
}

#[test]
fn a_file_is_checked_and_its_mistake_names_the_file() {
  let dir = config_dir(&[("aperio.yaml", "servr: {url: x}\n")]);
  let Err(err) = parse_config_tree(&dir.join("aperio.yaml")) else {
    panic!("a key nothing reads is refused")
  };
  assert!(
    err.contains("aperio.yaml") && err.contains("did you mean `server`"),
    "{err}"
  );

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
      "include: [shared.yaml]\ndefaults: {timeout: 99s}\nservices:\n  web: {target: 'http://localhost:1'}\n",
    ),
    (
      "shared.yaml",
      "defaults: {timeout: 5s, limits: {redirects: 3}}\nservices:\n  api: {target: 'http://localhost:2'}\n",
    ),
  ]);
  let (doc, files) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  let s = resolve_settings(&no_cli(), &Mapping::new(), &doc).unwrap();
  let api = &s.services[0].entry.policy;
  // A key only the include sets is used.
  assert_eq!(api.limits.as_ref().unwrap().redirects, Some(3));
  // A key both set belongs to the including file.
  assert_eq!(api.timeout.unwrap().as_secs(), 99);
  // Services merge by name, includes first, so a fragment adds a service
  // rather than replacing the list.
  let names: Vec<_> = s
    .services
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
    (
      "conf/base.yaml",
      "include: [nested.yaml]\nname: from_base\n",
    ),
    ("conf/nested.yaml", "reload_drain: 4s\n"),
  ]);
  let (doc, files) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  assert_eq!(doc["name"], serde_yaml::Value::String("from_base".into()));
  assert_eq!(
    doc["reload_drain"],
    serde_yaml::Value::String("4s".into()),
    "the nested include resolved next to its parent"
  );
  assert_eq!(files.len(), 3);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn later_includes_win_over_earlier_ones() {
  let dir = config_dir(&[
    ("aperio.yaml", "include: [a.yaml, b.yaml]\n"),
    ("a.yaml", "name: a\n"),
    ("b.yaml", "name: b\n"),
  ]);
  let (doc, _) = parse_config_tree(&dir.join("aperio.yaml")).unwrap();
  assert_eq!(doc["name"], serde_yaml::Value::String("b".into()));
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
fn a_file_included_by_two_siblings_is_not_a_cycle() {
  // A root that includes two per-environment fragments, both of which include
  // a shared one. This is a diamond, not a cycle: nothing includes itself,
  // directly or transitively, and every real layout with a shared fragment
  // has this shape.
  let dir = config_dir(&[
    (
      "common.yaml",
      "defaults: {limits: {concurrent_requests: 4}}\n",
    ),
    (
      "a.yaml",
      "include: common.yaml\nservices:\n  a: {target: 'http://localhost:3000'}\n",
    ),
    (
      "b.yaml",
      "include: common.yaml\nservices:\n  b: {target: 'http://localhost:4000'}\n",
    ),
    ("root.yaml", "include: [a.yaml, b.yaml]\n"),
  ]);
  let parsed = parse_config_tree(&dir.join("root.yaml"));
  let _ = std::fs::remove_dir_all(&dir);
  let (doc, files) = parsed.expect("a diamond is not a cycle");
  let s = resolve_settings(&no_cli(), &Mapping::new(), &doc).unwrap();
  assert_eq!(s.services.len(), 2, "both fragments contributed");
  assert_eq!(
    s.services[0]
      .entry
      .policy
      .limits
      .as_ref()
      .unwrap()
      .concurrent_requests,
    Some(4)
  );
  // Every file that contributed is watched, including the shared one.
  assert_eq!(files.len(), 4, "{files:?}");
}

#[test]
fn a_real_cycle_is_still_refused() {
  let dir = config_dir(&[
    ("a.yaml", "include: b.yaml\n"),
    ("b.yaml", "include: a.yaml\n"),
  ]);
  let parsed = parse_config_tree(&dir.join("a.yaml"));
  let _ = std::fs::remove_dir_all(&dir);
  let err = parsed.expect_err("a cycle is refused");
  assert!(err.contains("cycle"), "{err}");
}

// --- the command line -------------------------------------------------------

#[test]
fn the_positional_target_is_normalized() {
  let cli = Cli::try_parse_from(["aperio-client", "3000"]).unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(args.mode, CliMode::Run));
  assert_eq!(args.target.as_deref(), Some("http://localhost:3000"));
}

#[test]
fn bind_takes_tunnel_names() {
  let cli = Cli::try_parse_from(["aperio-client", "bind", "pg_main", "acme@redis"]).unwrap();
  let args = cli_to_args(cli);
  assert!(
    matches!(args.mode, CliMode::Bind(ref names) if names == &["pg_main".to_string(), "acme@redis".to_string()])
  );

  // With no name the `binds:` section drives it.
  let cli = Cli::try_parse_from(["aperio-client", "bind", "--config", "x.yaml"]).unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(args.mode, CliMode::Bind(ref names) if names.is_empty()));
  assert_eq!(args.opts.config.as_deref(), Some("x.yaml"));
}

#[test]
fn the_removed_flags_and_aliases_are_refused() {
  // One name per setting: an alias is a second spelling to explain, and a
  // removed flag that still parsed would be a setting silently not in force.
  for flags in [
    &["--bind-tunnels", "x"][..],
    &["--target", "3000"],
    &["--server", "https://x"],
    &["--token", "t"],
    &["--host", "a.example.com"],
    &["--concurrency", "4"],
    &["--public"],
    &["--visitor-auth", "a:b"],
    &["--no-capture"],
    &["--resilience"],
    &["--api-key", "k"],
    &["--egress-proxy", "p:1"],
    &["--ip-family", "ipv4"],
  ] {
    let argv: Vec<&str> = std::iter::once("aperio-client")
      .chain(flags.iter().copied())
      .collect();
    assert!(
      Cli::try_parse_from(&argv).is_err(),
      "{flags:?} still parses"
    );
  }
}

#[test]
fn a_flag_writes_the_key_its_help_names() {
  let cli = Cli::try_parse_from([
    "aperio-client",
    "3000",
    "--hostname",
    "a.example.com,b.example.com",
    "--access",
    "public",
    "--server-proxy",
    "proxy:3128",
  ])
  .unwrap();
  let overlay = cli_to_args(cli).overlay().unwrap();
  assert_eq!(
    overlay.service["target"],
    serde_yaml::Value::String("http://localhost:3000".into())
  );
  assert_eq!(overlay.service["hostname"].as_sequence().unwrap().len(), 2);
  assert_eq!(
    overlay.file["defaults"]["access"],
    serde_yaml::Value::String("public".into())
  );
  assert_eq!(
    overlay.file["server"]["proxy"],
    serde_yaml::Value::String("proxy:3128".into())
  );
}

// --- layering ---------------------------------------------------------------

#[test]
fn a_duration_that_is_not_one_is_reported() {
  // Must be reported, never fatal: this runs on hot-reload too, where a typo
  // saved into aperio.yaml used to take down a client that was serving traffic.
  let Err(err) = resolve_settings(&no_cli(), &Mapping::new(), &yaml("idle_timeout: soon\n")) else {
    panic!("an unparsable idle_timeout must be reported as an error");
  };
  assert!(err.contains("idle_timeout"), "got: {err}");

  // A valid value still resolves, and 0 means "no idle shutdown".
  let s = resolve_settings(&no_cli(), &Mapping::new(), &yaml("idle_timeout: 90s\n")).unwrap();
  assert_eq!(s.idle_timeout, Some(90));
  let s = resolve_settings(&no_cli(), &Mapping::new(), &yaml("idle_timeout: 0\n")).unwrap();
  assert_eq!(s.idle_timeout, None);
}

#[test]
fn test_resolve_settings_layering() {
  // The command line beats the local file; the local file beats the home file.
  let cli = CliArgs {
    mode: CliMode::Run,
    target: Some("http://localhost:9999".to_string()),
    local_port: None,
    opts: CommonOpts {
      server_token: Some("apr_cli".to_string()),
      ..Default::default()
    },
  };
  let home =
    yaml("server: {url: 'https://home.example.com', token: apr_home}\ndefaults: {priority: 3}\n");
  let local = yaml("server: {url: 'https://local.example.com'}\n");

  let s = resolve_settings(&cli, &home, &local).unwrap();
  assert_eq!(s.token.as_deref(), Some("apr_cli")); // the command line wins
  assert_eq!(s.server.as_deref(), Some("https://local.example.com")); // local beats home
  assert!(s.single, "a target on the command line is the one service");
  let service = &s.services[0].entry;
  assert_eq!(service.target.as_deref(), Some("http://localhost:9999"));
  assert_eq!(service.policy.priority, Some(3)); // home fills the gaps
}

#[test]
fn the_home_file_is_a_layer_like_any_other() {
  // `services:` and `tunnels:` in ~/.aperio.yaml used to be read only when the
  // local file had none, and replaced wholesale when it did. They are maps
  // now, and merge by name like everything else.
  let home = yaml(
    "services:\n  web: {target: 'http://localhost:3000'}\n\
     tunnels:\n  mongo: {target: '127.0.0.1:27017'}\n",
  );
  let s = resolve_settings(&no_cli(), &home, &Mapping::new()).unwrap();
  assert_eq!(s.services.len(), 1);
  assert_eq!(s.tunnels.len(), 1);

  let local = yaml("services:\n  api: {target: 'http://localhost:4000'}\n");
  let s = resolve_settings(&no_cli(), &home, &local).unwrap();
  let names: Vec<_> = s.services.iter().filter_map(|s| s.name.clone()).collect();
  assert_eq!(names, vec!["web", "api"]);
}

#[test]
fn a_top_level_service_key_says_where_it_goes_now() {
  for body in [
    "target: http://localhost:3000\n",
    "serve: ./dist\n",
    "hostname: app.example.com\nservices:\n  web: {target: '3000'}\n",
    "tcp_target: 127.0.0.1:5432\n",
  ] {
    let dir = config_dir(&[("aperio.yaml", body)]);
    let Err(err) = parse_config_tree(&dir.join("aperio.yaml")) else {
      panic!("accepted a top-level service: {body:?}");
    };
    assert!(
      err.contains("describes a single service") && err.contains("services:"),
      "the message says where to move it: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
  }
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
    hsts_max_age: Some(aperio_config::units::Span::secs(60)),
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

// ---------------------------------------------------------------------------
// Shell completion (planned_features #70)
// ---------------------------------------------------------------------------

#[test]
fn completions_is_a_mode_that_prints_and_exits() {
  let cli = Cli::parse_from(["aperio-client", "completions", "bash"]);
  let args = cli_to_args(cli);
  assert!(matches!(
    args.mode,
    CliMode::Completions(clap_complete::Shell::Bash)
  ));
  // No target is resolved for it: the script is printed before anything reads
  // a config file, so a completion run in a directory with a broken
  // aperio.yaml still works.
  assert!(args.target.is_none());
}

#[test]
fn every_shell_clap_names_is_accepted() {
  for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
    let cli = Cli::try_parse_from(["aperio-client", "completions", shell]);
    assert!(cli.is_ok(), "{shell} should parse");
  }
  assert!(Cli::try_parse_from(["aperio-client", "completions", "csh"]).is_err());
}

#[test]
fn the_generated_script_mentions_the_subcommands_it_completes() {
  // Generated from the clap definition rather than written by hand, which is
  // the whole point: a hand-written script describes the flags somebody
  // remembered on the day they wrote it. This asserts the generator is
  // actually wired to *this* CLI, not a placeholder.
  let mut command = <Cli as clap::CommandFactory>::command();
  let mut out: Vec<u8> = Vec::new();
  clap_complete::generate(
    clap_complete::Shell::Bash,
    &mut command,
    "aperio-client",
    &mut out,
  );
  let script = String::from_utf8(out).expect("a completion script is text");
  for expected in ["check", "api", "completions", "bind", "--server-url"] {
    assert!(script.contains(expected), "missing {expected}");
  }
}

#[test]
fn init_takes_the_target_and_the_flags_of_a_one_liner() {
  let cli = Cli::try_parse_from([
    "aperio-client",
    "init",
    "3000",
    "--hostname",
    "app.example.com",
    "--service",
    "app",
    "--stdout",
  ])
  .unwrap();
  let args = cli_to_args(cli);
  assert!(matches!(
    args.mode,
    CliMode::Init { ref service, force: false, stdout: true } if service.as_deref() == Some("app")
  ));
  assert_eq!(args.target.as_deref(), Some("http://localhost:3000"));
  assert_eq!(args.opts.hostname.as_deref(), Some("app.example.com"));
}
