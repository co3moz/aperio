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
    timeout_secs: 30,
    max_concurrent: None,
    connections: None,
    priority: 0,
    bandwidth: None,
    max_message_size: 32 * 1024 * 1024,
    max_redirects: 5,
    tcp_target: None,
    target_health: None,
    health_interval: 10,
    health_timeout: 5,
    health_threshold: 2,
    public: false,
    visitor_auth: None,
    allowed_ips: Vec::new(),
    headers: None,
    cache: false,
    resilience: false,
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
  // Default is a single connection.
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
  let before = started.clone();
  let mut reloaded = base_settings();
  reloaded.target = None;
  reloaded.services = settings.services.clone();
  for entry in &mut reloaded.services {
    entry.target = None; // as freshly parsed from the config file
  }
  apply_serve_mode(&mut reloaded, &mut started).await.unwrap();
  assert_eq!(before, started);
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
