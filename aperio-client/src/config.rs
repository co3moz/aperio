//! Configuration: CLI arguments (clap), the `aperio.yaml` files, environment
//! variables, and the layering between them.
//!
//! Sources from lowest to highest precedence:
//!
//! 1. `~/.aperio.yaml` — user-level defaults shared across projects
//! 2. environment variables
//! 3. `./aperio.yaml` (or the `--config` path)
//! 4. CLI arguments
//!
//! Naming is mechanical across the three surfaces: CLI `--server-token` ↔
//! yaml `server.token` ↔ env `APERIO_SERVER_TOKEN`. The pre-rename spellings
//! (`APERIO_CLIENT_*`, `APERIO_HOSTNAME_BIND`, flat yaml `token:`) remain
//! accepted as aliases so existing Docker setups keep working.

use clap::{Args, Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{error, info, warn};

use crate::protocol::TunnelDecl;

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

// --- CLI ------------------------------------------------------------------

#[derive(Parser)]
#[command(
  name = "aperio-client",
  version,
  about = "Aperio tunnel client — expose a local service through an Aperio server",
  after_help = "Precedence: CLI arguments > ./aperio.yaml > environment variables > ~/.aperio.yaml\n\n\
Examples:\n  \
aperio-client                          run from config file / environment (Docker mode)\n  \
aperio-client 3000                     expose http://localhost:3000\n  \
aperio-client example.com              expose http://example.com\n  \
aperio-client --bind-tunnels <id>      bind the declared tunnels of a peer client locally\n  \
aperio-client check                    diagnose configuration and connectivity"
)]
struct Cli {
  /// What to expose: a port (3000 → http://localhost:3000), a hostname
  /// (example.com → http://example.com) or a full URL. Optional when the
  /// target comes from a config file or the environment.
  target: Option<String>,

  /// Bind the tunnels declared by a peer client (its `tunnels:` list) as
  /// local listeners. Requires the peer's client id and the same token it
  /// connected with. Without a value, every entry of the local
  /// `bind-tunnels:` yaml section is bound.
  #[arg(
    long,
    value_name = "CLIENT_ID",
    num_args = 0..=1,
    default_missing_value = "",
    conflicts_with = "target"
  )]
  bind_tunnels: Option<String>,

  #[command(subcommand)]
  command: Option<Command>,

  #[command(flatten)]
  opts: CommonOpts,
}

#[derive(Subcommand)]
enum Command {
  /// Bridge a local TCP port to the server's /aperio/tcp endpoint
  #[command(hide = true)]
  Tcp {
    /// Local port to listen on (127.0.0.1)
    local_port: u16,
  },
  /// Diagnose configuration and connectivity
  Check,
}

/// Options shared by all modes. Each maps mechanically onto a yaml key and
/// an `APERIO_*` environment variable.
#[derive(Args, Clone, Default)]
pub(crate) struct CommonOpts {
  /// Aperio server URL (yaml: server.url, env: APERIO_SERVER_URL)
  #[arg(long, visible_alias = "server", global = true, value_name = "URL")]
  pub(crate) server_url: Option<String>,
  /// Tunnel token, master or dynamic (yaml: server.token, env: APERIO_SERVER_TOKEN)
  #[arg(long, visible_alias = "token", global = true, value_name = "TOKEN")]
  pub(crate) server_token: Option<String>,
  /// What to expose or check, same forms as the positional argument
  /// (yaml: target, env: APERIO_TARGET)
  #[arg(long = "target", global = true, value_name = "TARGET")]
  pub(crate) target_opt: Option<String>,
  /// Persistent client instance id, a UUID. Defaults to a random UUID per
  /// run (yaml: client_id, env: APERIO_CLIENT_ID)
  #[arg(long, global = true, value_name = "UUID")]
  pub(crate) client_id: Option<String>,
  /// Hostname bind, e.g. app.example.com (yaml: hostname, env: APERIO_HOSTNAME)
  #[arg(long, visible_alias = "host", global = true, value_name = "HOSTNAME")]
  pub(crate) hostname: Option<String>,
  /// Path bind, e.g. /api (yaml: path, env: APERIO_PATH)
  #[arg(long, global = true, value_name = "PREFIX")]
  pub(crate) path: Option<String>,
  /// Max concurrent requests (yaml: max_concurrent, env: APERIO_MAX_CONCURRENT)
  #[arg(long, visible_alias = "concurrency", global = true, value_name = "N")]
  pub(crate) max_concurrent: Option<u32>,
  /// Load-balancing priority tier: 0 = primary, higher = standby
  /// (yaml: priority, env: APERIO_PRIORITY)
  #[arg(long, global = true, value_name = "N")]
  pub(crate) priority: Option<u32>,
  /// Forward the original Host header to the backend
  /// (yaml: pass_hostname, env: APERIO_PASS_HOSTNAME)
  #[arg(long, global = true)]
  pub(crate) pass_hostname: bool,
  /// Declare the exposed service public: ask the server to skip its
  /// visitor password / OIDC gate for this service (needs token permission)
  /// (yaml: public, env: APERIO_PUBLIC)
  #[arg(long, global = true)]
  pub(crate) public: bool,
  /// Config file path (default: ./aperio.yaml)
  #[arg(long, global = true, value_name = "FILE")]
  pub(crate) config: Option<String>,
}

