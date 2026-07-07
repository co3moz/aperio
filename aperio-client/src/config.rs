//! Configuration: CLI arguments, environment variables, the `aperio.yaml`
//! file, and the resolution precedence between them (CLI > env > file).

use tracing::{error, info};

/// Parses a human bandwidth value into bytes/second. Bit-based suffixes
/// (`kbit`, `mbit`, `gbit`) divide by 8; byte-based suffixes (`kb`, `mb`,
/// `gb`, or bare `k`/`m`/`g`) multiply by powers of 1000; a bare number is
/// bytes/second. Case-insensitive; fractions like "1.5mbit" are accepted.
pub(crate) fn parse_bandwidth(raw: &str) -> Option<u64> {
  let value = raw.trim().to_ascii_lowercase().replace(' ', "");
  let (number, multiplier): (&str, f64) = if let Some(n) = value.strip_suffix("kbit") {
    (n, 1_000.0 / 8.0)
  } else if let Some(n) = value.strip_suffix("mbit") {
    (n, 1_000_000.0 / 8.0)
  } else if let Some(n) = value.strip_suffix("gbit") {
    (n, 1_000_000_000.0 / 8.0)
  } else if let Some(n) = value.strip_suffix("kb").or_else(|| value.strip_suffix('k')) {
    (n, 1_000.0)
  } else if let Some(n) = value.strip_suffix("mb").or_else(|| value.strip_suffix('m')) {
    (n, 1_000_000.0)
  } else if let Some(n) = value.strip_suffix("gb").or_else(|| value.strip_suffix('g')) {
    (n, 1_000_000_000.0)
  } else {
    (value.as_str(), 1.0)
  };
  let parsed = number.parse::<f64>().ok()?;
  if !parsed.is_finite() || parsed <= 0.0 {
    return None;
  }
  Some((parsed * multiplier) as u64)
}

/// Configuration file schema (`aperio.yaml`). All keys are optional; CLI
/// arguments and environment variables take precedence over file values.
///
/// ```yaml
/// # Aperio client configuration
/// server: https://tunnel.example.com   # Aperio server URL
/// token: apr_xxxxxxxxxxxxxxxx          # tunnel token (master or dynamic)
/// target: http://localhost:3000        # local backend to expose
/// hostname: a.example.com              # optional hostname bind
/// path: /api                           # optional path bind
/// trim_bind: true                      # strip the path bind before forwarding
/// pass_hostname: false                 # forward the original Host header
/// max_concurrent: 8                    # local concurrency limit
/// tcp_target: localhost:5432           # optional raw TCP target
/// ```
#[derive(serde::Deserialize, Default)]
pub(crate) struct FileConfig {
  pub(crate) server: Option<String>,
  pub(crate) token: Option<String>,
  pub(crate) target: Option<String>,
  pub(crate) hostname: Option<String>,
  pub(crate) path: Option<String>,
  pub(crate) trim_bind: Option<bool>,
  pub(crate) pass_hostname: Option<bool>,
  pub(crate) max_concurrent: Option<u32>,
  pub(crate) max_response_body: Option<usize>,
  pub(crate) timeout: Option<u64>,
  pub(crate) max_message_size: Option<usize>,
  pub(crate) tcp_target: Option<String>,
  /// Health endpoint of the local target (path like `/health` or full URL).
  pub(crate) target_health: Option<String>,
  /// Seconds between backend health probes.
  pub(crate) health_interval: Option<u64>,
  /// Per-probe timeout in seconds.
  pub(crate) health_timeout: Option<u64>,
  /// Consecutive probe failures before the backend is reported unhealthy.
  pub(crate) health_threshold: Option<u32>,
  /// Load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: Option<u32>,
  /// Link capacity of this client's network, e.g. "8mbit", "500kbit", "2MB".
  pub(crate) bandwidth: Option<String>,
  /// Max backend redirects to follow transparently (same-host scheme
  /// upgrades and same-root-domain hops); 0 passes redirects through.
  pub(crate) max_redirects: Option<usize>,
}

