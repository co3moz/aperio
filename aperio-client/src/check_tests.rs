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
use crate::config::ClientSettings;
use aperio_config::TunnelDecl;
use aperio_config::layers::Layer;
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
  /// The release the mock announces, in the health JSON and on the
  /// WebSocket upgrade (None = announce nothing).
  version: Option<String>,
  /// Answer `/aperio/health` with `status` only unless the request carries
  /// an Authorization header, the way a current server does.
  health_needs_auth: bool,
  /// How the `/aperio/ws` upgrade is answered.
  ws: Ws,
  /// Status for the auxiliary health path (`/h`, `/badhealth`).
  aux_status: u16,
}

impl Default for MockCfg {
  fn default() -> Self {
    MockCfg {
      health_status: 200,
      version: Some(env!("CARGO_PKG_VERSION").to_string()),
      health_needs_auth: false,
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
        let release = cfg
          .version
          .as_deref()
          .map(|v| format!("x-aperio-release: {v}\r\n"))
          .unwrap_or_default();
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
          "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{release}\r\n"
        );
      }
    }
    return;
  }

  if path == "/aperio/health" {
    let authorized = head
      .lines()
      .any(|l| l.to_ascii_lowercase().starts_with("authorization: bearer "));
    if cfg.health_needs_auth && !authorized {
      write_resp(
        &mut stream,
        200,
        "application/json",
        "{\"status\":\"healthy\",\"ui_language\":\"en\"}",
      );
      return;
    }
    if (200..300).contains(&cfg.health_status) {
      let mut fields: Vec<String> = Vec::new();
      if let Some(v) = &cfg.version {
        fields.push(format!("\"version\":\"{v}\""));
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

/// No server, no token, no service: each scenario adds what it is about.
fn base_settings() -> ClientSettings {
  let mut s = crate::tests::settings("");
  s.server = None;
  s.token = None;
  s
}

/// The one service of the command line, from a yaml mapping.
fn single(s: &mut ClientSettings, yaml: &str) {
  let mut service = crate::tests::svc("x", yaml);
  service.name = None;
  s.services = vec![service];
  s.single = true;
}

fn tunnel(target: &str) -> TunnelDecl {
  TunnelDecl {
    custom_name: None,
    name: None,
    target: target.to_string(),
    protocol: "tcp".to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
  }
}

fn tempdir(tag: &str) -> String {
  let p = std::env::temp_dir().join(format!("aperio-check-{}-{}", tag, uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&p).unwrap();
  p.to_string_lossy().into_owned()
}

/// Builds the settings/sources for a scenario and runs `run_check`, which
/// exits the process, so this never returns.
async fn drive(scenario: &str) -> ! {
  let mut s = base_settings();

  match scenario {
    "pass" => {
      let port = spawn_mock(MockCfg::default());
      let http = format!("http://127.0.0.1:{port}");
      let addr = format!("127.0.0.1:{port}");
      s.server = Some(http.clone());
      s.token = Some("tok".to_string());
      single(
        &mut s,
        &format!(
          "{{target: '{http}', tcp_target: '{addr}', health: {{endpoint: /h}}, access: {{method: basic, users: 'user:pw'}}}}"
        ),
      );
      s.tunnels = vec![tunnel(&addr)];
      // Exercise the `from(...)` source labels on the pass path.
      s.sources.insert("server.url".into(), Layer::Cli);
      s.sources.insert("server.token".into(), Layer::Local);
      s.sources.insert("service.target".into(), Layer::Env);
    }
    "missing" => {
      // Everything unset: server/token/target all fail; token is filtered when
      // present-but-blank, so a whitespace token still reads as missing.
      s.token = Some("   ".to_string());
    }
    "serve" => {
      single(&mut s, &format!("{{serve: '{}'}}", tempdir("serve")));
    }
    "services" => {
      let port = spawn_mock(MockCfg::default());
      let http = format!("http://127.0.0.1:{port}");
      s.services = vec![
        // Absolute health URL branch.
        crate::tests::svc(
          "api",
          &format!("{{target: '{http}', health: {{endpoint: '{http}/h'}}}}"),
        ),
        // Unreachable, so the probe fails; an invalid gate (no separator);
        // a health endpoint that is unreachable too (Err branch).
        crate::tests::svc(
          "down",
          "{target: 'http://127.0.0.1:1', health: {endpoint: /h}, access: {method: basic, users: x}}",
        ),
        crate::tests::svc("static", &format!("{{serve: '{}'}}", tempdir("svc-static"))),
        crate::tests::svc("broken", "{serve: /no/such/aperio/dir}"),
        // Relative health path answered with 503.
        crate::tests::svc(
          "badhealth",
          &format!("{{target: '{http}', health: {{endpoint: /badhealth}}}}"),
        ),
        // h2c is rewritten to http for the probe.
        crate::tests::svc("grpc", &format!("{{target: 'h2c://127.0.0.1:{port}'}}")),
      ];
    }
    "binder" => {
      // A file made only of `binds:` is a binder: it exposes nothing and
      // exists to open local listeners onto tunnels someone else declared.
      // `check` used to call that a missing target.
      let port = spawn_mock(MockCfg::default());
      s.server = Some(format!("http://127.0.0.1:{port}"));
      s.token = Some("t".to_string());
      s.binds
        .insert("pg_main".to_string(), aperio_config::BindValue::Port(15432));
    }
    "tunnels" => {
      let port = spawn_mock(MockCfg::default());
      s.services = vec![crate::tests::svc("raw", "{tcp_target: '127.0.0.1:1'}")]; // refused
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
    // A server on another minor release.
    "releasemismatch" => {
      let port = spawn_mock(MockCfg {
        version: Some("0.1.0".to_string()),
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
      s.token = Some("tok".to_string());
    }
    // A server that announces no release at all predates the rule.
    "releasenone" => {
      let port = spawn_mock(MockCfg {
        version: None,
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
      s.token = Some("tok".to_string());
    }
    // A current server: the numbers come out for the token the check sends.
    "healthwithtoken" => {
      let port = spawn_mock(MockCfg {
        health_needs_auth: true,
        ..Default::default()
      });
      s.server = Some(format!("http://127.0.0.1:{port}"));
      s.token = Some("tok".to_string());
    }
    // The same server, no token configured: liveness only, and the report
    // says why the release was not compared instead of guessing.
    "healthwithouttoken" => {
      let port = spawn_mock(MockCfg {
        health_needs_auth: true,
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

  run_check(&s).await
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

/// Runs a scenario and returns its exit code together with what it printed,
/// for the checks whose whole point is *which* line came out.
fn run_scenario_output(scenario: &str) -> (i32, String) {
  let exe = std::env::current_exe().unwrap();
  let out = std::process::Command::new(exe)
    .args(["check::tests::check_driver", "--exact", "--nocapture"])
    .env("APERIO_CHECK_SCENARIO", scenario)
    .stderr(std::process::Stdio::null())
    .output()
    .unwrap();
  (
    out.status.code().unwrap_or(-1),
    String::from_utf8_lossy(&out.stdout).into_owned(),
  )
}

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
fn a_binder_only_file_is_a_complete_configuration() {
  let (code, out) = run_scenario_output("binder");
  assert!(
    out.contains("binds 1 tunnel(s)"),
    "should report what it binds, got:\n{out}"
  );
  assert!(!out.contains("FAIL  target"), "got:\n{out}");
  assert_eq!(code, 0, "nothing in this file is wrong:\n{out}");
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
fn a_server_on_another_minor_fails_the_check() {
  let (code, out) = run_scenario_output("releasemismatch");
  assert_eq!(code, 1, "{out}");
  assert!(out.contains("this server is 0.1.0"), "{out}");
  assert!(out.contains("Upgrade the server"), "{out}");
}

#[test]
fn a_server_that_announces_no_release_fails_the_check() {
  let (code, out) = run_scenario_output("releasenone");
  assert_eq!(code, 1, "{out}");
  assert!(out.contains("announced no release"), "{out}");
}

#[test]
fn the_health_numbers_come_out_for_the_configured_token() {
  let (_, out) = run_scenario_output("healthwithtoken");
  assert!(
    out.contains(&format!("server v{}", env!("CARGO_PKG_VERSION"))),
    "{out}"
  );
  assert!(out.contains("3 client(s) connected"), "{out}");
  assert!(out.contains("share a minor release"), "{out}");
}

#[test]
fn a_check_without_a_token_is_told_the_release_was_withheld() {
  let (_, out) = run_scenario_output("healthwithouttoken");
  assert!(out.contains("healthy"), "{out}");
  assert!(
    out.contains("not reported to a check without a token"),
    "{out}"
  );
  assert!(!out.contains("announced no release"), "{out}");
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

// ---------------------------------------------------------------------------
// Literal secrets in a config file (planned_features #63)
// ---------------------------------------------------------------------------

#[test]
fn literal_secrets_are_found_and_env_references_are_not() {
  let keys = super::literal_secrets_in(
    "server:\n  url: https://t.example.com\n  token: apr_literal_secret\n\
     services:\n  - name: web\n    target: http://localhost:3000\n\
     tunnels:\n  - target: 127.0.0.1:5432\n    psk: ${DB_PSK}\n\
     # token: apr_commented_out\n",
  );
  // The literal token is reported; the `${DB_PSK}` reference is exactly what
  // is being recommended, so flagging it would be advice to do what was done.
  assert!(keys.contains(&"token".to_string()), "{keys:?}");
  assert!(!keys.contains(&"psk".to_string()), "{keys:?}");
}

#[test]
fn a_commented_out_credential_is_not_a_credential() {
  assert!(super::literal_secrets_in("# token: apr_old\n").is_empty());
}

#[test]
fn a_file_with_no_credentials_reports_nothing() {
  assert!(
    super::literal_secrets_in("services:\n  - name: web\n    target: http://localhost:3000\n")
      .is_empty()
  );
}

#[test]
fn a_quoted_literal_is_still_a_literal() {
  // Quoting changes nothing about where the secret lives.
  assert_eq!(
    super::literal_secrets_in("  client_secret: \"s3cr3t\"\n"),
    vec!["client_secret".to_string()]
  );
}
