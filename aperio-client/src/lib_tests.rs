//! What `build_specs` turns a configuration into, which is the only place the
//! whole file is seen at once: per-service fallbacks, connection pools, serve mode,
//! the bandwidth budget, and the refusals that belong at config time rather than
//! at runtime.

use super::*;
use config::ServiceEntry;

fn base_settings() -> ClientSettings {
  ClientSettings {
    custom_name: None,
    token: Some("apr_test".to_string()),
    api_key: None,
    scaling: None,
    server_urls: Vec::new(),
    serve_spa: false,
    serve_404: None,
    device_key: None,
    device_key_file: None,
    idle_timeout: None,
    config_version: None,
    server: Some("https://tunnel.example.com".to_string()),
    target: Some("http://localhost:3000".to_string()),
    serve: None,
    hostnames: vec!["app.example.com".to_string()],
    path: None,
    trim_bind: None,
    pass_hostname: false,
    max_response_body: 50 * 1024 * 1024,
    reload_drain_secs: 10,
    retry_attempts: 1,
    retry_backoff_ms: 100,
    retry_all_methods: false,
    breaker_failures: 0,
    breaker_open_for_secs: 30,
    max_request_body: None,
    response_timeout: None,
    timeout_secs: 30,
    max_concurrent: None,
    connections: None,
    metrics_labels: Default::default(),
    adaptive_concurrency: false,
    multiplex: false,
    otel_bridge: None,
    startup_delay: None,
    pid_file: None,
    connect_timeout: None,
    min_tls_version: None,
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
    capture: true,
    webhook_inbox: false,
    denied: None,
    ip_family: crate::dial::IpFamily::Auto,
    tls_policy: crate::dial::TlsPolicy::default(),
    egress_proxy: None,
    services: Vec::new(),
    client_id: None,
    tunnels: Vec::new(),
    bind_tunnels: std::collections::HashMap::new(),
    subscribe: Vec::new(),
    messages_listen: None,
    messages_mqtt_listen: None,
  }
}

#[test]
fn a_pool_hands_out_the_lowest_free_connection_number() {
  // The pool used to derive the next number from its length, which is the
  // same thing only while entries leave from the end. A connection past the
  // server's ceiling stands down by itself, so a pool of [1, 3] would have
  // been told to open "3" again: two clients answering to one id, which is
  // the ambiguity the per-connection suffix exists to prevent.
  assert_eq!(next_connection_number([]), 1);
  assert_eq!(next_connection_number([1, 2, 3]), 4);
  assert_eq!(next_connection_number([1, 3]), 2);
  assert_eq!(next_connection_number([2, 3]), 1);
}

#[test]
fn an_unusable_min_tls_version_is_refused_by_the_config_path() {
  // It used to be parsed inside the running service task, which had nowhere
  // to return an error to and so exited the process. That turned one typo in
  // a hot-reloaded file into an outage for every service in it, when a bad
  // reload is supposed to be warned about and the previous configuration
  // kept. `--check-config` was blind to it for the same reason.
  let mut settings = base_settings();
  settings.min_tls_version = Some("1.1".to_string());
  let err = build_specs(&settings, "base-id", false).expect_err("1.1 is not offered");
  assert!(err.contains("min_tls_version"), "{err}");

  for good in ["1.2", "1.3", "TLSv1.2", ""] {
    settings.min_tls_version = Some(good.to_string());
    assert!(
      build_specs(&settings, "base-id", false).is_ok(),
      "{good} should be accepted"
    );
  }
  settings.min_tls_version = None;
  assert!(build_specs(&settings, "base-id", false).is_ok());
}