/// Parsed command line, normalized for the rest of the client.
pub(crate) struct CliArgs {
  pub(crate) mode: CliMode,
  /// Normalized target from the positional argument (port → localhost URL,
  /// bare hostname → http:// URL).
  pub(crate) target: Option<String>,
  pub(crate) local_port: Option<u16>,
  pub(crate) opts: CommonOpts,
}

pub(crate) enum CliMode {
  /// Normal tunnel operation (config file / env / positional target).
  Run,
  /// `aperio-client tcp <local_port>`: local TCP bridge to /aperio/tcp.
  TcpBridge,
  /// `aperio-client check`: configuration & connectivity diagnostics.
  Check,
  /// `aperio-client --bind-tunnels [id]`: bind the declared tunnels of one
  /// (or every configured) peer client as local listeners. The id is empty
  /// when the flag was given without a value (yaml section drives it).
  BindTunnels(String),
}

/// Interprets the positional target: a bare port number becomes a localhost
/// URL, a bare hostname gets an http:// scheme, URLs pass through.
fn normalize_target(raw: &str) -> String {
  let trimmed = raw.trim();
  if let Ok(port) = trimmed.parse::<u16>() {
    format!("http://localhost:{}", port)
  } else if trimmed.contains("://") {
    trimmed.to_string()
  } else {
    format!("http://{}", trimmed)
  }
}

pub(crate) fn parse_cli() -> CliArgs {
  cli_to_args(Cli::parse())
}

fn cli_to_args(cli: Cli) -> CliArgs {
  let (mode, local_port) = match (cli.command, cli.bind_tunnels) {
    (None, Some(id)) => (CliMode::BindTunnels(id.trim().to_string()), None),
    (None, None) => (CliMode::Run, None),
    (Some(Command::Tcp { local_port }), _) => (CliMode::TcpBridge, Some(local_port)),
    (Some(Command::Check), _) => (CliMode::Check, None),
  };
  CliArgs {
    mode,
    target: cli
      .target
      .as_deref()
      .or(cli.opts.target_opt.as_deref())
      .map(normalize_target),
    local_port,
    opts: cli.opts,
  }
}

// --- Config files ----------------------------------------------------------

/// The `server:` key accepts both the legacy plain URL string and the
/// canonical nested section.
#[derive(serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum ServerValue {
  /// `server: https://tunnel.example.com` (legacy flat form)
  Url(String),
  /// `server: { url: ..., token: ... }`
  Section {
    url: Option<String>,
    token: Option<String>,
  },
}

