//! What `build_specs` turns a configuration into, which is the only place the
//! whole file is seen at once: per-service fallbacks, connection pools, serve mode,
//! the bandwidth budget, and the refusals that belong at config time rather than
//! at runtime.

use super::*;
use crate::config::ClientSettings;
use crate::service::ServiceSpec;
use config::ServiceEntry;

pub(crate) fn base_settings() -> ClientSettings {
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
pub(crate) fn a_pool_hands_out_the_lowest_free_connection_number() {
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
pub(crate) fn an_unusable_min_tls_version_is_refused_by_the_config_path() {
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
pub(crate) fn test_build_specs_tunnels_only() {
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
pub(crate) fn test_build_specs_tunnels_validation() {
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
pub(crate) fn test_build_specs_single_service() {
  let specs = build_specs(&base_settings(), "base-id", false).unwrap();
  assert_eq!(specs.len(), 1);
  assert_eq!(specs[0].client_id, "base-id");
  assert_eq!(specs[0].target, "http://localhost:3000");
  assert_eq!(specs[0].hostnames, vec!["app.example.com".to_string()]);
  assert!(specs[0].name.is_none());
}

#[test]
pub(crate) fn test_build_specs_multi_service_fallbacks() {
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
pub(crate) fn test_build_specs_connections() {
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
pub(crate) fn test_build_specs_elastic_connections() {
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
pub(crate) fn test_build_specs_cli_target_overrides_services() {
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
pub(crate) fn test_build_specs_missing_service_target_fails() {
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
pub(crate) fn test_multi_hostname_list() {
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
pub(crate) fn init_tracing() {
  use std::sync::Once;
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    let _ = tracing_subscriber::fmt()
      .with_max_level(tracing::Level::TRACE)
      .with_test_writer()
      .try_init();
  });
}

pub(crate) fn tcp_tunnel(target: &str) -> protocol::TunnelDecl {
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
pub(crate) fn test_build_specs_requires_token_and_server() {
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
pub(crate) fn test_build_specs_invalid_allowed_ips() {
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
pub(crate) fn test_build_specs_invalid_unix_target() {
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
pub(crate) fn test_build_specs_invalid_denied() {
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
pub(crate) fn test_build_specs_invalid_bandwidth_warns() {
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
pub(crate) fn test_build_specs_server_urls_failover() {
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
pub(crate) fn test_build_specs_clamps_connections_warn() {
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
pub(crate) fn test_validate_tunnels_encrypt_and_expose() {
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
pub(crate) fn test_build_specs_requires_target() {
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
pub(crate) fn test_build_specs_single_service_path_trim_bind() {
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
pub(crate) fn specs_with_deps(entries: &[(&str, &[&str])]) -> Vec<ServiceSpec> {
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
pub(crate) fn depends_on_accepts_an_order_that_can_be_satisfied() {
  let specs = specs_with_deps(&[("db", &[]), ("api", &["db"]), ("web", &["api", "db"])]);
  assert_eq!(specs.len(), 3);
}

#[test]
pub(crate) fn depends_on_rejects_a_name_that_is_not_in_the_file() {
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
pub(crate) fn depends_on_rejects_a_cycle() {
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
pub(crate) fn depends_on_rejects_depending_on_itself() {
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
