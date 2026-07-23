use super::*;
use config::ServiceEntry;

fn base_settings() -> ClientSettings {
  ClientSettings {
    token: Some("apr_test".to_string()),
    server: Some("https://tunnel.example.com".to_string()),
    target: Some("http://localhost:3000".to_string()),
    serve: None,
    hostnames: vec!["app.example.com".to_string()],
    path: None,
    trim_bind: None,
    pass_hostname: false,
    max_response_body: 50 * 1024 * 1024,
    max_request_body: None,
    response_timeout: None,
    timeout_secs: 30,
    max_concurrent: None,
    connections: None,
    priority: 0,
    bandwidth: None,
    max_message_size: 32 * 1024 * 1024,
    max_redirects: 5,
    tcp_target: None,
    target_health: None,
    wait_for_backend: false,
    health_interval: 10,
    health_timeout: 5,
    health_threshold: 2,
    public: false,
    visitor_auth: None,
    allowed_ips: Vec::new(),
    headers: None,
    security_headers: None,
    cache: false,
    resilience: false,
    webhook_inbox: false,
    denied: None,
    ip_family: crate::dial::IpFamily::Auto,
    services: Vec::new(),
    client_id: None,
    tunnels: Vec::new(),
    bind_tunnels: std::collections::HashMap::new(),
  }
}

#[test]
fn test_build_specs_tunnels_only() {
  // A client may run with only a tunnels: list — nothing exposed, the
  // connection exists so a peer can bind the declared tunnels.
  let mut settings = base_settings();
  settings.target = None;
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs.len(), 1);
  assert!(specs[0].target.is_empty());
  assert_eq!(specs[0].tunnels.len(), 1);
}

#[test]
fn test_build_specs_tunnels_validation() {
  let mut settings = base_settings();
  // UDP is accepted alongside TCP; anything else is rejected.
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].tunnels[0].protocol, "udp");
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:53".to_string(),
    protocol: "sctp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("only tcp and udp"), "got: {err}");

  // The same target may be declared once per protocol (e.g. DNS tcp+udp).
  settings.tunnels = vec![
    protocol::TunnelDecl {
      target: "127.0.0.1:53".to_string(),
      protocol: "tcp".to_string(),
      encrypt: false,
      psk: None,
      idle_timeout: None,
      expose: None,
    },
    protocol::TunnelDecl {
      target: "127.0.0.1:53".to_string(),
      protocol: "udp".to_string(),
      encrypt: false,
      psk: None,
      idle_timeout: None,
      expose: None,
    },
  ];
  assert!(build_specs(&settings, "base-id", false).is_ok());

  // Targets must be host:port.
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("host:port"), "got: {err}");

  // Duplicates are rejected.
  let decl = protocol::TunnelDecl {
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  };
  settings.tunnels = vec![decl.clone(), decl];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("more than once"), "got: {err}");

  // idle_timeout is udp-only and must be at least 1 second.
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: Some(120),
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("only supported for udp"), "got: {err}");
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: Some(0),
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("at least 1 second"), "got: {err}");
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: Some(300),
    expose: None,
  }];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].tunnels[0].idle_timeout, Some(300));
}

#[test]
fn test_build_specs_single_service() {
  let specs = build_specs(&base_settings(), "base-id", false).unwrap();
  assert_eq!(specs.len(), 1);
  assert_eq!(specs[0].client_id, "base-id");
  assert_eq!(specs[0].target, "http://localhost:3000");
  assert_eq!(specs[0].hostnames, vec!["app.example.com".to_string()]);
  assert!(specs[0].name.is_none());
}

#[test]
fn test_build_specs_multi_service_fallbacks() {
  let mut settings = base_settings();
  settings.timeout_secs = 42;
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      hostname: Some(aperio_config::Hostnames::One("Web.Example.COM".to_string())),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      path: Some("/api".to_string()),
      timeout: Some(7),
      max_concurrent: Some(4),
      ..Default::default()
    },
  ];

  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs.len(), 2);

  // Per-service ids derive from the base id by index (stable across reloads).
  assert_eq!(specs[0].client_id, "base-id-0");
  assert_eq!(specs[1].client_id, "base-id-1");

  // Binds are strictly per entry: the top-level hostname must NOT leak in.
  assert_eq!(specs[0].hostnames, vec!["web.example.com".to_string()]);
  assert!(specs[1].hostnames.is_empty());

  // Tuning knobs fall back to the top-level resolved values.
  assert_eq!(specs[0].timeout_secs, 42);
  assert_eq!(specs[1].timeout_secs, 7);
  assert_eq!(specs[1].max_concurrent, Some(4));

  // trim_bind defaults to true when the entry has a path bind.
  assert!(!specs[0].trim_bind);
  assert!(specs[1].trim_bind);
  assert_eq!(specs[0].name.as_deref(), Some("web"));
}