/// Loads `aperio.yaml` (or an explicit `--config` path). A missing default
/// file is fine; an unreadable/invalid explicit file is a fatal error.
pub(crate) fn load_file_config(explicit: Option<&str>) -> FileConfig {
  let path = explicit.unwrap_or("aperio.yaml");
  match std::fs::read_to_string(path) {
    Ok(raw) => match serde_yaml::from_str::<FileConfig>(&raw) {
      Ok(cfg) => {
        info!("Loaded configuration from {}", path);
        cfg
      }
      Err(e) => {
        error!("Failed to parse {}: {}", path, e);
        std::process::exit(1);
      }
    },
    Err(e) => {
      if explicit.is_some() {
        error!("Failed to read config file {}: {}", path, e);
        std::process::exit(1);
      }
      FileConfig::default()
    }
  }
}

/// Parsed command line. With no arguments the client behaves exactly like
/// before (environment-variable driven), so Docker setups are unaffected.
pub(crate) struct CliArgs {
  pub(crate) mode: CliMode,
  pub(crate) server: Option<String>,
  pub(crate) token: Option<String>,
  pub(crate) target: Option<String>,
  pub(crate) hostname: Option<String>,
  pub(crate) path: Option<String>,
  pub(crate) concurrency: Option<u32>,
  pub(crate) priority: Option<u32>,
  pub(crate) pass_hostname: bool,
  pub(crate) config: Option<String>,
  pub(crate) local_port: Option<u16>,
}

pub(crate) enum CliMode {
  /// Legacy env-driven mode (no CLI arguments given).
  Env,
  /// `aperio-client http <port>`: expose a local HTTP port.
  Http,
  /// `aperio-client run`: fully config-file driven.
  Run,
  /// `aperio-client tcp <local_port>`: local TCP bridge to /aperio/tcp.
  TcpBridge,
  /// `aperio-client check`: configuration & connectivity diagnostics.
  Check,
}

fn print_usage() {
  eprintln!(
    "Aperio Client\n\nUsage:\n  aperio-client                        Run with environment variables (Docker mode)\n  aperio-client http <port> [options]  Expose http://localhost:<port>\n  aperio-client run [--config FILE]     Run from aperio.yaml\n  aperio-client tcp <local_port> [options]\n                                        Bridge a local TCP port to the server's /aperio/tcp endpoint\n  aperio-client check [options]         Diagnose configuration and connectivity\n\nOptions:\n  --server URL       Aperio server URL (or APERIO_SERVER_URL / yaml: server)\n  --token TOKEN      Tunnel token (or APERIO_SERVER_TOKEN / yaml: token)\n  --host HOSTNAME    Hostname bind (or APERIO_HOSTNAME_BIND / yaml: hostname)\n  --path PREFIX      Path bind (or APERIO_PATH_BIND / yaml: path)\n  --concurrency N    Max concurrent requests (or APERIO_CLIENT_MAX_CONCURRENT)\n  --priority N       Load-balancing priority tier: 0 = primary, higher = standby (or APERIO_CLIENT_PRIORITY / yaml: priority)\n  --pass-hostname    Forward the original Host header to the backend\n  --config FILE      Config file path (default: ./aperio.yaml)\n  --version          Print the client version\n  --help             Show this help\n\nPrecedence: CLI arguments > environment variables > aperio.yaml"
  );
}