/// One entry of the `services:` list — a single exposed target with its own
/// binds and knobs. Unset fields fall back to the top-level / layered value
/// (so shared settings like health cadence live once at the top).
#[derive(serde::Deserialize, Default, Clone)]
pub(crate) struct ServiceEntry {
  /// Display name used in logs and the dashboard.
  pub(crate) name: Option<String>,
  /// Local backend to expose. Required per entry.
  pub(crate) target: Option<String>,
  pub(crate) hostname: Option<String>,
  pub(crate) path: Option<String>,
  pub(crate) trim_bind: Option<bool>,
  pub(crate) pass_hostname: Option<bool>,
  pub(crate) max_concurrent: Option<u32>,
  pub(crate) priority: Option<u32>,
  pub(crate) bandwidth: Option<String>,
  pub(crate) timeout: Option<u64>,
  pub(crate) max_response_body: Option<usize>,
  pub(crate) max_redirects: Option<usize>,
  pub(crate) tcp_target: Option<String>,
  pub(crate) target_health: Option<String>,
  pub(crate) health_interval: Option<u64>,
  pub(crate) health_timeout: Option<u64>,
  pub(crate) health_threshold: Option<u32>,
  /// Declare this service public (skip the server's visitor auth gate).
  pub(crate) public: Option<bool>,
}

/// Configuration file schema (`aperio.yaml` / `~/.aperio.yaml`). All keys
/// are optional.
///
/// ```yaml
/// server:
///   url: https://tunnel.example.com    # Aperio server URL
///   token: apr_xxxxxxxxxxxxxxxx        # tunnel token (master or dynamic)
/// target: http://localhost:3000        # local backend to expose
/// hostname: a.example.com              # optional hostname bind
/// path: /api                           # optional path bind
/// trim_bind: true                      # strip the path bind before forwarding
/// pass_hostname: false                 # forward the original Host header
/// max_concurrent: 8                    # local concurrency limit
/// ```
///
/// Instead of a single top-level `target`, a `services:` list exposes
/// several targets from one client process (one tunnel connection each):
///
/// ```yaml
/// server:
///   url: https://tunnel.example.com
///   token: apr_xxxxxxxxxxxxxxxx
/// services:
///   - name: web
///     target: http://localhost:3000
///     hostname: app.example.com
///   - name: api
///     target: http://localhost:4000
///     path: /api
/// ```
#[derive(serde::Deserialize, Default)]
pub(crate) struct FileConfig {
  server: Option<ServerValue>,
  /// Legacy flat `token:` key (canonical form is `server.token`).
  token: Option<String>,
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
  /// Multiple exposed targets (one tunnel connection each). Read only from
  /// the local config file; when non-empty it replaces the single top-level
  /// `target`.
  pub(crate) services: Option<Vec<ServiceEntry>>,
  /// Declare the exposed service public: the server skips its visitor
  /// password / OIDC gate for traffic routed here (requires a token that
  /// permits publishing public services).
  pub(crate) public: Option<bool>,
  /// Persistent client instance id (a UUID). Defaults to a random UUID per
  /// run when unset.
  pub(crate) client_id: Option<String>,
  /// Tunnels declared by this client: normally unexposed local services a
  /// peer client may bind with `--bind-tunnels` (same token, this client's
  /// id required). Local config file only.
  ///
  /// ```yaml
  /// tunnels:
  ///   - target: 127.0.0.1:27017
  ///     protocol: tcp
  /// ```
  pub(crate) tunnels: Option<Vec<TunnelDecl>>,
  /// Peer clients whose declared tunnels this process binds locally when
  /// run with `--bind-tunnels`. Keys are peer client ids. Local config file
  /// only.
  ///
  /// ```yaml
  /// bind-tunnels:
  ///   '<client-id>':
  ///     token: <that client's token>
  ///     override:
  ///       '127.0.0.1:27017': 15000   # local port override per target
  /// ```
  #[serde(rename = "bind-tunnels", alias = "bind_tunnels")]
  pub(crate) bind_tunnels: Option<HashMap<String, BindTunnelEntry>>,
}

/// One `bind-tunnels:` entry: how to reach (and locally map) the tunnels of
/// one peer client.
#[derive(serde::Deserialize, Default, Clone)]
pub(crate) struct BindTunnelEntry {
  /// Token the peer client connected with (falls back to the layered
  /// server token when unset).
  pub(crate) token: Option<String>,
  /// Local port overrides: declared target → local port. Without an
  /// override the local listener uses the port of the declared target.
  #[serde(default, rename = "override")]
  pub(crate) overrides: HashMap<String, u16>,
}