#[test]
fn test_build_specs_tunnels_only() {
  // A client may run with only a tunnels: list, nothing exposed, the
  // connection exists so a peer can bind the declared tunnels.
  let mut settings = base_settings();
  settings.target = None;
  settings.tunnels = vec![protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
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
    custom_name: None,
    name: None,
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  }];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].tunnels[0].protocol, "udp");
  settings.tunnels = vec![protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:53".to_string(),
    protocol: "sctp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("use tcp, udp, or tcp/udp"), "got: {err}");

  // The same target may be declared once per protocol (e.g. DNS tcp+udp).
  settings.tunnels = vec![
    protocol::TunnelDecl {
      custom_name: None,
      name: None,
      target: "127.0.0.1:53".to_string(),
      protocol: "tcp".to_string(),
      encrypt: false,
      psk: None,
      proxy_protocol: false,
      idle_timeout: None,
      expose: None,
    },
    protocol::TunnelDecl {
      custom_name: None,
      name: None,
      target: "127.0.0.1:53".to_string(),
      protocol: "udp".to_string(),
      encrypt: false,
      psk: None,
      proxy_protocol: false,
      idle_timeout: None,
      expose: None,
    },
  ];
  assert!(build_specs(&settings, "base-id", false).is_ok());

  // Targets must be host:port.
  settings.tunnels = vec![protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("host:port"), "got: {err}");

  // Duplicates are rejected.
  let decl = protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  };
  settings.tunnels = vec![decl.clone(), decl];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("more than once"), "got: {err}");

  // idle_timeout is udp-only and must be at least 1 second.
  settings.tunnels = vec![protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:27017".to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: Some(120),
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("only supported for udp"), "got: {err}");
  settings.tunnels = vec![protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: Some(0),
    expose: None,
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("at least 1 second"), "got: {err}");
  settings.tunnels = vec![protocol::TunnelDecl {
    custom_name: None,
    name: None,
    target: "127.0.0.1:53".to_string(),
    protocol: "udp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
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

  // Configured values pass through and a per-entry value overrides the top
  // level. What the server permits is applied at connect time, not here.
  let mut settings = base_settings();
  settings.connections = Some(aperio_config::Connections::Fixed(3));
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      connections: Some(aperio_config::Connections::Fixed(99)),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].connections, 3);
  assert_eq!(specs[1].connections, 99);
  // A fixed count is a pool that never moves: floor and ceiling are the same,
  // so the supervisor opens all of them and leaves them alone.
  assert_eq!(specs[0].connections_min, 3);
  assert_eq!(specs[1].connections_min, 99);
}

#[test]
fn test_build_specs_elastic_connections() {
  // A range opens the floor and leaves the ceiling as headroom.
  let mut settings = base_settings();
  settings.connections = Some(aperio_config::Connections::Range(
    aperio_config::ConnectionRange {
      min: Some(2),
      max: Some(8),
    },
  ));
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].connections_min, 2);
  assert_eq!(specs[0].connections, 8);

  // A range written the wrong way round is a typo, and the floor wins: the
  // alternative is opening fewer connections than the file's own `min` says.
  settings.connections = Some(aperio_config::Connections::Range(
    aperio_config::ConnectionRange {
      min: Some(6),
      max: Some(2),
    },
  ));
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].connections_min, 6);
  assert_eq!(specs[0].connections, 6);

  // `max` alone is a fixed pool of that size, not an elastic one from 1.
  settings.connections = Some(aperio_config::Connections::Range(
    aperio_config::ConnectionRange {
      min: None,
      max: Some(4),
    },
  ));
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].connections_min, 1);
  assert_eq!(specs[0].connections, 4);
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
async fn test_apply_serve_mode_defers_listener_teardown() {
  let root = std::env::temp_dir().join(format!("aperio-serve-defer-{}", uuid::Uuid::new_v4()));
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
      serve: Some(dir_b.clone()),
      ..Default::default()
    },
  ];
  let mut started = std::collections::HashMap::new();
  apply_serve_mode(&mut settings, &mut started).await.unwrap();
  assert_eq!(started.len(), 2);

  // A reload that drops directory b. apply_serve_mode must NOT close b's
  // listener yet: the services still running were built from the previous
  // config and are pointing at it, and this reload may still fail validation.
  let mut reloaded = base_settings();
  reloaded.target = None;
  reloaded.services = vec![ServiceEntry {
    name: Some("a".to_string()),
    serve: Some(dir_a.clone()),
    ..Default::default()
  }];
  let needed = apply_serve_mode(&mut reloaded, &mut started).await.unwrap();
  assert_eq!(
    started.len(),
    2,
    "listeners must survive until the new config is adopted"
  );
  assert!(needed.contains(&dir_a));
  assert!(!needed.contains(&dir_b));

  // Only once the caller adopts the new config is b's listener retired.
  retire_unused_serve_listeners(&needed, &mut started);
  assert_eq!(started.len(), 1);
  assert!(started.contains_key(&dir_a));

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

  // The top-level serve still refuses a services: list, it drives
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
    custom_name: None,
    name: None,
    target: target.to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
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

