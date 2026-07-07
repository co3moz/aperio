use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

mod check;
mod config;
mod protocol;
mod proxy;
mod service;
mod tcp;

use check::run_check;
use config::{
  ClientSettings, CliMode, FileConfig, build_ws_url, load_file_config, load_home_config,
  parse_bandwidth, parse_cli, resolve_settings,
};
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

  // Initialize logging with structured JSON output (pino.js style)
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });

  tracing_subscriber::fmt()
    .json()
    .with_current_span(false)
    .with_span_list(false)
    .flatten_event(true)
    .with_env_filter(log_filter)
    .init();

  info!("Starting Aperio Client...");

  // Configuration layering: CLI > ./aperio.yaml > environment > ~/.aperio.yaml.
  let home_cfg = load_home_config();
  let file_cfg = load_file_config(cli.opts.config.as_deref());
  let settings = resolve_settings(&cli, &home_cfg, &file_cfg);

  // Diagnostics mode reports missing config instead of exiting on it.
  if let CliMode::Check = cli.mode {
    run_check(&settings).await;
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

  // Stable instance id, kept across reconnects and config respawns so the
  // server's failover `wait` mode keeps recognizing this client.
  let client_id = uuid::Uuid::new_v4().to_string();

  let mut spec = match build_spec(&settings, client_id.clone()) {
    Ok(spec) => spec,
    Err(e) => {
      error!("{}", e);
      std::process::exit(1);
    }
  };
  log_spec(&spec);

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

  // Supervisor: run the service, respawn it with fresh settings on reload.
  let (mut cancel_tx, mut task) = spawn_service(spec.clone(), &shared);
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
        match build_spec(&s, client_id.clone()) {
          Ok(new_spec) => {
            let _ = cancel_tx.send(true);
            let _ = task.await;
            spec = new_spec;
            info!(
              "Configuration reloaded from {} (target: {}, hostname bind: {:?}, path bind: {:?})",
              config_path, spec.target, spec.hostname, spec.path
            );
            (cancel_tx, task) = spawn_service(spec.clone(), &shared);
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
  let _ = task.await;
}

/// Spawns one service task with its own cancel channel.
fn spawn_service(
  spec: ServiceSpec,
  shared: &Shared,
) -> (watch::Sender<bool>, tokio::task::JoinHandle<()>) {
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let handle = tokio::spawn(run_service(spec, shared.clone(), cancel_rx));
  (cancel_tx, handle)
}

/// Validates the resolved settings and builds the runnable service spec.
/// Returns an error message (used verbatim in logs) when a required value is
/// missing or invalid.
fn build_spec(settings: &ClientSettings, client_id: String) -> Result<ServiceSpec, String> {
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
  let target = settings.target.clone().ok_or(
    "CRITICAL ERROR: the target is required (positional argument, APERIO_TARGET, or yaml: target)!",
  )?;
  let ws_url =
    build_ws_url(&server_addr).map_err(|e| format!("Failed to build WebSocket URL: {}", e))?;

  // Announced link capacity (bytes/second); the server paces its frames so
  // this client's network is never flooded (e.g. "8mbit" on a DSL uplink).
  let bandwidth_bps = settings.bandwidth.as_deref().and_then(|raw| {
    let parsed = parse_bandwidth(raw);
    if parsed.is_none() {
      warn!("Invalid bandwidth value '{}'; ignoring", raw);
    }
    parsed
  });

  Ok(ServiceSpec {
    name: None,
    client_id,
    token,
    server_addr,
    ws_url,
    hostname: settings.hostname.clone(),
    trim_bind: if settings.path.is_some() {
      settings.trim_bind.unwrap_or(true)
    } else {
      false
    },
    path: settings.path.clone(),
    pass_hostname: settings.pass_hostname,
    max_response_body: settings.max_response_body,
    timeout_secs: settings.timeout_secs,
    max_concurrent: settings.max_concurrent,
    priority: settings.priority,
    bandwidth_bps,
    max_message_size: settings.max_message_size,
    max_redirects: settings.max_redirects,
    tcp_target: settings.tcp_target.clone(),
    target_health: settings.target_health.clone(),
    health_interval: settings.health_interval,
    health_timeout: settings.health_timeout,
    health_threshold: settings.health_threshold,
    target,
  })
}

/// Logs the effective configuration of a service at startup.
fn log_spec(spec: &ServiceSpec) {
  info!("Configuration loaded:");
  info!("- Client ID: {}", spec.client_id);
  info!("- Target: {}", spec.target);
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
    info!("- Load Balancing Priority: {} (standby tier)", spec.priority);
  }
  if let Some(bw) = spec.bandwidth_bps {
    info!("- Announced Bandwidth: {} bytes/s", bw);
  }
  if let Some(ref t) = spec.tcp_target {
    info!("- TCP Target: {}", t);
  }
  info!("- WebSocket URL: {}", spec.ws_url);
}
