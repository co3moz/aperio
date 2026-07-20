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
    serve: None,
    hostnames: Vec::new(),
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
    expose: None,
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

#[test]
fn test_tunnel_ws_url_invalid_server() {
  // An unsupported scheme propagates the build error.
  assert!(tunnel_ws_url("ftp://host", "/aperio/tcp", "c", "127.0.0.1:1").is_err());
}

// ---------------------------------------------------------------------------
// Mock HTTP server + integration tests for discovery and binding.
// ---------------------------------------------------------------------------

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Installs a process-wide TRACE subscriber once, so `info!`/`warn!`/`error!`
/// argument expressions are actually evaluated (and thus covered) during
/// tests. Without a subscriber, tracing skips argument evaluation entirely.
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

/// A tiny loopback HTTP server. The handler receives the request path and
/// returns `(status_code, json_body)`. Returns the server's base URL.
async fn spawn_http<F>(handler: F) -> String
where
  F: Fn(&str) -> (u16, String) + Send + Sync + 'static,
{
  let handler = Arc::new(handler);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let handler = handler.clone();
      tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
          .lines()
          .next()
          .and_then(|l| l.split_whitespace().nth(1))
          .unwrap_or("/")
          .to_string();
        let (status, body) = handler(&path);
        let resp = format!(
          "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.flush().await;
      });
    }
  });
  format!("http://127.0.0.1:{port}")
}

fn tcp_decl(target: &str, encrypt: bool) -> TunnelDecl {
  TunnelDecl {
    target: target.to_string(),
    protocol: "tcp".to_string(),
    encrypt,
    psk: None,
    idle_timeout: None,
    expose: None,
  }
}

fn udp_decl(target: &str, encrypt: bool) -> TunnelDecl {
  TunnelDecl {
    target: target.to_string(),
    protocol: "udp".to_string(),
    encrypt,
    psk: None,
    idle_timeout: None,
    expose: None,
  }
}

fn spec_for(client_id: &str, psk: Option<&str>) -> BindSpec {
  BindSpec {
    client_id: client_id.to_string(),
    token: "apr_test".to_string(),
    overrides: HashMap::new(),
    psk: psk.map(|p| p.to_string()),
  }
}

#[tokio::test]
async fn test_discover_with_retry_success() {
  init_tracing();
  let tunnels = vec![
    tcp_decl("127.0.0.1:5432", false),
    udp_decl("127.0.0.1:53", false),
  ];
  let body = serde_json::to_string(&tunnels).unwrap();
  let server = spawn_http(move |path| {
    assert!(path.contains("/aperio/tunnels/peer-1"));
    (200, body.clone())
  })
  .await;
  let got = discover_with_retry(&server, &spec_for("peer-1", None)).await;
  assert_eq!(got.len(), 2);
  assert_eq!(got[0].target, "127.0.0.1:5432");
}

#[tokio::test]
async fn test_discover_with_retry_transient_arms() {
  init_tracing();
  // Each of these arms logs and then sleeps for the (long) retry interval;
  // a short timeout lets one iteration run without waiting it out.
  // 404 → "not connected yet".
  let s404 = spawn_http(|_| (404, String::new())).await;
  assert!(
    tokio::time::timeout(
      Duration::from_millis(300),
      discover_with_retry(&s404, &spec_for("peer-1", None))
    )
    .await
    .is_err()
  );
  // 500 → generic retry.
  let s500 = spawn_http(|_| (500, String::new())).await;
  assert!(
    tokio::time::timeout(
      Duration::from_millis(300),
      discover_with_retry(&s500, &spec_for("peer-1", None))
    )
    .await
    .is_err()
  );
  // 200 with a body that is not a tunnel list → parse error, then retry.
  let sbad = spawn_http(|_| (200, "not json".to_string())).await;
  assert!(
    tokio::time::timeout(
      Duration::from_millis(300),
      discover_with_retry(&sbad, &spec_for("peer-1", None))
    )
    .await
    .is_err()
  );
  // Connection error (nothing listening) → retry.
  let free = {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
  };
  assert!(
    tokio::time::timeout(
      Duration::from_millis(300),
      discover_with_retry(
        &format!("http://127.0.0.1:{free}"),
        &spec_for("peer-1", None)
      )
    )
    .await
    .is_err()
  );
}