#[test]
fn test_build_specs_connections() {
  // Default is a single connection; parallelism is opt-in via `connections: N`.
  let specs = build_specs(&base_settings(), "base-id", false).unwrap();
  assert_eq!(specs[0].connections, 1);

  // Configured values pass through; per-entry overrides the top level;
  // out-of-range values are clamped to 16.
  let mut settings = base_settings();
  settings.connections = Some(3);
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      connections: Some(99),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].connections, 3);
  assert_eq!(specs[1].connections, 16);
}

#[test]
fn test_build_specs_cli_target_overrides_services() {
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    target: Some("http://localhost:9000".to_string()),
    ..Default::default()
  }];
  // A positional CLI target forces single-service mode.
  let specs = build_specs(&settings, "base-id", true).unwrap();
  assert_eq!(specs.len(), 1);
  assert_eq!(specs[0].target, "http://localhost:3000");
}

#[test]
fn test_build_specs_missing_service_target_fails() {
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("broken".to_string()),
    ..Default::default()
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("broken"), "got: {err}");
}

#[tokio::test]
async fn test_apply_serve_mode_per_service() {
  let root = std::env::temp_dir().join(format!("aperio-serve-svc-{}", uuid::Uuid::new_v4()));
  let dir_a = root.join("a");
  let dir_b = root.join("b");
  std::fs::create_dir_all(&dir_a).unwrap();
  std::fs::create_dir_all(&dir_b).unwrap();
  let (dir_a, dir_b) = (
    dir_a.to_string_lossy().into_owned(),
    dir_b.to_string_lossy().into_owned(),
  );

  let mut settings = base_settings();
  settings.target = None;
  settings.services = vec![
    ServiceEntry {
      name: Some("a".to_string()),
      serve: Some(dir_a.clone()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("b".to_string()),
      serve: Some(dir_b),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("a2".to_string()),
      serve: Some(dir_a),
      ..Default::default()
    },
  ];
  let mut started = std::collections::HashMap::new();
  apply_serve_mode(&mut settings, &mut started).await.unwrap();

  // Every serve entry is rewritten to a loopback target; distinct
  // directories get distinct servers, the same directory shares one.
  let targets: Vec<String> = settings
    .services
    .iter()
    .map(|e| e.target.clone().unwrap())
    .collect();
  assert!(targets.iter().all(|t| t.starts_with("http://127.0.0.1:")));
  assert_ne!(targets[0], targets[1]);
  assert_eq!(targets[0], targets[2]);
  assert_eq!(started.len(), 2);

  // The rewritten entries build valid specs.
  assert_eq!(build_specs(&settings, "base-id", false).unwrap().len(), 3);

  // A reload with the same directories reuses the running servers.
  let ports = |m: &std::collections::HashMap<String, (u16, tokio::task::JoinHandle<()>)>| {
    let mut v: Vec<(String, u16)> = m.iter().map(|(k, (p, _))| (k.clone(), *p)).collect();
    v.sort();
    v
  };
  let before = ports(&started);
  let mut reloaded = base_settings();
  reloaded.target = None;
  reloaded.services = settings.services.clone();
  for entry in &mut reloaded.services {
    entry.target = None; // as freshly parsed from the config file
  }
  apply_serve_mode(&mut reloaded, &mut started).await.unwrap();
  assert_eq!(before, ports(&started));
  assert_eq!(
    reloaded.services[0].target, settings.services[0].target,
    "the reloaded entry points at the same loopback server"
  );

  let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn test_apply_serve_mode_conflicts() {
  // A services: entry cannot combine serve with a backend target.
  let mut settings = base_settings();
  settings.target = None;
  settings.services = vec![ServiceEntry {
    name: Some("clash".to_string()),
    target: Some("http://localhost:3000".to_string()),
    serve: Some(".".to_string()),
    ..Default::default()
  }];
  let mut started = std::collections::HashMap::new();
  let err = apply_serve_mode(&mut settings, &mut started)
    .await
    .unwrap_err();
  assert!(
    err.contains("clash") && err.contains("serve together with"),
    "got: {err}"
  );

  // The top-level serve still refuses a services: list — it drives
  // single-service mode; per-service serving lives on the entries.
  let mut settings = base_settings();
  settings.target = None;
  settings.serve = Some(".".to_string());
  settings.services = vec![ServiceEntry {
    target: Some("http://localhost:3000".to_string()),
    ..Default::default()
  }];
  let err = apply_serve_mode(&mut settings, &mut started)
    .await
    .unwrap_err();
  assert!(err.contains("single-service mode"), "got: {err}");
}

#[test]
fn test_multi_hostname_list() {
  // A service may claim several hostnames via a list; the first is the
  // primary and all are normalized to lowercase.
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    target: Some("http://localhost:9000".to_string()),
    hostname: Some(aperio_config::Hostnames::Many(vec![
      "App.Example.com".to_string(),
      "www.example.com".to_string(),
    ])),
    ..Default::default()
  }];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(
    specs[0].hostnames,
    vec!["app.example.com".to_string(), "www.example.com".to_string()]
  );
}