pub(crate) fn parse_cli() -> CliArgs {
  let mut args = std::env::args().skip(1).peekable();
  let mut cli = CliArgs {
    mode: CliMode::Env,
    server: None,
    token: None,
    target: None,
    hostname: None,
    path: None,
    concurrency: None,
    priority: None,
    pass_hostname: false,
    config: None,
    local_port: None,
  };

  let Some(first) = args.next() else {
    return cli;
  };
  match first.as_str() {
    "http" => {
      cli.mode = CliMode::Http;
      let Some(port) = args.next().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("error: 'http' requires a local port number\n");
        print_usage();
        std::process::exit(2);
      };
      cli.target = Some(format!("http://localhost:{}", port));
    }
    "run" => cli.mode = CliMode::Run,
    "check" => cli.mode = CliMode::Check,
    "tcp" => {
      cli.mode = CliMode::TcpBridge;
      let Some(port) = args.next().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("error: 'tcp' requires a local port number\n");
        print_usage();
        std::process::exit(2);
      };
      cli.local_port = Some(port);
    }
    "help" | "--help" | "-h" => {
      print_usage();
      std::process::exit(0);
    }
    "version" | "--version" | "-V" => {
      println!("aperio-client {}", env!("CARGO_PKG_VERSION"));
      std::process::exit(0);
    }
    other => {
      eprintln!("error: unknown command '{}'\n", other);
      print_usage();
      std::process::exit(2);
    }
  }

  while let Some(arg) = args.next() {
    let mut take = |name: &str| -> String {
      match args.next() {
        Some(v) => v,
        None => {
          eprintln!("error: {} requires a value", name);
          std::process::exit(2);
        }
      }
    };
    match arg.as_str() {
      "--server" => cli.server = Some(take("--server")),
      "--token" => cli.token = Some(take("--token")),
      "--host" => cli.hostname = Some(take("--host")),
      "--path" => cli.path = Some(take("--path")),
      "--config" => cli.config = Some(take("--config")),
      "--concurrency" => {
        let v = take("--concurrency");
        cli.concurrency = v.parse::<u32>().ok();
      }
      "--priority" => {
        let v = take("--priority");
        cli.priority = v.parse::<u32>().ok();
      }
      "--pass-hostname" => cli.pass_hostname = true,
      "--help" | "-h" => {
        print_usage();
        std::process::exit(0);
      }
      other => {
        eprintln!("error: unknown option '{}'\n", other);
        print_usage();
        std::process::exit(2);
      }
    }
  }
  cli
}

/// Resolution helper: CLI > environment variable > config file.
pub(crate) fn resolve(cli: Option<String>, env_key: &str, file: Option<String>) -> Option<String> {
  cli
    .or_else(|| std::env::var(env_key).ok().filter(|s| !s.trim().is_empty()))
    .or(file)
}

/// Builds a WebSocket connection URL from an HTTP or WS address.
/// Ensures the scheme is set to `ws` or `wss` and applies the given path.
pub(crate) fn build_ws_url_with_path(server: &str, path: &str) -> Result<String, String> {
  let mut server_clean = server.to_string();
  if !server_clean.contains("://") {
    server_clean = format!("http://{}", server_clean);
  }

  let mut parsed = url::Url::parse(&server_clean).map_err(|e| e.to_string())?;

  let ws_scheme = match parsed.scheme() {
    "https" | "wss" => "wss",
    "http" | "ws" => "ws",
    other => return Err(format!("Unsupported scheme: {}", other)),
  };

  parsed
    .set_scheme(ws_scheme)
    .map_err(|_| "Failed to set WebSocket scheme".to_string())?;
  parsed.set_path(path);

  Ok(parsed.to_string())
}

/// Tunnel WebSocket URL (`/aperio/ws`).
pub(crate) fn build_ws_url(server: &str) -> Result<String, String> {
  build_ws_url_with_path(server, "/aperio/ws")
}

/// HTTP(S) URL on the server for a given path (used by `check`).
pub(crate) fn build_http_url(server: &str, path: &str) -> Result<String, String> {
  let mut server_clean = server.to_string();
  if !server_clean.contains("://") {
    server_clean = format!("http://{}", server_clean);
  }
  let mut parsed = url::Url::parse(&server_clean).map_err(|e| e.to_string())?;
  let scheme = match parsed.scheme() {
    "https" | "wss" => "https",
    "http" | "ws" => "http",
    other => return Err(format!("Unsupported scheme: {}", other)),
  };
  parsed
    .set_scheme(scheme)
    .map_err(|_| "Failed to set HTTP scheme".to_string())?;
  parsed.set_path(path);
  Ok(parsed.to_string())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
