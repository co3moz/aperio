use super::*;
use config::ServiceEntry;

fn base_settings() -> ClientSettings {
  ClientSettings {
    token: Some("apr_test".to_string()),
    server: Some("https://tunnel.example.com".to_string()),
    target: Some("http://localhost:3000".to_string()),
    hostname: Some("app.example.com".to_string()),
    path: None,
    trim_bind: None,
    pass_hostname: false,
    max_response_body: 50 * 1024 * 1024,
    timeout_secs: 30,
    max_concurrent: None,
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
  }];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs.len(), 1);
  assert!(specs[0].target.is_empty());
  assert_eq!(specs[0].tunnels.len(), 1);
}

#[test]
fn test_build_specs_tunnels_validation() {
  let mut settings = base_settings();
  // UDP is not supported yet.
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("only tcp"), "got: {err}");

  // Targets must be host:port.
  settings.tunnels = vec![protocol::TunnelDecl {
    target: "27017".to_string(),
    protocol: "tcp".to_string(),
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("host:port"), "got: {err}");

  // Duplicates are rejected.
  let decl = protocol::TunnelDecl {
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
  };
  settings.tunnels = vec![decl.clone(), decl];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("more than once"), "got: {err}");
}

#[test]
fn test_build_specs_single_service() {
  let specs = build_specs(&base_settings(), "base-id", false).unwrap();
  assert_eq!(specs.len(), 1);
  assert_eq!(specs[0].client_id, "base-id");
  assert_eq!(specs[0].target, "http://localhost:3000");
  assert_eq!(specs[0].hostname.as_deref(), Some("app.example.com"));
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
      hostname: Some("Web.Example.COM".to_string()),
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
  assert_eq!(specs[0].hostname.as_deref(), Some("web.example.com"));
  assert_eq!(specs[1].hostname, None);

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