/// Installs a process-wide TRACE subscriber once so `info!`/`warn!`/`error!`
/// argument expressions are evaluated (and covered) during tests.
fn init_tracing() {
  use std::sync::Once;
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    let _ = tracing_subscriber::fmt()
      .with_max_level(tracing::Level::TRACE)
      .with_test_writer()
      .try_init();
  });
}

fn tcp_tunnel(target: &str) -> protocol::TunnelDecl {
  protocol::TunnelDecl {
    target: target.to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }
}

// ---------------------------------------------------------------------------
// build_specs: validation error branches.
// ---------------------------------------------------------------------------

#[test]
fn test_build_specs_requires_token_and_server() {
  init_tracing();
  let mut settings = base_settings();
  settings.token = None;
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("tunnel token is required"), "got: {err}");

  let mut settings = base_settings();
  settings.server = None;
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("server URL is required"), "got: {err}");

  // A malformed server URL fails the WebSocket-URL build.
  let mut settings = base_settings();
  settings.server = Some("ftp://nope".to_string());
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("WebSocket URL"), "got: {err}");
}

#[test]
fn test_build_specs_invalid_allowed_ips() {
  init_tracing();
  // Client-level invalid allowlist entry.
  let mut settings = base_settings();
  settings.allowed_ips = vec!["not-an-ip".to_string()];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("allowed_ips"), "got: {err}");

  // Per-service invalid allowlist entry.
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("svc".to_string()),
    target: Some("http://localhost:3000".to_string()),
    allowed_ips: Some(vec!["999.999.0.0/8".to_string()]),
    ..Default::default()
  }];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(
    err.contains("svc") && err.contains("allowed_ips"),
    "got: {err}"
  );

  // A valid mixture (IP, CIDR, '*') builds fine.
  let mut settings = base_settings();
  settings.allowed_ips = vec![
    "10.0.0.1".to_string(),
    "192.168.0.0/16".to_string(),
    "*".to_string(),
  ];
  assert!(build_specs(&settings, "id", false).is_ok());
}

#[cfg(unix)]
#[test]
fn test_build_specs_invalid_unix_target() {
  init_tracing();
  // A unix:// target without a socket path is rejected.
  let mut settings = base_settings();
  settings.target = Some("unix://".to_string());
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(
    err.contains("unix://") && err.contains("socket path"),
    "got: {err}"
  );

  // Per-service unix target without a path.
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("sock".to_string()),
    target: Some("unix://".to_string()),
    ..Default::default()
  }];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("sock"), "got: {err}");

  // A well-formed unix target passes validation.
  let mut settings = base_settings();
  settings.target = Some("unix:///tmp/app.sock".to_string());
  assert!(build_specs(&settings, "id", false).is_ok());
}

