use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

mod bind_tunnels;
mod check;
mod config;
mod protocol;
mod proxy;
mod service;
mod tcp;
mod udp;

use check::run_check;
use config::{
  CliMode, ClientSettings, FileConfig, build_ws_url, load_file_config, load_home_config,
  parse_bandwidth, parse_cli, resolve_settings, resolve_sources,
};

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
use service::{ServiceSpec, Shared, run_service};
use tcp::run_tcp_bridge;

#[tokio::main]
/// Entry point for the Aperio client. Resolves the layered configuration,
/// spawns one service task per exposed target, and supervises them:
/// a config-file change re-resolves everything and respawns the services,
/// so every setting takes effect on hot-reload.
async fn main() {
  // Parse CLI first so `--help` and argument errors never emit JSON logs.
  let cli = parse_cli();

  // Initialize logging. Interactive terminals get human-readable output;
  // non-TTY stdout (Docker, pipes, service managers) keeps the structured
  // JSON format (pino.js style). APERIO_LOG_FORMAT=json|pretty overrides
  // the auto-detection.
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });

  let json_logs = match std::env::var("APERIO_LOG_FORMAT").ok().as_deref() {
    Some("json") => true,
    Some("pretty") | Some("text") => false,
    _ => {
      use std::io::IsTerminal;
      !std::io::stdout().is_terminal()
    }
  };
  if json_logs {
    tracing_subscriber::fmt()
      .json()
      .with_current_span(false)
      .with_span_list(false)
      .flatten_event(true)
      .with_env_filter(log_filter)
      .init();
  } else {
    tracing_subscriber::fmt()
      .compact()
      .with_target(false)
      .with_env_filter(log_filter)
      .init();
  }

  info!("Starting Aperio Client...");

  // Configuration layering: CLI > ./aperio.yaml > environment > ~/.aperio.yaml.
  let home_cfg = load_home_config();
  let file_cfg = load_file_config(cli.opts.config.as_deref());
  let settings = resolve_settings(&cli, &home_cfg, &file_cfg);

  // Diagnostics mode reports missing config instead of exiting on it.
  if let CliMode::Check = cli.mode {
    run_check(&settings, &resolve_sources(&cli, &home_cfg, &file_cfg)).await;
  }

  // TCP bridge mode short-circuits the tunnel client entirely.
  if let CliMode::TcpBridge = cli.mode {
    let token = settings.token.clone().unwrap_or_else(|| {
      error!("CRITICAL SECURITY ERROR: a tunnel token is required (--server-token, APERIO_SERVER_TOKEN, or yaml: server.token)!");
      std::process::exit(1);
    });
    let server = settings.server.clone().unwrap_or_else(|| {
      error!("CRITICAL ERROR: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)!");
      std::process::exit(1);
    });
    run_tcp_bridge(cli.local_port.unwrap_or(0), &server, &token).await;
    return;
  }

  // Bind-tunnels mode: run local listeners for a peer client's declared
  // tunnels instead of exposing anything.
  if let CliMode::BindTunnels(ref id) = cli.mode {
    let server = settings.server.clone().unwrap_or_else(|| {
      error!("CRITICAL ERROR: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)!");
      std::process::exit(1);
    });
    bind_tunnels::run_bind_tunnels(&settings, &server, id).await;
  }

  // Stable instance id base, kept across reconnects and config respawns so
  // the server's failover `wait` mode keeps recognizing this client. Each
  // service derives its own id from it by index. `--client-id` (or yaml
  // client_id / APERIO_CLIENT_ID) makes it persistent across runs; it must
  // be a UUID like the generated default.
  let client_id = match settings.client_id {
    Some(ref explicit) => match uuid::Uuid::parse_str(explicit) {
      Ok(u) => u.to_string(),
      Err(_) => {
        error!(
          "CRITICAL ERROR: client_id '{}' is not a valid UUID (--client-id / APERIO_CLIENT_ID / yaml: client_id)",
          explicit
        );
        std::process::exit(1);
      }
    },
    None => uuid::Uuid::new_v4().to_string(),
  };

  let mut specs = match build_specs(&settings, &client_id, cli.target.is_some()) {
    Ok(specs) => specs,
    Err(e) => {
      error!("{}", e);
      std::process::exit(1);
    }
  };
  for spec in &specs {
    log_spec(spec);
  }

  // Graceful shutdown state: a signal marks the client as draining, the
  // server is notified, and the process exits once in-flight work finishes.
  let shared = Shared {
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
  };
  {
    let shutting_down = shared.shutting_down.clone();
    let shutdown_notify = shared.shutdown_notify.clone();
    tokio::spawn(async move {
      let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
      };
      #[cfg(unix)]
      let terminate = async {
        if let Ok(mut sig) =
          tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
          sig.recv().await;
        } else {
          std::future::pending::<()>().await;
        }
      };
      #[cfg(not(unix))]
      let terminate = std::future::pending::<()>();

      tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
      }
      info!("Shutdown signal received: draining before exit...");
      shutting_down.store(true, Ordering::SeqCst);
      shutdown_notify.notify_waiters();
    });
  }

  // Config hot-reload: when the yaml config file changes on disk, the
  // supervisor re-resolves the full layered configuration and respawns the
  // service with it, so every setting (not just a subset) is applied. CLI
  // arguments and environment variables keep their place in the layering.
  let config_path = cli
    .opts
    .config
    .clone()
    .unwrap_or_else(|| "aperio.yaml".to_string());
  let (reload_tx, mut reload_rx) = watch::channel(0u64);
  if std::path::Path::new(&config_path).exists() {
    let watch_path = config_path.clone();
    let mut last_mtime = std::fs::metadata(&watch_path)
      .ok()
      .and_then(|m| m.modified().ok());
    info!("- Watching {} for configuration changes", watch_path);
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mtime = std::fs::metadata(&watch_path)
          .ok()
          .and_then(|m| m.modified().ok());
        if mtime != last_mtime {
          last_mtime = mtime;
          info!(
            "Configuration file {} changed; reloading and restarting services",
            watch_path
          );
          reload_tx.send_modify(|generation| *generation += 1);
        }
      }
    });
  }

  // Supervisor: run the services, respawn them with fresh settings on reload.
  let mut running = spawn_services(&specs, &shared);
  loop {
    if reload_rx.changed().await.is_err() {
      break;
    }
    let reloaded = std::fs::read_to_string(&config_path)
      .map_err(|e| e.to_string())
      .and_then(|raw| serde_yaml::from_str::<FileConfig>(&raw).map_err(|e| e.to_string()));
    match reloaded {
      Ok(new_file_cfg) => {
        let s = resolve_settings(&cli, &load_home_config(), &new_file_cfg);
        match build_specs(&s, &client_id, cli.target.is_some()) {
          Ok(new_specs) => {
            for (cancel_tx, _) in &running {
              let _ = cancel_tx.send(true);
            }
            for (_, task) in running.drain(..) {
              let _ = task.await;
            }
            specs = new_specs;
            info!(
              "Configuration reloaded from {} ({} service(s))",
              config_path,
              specs.len()
            );
            for spec in &specs {
              log_spec(spec);
            }
            running = spawn_services(&specs, &shared);
          }
          Err(e) => warn!(
            "Config reload from {} produced an invalid configuration ({}); keeping previous",
            config_path, e
          ),
        }
      }
      Err(e) => warn!(
        "Config reload from {} failed ({}); keeping previous configuration",
        config_path, e
      ),
    }
  }
  for (_, task) in running {
    let _ = task.await;
  }
}

