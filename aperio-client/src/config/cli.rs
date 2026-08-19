//! The command line: what `aperio-client` accepts, and turning it into the
//! one struct the rest of the resolution treats as the highest layer.

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "aperio-client",
  version,
  about = "Aperio tunnel client, expose a local service through an Aperio server",
  after_help = "Precedence: CLI arguments > ./aperio.yaml > environment variables > ~/.aperio.yaml\n\n\
Examples:\n  \
aperio-client                          run from config file / environment (Docker mode)\n  \
aperio-client 3000                     expose http://localhost:3000\n  \
aperio-client example.com              expose http://example.com\n  \
aperio-client --bind-tunnels <id>      bind the declared tunnels of a peer client locally\n  \
aperio-client check                    diagnose configuration and connectivity"
)]
pub(crate) struct Cli {
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
pub(crate) enum Command {
  /// Bridge a local TCP port to the server's /aperio/tcp endpoint
  #[command(hide = true)]
  Tcp {
    /// Local port to listen on (127.0.0.1)
    local_port: u16,
  },
  /// Diagnose configuration and connectivity
  Check,
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
  /// Admin API key for the `api` subcommand, not the tunnel itself
  /// (yaml: server.api_key, env: APERIO_API_KEY)
  #[arg(long = "api-key", global = true, value_name = "KEY")]
  pub(crate) api_key: Option<String>,
  /// What to expose or check, same forms as the positional argument
  /// (yaml: target, env: APERIO_TARGET)
  #[arg(long = "target", global = true, value_name = "TARGET")]
  pub(crate) target_opt: Option<String>,
  /// Serve a local directory of static files instead of forwarding to a
  /// backend (yaml: serve, env: APERIO_SERVE)
  #[arg(long, global = true, value_name = "DIR")]
  pub(crate) serve: Option<String>,
  /// Persistent client instance id, a UUID. Defaults to a random UUID per
  /// run (yaml: client_id, env: APERIO_CLIENT_ID)
  #[arg(long, global = true, value_name = "UUID")]
  pub(crate) client_id: Option<String>,
  /// What to call this client on screen and in the server's logs, e.g.
  /// eu_server_1. A label, not an address: client_id stays the identity
  /// (yaml: name, env: APERIO_NAME)
  #[arg(long, global = true, value_name = "NAME")]
  pub(crate) name: Option<String>,
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
  /// Per-service visitor login as `user:password`: the server gates this
  /// service behind a login with these credentials, overriding its own
  /// APERIO_SERVER_AUTH for this service (needs the same token permission as
  /// `public`; ignored if the server sets APERIO_IGNORE_CLIENT_AUTH)
  /// (yaml: auth, env: APERIO_VISITOR_AUTH)
  #[arg(long = "visitor-auth", global = true, value_name = "USER:PASSWORD")]
  pub(crate) visitor_auth: Option<String>,
  /// Visitor IPs/CIDRs allowed to reach the exposed service, comma-separated
  /// (e.g. 203.0.113.7,10.0.0.0/8); unset = everyone. Enforced by the server
  /// (yaml: allowed_ips, env: APERIO_ALLOWED_IPS)
  #[arg(long = "allowed-ips", global = true, value_name = "IPS")]
  pub(crate) allowed_ips: Option<String>,
  /// Keep serving cached responses while this client is offline: the server
  /// answers from its cache (marked, even expired) instead of a 504; needs
  /// `cache` and the server-side cache (yaml: resilience, env: APERIO_RESILIENCE)
  #[arg(long, global = true)]
  pub(crate) resilience: bool,
  /// Do not record this client's transactions for the dashboard's request
  /// inspector. Off (so: recorded) by default; a service carrying heavy
  /// traffic can buy back the per-request capture (yaml: capture: false, env:
  /// APERIO_CAPTURE=0)
  #[arg(long = "no-capture", global = true)]
  pub(crate) no_capture: bool,
  /// IP family to dial the server over: auto (default), ipv4, or ipv6. Use
  /// ipv4 when the server hostname resolves to an unreachable IPv6 address
  /// (yaml: ip_family, env: APERIO_IP_FAMILY)
  #[arg(long = "ip-family", global = true, value_name = "auto|ipv4|ipv6")]
  pub(crate) ip_family: Option<String>,
  /// HTTP proxy to dial the tunnel server through, on a network with no
  /// direct outbound connection. Tunnel only; your backend is never reached
  /// through it (yaml: egress_proxy, env: APERIO_EGRESS_PROXY)
  #[arg(
    long = "egress-proxy",
    global = true,
    value_name = "[user:password@]host:port"
  )]
  pub(crate) egress_proxy: Option<String>,
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
  /// `aperio-client --bind-tunnels [id]`: bind the declared tunnels of one
  /// (or every configured) peer client as local listeners. The id is empty
  /// when the flag was given without a value (yaml section drives it).
  BindTunnels(String),
}

/// Interprets the positional target: a bare port number becomes a localhost
/// URL, a bare hostname gets an http:// scheme, URLs pass through.
pub(crate) fn normalize_target(raw: &str) -> String {
  let trimmed = raw.trim();
  if let Ok(port) = trimmed.parse::<u16>() {
    format!("http://localhost:{}", port)
  } else if trimmed.contains("://") {
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
  let (mode, local_port) = match (cli.command, cli.bind_tunnels) {
    (None, Some(id)) => (CliMode::BindTunnels(id.trim().to_string()), None),
    (None, None) => (CliMode::Run, None),
    (Some(Command::Tcp { local_port }), _) => (CliMode::TcpBridge, Some(local_port)),
    (Some(Command::Check), _) => (CliMode::Check, None),
    (Some(Command::Completions { shell }), _) => (CliMode::Completions(shell), None),
    (Some(Command::Api { command }), _) => (CliMode::Api(command), None),
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
