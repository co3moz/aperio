//! Tests for `aperio-client check`.
//!
//! `run_check` runs a fixed sequence of diagnostics and then calls
//! `std::process::exit`, so it cannot return into the test harness. We drive it
//! the standard way for `exit`-terminating code: a hidden `#[test]`
//! (`check_driver`) re-executes this very test binary with an
//! `APERIO_CHECK_SCENARIO` env var, builds settings for that scenario (spinning
//! up loopback mock servers as needed), and calls `run_check`. The parent tests
//! spawn that child and assert the exit code. Because the child is the same
//! instrumented binary, its coverage is recorded and merged.

use super::run_check;
use crate::config::{ClientSettings, SettingsSources, Source};
use crate::protocol::PROTOCOL_VERSION;
use aperio_config::{ServiceEntry, TunnelDecl};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

// --- Loopback mock server --------------------------------------------------

#[derive(Clone, Copy)]
enum Ws {
  Accept,
  Reject,
  Slow,
}

#[derive(Clone)]
struct MockCfg {
  /// HTTP status returned for `/aperio/health`.
  health_status: u16,
  /// `protocol` field of the health JSON (None = omit it).
  protocol: Option<i64>,
  /// Whether the health JSON carries a `version` field.
  version: bool,
  /// How the `/aperio/ws` upgrade is answered.
  ws: Ws,
  /// Status for the auxiliary health path (`/h`, `/badhealth`).
  aux_status: u16,
}

impl Default for MockCfg {
  fn default() -> Self {
    MockCfg {
      health_status: 200,
      protocol: Some(PROTOCOL_VERSION as i64),
      version: true,
      ws: Ws::Accept,
      aux_status: 200,
    }
  }
}

/// Binds a blocking loopback server on a random port and returns it. Runs on
/// its own OS threads so it is independent of the tokio runtime `run_check`
/// uses.
fn spawn_mock(cfg: MockCfg) -> u16 {
  let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
  let port = listener.local_addr().unwrap().port();
  std::thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(stream) = stream else { continue };
      let cfg = cfg.clone();
      std::thread::spawn(move || handle_conn(stream, cfg));
    }
  });
  port
}

fn reason(code: u16) -> &'static str {
  match code {
    200 => "OK",
    401 => "Unauthorized",
    500 => "Internal Server Error",
    503 => "Service Unavailable",
    _ => "Status",
  }
}

fn write_resp(stream: &mut std::net::TcpStream, code: u16, ctype: &str, body: &str) {
  let _ = write!(
    stream,
    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
    code,
    reason(code),
    ctype,
    body.len(),
    body
  );
}

fn handle_conn(mut stream: std::net::TcpStream, cfg: MockCfg) {
  // Read the request head (up to the blank line).
  let mut buf = Vec::new();
  let mut chunk = [0u8; 1024];
  loop {
    match stream.read(&mut chunk) {
      Ok(0) => break,
      Ok(n) => {
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
          break;
        }
      }
      Err(_) => return,
    }
  }
  let head = String::from_utf8_lossy(&buf);
  let path = head
    .lines()
    .next()
    .and_then(|l| l.split_whitespace().nth(1))
    .unwrap_or("/")
    .to_string();

  if path == "/aperio/ws" {
    match cfg.ws {
      Ws::Slow => {
        std::thread::sleep(std::time::Duration::from_secs(7));
      }
      Ws::Reject => write_resp(&mut stream, 401, "text/plain", "no"),
      Ws::Accept => {
        let key = head
          .lines()
          .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
          .or_else(|| {
            head
              .lines()
              .find_map(|l| l.strip_prefix("sec-websocket-key: "))
          })
          .unwrap_or("")
          .trim();
        let accept = derive_accept_key(key.as_bytes());
        let _ = write!(
          stream,
          "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
      }
    }
    return;
  }

  if path == "/aperio/health" {
    if (200..300).contains(&cfg.health_status) {
      let mut fields: Vec<String> = Vec::new();
      if cfg.version {
        fields.push("\"version\":\"9.9.9\"".to_string());
      }
      if let Some(p) = cfg.protocol {
        fields.push(format!("\"protocol\":{p}"));
      }
      fields.push("\"connected_clients\":3".to_string());
      let body = format!("{{{}}}", fields.join(","));
      write_resp(&mut stream, cfg.health_status, "application/json", &body);
    } else {
      write_resp(&mut stream, cfg.health_status, "text/plain", "unhealthy");
    }
    return;
  }

  if path == "/badhealth" {
    write_resp(&mut stream, 503, "text/plain", "aux");
    return;
  }
  if path == "/h" {
    write_resp(&mut stream, cfg.aux_status, "text/plain", "aux");
    return;
  }

  write_resp(&mut stream, 200, "text/plain", "ok");
}

