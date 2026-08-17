//! Configuration: CLI arguments (clap), the `aperio.yaml` files, environment
//! variables, and the layering between them.
//!
//! Sources from lowest to highest precedence:
//!
//! 1. `~/.aperio.yaml`, user-level defaults shared across projects
//! 2. environment variables
//! 3. `./aperio.yaml` (or the `--config` path)
//! 4. CLI arguments
//!
//! Naming is mechanical across the three surfaces: CLI `--server-token` ↔
//! yaml `server.token` ↔ env `APERIO_SERVER_TOKEN`. Each setting has exactly
//! one canonical env name (no `APERIO_CLIENT_*` scoping aliases), `client_id`
//! keeps `APERIO_CLIENT_ID` because "client" is part of the concept, not a
//! redundant prefix.

// Split by which layer each part is about: the command line, the file, and the
// pass that turns four sources into one settings struct.
pub(crate) mod cli;
pub(crate) mod file;
pub(crate) mod resolve;

pub(crate) use cli::*;
pub(crate) use file::*;
pub(crate) use resolve::*;

use std::collections::HashMap;

use crate::protocol::TunnelDecl;
// The `aperio.yaml` structs live in the shared `aperio-config` crate so the
// build script can derive a JSON Schema from the exact types parsed here.
pub(crate) use aperio_config::{FileConfig, HeaderRules, SecurityHeaders, ServiceEntry};

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

