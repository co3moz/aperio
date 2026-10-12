//! Configuration: CLI arguments (clap), the `aperio.yaml` files, environment
//! variables, and the layering between them.
//!
//! Layers from lowest to highest precedence, merged key by key by the one rule
//! in `aperio_config::layers`:
//!
//! 1. `~/.aperio.yaml`, user-level defaults shared across projects
//! 2. `./aperio.yaml` (or the `--config` path)
//! 3. environment variables
//! 4. CLI arguments
//!
//! Every setting has one name. Its environment variable is derived from it
//! (`server.url` is `APERIO_SERVER_URL`, `defaults.timeout` is
//! `APERIO_TIMEOUT`), and a CLI flag spells the same key (`--server-url`).

// Split by which layer each part is about: the command line, the file, and the
// pass that turns the layers into one settings struct.
pub(crate) mod cli;
pub(crate) mod file;
pub(crate) mod resolve;

pub(crate) use cli::*;
pub(crate) use file::*;
pub(crate) use resolve::*;

use crate::protocol::TunnelDecl;
// The `aperio.yaml` structs live in the shared `aperio-config` crate so the
// build script can derive a JSON Schema from the exact types parsed here.
pub(crate) use aperio_config::{HeaderRules, SecurityHeaders, ServiceEntry};

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

/// One service the client runs: its name (none for the one service the
/// command line or the environment describes) and its entry, with
/// `defaults:` already laid under it.
#[derive(Clone)]
pub(crate) struct Service {
  pub(crate) name: Option<String>,
  pub(crate) entry: ServiceEntry,
}

/// Fully resolved client settings, after merging the layers and applying
/// defaults.
pub(crate) struct ClientSettings {
  pub(crate) token: Option<String>,
  pub(crate) server: Option<String>,
  /// Additional server URLs to fail over to, tried in order after `server`.
  pub(crate) server_urls: Vec<String>,
  /// Admin API key used by the `api` subcommand (never by the tunnel).
  pub(crate) api_key: Option<String>,
  /// IP family to dial the server over (auto/ipv4/ipv6). Process-wide; applied
  /// at startup via `dial::set_ip_family`.
  pub(crate) ip_family: crate::dial::IpFamily,
  /// TLS floor and cipher suites for the tunnel dial. Process-wide like
  /// `ip_family`, and applied at startup via `dial::set_tls_policy`.
  pub(crate) tls_policy: crate::dial::TlsPolicy,
  /// Proxy the tunnel dial goes through, if the network needs one.
  /// Process-wide like the two above, applied via `dial::set_egress_proxy`.
  pub(crate) egress_proxy: Option<crate::egress::EgressProxy>,
  /// Fixed instance id. None = a random UUID per run.
  pub(crate) client_id: Option<String>,
  /// What to call this client on screen and in the server's logs. A label,
  /// never an address: `client_id` stays the identity.
  pub(crate) name: Option<String>,
  /// The services to run, in order.
  pub(crate) services: Vec<Service>,
  /// True when `services` is the one service the command line or the
  /// environment describes, rather than a file's map.
  pub(crate) single: bool,
  /// `defaults:` alone, for the connection a client with no service still
  /// opens to carry its tunnels and messages.
  pub(crate) defaults: ServiceEntry,
  /// Tunnels declared by this client.
  pub(crate) tunnels: Vec<TunnelDecl>,
  /// `binds:` entries, by tunnel name.
  pub(crate) binds: aperio_config::Named<aperio_config::BindValue>,
  /// `subscribe:` entries: the filters this process listens to, and the
  /// commands some of them run.
  pub(crate) subscribe: Vec<aperio_config::SubscribeEntry>,
  /// Local address the message face listens on (None = no local listener).
  pub(crate) messages_listen: Option<String>,
  /// Local address the MQTT face listens on (None = no MQTT listener).
  pub(crate) messages_mqtt_listen: Option<String>,
  /// The OTLP bridge block, unset = no bridge.
  pub(crate) otel_bridge: Option<aperio_config::OtelBridge>,
  /// Autoscaling declaration: the endpoint the server calls when a service of
  /// this client needs capacity.
  pub(crate) scaling: Option<aperio_config::ScalingDecl>,
  /// Retire this client after it has served nothing for this long, in
  /// seconds (None = never).
  pub(crate) idle_timeout: Option<u64>,
  /// Seconds a config reload waits for in-flight requests before dropping a
  /// stopped service's connection (0 = drop at once).
  pub(crate) reload_drain_secs: u64,
  /// The client's whole link budget, divided across its services.
  pub(crate) bandwidth: Option<String>,
  pub(crate) max_message_size: usize,
  /// Answer a `forward` check whose endpoint the server wrote, not this
  /// client's own config (`allow_server_forward`).
  pub(crate) allow_server_forward: bool,
  /// Static-file mode: SPA history fallback (process-wide).
  pub(crate) serve_spa: bool,
  /// Static-file mode: custom 404 page path (process-wide).
  pub(crate) serve_404: Option<String>,
  /// Trust-on-first-use device key announced with the token.
  pub(crate) device_key: Option<String>,
  /// File the device key is read from (and generated into on first run).
  pub(crate) device_key_file: Option<String>,
  /// Path to write the process pid to.
  pub(crate) pid_file: Option<String>,
  /// Which layer each value came from, for `check`.
  pub(crate) sources: std::collections::BTreeMap<String, aperio_config::layers::Layer>,
  // `log_level` / `log_format` are deliberately absent: the subscriber has to
  // be installed before the config files are loaded, so they are resolved by
  // `log_settings` instead (same layering, just earlier).
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
/// silently, and the ordinary resolution happens later as usual. A file that
/// does not parse is ignored rather than reported: the real load a moment
/// later reports it properly, through the logger this call configures.
pub(crate) fn log_settings(explicit_config: Option<&str>) -> LogSettings {
  let read = |path: &std::path::Path| -> serde_yaml::Mapping {
    std::fs::read_to_string(path)
      .ok()
      .and_then(|raw| aperio_config::layers::parse_document(&raw).ok())
      .unwrap_or_default()
  };
  let local = read(std::path::Path::new(
    explicit_config.unwrap_or("aperio.yaml"),
  ));
  let home = home_config_path().map(|p| read(&p)).unwrap_or_default();
  let pick = |key: &str| -> Option<String> {
    let var = aperio_config::layers::env_var(key);
    let from_file =
      |doc: &serde_yaml::Mapping| doc.get(key).and_then(|v| v.as_str()).map(str::to_string);
    std::env::var(&var)
      .ok()
      .filter(|v| !v.trim().is_empty())
      .or_else(|| from_file(&local))
      .or_else(|| from_file(&home))
  };
  LogSettings {
    level: pick("log_level"),
    format: pick("log_format"),
  }
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