/// A `services:` entry with just a target, an optional bandwidth request and
/// an optional parallel-connection count.
fn bw_service(name: &str, bandwidth: Option<&str>, connections: u32) -> ServiceEntry {
  ServiceEntry {
    name: Some(name.to_string()),
    target: Some("http://localhost:3000".to_string()),
    bandwidth: bandwidth.map(|s| s.to_string()),
    connections: Some(aperio_config::Connections::Fixed(connections)),
    ..Default::default()
  }
}

/// Maps service name to the rate a single connection of it announces.
fn announced(settings: &ClientSettings) -> Vec<(String, Option<u64>)> {
  build_specs(settings, "id", false)
    .unwrap()
    .into_iter()
    .map(|s| (s.name.clone().unwrap_or_default(), s.bandwidth_bps))
    .collect()
}

#[test]
fn test_config_notes_report_declared_versus_announced() {
  init_tracing();
  // A service whose budget share is divided across its connections announces
  // a rate the operator never wrote, so it reports both sides for the
  // dashboard's config view.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![bw_service("x", Some("10mbit"), 10)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = specs[0]
    .config_notes
    .iter()
    .find(|n| n.field == "bandwidth")
    .expect("a divided rate is reported");
  assert_eq!(note.declared, "10mbit");
  assert_eq!(note.effective, "1mbit");
  assert!(
    note.reason.contains("split across 10 parallel connections"),
    "got: {}",
    note.reason
  );

  // A service that asked for nothing and took a share of the budget reports
  // it too, with an empty `declared` standing for "nothing was configured".
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", None, 1), bw_service("y", None, 1)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = &specs[0].config_notes[0];
  assert_eq!(note.declared, "");
  assert_eq!(note.effective, "1mbit");

  // A rate that fits the budget on its own is announced as written, so there
  // is nothing to report.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("1mbit"), 1)];
  assert!(
    build_specs(&settings, "id", false).unwrap()[0]
      .config_notes
      .is_empty()
  );
}

#[test]
fn test_config_notes_report_invalid_and_clamped_values() {
  init_tracing();
  // An unparseable rate is ignored; the note says so rather than leaving the
  // dashboard to show an unexplained "unlimited".
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("very fast"), 1)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = &specs[0].config_notes[0];
  assert_eq!(note.field, "bandwidth");
  assert_eq!(note.declared, "very fast");
  assert_eq!(note.effective, "unlimited");

  // Past the sanity bound: what was asked for, next to what runs. The
  // server's own ceiling is applied at connect time and reported in the
  // client's log, not here, since this runs before anything has connected.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", None, 100_000)];
  let specs = build_specs(&settings, "id", false).unwrap();
  let note = &specs[0].config_notes[0];
  assert_eq!(note.field, "connections");
  assert_eq!(note.declared, "100000");
  assert_eq!(note.effective, "256");
}

#[test]
fn test_bandwidth_split_across_parallel_connections() {
  init_tracing();
  // Scenario A: a service's own limit is divided by its connections, since
  // the server shapes each connection with a bucket of its own.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("10mbit"), 10)];
  assert_eq!(announced(&settings), vec![("x".into(), Some(125_000))]);

  // The same holds in single-service mode, where the top-level value is both
  // the budget and the only service's request.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.connections = Some(aperio_config::Connections::Fixed(4));
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].bandwidth_bps, Some(312_500));
}