#[tokio::test]
async fn test_run_bind_tunnels_binds_everything() {
  init_tracing();
  // Pre-bind a port so one declared tunnel hits the bind-error branch.
  let blocked = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let blocked_port = blocked.local_addr().unwrap().port();

  // A rich tunnel list for peerA (psk set) exercising: a plain tcp listener
  // (+ "psk unused" warning), a duplicate (port conflict), a portless target
  // (no local port), a udp relay, a udp-with-encrypt error, an already-bound
  // port (bind error), and an encrypted tcp tunnel (encrypt info with PSK).
  let a_tunnels = vec![
    tcp_decl("127.0.0.1:39110", false),
    tcp_decl("127.0.0.1:39110", false), // duplicate → conflict
    tcp_decl("no-port-here", false),    // no derivable local port
    udp_decl("127.0.0.1:39111", false),
    udp_decl("127.0.0.1:39112", true), // encrypt on udp → error
    tcp_decl(&format!("127.0.0.1:{blocked_port}"), false), // bind error
    tcp_decl("127.0.0.1:39113", true), // encrypted tcp (psk present)
  ];
  // peerB declares nothing → "no tunnels to bind" warning.
  let a_body = serde_json::to_string(&a_tunnels).unwrap();
  let b_body = serde_json::to_string(&Vec::<TunnelDecl>::new()).unwrap();
  let server = spawn_http(move |path| {
    if path.contains("peerB") {
      (200, b_body.clone())
    } else {
      (200, a_body.clone())
    }
  })
  .await;

  let mut map = HashMap::new();
  map.insert(
    "peerA".to_string(),
    BindTunnelEntry {
      token: Some("apr_test".to_string()),
      overrides: HashMap::new(),
      psk: Some("shared-secret".to_string()),
    },
  );
  map.insert(
    "peerB".to_string(),
    BindTunnelEntry {
      token: Some("apr_test".to_string()),
      overrides: HashMap::new(),
      psk: None,
    },
  );
  let settings = settings_with(Some("apr_test"), map);

  // run_bind_tunnels never returns (it ends in a pending future); run it as a
  // background task, drive one accepted connection, then let the test end.
  let server2 = server.clone();
  tokio::spawn(async move {
    run_bind_tunnels(&settings, &server2, "").await;
  });
  // Give discovery + binding time to establish the 127.0.0.1:39110 listener.
  tokio::time::sleep(Duration::from_millis(500)).await;
  // Connect to the bound listener so the accept loop runs (and spawns a
  // bridge_connection that then fails to reach the fake server).
  if let Ok(mut c) = tokio::net::TcpStream::connect("127.0.0.1:39110").await {
    let _ = c.write_all(b"hello").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  drop(blocked);
}

#[tokio::test]
async fn test_run_bind_tunnels_encrypted_no_psk() {
  init_tracing();
  // An encrypted tcp tunnel with no configured psk hits the "no PSK" warning.
  let tunnels = vec![tcp_decl("127.0.0.1:39120", true)];
  let body = serde_json::to_string(&tunnels).unwrap();
  let server = spawn_http(move |_| (200, body.clone())).await;

  let mut map = HashMap::new();
  map.insert(
    "peerC".to_string(),
    BindTunnelEntry {
      token: Some("apr_test".to_string()),
      overrides: HashMap::new(),
      psk: None,
    },
  );
  let settings = settings_with(Some("apr_test"), map);
  let server2 = server.clone();
  let _ = tokio::time::timeout(
    Duration::from_millis(800),
    run_bind_tunnels(&settings, &server2, "peerC"),
  )
  .await;
}