impl FileConfig {
  pub(crate) fn server_url(&self) -> Option<String> {
    match &self.server {
      Some(ServerValue::Url(s)) => Some(s.clone()),
      Some(ServerValue::Section { url, .. }) => url.clone(),
      None => None,
    }
  }

  pub(crate) fn server_token(&self) -> Option<String> {
    match &self.server {
      Some(ServerValue::Section { token: Some(t), .. }) => Some(t.clone()),
      _ => self.token.clone(),
    }
  }
}

/// Loads `./aperio.yaml` (or an explicit `--config` path). A missing default
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

/// Path of the user-level config (`~/.aperio.yaml`).
fn home_config_path() -> Option<PathBuf> {
  let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
  std::env::var_os(var).map(|home| PathBuf::from(home).join(".aperio.yaml"))
}

/// Loads `~/.aperio.yaml` — the lowest-precedence layer, shared across
/// projects (typically holding `server.url` and `server.token`). Missing is
/// fine; an unparseable file is skipped with a warning rather than being
/// fatal, since it may belong to another aperio version.
pub(crate) fn load_home_config() -> FileConfig {
  let Some(path) = home_config_path() else {
    return FileConfig::default();
  };
  match std::fs::read_to_string(&path) {
    Ok(raw) => match serde_yaml::from_str::<FileConfig>(&raw) {
      Ok(cfg) => {
        info!("Loaded user configuration from {:?}", path);
        cfg
      }
      Err(e) => {
        warn!("Ignoring unparseable {:?}: {}", path, e);
        FileConfig::default()
      }
    },
    Err(_) => FileConfig::default(),
  }
}

// --- Layered resolution -----------------------------------------------------

/// Fully resolved client settings, after layering CLI > ./aperio.yaml >
/// environment > ~/.aperio.yaml and applying defaults.
pub(crate) struct ClientSettings {
  pub(crate) token: Option<String>,
  pub(crate) server: Option<String>,
  pub(crate) target: Option<String>,
  pub(crate) hostname: Option<String>,
  pub(crate) path: Option<String>,
  /// Explicit trim_bind wish; `None` = default (true when a path bind is set).
  pub(crate) trim_bind: Option<bool>,
  pub(crate) pass_hostname: bool,
  pub(crate) max_response_body: usize,
  pub(crate) timeout_secs: u64,
  pub(crate) max_concurrent: Option<u32>,
  pub(crate) priority: u32,
  pub(crate) bandwidth: Option<String>,
  pub(crate) max_message_size: usize,
  pub(crate) max_redirects: usize,
  pub(crate) tcp_target: Option<String>,
  pub(crate) target_health: Option<String>,
  pub(crate) health_interval: u64,
  pub(crate) health_timeout: u64,
  pub(crate) health_threshold: u32,
  /// Ask the server to skip its visitor auth gate for this service.
  pub(crate) public: bool,
  /// `services:` entries from the local config file (empty = single-service
  /// mode driven by `target`). Per-entry gaps fall back to the resolved
  /// top-level values above.
  pub(crate) services: Vec<ServiceEntry>,
  /// Persistent client instance id (CLI > local file > env). None = a
  /// random UUID is generated per run.
  pub(crate) client_id: Option<String>,
  /// Tunnels declared by this client (local config file only).
  pub(crate) tunnels: Vec<TunnelDecl>,
  /// `bind-tunnels:` entries (local config file only).
  pub(crate) bind_tunnels: HashMap<String, BindTunnelEntry>,
}

/// Which configuration layer supplied a value (used by `check` to explain
/// where each setting came from).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
  Cli,
  LocalFile,
  Env,
  HomeFile,
}

impl Source {
  pub(crate) fn label(&self) -> &'static str {
    match self {
      Source::Cli => "CLI argument",
      Source::LocalFile => "./aperio.yaml",
      Source::Env => "environment",
      Source::HomeFile => "~/.aperio.yaml",
    }
  }
}