#[test]
fn test_bandwidth_without_budget_leaves_others_unlimited() {
  init_tracing();
  // Scenarios B and H: with no top-level budget there is nothing to settle
  // requests against, so a service keeps what it asked for and a service that
  // asked for nothing stays unlimited.
  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("1mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(125_000)), ("y".into(), None)]
  );

  let mut settings = base_settings();
  settings.services = vec![bw_service("x", Some("3mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(375_000)), ("y".into(), None)]
  );
}

#[test]
fn test_bandwidth_budget_split_equally_then_per_connection() {
  init_tracing();
  // Scenario C: no service named a rate, so the budget is split equally per
  // service (not per connection), then divided within each service.
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", None, 2), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(62_500)), ("y".into(), Some(125_000))]
  );
}

#[test]
fn test_bandwidth_requests_starving_others_are_dropped() {
  init_tracing();
  // Scenario D: x claims the whole budget, leaving y nothing. Every named
  // rate is dropped and the budget is split equally instead.
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", Some("2mbit"), 2), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(62_500)), ("y".into(), Some(125_000))]
  );

  // The same rule covers an overshoot with an unspecified service present.
  let mut settings = base_settings();
  settings.bandwidth = Some("2mbit".to_string());
  settings.services = vec![bw_service("x", Some("4mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(125_000)), ("y".into(), Some(125_000))]
  );
}

#[test]
fn test_bandwidth_remainder_goes_to_unspecified_services() {
  init_tracing();
  // Scenario E: x keeps its 3mbit, y gets the remaining 7mbit.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![bw_service("x", Some("3mbit"), 1), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(375_000)), ("y".into(), Some(875_000))]
  );

  // Scenario G: the remainder is shared equally among the services without a
  // request of their own.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![
    bw_service("x", Some("3mbit"), 1),
    bw_service("y", None, 1),
    bw_service("z", None, 1),
  ];
  assert_eq!(
    announced(&settings),
    vec![
      ("x".into(), Some(375_000)),
      ("y".into(), Some(437_500)),
      ("z".into(), Some(437_500)),
    ]
  );
}

#[test]
fn test_bandwidth_over_budget_requests_scale_proportionally() {
  init_tracing();
  // Scenario F: every service named a rate and together they overshoot, so
  // the rates keep their relative weight and are scaled to fit (3+7 over a
  // 5mbit budget becomes 1.5 and 3.5).
  let mut settings = base_settings();
  settings.bandwidth = Some("5mbit".to_string());
  settings.services = vec![
    bw_service("x", Some("3mbit"), 1),
    bw_service("y", Some("7mbit"), 1),
  ];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(187_500)), ("y".into(), Some(437_500))]
  );

  // Under budget, named rates are left alone and the surplus stays unused.
  let mut settings = base_settings();
  settings.bandwidth = Some("10mbit".to_string());
  settings.services = vec![
    bw_service("x", Some("3mbit"), 1),
    bw_service("y", Some("1mbit"), 1),
  ];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(375_000)), ("y".into(), Some(125_000))]
  );
}

#[test]
fn test_bandwidth_share_never_rounds_to_unlimited() {
  init_tracing();
  // A share small enough to floor to 0 is clamped to 1 byte/s: the server
  // reads an announced 0 as unlimited, the opposite of a tiny share.
  let mut settings = base_settings();
  settings.bandwidth = Some("10".to_string());
  settings.services = vec![bw_service("x", None, 16), bw_service("y", None, 1)];
  assert_eq!(
    announced(&settings),
    vec![("x".into(), Some(1)), ("y".into(), Some(5))]
  );
}

#[test]
fn test_build_specs_server_urls_failover() {
  init_tracing();
  // server.urls / APERIO_SERVER_URLS add failover candidates; duplicates and
  // invalid entries are skipped (with a warning).
  let mut settings = base_settings();
  settings.server_urls = vec![
    "https://backup.example.com".to_string(),
    "https://tunnel.example.com".to_string(),
    "::not a url".to_string(),
  ];
  let specs = build_specs(&settings, "id", false).unwrap();
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
  // The client's own bound is a sanity bound, not the policy: the real
  // ceiling is the server's, announced on connect. 50 is a number an operator
  // might mean, so it survives; an absurd one is cut back to something a
  // process can actually spawn.
  let mut settings = base_settings();
  settings.connections = Some(aperio_config::Connections::Fixed(50));
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].connections, 50);

  settings.connections = Some(aperio_config::Connections::Fixed(100_000));
  let specs = build_specs(&settings, "id", false).unwrap();
  assert_eq!(specs[0].connections, 256);
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
  // Nothing to expose, nothing to tunnel, nothing to carry: there is no
  // reason for the process to exist and it says so instead of connecting.
  let mut settings = base_settings();
  settings.target = None;
  let err = build_specs(&settings, "id", false).unwrap_err();
  assert!(err.contains("nothing for this client to do"), "got: {err}");

  // Messaging alone is reason enough, the same way a tunnels: list is: the
  // connection serves no HTTP target and exists to carry something else.
  settings.messages_listen = Some("127.0.0.1:1888".to_string());
  let specs = build_specs(&settings, "id", false).expect("a publish-only client is complete");
  assert_eq!(specs.len(), 1);
  assert!(specs[0].target.is_empty());
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
  settings.connections = Some(aperio_config::Connections::Fixed(3));
  let specs = build_specs(&settings, "base-id", false).unwrap();
  let shared = Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
    ready_services: watch::channel(std::collections::HashMap::new()).0,
    otel_exports: None,
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    messages: crate::pubsub::MessageBus::new(Vec::new()),
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
    connections: Some(aperio_config::Connections::Fixed(4)),
    tcp_target: Some("127.0.0.1:5432".to_string()),
    public: Some(true),
    auth: Some(aperio_config::AuthSetting::Credentials(
      "user:pass".to_string(),
    )),
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