// --- Settings builders -----------------------------------------------------

fn base_settings() -> ClientSettings {
  ClientSettings {
    token: None,
    server: None,
    target: None,
    serve: None,
    hostnames: Vec::new(),
    path: None,
    trim_bind: None,
    pass_hostname: false,
    max_response_body: 0,
    max_request_body: None,
    response_timeout: None,
    timeout_secs: 30,
    max_concurrent: None,
    connections: None,
    priority: 0,
    bandwidth: None,
    max_message_size: 0,
    max_redirects: 5,
    tcp_target: None,
    target_health: None,
    wait_for_backend: false,
    health_interval: 0,
    health_timeout: 0,
    health_threshold: 0,
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
    bind_tunnels: HashMap::new(),
  }
}

fn tunnel(target: &str) -> TunnelDecl {
  TunnelDecl {
    target: target.to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }
}

fn tempdir(tag: &str) -> String {
  let p = std::env::temp_dir().join(format!("aperio-check-{}-{}", tag, uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&p).unwrap();
  p.to_string_lossy().into_owned()
}

/// Builds the settings/sources for a scenario and runs `run_check` — which
/// exits the process, so this never returns.
async fn drive(scenario: &str) -> ! {
  let mut s = base_settings();
  let mut sources = SettingsSources {
    server: None,
    token: None,
    target: None,
  };

  match scenario {
    "pass" => {
      let port = spawn_mock(MockCfg::default());
      let http = format!("http://127.0.0.1:{port}");
      let addr = format!("127.0.0.1:{port}");
      s.server = Some(http.clone());
      s.token = Some("tok".to_string());
      s.target = Some(http.clone());
      s.target_health = Some("/h".to_string());
      s.visitor_auth = Some("user:pw".to_string());
      s.services = vec![ServiceEntry {
        name: Some("svc".to_string()),
        target: Some(http.clone()),
        auth: Some("admin:s3cret".to_string()),
        ..Default::default()
      }];
      s.tcp_target = Some(addr.clone());
      s.tunnels = vec![tunnel(&addr)];
      // Exercise the `from(...)` source labels on the pass path.
      sources.server = Some(Source::Cli);
      sources.token = Some(Source::LocalFile);
      sources.target = Some(Source::Env);
    }
    "missing" => {
      // Everything unset: server/token/target all fail; token is filtered when
      // present-but-blank, so a whitespace token still reads as missing.
      s.token = Some("   ".to_string());
    }
    "serve" => {
      s.serve = Some(tempdir("serve"));
    }
    "services" => {
      let port = spawn_mock(MockCfg::default());
      let http = format!("http://127.0.0.1:{port}");
      s.visitor_auth = Some("nocolon".to_string()); // invalid: no separator
      s.services = vec![
        ServiceEntry {
          name: Some("api".to_string()),
          target: Some(http.clone()),
          // Absolute health URL branch.
          target_health: Some(format!("{http}/h")),
          ..Default::default()
        },
        ServiceEntry {
          // No name → "services[1]" label; unreachable → probe fails; invalid
          // auth; health endpoint is unreachable too (Err branch).
          target: Some("http://127.0.0.1:1".to_string()),
          target_health: Some("/h".to_string()),
          auth: Some("x".to_string()),
          ..Default::default()
        },
        ServiceEntry {
          name: Some("static".to_string()),
          serve: Some(tempdir("svc-static")),
          ..Default::default()
        },
        ServiceEntry {
          name: Some("broken".to_string()),
          serve: Some("/no/such/aperio/dir".to_string()),
          ..Default::default()
        },
        ServiceEntry {
          name: Some("badhealth".to_string()),
          target: Some(http.clone()),
          target_health: Some("/badhealth".to_string()), // relative + 503
          ..Default::default()
        },
        ServiceEntry {
          name: Some("grpc".to_string()),
          target: Some(format!("h2c://127.0.0.1:{port}")), // h2c → http rewrite
          ..Default::default()
        },
      ];
    }
    "tunnels" => {
      let port = spawn_mock(MockCfg::default());
      s.tcp_target = Some("127.0.0.1:1".to_string()); // connection refused
      s.tunnels = vec![
        tunnel(&format!("127.0.0.1:{port}")), // reachable
        tunnel("10.255.255.1:80"),            // unroutable → connect timeout
      ];
    }
    "badscheme" => {
      s.server = Some("ftp://example.com".to_string());
      s.token = Some("tok".to_string());
    }
    "unreachable" => {
      s.server = Some("http://127.0.0.1:1".to_string());
      s.token = Some("tok".to_string());
    }
    "healthstatus" => {
      let port = spawn_mock(MockCfg {
        health_status: 500,
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
    }
    "protomismatch" => {
      let port = spawn_mock(MockCfg {
        protocol: Some(999),
        version: false, // → "unknown" version, missing connected_clients default
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
    }
    "protonone" => {
      let port = spawn_mock(MockCfg {
        protocol: None, // omit → "predates protocol reporting"
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
    }
    "wsreject" => {
      let port = spawn_mock(MockCfg {
        ws: Ws::Reject,
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
      s.token = Some("tok".to_string());
    }
    "wstimeout" => {
      let port = spawn_mock(MockCfg {
        ws: Ws::Slow,
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
      s.token = Some("tok".to_string());
    }
    "tokenbuild" => {
      let port = spawn_mock(MockCfg::default());
      s.server = Some(format!("http://127.0.0.1:{port}"));
      // A newline makes the Authorization header value unbuildable.
      s.token = Some("bad\nvalue".to_string());
    }
    other => panic!("unknown scenario {other}"),
  }

  run_check(&s, &sources).await
}

/// Hidden driver: only does work when the scenario env var is set, so a plain
/// `cargo test` run treats it as a no-op.
#[test]
fn check_driver() {
  let Ok(scenario) = std::env::var("APERIO_CHECK_SCENARIO") else {
    return;
  };
  let rt = tokio::runtime::Runtime::new().unwrap();
  rt.block_on(drive(&scenario));
}

// --- Parent tests: spawn the driver child and assert the exit code ---------

fn run_scenario(scenario: &str) -> i32 {
  let exe = std::env::current_exe().unwrap();
  let status = std::process::Command::new(exe)
    .args(["check::tests::check_driver", "--exact", "--nocapture"])
    .env("APERIO_CHECK_SCENARIO", scenario)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .unwrap();
  status.code().unwrap_or(-1)
}

#[test]
fn all_checks_pass_exits_zero() {
  assert_eq!(run_scenario("pass"), 0);
}

#[test]
fn missing_core_settings_fail() {
  assert_eq!(run_scenario("missing"), 1);
}

#[test]
fn serve_mode_target_and_dir() {
  assert_eq!(run_scenario("serve"), 1);
}

#[test]
fn services_mode_probes_each_entry() {
  assert_eq!(run_scenario("services"), 1);
}

#[test]
fn tunnels_and_tcp_targets() {
  assert_eq!(run_scenario("tunnels"), 1);
}

#[test]
fn bad_server_scheme_fails_url_builders() {
  assert_eq!(run_scenario("badscheme"), 1);
}

#[test]
fn unreachable_server_fails_health_and_token() {
  assert_eq!(run_scenario("unreachable"), 1);
}

#[test]
fn health_non_success_status_fails() {
  assert_eq!(run_scenario("healthstatus"), 1);
}

#[test]
fn protocol_mismatch_fails() {
  assert_eq!(run_scenario("protomismatch"), 1);
}

#[test]
fn protocol_absent_is_assumed_compatible() {
  assert_eq!(run_scenario("protonone"), 1);
}

#[test]
fn websocket_rejection_fails_token_check() {
  assert_eq!(run_scenario("wsreject"), 1);
}

#[test]
fn websocket_timeout_fails_token_check() {
  assert_eq!(run_scenario("wstimeout"), 1);
}

#[test]
fn unbuildable_authorization_header_fails() {
  assert_eq!(run_scenario("tokenbuild"), 1);
}