/// Spawns one task per service, each with its own cancel channel.
fn spawn_services(
  specs: &[ServiceSpec],
  shared: &Shared,
) -> Vec<(watch::Sender<bool>, tokio::task::JoinHandle<()>)> {
  specs
    .iter()
    .map(|spec| {
      let (cancel_tx, cancel_rx) = watch::channel(false);
      let handle = tokio::spawn(run_service(spec.clone(), shared.clone(), cancel_rx));
      (cancel_tx, handle)
    })
    .collect()
}

/// Validates the resolved settings and builds the runnable service specs.
///
/// Single-service mode uses the top-level `target`; a non-empty `services:`
/// list in the local config file expands to one spec per entry, with unset
/// per-entry knobs falling back to the top-level resolved values. A CLI
/// positional target always wins and forces single-service mode. Returns an
/// error message (used verbatim in logs) when a required value is missing or
/// invalid.
fn build_specs(
  settings: &ClientSettings,
  client_id_base: &str,
  cli_target_given: bool,
) -> Result<Vec<ServiceSpec>, String> {
  let token = settings
    .token
    .clone()
    .filter(|t| !t.trim().is_empty())
    .ok_or(
      "CRITICAL SECURITY ERROR: a tunnel token is required (--server-token, APERIO_SERVER_TOKEN, or yaml: server.token)!",
    )?;
  let server_addr = settings.server.clone().ok_or(
    "CRITICAL ERROR: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)!",
  )?;
  let ws_url =
    build_ws_url(&server_addr).map_err(|e| format!("Failed to build WebSocket URL: {}", e))?;

  let parse_bw = |raw: Option<&str>| {
    raw.and_then(|raw| {
      let parsed = parse_bandwidth(raw);
      if parsed.is_none() {
        warn!("Invalid bandwidth value '{}'; ignoring", raw);
      }
      parsed
    })
  };

  let tunnels = validate_tunnels(&settings.tunnels)?;

  if settings.services.is_empty() || cli_target_given {
    if cli_target_given && !settings.services.is_empty() {
      warn!(
        "A positional target was given on the command line; ignoring the {} entry/entries of the services: list",
        settings.services.len()
      );
    }
    // A client may run with only a tunnels: list (nothing exposed): the
    // connection then serves no HTTP target and exists purely so a peer
    // can bind the declared tunnels in an emergency.
    let target = match settings.target.clone() {
      Some(t) => t,
      None if !tunnels.is_empty() => String::new(),
      None => {
        return Err(
          "CRITICAL ERROR: the target is required (positional argument, APERIO_TARGET, yaml: target, or a services:/tunnels: list)!".to_string(),
        );
      }
    };
    return Ok(vec![ServiceSpec {
      name: None,
      client_id: client_id_base.to_string(),
      token,
      server_addr,
      ws_url,
      target,
      hostname: settings.hostname.clone(),
      path: settings.path.clone(),
      trim_bind: if settings.path.is_some() {
        settings.trim_bind.unwrap_or(true)
      } else {
        false
      },
      pass_hostname: settings.pass_hostname,
      max_response_body: settings.max_response_body,
      timeout_secs: settings.timeout_secs,
      max_concurrent: settings.max_concurrent,
      priority: settings.priority,
      bandwidth_bps: parse_bw(settings.bandwidth.as_deref()),
      max_message_size: settings.max_message_size,
      max_redirects: settings.max_redirects,
      tcp_target: settings.tcp_target.clone(),
      target_health: settings.target_health.clone(),
      health_interval: settings.health_interval,
      health_timeout: settings.health_timeout,
      health_threshold: settings.health_threshold,
      public: settings.public,
      visitor_auth: settings.visitor_auth.clone(),
      tunnels,
      headers: settings.headers.clone(),
    }]);
  }

  // Multi-service mode: one spec (and one tunnel connection) per entry.
  // Binds (hostname/path/tcp_target/target_health) are strictly per entry;
  // tuning knobs fall back to the top-level resolved values.
  settings
    .services
    .iter()
    .enumerate()
    .map(|(i, entry)| {
      let describe = || {
        entry
          .name
          .clone()
          .unwrap_or_else(|| format!("services[{}]", i))
      };
      let target = entry
        .target
        .clone()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| format!("CRITICAL ERROR: service '{}' has no target!", describe()))?;
      let path = entry.path.clone();
      Ok(ServiceSpec {
        name: entry.name.clone(),
        client_id: format!("{}-{}", client_id_base, i),
        token: token.clone(),
        server_addr: server_addr.clone(),
        ws_url: ws_url.clone(),
        target,
        hostname: entry
          .hostname
          .clone()
          .map(|h| h.trim().to_ascii_lowercase())
          .filter(|h| !h.is_empty()),
        trim_bind: if path.is_some() {
          entry.trim_bind.or(settings.trim_bind).unwrap_or(true)
        } else {
          false
        },
        path,
        pass_hostname: entry.pass_hostname.unwrap_or(settings.pass_hostname),
        max_response_body: entry
          .max_response_body
          .unwrap_or(settings.max_response_body),
        timeout_secs: entry.timeout.unwrap_or(settings.timeout_secs),
        max_concurrent: entry
          .max_concurrent
          .or(settings.max_concurrent)
          .filter(|n| *n > 0),
        priority: entry.priority.unwrap_or(settings.priority),
        bandwidth_bps: parse_bw(entry.bandwidth.as_deref().or(settings.bandwidth.as_deref())),
        max_message_size: settings.max_message_size,
        max_redirects: entry.max_redirects.unwrap_or(settings.max_redirects),
        tcp_target: entry
          .tcp_target
          .clone()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty()),
        target_health: entry
          .target_health
          .clone()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty()),
        health_interval: entry
          .health_interval
          .unwrap_or(settings.health_interval)
          .max(1),
        health_timeout: entry
          .health_timeout
          .unwrap_or(settings.health_timeout)
          .max(1),
        health_threshold: entry
          .health_threshold
          .unwrap_or(settings.health_threshold)
          .max(1),
        public: entry.public.unwrap_or(settings.public),
        visitor_auth: entry
          .auth
          .clone()
          .filter(|s| !s.trim().is_empty())
          .or_else(|| settings.visitor_auth.clone()),
        tunnels: tunnels.clone(),
        headers: entry.headers.clone().or_else(|| settings.headers.clone()),
      })
    })
    .collect()
}