// ---------------------------------------------------------------------------
// The combined `tcp/udp` declaration: one tunnel, both transports.
// ---------------------------------------------------------------------------

#[test]
fn test_validate_tunnels_accepts_the_combined_protocol() {
  let decl = |protocol: &str| protocol::TunnelDecl {
    custom_name: None,
    name: Some("dns".to_string()),
    target: "192.168.3.100:53".to_string(),
    protocol: protocol.to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: Some(30),
    expose: None,
  };

  let out = validate_tunnels(&[decl("tcp/udp")]).expect("tcp/udp is accepted");
  assert_eq!(out[0].protocol, "tcp/udp");
  // The idle timeout belongs to the datagram half, so a combined tunnel keeps
  // it rather than being told it is a tcp-only setting.
  assert_eq!(out[0].idle_timeout, Some(30));

  // Written the other way round it means the same thing, and is normalized so
  // everything downstream compares against one spelling.
  let out = validate_tunnels(&[decl("UDP/TCP")]).expect("udp/tcp is the same declaration");
  assert_eq!(out[0].protocol, "tcp/udp");
}

#[test]
fn test_validate_tunnels_refuses_encrypt_on_a_combined_tunnel() {
  // Encryption is the tcp-only handshake; accepting it here would leave the
  // udp half in the clear under a flag that says otherwise.
  let err = validate_tunnels(&[protocol::TunnelDecl {
    custom_name: None,
    name: Some("dns".to_string()),
    target: "192.168.3.100:53".to_string(),
    protocol: "tcp/udp".to_string(),
    encrypt: true,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  }])
  .unwrap_err();
  assert!(err.contains("only supported for tcp tunnels"), "got: {err}");
}

#[test]
fn test_validate_tunnels_allows_expose_on_a_combined_tunnel() {
  // A public port relays TCP; the tunnel's tcp half qualifies.
  let out = validate_tunnels(&[protocol::TunnelDecl {
    custom_name: None,
    name: Some("dns".to_string()),
    target: "192.168.3.100:53".to_string(),
    protocol: "tcp/udp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: Some("a-long-shared-secret".to_string()),
  }])
  .expect("expose is accepted on the tcp half");
  assert_eq!(out.len(), 1);
}

// ---------------------------------------------------------------------------
// depends_on validation (planned_features #62)
// ---------------------------------------------------------------------------

/// Two service entries with the given names and dependencies.
fn specs_with_deps(entries: &[(&str, &[&str])]) -> Vec<ServiceSpec> {
  let mut settings = base_settings();
  settings.services = entries
    .iter()
    .map(|(name, deps)| ServiceEntry {
      name: Some((*name).to_string()),
      target: Some("http://localhost:3000".to_string()),
      depends_on: Some(deps.iter().map(|d| (*d).to_string()).collect()),
      ..Default::default()
    })
    .collect();
  build_specs(&settings, "base-id", false).unwrap_or_default()
}

#[test]
fn depends_on_accepts_an_order_that_can_be_satisfied() {
  let specs = specs_with_deps(&[("db", &[]), ("api", &["db"]), ("web", &["api", "db"])]);
  assert_eq!(specs.len(), 3);
}

#[test]
fn depends_on_rejects_a_name_that_is_not_in_the_file() {
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("api".to_string()),
    target: Some("http://localhost:3000".to_string()),
    depends_on: Some(vec!["databse".to_string()]),
    ..Default::default()
  }];
  // A typo would otherwise be invisible: at runtime everybody waits out the
  // grace period and then starts anyway, which looks exactly like working.
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("databse"), "{err}");
}