/// Fully resolved client settings, after layering CLI > ./aperio.yaml >
/// environment > ~/.aperio.yaml and applying defaults.
pub(crate) struct ClientSettings {
  pub(crate) token: Option<String>,
  /// Autoscaling declaration (config files only): the endpoint the server
  /// calls when a service of this client needs capacity.
  pub(crate) scaling: Option<aperio_config::ScalingDecl>,
  /// Retire this client after it has served nothing for this long, in
  /// seconds (config files / env only; None = never).
  pub(crate) idle_timeout: Option<u64>,
  /// The Aperio version this config declares it was written for; drives the
  /// upgrade-safety check at startup. None = the file says nothing.
  pub(crate) config_version: Option<String>,
  /// Admin API key used by the `api` subcommand (never by the tunnel).
  pub(crate) api_key: Option<String>,
  pub(crate) server: Option<String>,
  /// Additional server URLs to fail over to, tried in order after `server`.
  pub(crate) server_urls: Vec<String>,
  pub(crate) target: Option<String>,
  /// Static directory to serve instead of a backend (single-service mode;
  /// mutually exclusive with `target`).
  pub(crate) serve: Option<String>,
  /// Public hostname(s) claimed for this client's traffic (one string, a
  /// list, or a comma-separated CLI/env value).
  pub(crate) hostnames: Vec<String>,
  pub(crate) path: Option<String>,
  /// Explicit trim_bind wish; `None` = default (true when a path bind is set).
  pub(crate) trim_bind: Option<bool>,
  pub(crate) pass_hostname: bool,
  pub(crate) max_response_body: usize,
  /// Backend retry policy: total attempts (1 = off), first backoff in
  /// milliseconds (doubled per attempt), and whether non-idempotent methods
  /// are retried too.
  /// Seconds a config reload waits for in-flight requests before dropping a
  /// stopped service's connection (0 = drop at once).
  pub(crate) reload_drain_secs: u64,
  pub(crate) retry_attempts: u32,
  pub(crate) retry_backoff_ms: u64,
  pub(crate) retry_all_methods: bool,
  /// Backend circuit breaker: consecutive failures that open it (0 = off) and
  /// how long it stays open before one request probes the backend again.
  pub(crate) breaker_failures: u32,
  pub(crate) breaker_open_for_secs: u64,
  /// Largest request body, in bytes, visitors may upload (None = only the
  /// server's global limit applies). Announced via Ping; the server rejects
  /// bigger uploads with an early 413 before they enter the tunnel.
  pub(crate) max_request_body: Option<u64>,
  /// Per-service override of the server's gateway response timeout, in seconds
  /// (announced via Ping; None = the server's global value applies).
  pub(crate) response_timeout: Option<u64>,
  pub(crate) timeout_secs: u64,
  pub(crate) max_concurrent: Option<u32>,
  /// Seconds to wait for the TCP connection to a backend (yaml
  /// `connect_timeout`, env `APERIO_CONNECT_TIMEOUT`; None = only the
  /// whole-request timeout applies).
  pub(crate) connect_timeout: Option<u64>,
  /// Lowest TLS version accepted from an `https://` backend, `1.2` or `1.3`
  /// (yaml `min_tls_version`, env `APERIO_MIN_TLS_VERSION`).
  pub(crate) min_tls_version: Option<String>,
  /// Move the announced `max_concurrent` with backend pressure (yaml
  /// `adaptive_concurrency`, env `APERIO_ADAPTIVE_CONCURRENCY`).
  pub(crate) adaptive_concurrency: bool,
  /// The OTLP bridge block (yaml `otel_bridge`), unset = no bridge.
  pub(crate) otel_bridge: Option<aperio_config::OtelBridge>,
  /// Seconds a service waits before opening its tunnel (yaml `startup_delay`,
  /// env `APERIO_STARTUP_DELAY`).
  pub(crate) startup_delay: Option<u64>,
  /// Path to write the process pid to (yaml `pid_file`, env
  /// `APERIO_PID_FILE`).
  pub(crate) pid_file: Option<String>,
  /// Static Prometheus labels announced to the server (yaml `metrics_labels`,
  /// env `APERIO_METRICS_LABELS` as `k=v,k=v`).
  pub(crate) metrics_labels: std::collections::BTreeMap<String, String>,
  /// Parallel tunnel connections per service (yaml `connections`, env
  /// `APERIO_CONNECTIONS` / `APERIO_CONNECTIONS_MIN` / `APERIO_CONNECTIONS_MAX`;
  /// 1 = default). A range makes the pool elastic.
  pub(crate) connections: Option<aperio_config::Connections>,
  /// Carry every service that asks for it on one WebSocket instead of one each
  /// (yaml `multiplex`, env `APERIO_MULTIPLEX`). The file-wide default; a
  /// `services:` entry may override it either way.
  pub(crate) multiplex: bool,
  pub(crate) priority: u32,
  pub(crate) bandwidth: Option<String>,
  pub(crate) max_message_size: usize,
  pub(crate) max_redirects: usize,
  pub(crate) tcp_target: Option<String>,
  /// What to call this client's service on screen, for a single service named
  /// on the command line or in the environment. A `services:` entry carries
  /// its own `custom_name:`.
  pub(crate) custom_name: Option<String>,
  pub(crate) target_health: Option<String>,
  /// Hold the service out of routing until the backend first accepts a
  /// connection (superseded by `target_health` when that is set).
  pub(crate) wait_for_backend: bool,
  pub(crate) health_interval: u64,
  pub(crate) health_timeout: u64,
  pub(crate) health_threshold: u32,
  /// Ask the server to skip its visitor auth gate for this service.
  pub(crate) public: bool,
  /// This service's visitor gate: the `user:password` scalar that predates
  /// the grammar, one `{method: ...}` block, or a list of them
  /// (None = no override, the server's own gate applies).
  pub(crate) visitor_auth: Option<aperio_config::AuthSetting>,
  /// Visitor IPs/CIDRs allowed to reach the exposed service (empty = everyone).
  pub(crate) allowed_ips: Vec<String>,
  /// Header add/remove rules for proxied traffic (config files only;
  /// per-service `headers:` entries override this).
  pub(crate) headers: Option<HeaderRules>,
  /// Security response-header preset (config files only; per-service
  /// `security_headers:` entries override this).
  pub(crate) security_headers: Option<SecurityHeaders>,
  /// Opt into the server-side response cache (server must enable APERIO_CACHE).
  pub(crate) cache: bool,
  /// Keep serving cached responses while this client is offline (server-side).
  pub(crate) resilience: bool,
  /// Record transactions for the dashboard's request inspector (default true).
  pub(crate) capture: bool,
  /// Persist inbound POSTs into the server's webhook inbox (announced via Ping).
  pub(crate) webhook_inbox: bool,
  /// Redirect URL for visitors rejected by `allowed_ips` (None = stealth).
  pub(crate) denied: Option<String>,
  /// IP family to dial the server over (auto/ipv4/ipv6). Process-wide; applied
  /// at startup via `dial::set_ip_family`.
  pub(crate) ip_family: crate::dial::IpFamily,
  /// TLS floor and cipher suites for the tunnel dial. Process-wide like
  /// `ip_family`, and applied at startup via `dial::set_tls_policy`.
  pub(crate) tls_policy: crate::dial::TlsPolicy,
  /// Proxy the tunnel dial goes through, if the network needs one.
  /// Process-wide like the two above, applied via `dial::set_egress_proxy`.
  pub(crate) egress_proxy: Option<crate::egress::EgressProxy>,
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
  pub(crate) bind_tunnels: HashMap<String, aperio_config::BindTunnelValue>,
  /// `subscribe:` entries: the filters this process listens to, and the
  /// commands some of them run.
  pub(crate) subscribe: Vec<aperio_config::SubscribeEntry>,
  /// Local address the message face listens on (None = no local listener).
  pub(crate) messages_listen: Option<String>,
  /// Local address the MQTT face listens on (None = no MQTT listener).
  pub(crate) messages_mqtt_listen: Option<String>,
  /// Static-file mode: SPA history fallback (process-wide).
  pub(crate) serve_spa: bool,
  /// Static-file mode: custom 404 page path (process-wide).
  pub(crate) serve_404: Option<String>,
  /// Trust-on-first-use device key announced with the token.
  pub(crate) device_key: Option<String>,
  /// File the device key is read from (and generated into on first run).
  pub(crate) device_key_file: Option<String>,
  // `log_level` / `log_format` are deliberately absent: the subscriber has to
  // be installed before the config files are loaded, so they are resolved by
  // `log_settings` instead (same layering, just earlier).
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

/// Reports which layer each core setting came from, the diagnostic
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
      env_str("APERIO_SERVER_URL").as_ref(),
      home_url.as_ref(),
    ),
    token: source_of(
      cli.opts.server_token.is_some(),
      local_token.as_ref(),
      env_str("APERIO_SERVER_TOKEN").as_ref(),
      home_token.as_ref(),
    ),
    target: source_of(
      cli.target.is_some(),
      local.target.as_ref(),
      env_str("APERIO_TARGET").as_ref(),
      home.target.as_ref(),
    ),
  }
}