/// Layer that supplied each core connection setting (None = unset anywhere).
pub(crate) struct SettingsSources {
  pub(crate) server: Option<Source>,
  pub(crate) token: Option<Source>,
  pub(crate) target: Option<Source>,
}

/// Highest-precedence layer that provides a value, mirroring `layered()`.
fn source_of<T>(cli: bool, local: Option<&T>, env: Option<&T>, home: Option<&T>) -> Option<Source> {
  if cli {
    Some(Source::Cli)
  } else if local.is_some() {
    Some(Source::LocalFile)
  } else if env.is_some() {
    Some(Source::Env)
  } else if home.is_some() {
    Some(Source::HomeFile)
  } else {
    None
  }
}

/// Reports which layer each core setting came from — the diagnostic
/// counterpart of [`resolve_settings`], used by `aperio-client check`.
pub(crate) fn resolve_sources(
  cli: &CliArgs,
  home: &FileConfig,
  local: &FileConfig,
) -> SettingsSources {
  let (local_url, home_url) = (local.server_url(), home.server_url());
  let (local_token, home_token) = (local.server_token(), home.server_token());
  SettingsSources {
    server: source_of(
      cli.opts.server_url.is_some(),
      local_url.as_ref(),
      env2("APERIO_SERVER_URL", "APERIO_SERVER_URL").as_ref(),
      home_url.as_ref(),
    ),
    token: source_of(
      cli.opts.server_token.is_some(),
      local_token.as_ref(),
      env2("APERIO_SERVER_TOKEN", "APERIO_SERVER_TOKEN").as_ref(),
      home_token.as_ref(),
    ),
    target: source_of(
      cli.target.is_some(),
      local.target.as_ref(),
      env2("APERIO_TARGET", "APERIO_CLIENT_TARGET").as_ref(),
      home.target.as_ref(),
    ),
  }
}

/// Non-empty environment lookup with a legacy alias.
fn env2(new: &str, old: &str) -> Option<String> {
  let get = |key: &str| std::env::var(key).ok().filter(|s| !s.trim().is_empty());
  get(new).or_else(|| get(old))
}

fn env_parse<T: std::str::FromStr>(new: &str, old: &str) -> Option<T> {
  env2(new, old).and_then(|v| v.parse().ok())
}