#[test]
fn depends_on_rejects_a_cycle() {
  let mut settings = base_settings();
  settings.services = ["a", "b", "c"]
    .iter()
    .zip([["c"], ["a"], ["b"]])
    .map(|(name, deps)| ServiceEntry {
      name: Some((*name).to_string()),
      target: Some("http://localhost:3000".to_string()),
      depends_on: Some(deps.iter().map(|d| (*d).to_string()).collect()),
      ..Default::default()
    })
    .collect();
  // No member of a cycle can ever come up first, so all of them wait and then
  // start anyway; the ordering the file asked for silently never happens.
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("cycle"), "{err}");
}

#[test]
fn depends_on_rejects_depending_on_itself() {
  let mut settings = base_settings();
  settings.services = vec![ServiceEntry {
    name: Some("api".to_string()),
    target: Some("http://localhost:3000".to_string()),
    depends_on: Some(vec!["api".to_string()]),
    ..Default::default()
  }];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("itself"), "{err}");
}

#[tokio::test]
async fn await_dependencies_returns_at_once_when_they_are_already_up() {
  let shared = Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
    ready_services: watch::channel(std::collections::HashMap::new()).0,
    otel_exports: None,
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    messages: crate::pubsub::MessageBus::new(Vec::new()),
  };
  shared.ready_services.send_replace(
    [("db".to_string(), 1)]
      .into_iter()
      .collect::<std::collections::HashMap<_, _>>(),
  );
  // Already-ready has to be seen without waiting for a change: a dependent
  // that starts after its dependency is the normal case, not the exception.
  let missing = tokio::time::timeout(
    std::time::Duration::from_secs(1),
    crate::service::await_dependencies(&shared, &["db".to_string()]),
  )
  .await
  .expect("an already-satisfied dependency does not wait");
  assert!(missing.is_empty());
}

#[tokio::test]
async fn a_dependency_that_went_away_is_not_still_reported_as_ready() {
  // Readiness was a set that nothing ever removed from, so a service that
  // connected once and then lost its tunnel stayed ready for the life of the
  // process: a dependent starting afterwards, after a config reload for
  // instance, was told its dependency was up when it was not, and opened
  // straight into the outage it was written to wait out.
  let shared = Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
    ready_services: watch::channel(std::collections::HashMap::new()).0,
    otel_exports: None,
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    messages: crate::pubsub::MessageBus::new(Vec::new()),
  };

  // Two connections of one service, as `connections: 2` gives it. The name
  // is up while either of them is.
  shared.ready_services.send_modify(|live| {
    *live.entry("db".to_string()).or_insert(0) += 1;
  });
  shared.ready_services.send_modify(|live| {
    *live.entry("db".to_string()).or_insert(0) += 1;
  });
  let drop_one = |shared: &Shared| {
    shared.ready_services.send_modify(|live| {
      if let Some(count) = live.get_mut("db") {
        *count -= 1;
        if *count == 0 {
          live.remove("db");
        }
      }
    });
  };

  drop_one(&shared);
  let missing = tokio::time::timeout(
    std::time::Duration::from_millis(200),
    crate::service::await_dependencies(&shared, &["db".to_string()]),
  )
  .await
  .expect("one connection is still up, so the dependency is up");
  assert!(missing.is_empty());

  // The last one goes: now the dependency really is down, and a dependent
  // starting from here waits rather than being waved through.
  drop_one(&shared);
  assert!(
    tokio::time::timeout(
      std::time::Duration::from_millis(200),
      crate::service::await_dependencies(&shared, &["db".to_string()]),
    )
    .await
    .is_err(),
    "a dependency with no live connection must not read as ready"
  );
}

#[tokio::test]
async fn await_dependencies_wakes_when_the_dependency_comes_up() {
  let shared = Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
    ready_services: watch::channel(std::collections::HashMap::new()).0,
    otel_exports: None,
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    messages: crate::pubsub::MessageBus::new(Vec::new()),
  };
  let waiting = {
    let shared = shared.clone();
    tokio::spawn(
      async move { crate::service::await_dependencies(&shared, &["db".to_string()]).await },
    )
  };
  tokio::task::yield_now().await;
  shared.ready_services.send_replace(
    [("db".to_string(), 1)]
      .into_iter()
      .collect::<std::collections::HashMap<_, _>>(),
  );
  let missing = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
    .await
    .expect("the waiter is woken by the dependency coming up")
    .unwrap();
  assert!(missing.is_empty());
}