/// Non-empty environment lookup.
fn env_str(key: &str) -> Option<String> {
  std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
  env_str(key).and_then(|v| v.parse().ok())
}

fn env_bool(key: &str) -> Option<bool> {
  env_str(key).map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Layered lookup: CLI > local file > environment > home file.
fn layered<T>(cli: Option<T>, local: Option<T>, env: Option<T>, home: Option<T>) -> Option<T> {
  cli.or(local).or(env).or(home)
}

/// The keys the client's yaml files actually write, so an upgrade report can
/// skip a change about a key this deployment never set.
///
/// Read separately from `FileConfig`: the typed struct cannot say whether a
/// key was written or merely defaulted, and that difference is the whole point
/// here. Both layers count, a key inherited from `~/.aperio.yaml` is as set
/// as one written next to the binary.
pub(crate) fn config_keys(explicit_config: Option<&str>) -> aperio_config::compat::ConfigKeys {
  let read = |path: &str| -> Option<serde_yaml::Mapping> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_yaml::from_str::<serde_yaml::Value>(&raw).ok()? {
      serde_yaml::Value::Mapping(map) => Some(map),
      _ => None,
    }
  };
  let mut merged = serde_yaml::Mapping::new();
  for doc in [
    home_config_path().and_then(|p| read(&p.to_string_lossy())),
    read(explicit_config.unwrap_or("aperio.yaml")),
  ]
  .into_iter()
  .flatten()
  {
    for (k, v) in doc {
      merged.insert(k, v);
    }
  }
  aperio_config::compat::ConfigKeys::from_mapping(&merged)
}