#[test]
fn test_build_specs_invalid_denied() {
  init_tracing();
  // Client-level denied must be an absolute http(s) URL.
  let mut settings = base_settings();
  settings.denied = Some("/relative".to_string());
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("denied"), "got: {err}");

  // Per-service denied is validated too.
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("d".to_string()),
    target: Some("http://localhost:3000".to_string()),
    denied: Some("ftp://x".to_string()),
    ..Default::default()
  }];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("d") && err.contains("denied"), "got: {err}");

  // A valid absolute URL is accepted and propagated.
  let mut settings = base_settings();
  settings.denied = Some("https://example.com/denied".to_string());
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(
    specs[0].denied.as_deref(),
    Some("https://example.com/denied")
  );
}

#[test]
fn test_build_specs_invalid_bandwidth_warns() {
  init_tracing();
  // An unparseable bandwidth value is ignored (warned) rather than fatal.
  let mut settings = base_settings();
  settings.bandwidth = Some("not-a-rate".to_string());
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].bandwidth_bps, None);

  // A valid value parses through.
  let mut settings = base_settings();
  settings.bandwidth = Some("8mbit".to_string());
  let specs = build_specs(&settings, "id", false).unwrap();
  assert!(specs[0].bandwidth_bps.is_some());
}

#[test]
fn test_build_specs_server_urls_failover() {
  init_tracing();
  // APERIO_SERVER_URLS adds failover candidates; duplicates and invalid
  // entries are skipped/warned.
  // SAFETY: the var is set and cleared within this test.
  unsafe {
    std::env::set_var(
      "APERIO_SERVER_URLS",
      "https://backup.example.com, https://tunnel.example.com, ::not a url",
    );
  }
  let specs = build_specs(&base_settings(), "id", false).unwrap();
  unsafe { std::env::remove_var("APERIO_SERVER_URLS") };
  // Primary + the one new valid backup (duplicate primary and the invalid
  // entry are dropped).
  assert!(specs[0].ws_urls.len() >= 2, "urls: {:?}", specs[0].ws_urls);
  assert!(
    specs[0]
      .ws_urls
      .iter()
      .any(|u| u.contains("backup.example.com"))
  );
}

#[test]
fn test_build_specs_clamps_connections_warn() {
  init_tracing();
  // Single-service mode clamps an out-of-range top-level connections value.
  let mut settings = base_settings();
  settings.connections = Some(50);
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].connections, 16);
}

// ---------------------------------------------------------------------------
// validate_tunnels: encrypt/psk/expose edge cases.
// ---------------------------------------------------------------------------

#[test]
fn test_validate_tunnels_encrypt_and_expose() {
  init_tracing();
  let mut settings = base_settings();

  // encrypt on a udp tunnel is rejected.
  let mut d = tcp_tunnel("127.0.0.1:5432");
  d.protocol = "udp".to_string();
  d.encrypt = true;
  settings.tunnels = vec![d];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("only supported for tcp"), "got: {err}");

  // psk without encrypt is rejected.
  let mut d = tcp_tunnel("127.0.0.1:5432");
  d.psk = Some("k".to_string());
  settings.tunnels = vec![d];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("psk without encrypt"), "got: {err}");

  // A valid encrypted tcp tunnel with a psk passes.
  let mut d = tcp_tunnel("127.0.0.1:5432");
  d.encrypt = true;
  d.psk = Some("k".to_string());
  settings.tunnels = vec![d];
  assert!(build_specs(&settings, "id", false).is_ok());

  // expose on a non-tcp tunnel is rejected.
  let mut d = tcp_tunnel("127.0.0.1:53");
  d.protocol = "udp".to_string();
  d.expose = Some("0.0.0.0:53".to_string());
  settings.tunnels = vec![d];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("only supported for tcp"), "got: {err}");

  // expose together with encrypt is rejected.
  let mut d = tcp_tunnel("127.0.0.1:5432");
  d.encrypt = true;
  d.expose = Some("0.0.0.0:5432".to_string());
  settings.tunnels = vec![d];
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("expose together with encrypt"), "got: {err}");

  // A plain exposed tcp tunnel passes and is normalized through.
  let mut d = tcp_tunnel("127.0.0.1:5432");
  d.expose = Some("0.0.0.0:5432".to_string());
  settings.tunnels = vec![d];
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].tunnels[0].expose.as_deref(), Some("0.0.0.0:5432"));
}

#[test]
fn test_build_specs_requires_target() {
  init_tracing();
  // No target, no tunnels, no services → the target is mandatory.
  let mut settings = base_settings();
  settings.target = None;
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("target is required"), "got: {err}");
}