/// A `services:` list whose entries all opt into multiplexing, for the tests
/// below. `n` entries, each named and pointed at its own port.
fn multiplexed_services(n: usize) -> Vec<ServiceEntry> {
  (0..n)
    .map(|i| ServiceEntry {
      name: Some(format!("svc{i}")),
      target: Some(format!("http://localhost:{}", 3000 + i)),
      multiplex: Some(true),
      ..Default::default()
    })
    .collect()
}

#[test]
fn multiplexed_services_share_one_group() {
  let mut settings = base_settings();
  settings.services = multiplexed_services(3);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  // One group, and every service in it: the ids are what `spawn_services`
  // groups by, so two groups here would be two connections.
  assert_eq!(
    specs.iter().map(|s| s.multiplex_group).collect::<Vec<_>>(),
    vec![Some(0), Some(0), Some(0)]
  );
}

#[test]
fn a_service_that_asks_to_multiplex_alone_keeps_its_own_connection() {
  // Nobody to share with is not an error and not a group: a group of one is
  // the ordinary connection it would have had anyway, and announcing a
  // one-entry `services` list instead would only narrow which servers can
  // read the Ping.
  let mut settings = base_settings();
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      multiplex: Some(true),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].multiplex_group, None);
  assert_eq!(specs[1].multiplex_group, None);
  // What it asked for is still recorded, so nothing downstream has to guess
  // why it is ungrouped.
  assert!(specs[0].multiplex);
  assert!(!specs[1].multiplex);
}

#[test]
fn a_file_wide_multiplex_can_be_turned_off_per_entry() {
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("bulk".to_string()),
      target: Some("http://localhost:5000".to_string()),
      multiplex: Some(false),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[0].multiplex_group, Some(0));
  assert_eq!(specs[1].multiplex_group, Some(0));
  // The entry that opted out keeps a connection of its own, which is the
  // point of being able to say `multiplex: false` in a file that turned it on
  // for everything: one service whose responses are large should not occupy
  // the writer the small ones send through.
  assert_eq!(specs[2].multiplex_group, None);
}

#[test]
fn a_multiplexed_service_must_be_named() {
  // Two unnamed services on one connection are told apart only by their
  // position in a list, and a name is what the server keeps routing, ejection
  // and statistics under. Refused at config time, where it is one line to fix.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = vec![
    ServiceEntry {
      target: Some("http://localhost:3000".to_string()),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      ..Default::default()
    },
  ];
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("multiplexed service needs a name"), "{err}");
}

#[test]
fn multiplexing_overrides_a_per_service_connection_pool_and_says_so() {
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = vec![
    ServiceEntry {
      name: Some("web".to_string()),
      target: Some("http://localhost:3000".to_string()),
      connections: Some(aperio_config::Connections::Fixed(4)),
      ..Default::default()
    },
    ServiceEntry {
      name: Some("api".to_string()),
      target: Some("http://localhost:4000".to_string()),
      connections: Some(aperio_config::Connections::Range(
        aperio_config::ConnectionRange {
          min: Some(2),
          max: Some(8),
        },
      )),
      ..Default::default()
    },
  ];
  let specs = build_specs(&settings, "base-id", false).unwrap();
  // One connection is what multiplexing means, so the pool is not something
  // these services can also have.
  for spec in &specs {
    assert_eq!(spec.connections, 1);
    assert_eq!(spec.connections_min, 1);
  }
  // Reported rather than silently dropped: the dashboard's config view is
  // where a value that did not survive its config is supposed to show up.
  let note = |spec: &ServiceSpec| {
    spec
      .config_notes
      .iter()
      .find(|n| n.field == "connections")
      .cloned()
      .unwrap_or_else(|| panic!("a note about connections"))
  };
  assert_eq!(note(&specs[0]).declared, "4");
  assert_eq!(note(&specs[0]).effective, "1");
  assert_eq!(note(&specs[1]).declared, "2-8");
  assert!(note(&specs[1]).reason.contains("share one connection"));
}