fn env_bool(new: &str, old: &str) -> Option<bool> {
  env2(new, old).map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Layered lookup: CLI > local file > environment > home file.
fn layered<T>(cli: Option<T>, local: Option<T>, env: Option<T>, home: Option<T>) -> Option<T> {
  cli.or(local).or(env).or(home)
}

/// Resolves every client setting from the four layers. Called at startup and
/// again on config hot-reload (with the freshly parsed files).
pub(crate) fn resolve_settings(
  cli: &CliArgs,
  home: &FileConfig,
  local: &FileConfig,
) -> ClientSettings {
  let o = &cli.opts;
  let nonempty = |s: String| {
    let t = s.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
  };
  ClientSettings {
    token: layered(
      o.server_token.clone(),
      local.server_token(),
      env2("APERIO_SERVER_TOKEN", "APERIO_SERVER_TOKEN"),
      home.server_token(),
    ),
    server: layered(
      o.server_url.clone(),
      local.server_url(),
      env2("APERIO_SERVER_URL", "APERIO_SERVER_URL"),
      home.server_url(),
    ),
    target: layered(
      cli.target.clone(),
      local.target.clone(),
      env2("APERIO_TARGET", "APERIO_CLIENT_TARGET"),
      home.target.clone(),
    ),
    hostname: layered(
      o.hostname.clone(),
      local.hostname.clone(),
      env2("APERIO_HOSTNAME", "APERIO_HOSTNAME_BIND"),
      home.hostname.clone(),
    )
    .map(|h| h.trim().to_ascii_lowercase())
    .filter(|h| !h.is_empty()),
    path: layered(
      o.path.clone(),
      local.path.clone(),
      env2("APERIO_PATH", "APERIO_PATH_BIND"),
      home.path.clone(),
    ),
    trim_bind: layered(
      None,
      local.trim_bind,
      env_bool("APERIO_TRIM_BIND", "APERIO_CLIENT_TRIM_BIND"),
      home.trim_bind,
    ),
    pass_hostname: o.pass_hostname
      || layered(
        None,
        local.pass_hostname,
        env_bool("APERIO_PASS_HOSTNAME", "APERIO_CLIENT_PASS_HOSTNAME"),
        home.pass_hostname,
      )
      .unwrap_or(false),
    max_response_body: layered(
      None,
      local.max_response_body,
      env_parse(
        "APERIO_MAX_RESPONSE_BODY",
        "APERIO_CLIENT_MAX_RESPONSE_BODY",
      ),
      home.max_response_body,
    )
    .unwrap_or(50 * 1024 * 1024),
    timeout_secs: layered(
      None,
      local.timeout,
      env_parse("APERIO_TIMEOUT", "APERIO_CLIENT_TIMEOUT"),
      home.timeout,
    )
    .unwrap_or(30),
    max_concurrent: layered(
      o.max_concurrent,
      local.max_concurrent,
      env_parse("APERIO_MAX_CONCURRENT", "APERIO_CLIENT_MAX_CONCURRENT"),
      home.max_concurrent,
    )
    .filter(|n| *n > 0),
    priority: layered(
      o.priority,
      local.priority,
      env_parse("APERIO_PRIORITY", "APERIO_CLIENT_PRIORITY"),
      home.priority,
    )
    .unwrap_or(0),
    bandwidth: layered(
      None,
      local.bandwidth.clone(),
      env2("APERIO_BANDWIDTH", "APERIO_CLIENT_BANDWIDTH"),
      home.bandwidth.clone(),
    ),
    max_message_size: layered(
      None,
      local.max_message_size,
      env_parse("APERIO_MAX_MESSAGE_SIZE", "APERIO_CLIENT_MAX_MESSAGE_SIZE"),
      home.max_message_size,
    )
    .unwrap_or(32 * 1024 * 1024),
    max_redirects: layered(
      None,
      local.max_redirects,
      env_parse("APERIO_MAX_REDIRECTS", "APERIO_CLIENT_MAX_REDIRECTS"),
      home.max_redirects,
    )
    .unwrap_or(5),
    tcp_target: layered(
      None,
      local.tcp_target.clone(),
      env2("APERIO_TCP_TARGET", "APERIO_CLIENT_TCP_TARGET"),
      home.tcp_target.clone(),
    )
    .and_then(nonempty),
    target_health: layered(
      None,
      local.target_health.clone(),
      env2("APERIO_TARGET_HEALTH", "APERIO_CLIENT_TARGET_HEALTH"),
      home.target_health.clone(),
    )
    .and_then(nonempty),
    health_interval: layered(
      None,
      local.health_interval,
      env_parse("APERIO_HEALTH_INTERVAL", "APERIO_CLIENT_HEALTH_INTERVAL"),
      home.health_interval,
    )
    .unwrap_or(10)
    .max(1),
    health_timeout: layered(
      None,
      local.health_timeout,
      env_parse("APERIO_HEALTH_TIMEOUT", "APERIO_CLIENT_HEALTH_TIMEOUT"),
      home.health_timeout,
    )
    .unwrap_or(5)
    .max(1),
    health_threshold: layered(
      None,
      local.health_threshold,
      env_parse("APERIO_HEALTH_THRESHOLD", "APERIO_CLIENT_HEALTH_THRESHOLD"),
      home.health_threshold,
    )
    .unwrap_or(2)
    .max(1),
    public: o.public
      || layered(
        None,
        local.public,
        env_bool("APERIO_PUBLIC", "APERIO_CLIENT_PUBLIC"),
        home.public,
      )
      .unwrap_or(false),
    services: local.services.clone().unwrap_or_default(),
    client_id: layered(
      o.client_id.clone(),
      local.client_id.clone(),
      env2("APERIO_CLIENT_ID", "APERIO_CLIENT_ID"),
      home.client_id.clone(),
    )
    .and_then(nonempty),
    tunnels: local.tunnels.clone().unwrap_or_default(),
    bind_tunnels: local.bind_tunnels.clone().unwrap_or_default(),
  }
}

// --- Server URL helpers ------------------------------------------------------

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
