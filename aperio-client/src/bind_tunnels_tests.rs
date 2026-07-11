use super::*;
use crate::config::BindTunnelEntry;

fn settings_with(
  token: Option<&str>,
  bind_tunnels: HashMap<String, BindTunnelEntry>,
) -> ClientSettings {
  ClientSettings {
    token: token.map(|t| t.to_string()),
    server: Some("https://tunnel.example.com".to_string()),
    target: None,
    hostname: None,
    path: None,
    trim_bind: None,
    pass_hostname: false,
    max_response_body: 50 * 1024 * 1024,
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
    services: Vec::new(),
    client_id: None,
    tunnels: Vec::new(),
    bind_tunnels,
  }
}

fn entry(token: Option<&str>, overrides: &[(&str, u16)]) -> BindTunnelEntry {
  BindTunnelEntry {
    token: token.map(|t| t.to_string()),
    overrides: overrides.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
    psk: None,
  }
}

#[test]
fn test_build_bind_specs_explicit_id() {
  // An explicit id with no yaml entry falls back to the layered token.
  let specs = build_bind_specs(&settings_with(Some("apr_x"), HashMap::new()), "client-1").unwrap();
  assert_eq!(specs.len(), 1);
  assert_eq!(specs[0].client_id, "client-1");
  assert_eq!(specs[0].token, "apr_x");
  assert!(specs[0].overrides.is_empty());

  // A yaml entry for that id supplies token and overrides (keys trimmed).
  let mut map = HashMap::new();
  map.insert(
    "client-1".to_string(),
    entry(Some("apr_entry"), &[(" 127.0.0.1:27017 ", 15000)]),
  );
  let specs = build_bind_specs(&settings_with(Some("apr_x"), map), "client-1").unwrap();
  assert_eq!(specs[0].token, "apr_entry");
  assert_eq!(specs[0].overrides.get("127.0.0.1:27017"), Some(&15000));
}

#[test]
fn test_build_bind_specs_yaml_entries() {
  // Without an id every yaml entry runs; per-entry tokens fall back to the
  // layered token.
  let mut map = HashMap::new();
  map.insert("a".to_string(), entry(Some("apr_a"), &[]));
  map.insert("b".to_string(), entry(None, &[]));
  let specs = build_bind_specs(&settings_with(Some("apr_shared"), map), "").unwrap();
  assert_eq!(specs.len(), 2);
  let token_of = |id: &str| {
    specs
      .iter()
      .find(|s| s.client_id == id)
      .map(|s| s.token.clone())
      .unwrap()
  };
  assert_eq!(token_of("a"), "apr_a");
  assert_eq!(token_of("b"), "apr_shared");
}

#[test]
fn test_build_bind_specs_errors() {
  // No id and no yaml section.
  let err = build_bind_specs(&settings_with(Some("apr_x"), HashMap::new()), "").unwrap_err();
  assert!(err.contains("bind-tunnels"), "got: {err}");

  // Explicit id with no token anywhere.
  let err = build_bind_specs(&settings_with(None, HashMap::new()), "client-1").unwrap_err();
  assert!(err.contains("token is required"), "got: {err}");

  // A yaml entry with no token and no layered fallback.
  let mut map = HashMap::new();
  map.insert("a".to_string(), entry(None, &[]));
  let err = build_bind_specs(&settings_with(None, map), "").unwrap_err();
  assert!(err.contains("'a'"), "got: {err}");
}

#[test]
fn test_local_port_for() {
  let decl = |target: &str| TunnelDecl {
    target: target.to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
  };
  let spec = BindSpec {
    client_id: "c".to_string(),
    token: "t".to_string(),
    overrides: [("127.0.0.1:27017".to_string(), 15000u16)]
      .into_iter()
      .collect(),
    psk: None,
  };
  // The override wins over the declared port.
  assert_eq!(local_port_for(&spec, &decl("127.0.0.1:27017")), Some(15000));
  // Without an override the declared target's port is used.
  assert_eq!(local_port_for(&spec, &decl("127.0.0.1:5432")), Some(5432));
  // No parseable port and no override → None.
  assert_eq!(local_port_for(&spec, &decl("no-port-here")), None);
}

#[test]
fn test_tunnel_ws_url() {
  let url = tunnel_ws_url(
    "https://tunnel.example.com",
    "/aperio/tcp",
    "client-1",
    "127.0.0.1:27017",
  )
  .unwrap();
  assert!(
    url.starts_with("wss://tunnel.example.com/aperio/tcp?"),
    "got: {url}"
  );
  assert!(url.contains("client=client-1"), "got: {url}");
  // The target is percent-encoded into the query.
  assert!(url.contains("target=127.0.0.1%3A27017"), "got: {url}");
  // The UDP endpoint uses the same query shape.
  let udp = tunnel_ws_url(
    "https://tunnel.example.com",
    "/aperio/udp",
    "client-1",
    "127.0.0.1:5353",
  )
  .unwrap();
  assert!(
    udp.starts_with("wss://tunnel.example.com/aperio/udp?"),
    "got: {udp}"
  );
}