#[test]
fn test_build_specs_single_service_path_trim_bind() {
  init_tracing();
  // A top-level path bind defaults trim_bind to true in single-service mode.
  let mut settings = base_settings();
  settings.path = Some("/api".to_string());
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].path.as_deref(), Some("/api"));
  assert!(specs[0].trim_bind);
}

// ---------------------------------------------------------------------------
// apply_serve_mode: top-level serve.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_apply_serve_mode_top_level() {
  init_tracing();
  let dir = std::env::temp_dir().join(format!("aperio-serve-top-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir = dir.to_string_lossy().into_owned();

  // Top-level serve with no target rewrites the top-level target.
  let mut settings = base_settings();
  settings.target = None;
  settings.serve = Some(dir.clone());
  let mut started = std::collections::HashMap::new();
  apply_serve_mode(&mut settings, &mut started).await.unwrap();
  assert!(settings.target.unwrap().starts_with("http://127.0.0.1:"));
  assert_eq!(started.len(), 1);

  // Top-level serve together with a target is a conflict.
  let mut settings = base_settings();
  settings.serve = Some(dir.clone());
  // base_settings sets target, so this is the mutual-exclusion path.
  let err = apply_serve_mode(&mut settings, &mut started)
    .await
    .unwrap_err();
  assert!(err.contains("mutually exclusive"), "got: {err}");

  // A services entry without serve is skipped by the serve rewrite.
  let mut settings = base_settings();
  settings.target = None;
  settings.services = vec![ServiceEntry {
    name: Some("plain".to_string()),
    target: Some("http://localhost:3000".to_string()),
    serve: None,
    ..Default::default()
  }];
  apply_serve_mode(&mut settings, &mut started).await.unwrap();
  assert_eq!(
    settings.services[0].target.as_deref(),
    Some("http://localhost:3000"),
    "a non-serve entry is left untouched"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// spawn_services
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_spawn_services_derives_connection_ids() {
  init_tracing();
  let mut settings = base_settings();
  settings.connections = Some(3);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  let shared = Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
  };
  let running = spawn_services(&specs, &shared);
  // One spec with connections: 3 → three service tasks.
  assert_eq!(running.len(), 3);
  // Cancel them all and let them wind down (they never connect).
  for (cancel_tx, _) in &running {
    let _ = cancel_tx.send(true);
  }
  for (_, task) in running {
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
  }
}

// ---------------------------------------------------------------------------
// log_spec
// ---------------------------------------------------------------------------

#[test]
fn test_log_spec_all_branches() {
  init_tracing();
  // A richly configured named service touches every optional log line.
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("web".to_string()),
    target: Some("http://localhost:3000".to_string()),
    path: Some("/api".to_string()),
    hostname: Some(aperio_config::Hostnames::Many(vec![
      "a.example.com".to_string(),
      "b.example.com".to_string(),
    ])),
    max_concurrent: Some(8),
    priority: Some(5),
    bandwidth: Some("8mbit".to_string()),
    connections: Some(4),
    tcp_target: Some("127.0.0.1:5432".to_string()),
    public: Some(true),
    auth: Some("user:pass".to_string()),
    ..Default::default()
  }];
  settings.tunnels = vec![tcp_tunnel("127.0.0.1:6000")];
  // Multiple failover servers so the failover log line runs.
  unsafe { std::env::set_var("APERIO_SERVER_URLS", "https://backup.example.com") };
  let specs = build_specs(&settings, "id", false).unwrap();
  unsafe { std::env::remove_var("APERIO_SERVER_URLS") };
  for spec in &specs {
    log_spec(spec);
  }

  // The single, unnamed, tunnels-only variant: empty target + single hostname.
  let mut settings = base_settings();
  settings.target = None;
  settings.hostnames = vec!["only.example.com".to_string()];
  settings.tunnels = vec![tcp_tunnel("127.0.0.1:6001")];
  let specs = build_specs(&settings, "id", false).unwrap();
  log_spec(&specs[0]);

  // A plain single service with no hostnames at all.
  let mut settings = base_settings();
  settings.hostnames = Vec::new();
  let specs = build_specs(&settings, "id", false).unwrap();
  log_spec(&specs[0]);
}