/// The two logging settings, resolved before the subscriber is installed.
pub(crate) struct LogSettings {
  pub(crate) level: Option<String>,
  pub(crate) format: Option<String>,
}

/// Resolves `log_level` / `log_format` ahead of everything else.
///
/// Logging has to be initialized before the configuration files are loaded,
/// since loading them logs; so these two keys are read from the files here,
/// silently, and the ordinary layered resolution happens later as usual. A
/// file that does not parse is ignored rather than reported: the real load a
/// moment later reports it properly, through the logger this call configures.
pub(crate) fn log_settings(explicit_config: Option<&str>) -> LogSettings {
  let read = |path: &str| -> Option<FileConfig> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str::<FileConfig>(&raw).ok()
  };
  let local = read(explicit_config.unwrap_or("aperio.yaml"));
  let home = home_config_path().and_then(|p| read(&p.to_string_lossy()));
  let pick = |f: fn(&FileConfig) -> Option<String>, env: &str| {
    layered(
      None,
      local.as_ref().and_then(f),
      env_str(env),
      home.as_ref().and_then(f),
    )
  };
  LogSettings {
    level: pick(|c| c.log_level.clone(), "LOG_LEVEL"),
    format: pick(|c| c.log_format.clone(), "APERIO_LOG_FORMAT"),
  }
}

/// Splits a comma-separated environment value into trimmed, non-empty items.
fn split_csv(raw: &str) -> Vec<String> {
  raw
    .split(',')
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect()
}

/// Resolves `otel_bridge:` across the file and the environment.
///
/// Field by field rather than all-or-nothing, like `scaling:`: a deployment
/// keeps the shape in the file and injects the one value that differs per
/// host. Any of the four variables alone is enough to turn the bridge on,
/// which is what a container platform needs, since it has no file to edit.
fn resolve_otel_bridge(local: &FileConfig, home: &FileConfig) -> Option<aperio_config::OtelBridge> {
  let block = local
    .otel_bridge
    .clone()
    .or_else(|| home.otel_bridge.clone());
  let listen = env_str("APERIO_OTEL_BRIDGE_LISTEN");
  let listen_grpc = env_str("APERIO_OTEL_BRIDGE_LISTEN_GRPC");
  let transport = env_str("APERIO_OTEL_BRIDGE_TRANSPORT");
  let queue: Option<usize> = env_parse("APERIO_OTEL_BRIDGE_QUEUE");
  if block.is_none()
    && listen.is_none()
    && listen_grpc.is_none()
    && transport.is_none()
    && queue.is_none()
  {
    return None;
  }
  let block = block.unwrap_or(aperio_config::OtelBridge {
    listen: None,
    listen_grpc: None,
    transport: None,
    queue: None,
  });
  Some(aperio_config::OtelBridge {
    listen: listen.or(block.listen),
    listen_grpc: listen_grpc.or(block.listen_grpc),
    transport: transport.or(block.transport),
    queue: queue.or(block.queue),
  })
}

/// Resolves `metrics_labels:` across the file and the environment.
///
/// The environment spelling is a flat `k=v,k=v` list rather than a nested
/// structure, because that is what a container platform can actually inject.
/// It replaces the file's map rather than merging into it: a half-overridden
/// label set is harder to reason about than either of the two it came from.
fn resolve_metrics_labels(
  local: &FileConfig,
  home: &FileConfig,
) -> std::collections::BTreeMap<String, String> {
  let from_env = || {
    let raw = env_str("APERIO_METRICS_LABELS")?;
    let parsed: std::collections::BTreeMap<String, String> = raw
      .split(',')
      .filter_map(|pair| pair.split_once('='))
      .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
      .filter(|(k, v)| !k.is_empty() && !v.is_empty())
      .collect();
    (!parsed.is_empty()).then_some(parsed)
  };
  layered(
    None,
    local.metrics_labels.clone(),
    from_env(),
    home.metrics_labels.clone(),
  )
  .unwrap_or_default()
}

