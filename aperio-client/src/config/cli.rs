//! The command line: what `aperio-client` accepts, and turning it into the
//! highest of the layers the configuration is merged from.

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "aperio-client",
  version,
  about = "Aperio tunnel client, expose a local service through an Aperio server",
  after_help = "Layers, lowest first: ~/.aperio.yaml < ./aperio.yaml < environment < command line\n\n\
Examples:\n  \
aperio-client                          run from config file / environment (Docker mode)\n  \
aperio-client 3000                     expose http://localhost:3000\n  \
aperio-client example.com              expose http://example.com\n  \
aperio-client bind pg_main            bind a tunnel a peer client declares, locally\n  \
aperio-client check                    diagnose configuration and connectivity"
)]
pub(crate) struct Cli {
  /// What to expose: a port (3000 → http://localhost:3000), a hostname
  /// (example.com → http://example.com) or a full URL. Optional when the
  /// services come from a config file or the environment.
  target: Option<String>,

  #[command(subcommand)]
  command: Option<Command>,

  #[command(flatten)]
  opts: CommonOpts,
}

#[derive(Subcommand)]
pub(crate) enum Command {
  /// Bridge a local TCP port to the server's /aperio/tcp endpoint
  #[command(hide = true)]
  Tcp {
    /// Local port to listen on (127.0.0.1)
    local_port: u16,
  },
  /// Diagnose configuration and connectivity
  Check,
  /// Bind tunnels a peer client declares as local listeners: the named ones,
  /// or every entry of the `binds:` section, or, with neither, everything
  /// the token may bind
  Bind {
    /// Tunnel names (`pg_main`, or `acme@pg_main` for one organization's)
    names: Vec<String>,
  },
  /// Print a shell completion script (bash, zsh, fish, elvish, powershell)
  Completions {
    /// Which shell to generate for
    shell: clap_complete::Shell,
  },
  /// Call the server's admin API: share links, tokens, tunnels, maintenance,
  /// users, orgs, webhooks, cache, and the read-only reports
  Api {
    #[command(subcommand)]
    command: crate::api::ApiCommand,
  },
}