#[test]
fn a_service_left_on_its_own_connection_keeps_its_pool() {
  // The clamp is the group's, not the flag's: an entry that opted out is
  // untouched even in a file that multiplexes everything else.
  let mut settings = base_settings();
  settings.multiplex = true;
  let mut services = multiplexed_services(2);
  services.push(ServiceEntry {
    name: Some("bulk".to_string()),
    target: Some("http://localhost:9000".to_string()),
    multiplex: Some(false),
    connections: Some(aperio_config::Connections::Fixed(4)),
    ..Default::default()
  });
  settings.services = services;
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs[2].connections, 4);
  assert!(
    specs[2]
      .config_notes
      .iter()
      .all(|n| n.field != "connections")
  );
}

#[test]
fn a_multiplexed_group_announces_the_budget_it_actually_gets_paced_at() {
  // The server shapes the socket, not the service: every service on a
  // connection announces into one token bucket and the last one wins. A share
  // per service is right when each has a connection of its own and wrong when
  // they share one, and the wrongness is silent and large: four services
  // splitting an 8mbit budget announced 2mbit each, the cell held 2mbit, and a
  // link sized at 8 ran at 2. At forty services it is a fortieth.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.bandwidth = Some("8mbit".to_string());
  settings.services = multiplexed_services(4);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  let budget = parse_bandwidth("8mbit").unwrap();
  for spec in &specs {
    assert_eq!(spec.bandwidth_bps, Some(budget));
  }
  // Said out loud, since what a service announces is no longer its own share.
  let note = specs[0]
    .config_notes
    .iter()
    .find(|n| n.field == "bandwidth")
    .expect("a note about bandwidth");
  assert!(
    note.reason.contains("share one shaped connection"),
    "{note:?}"
  );
}

#[test]
fn one_uncapped_service_uncaps_the_connection_it_shares_and_says_so() {
  // The server reads an absent limit as zero and zero as unlimited, so a
  // member without one wipes the cell whatever its neighbours declared. The
  // cap was already not being enforced; the only question was whether anything
  // said so. Capping the socket at the declared ones instead would throttle a
  // service the file never limited.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = multiplexed_services(3);
  settings.services[0].bandwidth = Some("4mbit".to_string());
  let specs = build_specs(&settings, "base-id", false).unwrap();
  for spec in &specs {
    assert_eq!(spec.bandwidth_bps, None);
  }
  let note = specs[0]
    .config_notes
    .iter()
    .find(|n| n.field == "bandwidth")
    .expect("a note about bandwidth");
  assert_eq!(note.effective, "unlimited");
  assert!(note.reason.contains("declares no limit"), "{note:?}");
}

#[test]
fn a_service_on_its_own_connection_still_splits_its_bandwidth_per_connection() {
  // The fix is the group's, not the flag's: an ordinary service keeps the
  // per-connection division, which is right because the server shapes each of
  // its connections separately.
  let mut settings = base_settings();
  settings.bandwidth = Some("8mbit".to_string());
  settings.connections = Some(aperio_config::Connections::Fixed(4));
  let specs = build_specs(&settings, "base-id", false).unwrap();
  let budget = parse_bandwidth("8mbit").unwrap();
  assert_eq!(specs[0].bandwidth_bps, Some(budget / 4));
}

#[test]
fn more_multiplexed_services_than_a_server_accepts_is_a_config_error() {
  // The server answers a longer list by dropping the connection, so refusing
  // here is what lets the message name the file: otherwise the operator sees a
  // client that connects and disconnects with the reason in somebody else's
  // log.
  let mut settings = base_settings();
  settings.multiplex = true;
  settings.services = multiplexed_services(service::MAX_MULTIPLEXED_SERVICES + 1);
  let err = build_specs(&settings, "base-id", false).unwrap_err();
  assert!(err.contains("share one connection"), "{err}");

  // Exactly at the ceiling is fine; the bound is a fence, not a limit anybody
  // legitimate is meant to feel.
  settings.services = multiplexed_services(service::MAX_MULTIPLEXED_SERVICES);
  let specs = build_specs(&settings, "base-id", false).unwrap();
  assert_eq!(specs.len(), service::MAX_MULTIPLEXED_SERVICES);
  assert!(specs.iter().all(|s| s.multiplex_group == Some(0)));
}