/// Resolves the top-level `connections:` across the file and the environment.
///
/// The three variables are layered as one value rather than field by field:
/// `APERIO_CONNECTIONS` and the `_MIN`/`_MAX` pair are two spellings of the
/// same setting, and letting a stray `APERIO_CONNECTIONS_MAX` quietly turn a
/// deliberate fixed count into an elastic pool is the kind of surprise this
/// setting can least afford.
fn resolve_connections(
  local: &FileConfig,
  home: &FileConfig,
) -> Option<aperio_config::Connections> {
  let from_env = || {
    let min: Option<u32> = env_parse("APERIO_CONNECTIONS_MIN");
    let max: Option<u32> = env_parse("APERIO_CONNECTIONS_MAX");
    if min.is_some() || max.is_some() {
      return Some(aperio_config::Connections::Range(
        aperio_config::ConnectionRange { min, max },
      ));
    }
    env_parse::<u32>("APERIO_CONNECTIONS")
      .filter(|n| *n > 0)
      .map(aperio_config::Connections::Fixed)
  };
  layered(
    None,
    local.connections.clone(),
    from_env(),
    home.connections.clone(),
  )
}

/// Builds the autoscaling declaration, layering the yaml block against the
/// `APERIO_SCALING_*` variables field by field rather than all-or-nothing, so
/// a deployment can keep the shape in the file and inject only the secret (or
/// the URL) from the environment. Without a URL there is nothing to call, so
/// the whole declaration stays `None`.
fn resolve_scaling(local: &FileConfig, home: &FileConfig) -> Option<aperio_config::ScalingDecl> {
  let l = local.scaling.as_ref();
  let h = home.scaling.as_ref();
  let url = layered(
    None,
    l.map(|s| s.url.clone()).filter(|u| !u.trim().is_empty()),
    env_str("APERIO_SCALING_URL"),
    h.map(|s| s.url.clone()).filter(|u| !u.trim().is_empty()),
  )?;
  Some(aperio_config::ScalingDecl {
    url,
    secret: layered(
      None,
      l.and_then(|s| s.secret.clone()),
      env_str("APERIO_SCALING_SECRET"),
      h.and_then(|s| s.secret.clone()),
    ),
    // min/max are plain numbers rather than options in the yaml struct, so a
    // block that set them is taken as authoritative and the environment only
    // fills in for a block that is absent entirely.
    min: layered(
      None,
      l.map(|s| s.min),
      env_parse("APERIO_SCALING_MIN"),
      h.map(|s| s.min),
    )
    .unwrap_or(0),
    max: layered(
      None,
      l.map(|s| s.max),
      env_parse("APERIO_SCALING_MAX"),
      h.map(|s| s.max),
    )
    .unwrap_or(0),
    cold_start: layered(
      None,
      l.and_then(|s| s.cold_start.clone()),
      env_str("APERIO_SCALING_COLD_START"),
      h.and_then(|s| s.cold_start.clone()),
    ),
    target_utilization: layered(
      None,
      l.and_then(|s| s.target_utilization),
      env_parse("APERIO_SCALING_TARGET_UTILIZATION"),
      h.and_then(|s| s.target_utilization),
    ),
    window: layered(
      None,
      l.and_then(|s| s.window.clone()),
      env_str("APERIO_SCALING_WINDOW"),
      h.and_then(|s| s.window.clone()),
    ),
    cooldown: layered(
      None,
      l.and_then(|s| s.cooldown.clone()),
      env_str("APERIO_SCALING_COOLDOWN"),
      h.and_then(|s| s.cooldown.clone()),
    ),
  })
}