/// Options shared by all modes. Each is the key its help names, and writes
/// the same place in the configuration as that key's environment variable.
#[derive(Args, Clone, Default)]
pub(crate) struct CommonOpts {
  /// Aperio server URL (yaml: server.url, env: APERIO_SERVER_URL)
  #[arg(long, global = true, value_name = "URL")]
  pub(crate) server_url: Option<String>,
  /// Tunnel token, master or dynamic (yaml: server.token, env: APERIO_SERVER_TOKEN)
  #[arg(long, global = true, value_name = "TOKEN")]
  pub(crate) server_token: Option<String>,
  /// Admin API key for the `api` subcommand, not the tunnel itself
  /// (yaml: server.api_key, env: APERIO_SERVER_API_KEY)
  #[arg(long, global = true, value_name = "KEY")]
  pub(crate) server_api_key: Option<String>,
  /// IP family to dial the server over: auto (default), ipv4, or ipv6. Use
  /// ipv4 when the server hostname resolves to an unreachable IPv6 address
  /// (yaml: server.ip_family, env: APERIO_SERVER_IP_FAMILY)
  #[arg(long, global = true, value_name = "auto|ipv4|ipv6")]
  pub(crate) server_ip_family: Option<String>,
  /// HTTP proxy to dial the server through, on a network with no direct
  /// outbound connection. The tunnel only; your backend is never reached
  /// through it (yaml: server.proxy, env: APERIO_SERVER_PROXY)
  #[arg(long, global = true, value_name = "[user:password@]host:port")]
  pub(crate) server_proxy: Option<String>,
  /// Fixed instance id, a UUID (yaml: client_id, env: APERIO_CLIENT_ID)
  #[arg(long, global = true, value_name = "UUID")]
  pub(crate) client_id: Option<String>,
  /// What to call this client on screen and in the server's logs, e.g.
  /// eu_server_1. A label, not an address: client_id stays the identity
  /// (yaml: name, env: APERIO_NAME)
  #[arg(long, global = true, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// Serve a local directory of static files instead of forwarding to a
  /// backend (a services entry's `serve`, env: APERIO_SERVE)
  #[arg(long, global = true, value_name = "DIR")]
  pub(crate) serve: Option<String>,
  /// Public hostname(s), comma-separated (a services entry's `hostname`,
  /// env: APERIO_HOSTNAME)
  #[arg(long, global = true, value_name = "HOSTNAME")]
  pub(crate) hostname: Option<String>,
  /// Public path prefix, e.g. /api (a services entry's `path`, env: APERIO_PATH)
  #[arg(long, global = true, value_name = "PREFIX")]
  pub(crate) path: Option<String>,
  /// Who may reach the service: `public`, or a method block as yaml, e.g.
  /// '{method: basic, users: "admin:secret"}' (yaml: defaults.access, env:
  /// APERIO_ACCESS)
  #[arg(long, global = true, value_name = "POLICY")]
  pub(crate) access: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach the service, comma-separated (e.g.
  /// 203.0.113.7,10.0.0.0/8); unset = everyone (yaml: defaults.allowed_ips,
  /// env: APERIO_ALLOWED_IPS)
  #[arg(long, global = true, value_name = "IPS")]
  pub(crate) allowed_ips: Option<String>,
  /// Failover tier: 0 = primary, higher = standby (yaml: defaults.priority,
  /// env: APERIO_PRIORITY)
  #[arg(long, global = true, value_name = "N")]
  pub(crate) priority: Option<u32>,
  /// Forward the visitor's Host header to the backend (yaml:
  /// defaults.pass_hostname, env: APERIO_PASS_HOSTNAME)
  #[arg(long, global = true)]
  pub(crate) pass_hostname: bool,
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
  /// `aperio-client completions <shell>`: print the script and exit.
  Completions(clap_complete::Shell),
  /// `aperio-client api ...`: one admin API call, then exit.
  Api(crate::api::ApiCommand),
  /// `aperio-client bind [name...]`: bind tunnels a peer client declares as
  /// local listeners. Empty when no name was given (the `binds:` section
  /// drives it).
  Bind(Vec<String>),
}

/// Interprets a target: a bare port number becomes a localhost URL, a bare
/// hostname gets an http:// scheme, URLs pass through.
pub(crate) fn normalize_target(raw: &str) -> String {
  let trimmed = raw.trim();
  if let Ok(port) = trimmed.parse::<u16>() {
    format!("http://localhost:{}", port)
  } else if trimmed.contains("://") || trimmed.is_empty() {
    trimmed.to_string()
  } else {
    format!("http://{}", trimmed)
  }
}

/// Writes a completion script for `shell` to stdout.
///
/// Generated from the same clap definition the CLI is parsed from, rather
/// than written by hand, which is the whole reason to have it: a hand-written
/// script describes the flags somebody remembered on the day they wrote it,
/// and this one cannot describe a flag that does not exist.
pub(crate) fn print_completions(shell: clap_complete::Shell) {
  let mut command = <Cli as clap::CommandFactory>::command();
  clap_complete::generate(shell, &mut command, "aperio-client", &mut std::io::stdout());
}

pub(crate) fn parse_cli() -> CliArgs {
  cli_to_args(Cli::parse())
}

pub(crate) fn cli_to_args(cli: Cli) -> CliArgs {
  let (mode, local_port) = match cli.command {
    None => (CliMode::Run, None),
    Some(Command::Tcp { local_port }) => (CliMode::TcpBridge, Some(local_port)),
    Some(Command::Check) => (CliMode::Check, None),
    Some(Command::Bind { names }) => (
      CliMode::Bind(
        names
          .into_iter()
          .map(|n| n.trim().to_string())
          .filter(|n| !n.is_empty())
          .collect(),
      ),
      None,
    ),
    Some(Command::Completions { shell }) => (CliMode::Completions(shell), None),
    Some(Command::Api { command }) => (CliMode::Api(command), None),
  };
  CliArgs {
    mode,
    target: cli.target.as_deref().map(normalize_target),
    local_port,
    opts: cli.opts,
  }
}

impl CliArgs {
  /// The command line as a layer: each flag written where its key is, read
  /// the way that key's environment variable is read.
  pub(crate) fn overlay(&self) -> Result<aperio_config::layers::Overlay, String> {
    use aperio_config::layers::{Overlay, Scope};
    let o = &self.opts;
    let mut overlay = Overlay::default();
    let mut set = |scope: Scope, path: &str, raw: Option<String>| -> Result<(), String> {
      let Some(raw) = raw.filter(|r| !r.trim().is_empty()) else {
        return Ok(());
      };
      let setting = aperio_config::layers::client_env()
        .iter()
        .find(|s| s.scope == scope && s.path == path)
        .ok_or_else(|| format!("--{}: no such setting", path.replace(['.', '_'], "-")))?;
      let value = aperio_config::layers::parse_env_value(setting, &raw)
        .map_err(|e| format!("--{}: {e}", path.replace(['.', '_'], "-")))?;
      overlay.set(scope, path, value);
      Ok(())
    };
    set(Scope::File, "server.url", o.server_url.clone())?;
    set(Scope::File, "server.token", o.server_token.clone())?;
    set(Scope::File, "server.api_key", o.server_api_key.clone())?;
    set(Scope::File, "server.ip_family", o.server_ip_family.clone())?;
    set(Scope::File, "server.proxy", o.server_proxy.clone())?;
    set(Scope::File, "client_id", o.client_id.clone())?;
    set(Scope::File, "name", o.name.clone())?;
    set(Scope::Defaults, "access", o.access.clone())?;
    set(Scope::Defaults, "allowed_ips", o.allowed_ips.clone())?;
    set(
      Scope::Defaults,
      "priority",
      o.priority.map(|p| p.to_string()),
    )?;
    set(
      Scope::Defaults,
      "pass_hostname",
      o.pass_hostname.then(|| "true".to_string()),
    )?;
    set(Scope::Service, "target", self.target.clone())?;
    set(Scope::Service, "serve", o.serve.clone())?;
    // `api` reads these two as the scope of its call, not as a service.
    if !matches!(self.mode, CliMode::Api(_)) {
      set(Scope::Service, "hostname", o.hostname.clone())?;
      set(Scope::Service, "path", o.path.clone())?;
    }
    Ok(overlay)
  }
}