/// Validates the `tunnels:` list: only TCP is supported for now, targets
/// must be `host:port`, and duplicates are rejected. Returns the normalized
/// declarations.
fn validate_tunnels(
  raw: &[crate::protocol::TunnelDecl],
) -> Result<Vec<crate::protocol::TunnelDecl>, String> {
  let mut seen = std::collections::HashSet::new();
  let mut out = Vec::with_capacity(raw.len());
  for decl in raw {
    let target = decl.target.trim().to_string();
    let protocol = decl.protocol.trim().to_ascii_lowercase();
    if protocol != "tcp" && protocol != "udp" {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' declares protocol '{}'; only tcp and udp are supported",
        target, decl.protocol
      ));
    }
    let port_ok = target
      .rsplit_once(':')
      .and_then(|(host, port)| {
        let port = port.parse::<u16>().ok().filter(|p| *p > 0)?;
        if host.is_empty() { None } else { Some(port) }
      })
      .is_some();
    if !port_ok {
      return Err(format!(
        "CRITICAL ERROR: tunnel target '{}' is not a host:port address",
        decl.target
      ));
    }
    if !seen.insert((target.clone(), protocol.clone())) {
      return Err(format!(
        "CRITICAL ERROR: tunnel target '{}' ({}) is declared more than once",
        target, protocol
      ));
    }
    out.push(crate::protocol::TunnelDecl { target, protocol });
  }
  Ok(out)
}