/// Builds a security-header selection from the environment:
/// `APERIO_SECURITY_HEADERS` alone enables (or disables) the preset, and the
/// granular `APERIO_SECURITY_HEADERS_*` variables pick headers individually,
/// mirroring the two yaml forms. `None` when nothing is set.
fn security_headers_from_env() -> Option<SecurityHeaders> {
  let options = aperio_config::SecurityHeaderOptions {
    hsts: env_bool("APERIO_SECURITY_HEADERS_HSTS"),
    hsts_max_age: env_parse("APERIO_SECURITY_HEADERS_HSTS_MAX_AGE"),
    frame_options: env_str("APERIO_SECURITY_HEADERS_FRAME_OPTIONS"),
    nosniff: env_bool("APERIO_SECURITY_HEADERS_NOSNIFF"),
    referrer_policy: env_str("APERIO_SECURITY_HEADERS_REFERRER_POLICY"),
    csp: env_str("APERIO_SECURITY_HEADERS_CSP"),
  };
  let any_granular = options.hsts.is_some()
    || options.hsts_max_age.is_some()
    || options.frame_options.is_some()
    || options.nosniff.is_some()
    || options.referrer_policy.is_some()
    || options.csp.is_some();
  if any_granular {
    return Some(SecurityHeaders::Detailed(options));
  }
  env_bool("APERIO_SECURITY_HEADERS").map(SecurityHeaders::Flag)
}

/// Folds a `security_headers:` preset into a service's response header rules.
/// Preset headers are injected as `add` rules, but explicit `headers:` rules
/// win: a name the user already adds or removes is left untouched.
pub(crate) fn merge_security_headers(
  headers: Option<HeaderRules>,
  preset: Option<&SecurityHeaders>,
) -> Option<HeaderRules> {
  let inject = preset.map(|p| p.headers()).unwrap_or_default();
  if inject.is_empty() {
    return headers;
  }
  let mut rules = headers.unwrap_or_default();
  let response = rules.response.get_or_insert_with(Default::default);
  for (name, value) in inject {
    let user_set = response.add.keys().any(|k| k.eq_ignore_ascii_case(&name))
      || response
        .remove
        .iter()
        .any(|k| k.eq_ignore_ascii_case(&name));
    if !user_set {
      response.add.insert(name, value);
    }
  }
  Some(rules)
}

/// Splits a comma-separated allowlist into trimmed, non-empty entries.
pub(crate) fn split_ip_list(raw: &str) -> Vec<String> {
  raw
    .split(',')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string)
    .collect()
}

/// Validates one visitor allowlist entry: `*`, a plain IP, or a CIDR range.
/// Mirrors the server's `valid_ip_entry` so misconfigurations fail at startup
/// instead of silently on the server.
pub(crate) fn valid_ip_entry(entry: &str) -> bool {
  let entry = entry.trim();
  if entry == "*" {
    return true;
  }
  match entry.split_once('/') {
    Some((base, prefix)) => {
      let Ok(base_ip) = base.parse::<std::net::IpAddr>() else {
        return false;
      };
      match prefix.parse::<u32>() {
        Ok(bits) => match base_ip {
          std::net::IpAddr::V4(_) => bits <= 32,
          std::net::IpAddr::V6(_) => bits <= 128,
        },
        Err(_) => false,
      }
    }
    None => entry.parse::<std::net::IpAddr>().is_ok(),
  }
}

/// Resolves the service hostname(s) across the layers: CLI `--hostname`
/// (comma-separated), the local file, the env (`APERIO_HOSTNAME`,
/// comma-separated), then the home file. The highest layer that sets any
/// hostname wins wholesale; values are normalized to lowercase.
fn resolve_hostnames(o: &CommonOpts, local: &FileConfig, home: &FileConfig) -> Vec<String> {
  let norm = |list: Vec<String>| -> Vec<String> {
    list
      .into_iter()
      .map(|h| h.trim().to_ascii_lowercase())
      .filter(|h| !h.is_empty())
      .collect::<Vec<_>>()
  };
  let from_cli = o
    .hostname
    .as_ref()
    .map(|s| norm(split_ip_list(s)))
    .filter(|v| !v.is_empty());
  let from_local = local
    .hostname
    .clone()
    .map(|h| norm(h.into_vec()))
    .filter(|v| !v.is_empty());
  let from_env = env_str("APERIO_HOSTNAME")
    .map(|s| norm(split_ip_list(&s)))
    .filter(|v| !v.is_empty());
  let from_home = home
    .hostname
    .clone()
    .map(|h| norm(h.into_vec()))
    .filter(|v| !v.is_empty());
  from_cli
    .or(from_local)
    .or(from_env)
    .or(from_home)
    .unwrap_or_default()
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