/// Logs the effective configuration of a service at startup.
fn log_spec(spec: &ServiceSpec) {
  match spec.name {
    Some(ref name) => info!("Service '{}' configured:", name),
    None => info!("Configuration loaded:"),
  }
  info!("- Client ID: {}", spec.client_id);
  if spec.target.is_empty() {
    info!("- Target: (none — tunnels only)");
  } else {
    info!("- Target: {}", spec.target);
  }
  info!("- Pass Hostname: {}", spec.pass_hostname);
  if let Some(ref bind) = spec.path {
    info!("- Path Bind: {}", bind);
    info!("- Trim Bind: {}", spec.trim_bind);
  }
  if let Some(ref host) = spec.hostname {
    info!("- Hostname Bind: {}", host);
  }
  if let Some(n) = spec.max_concurrent {
    info!("- Max Concurrent Requests: {}", n);
  }
  if spec.priority > 0 {
    info!(
      "- Load Balancing Priority: {} (standby tier)",
      spec.priority
    );
  }
  if let Some(bw) = spec.bandwidth_bps {
    info!("- Announced Bandwidth: {} bytes/s", bw);
  }
  if let Some(ref t) = spec.tcp_target {
    info!("- TCP Target: {}", t);
  }
  if spec.public {
    info!("- Public: visitor auth gate skipped for this service (token permitting)");
  }
  if spec.visitor_auth.is_some() {
    info!("- Visitor auth: this service is gated behind a client-set login (token permitting)");
  }
  for t in &spec.tunnels {
    info!(
      "- Tunnel: {} ({}) — bindable by a peer client with this token and client id",
      t.target, t.protocol
    );
  }
  info!("- WebSocket URL: {}", spec.ws_url);
}
