use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

mod api;
mod bind_tunnels;
mod check;
mod config;
mod dial;
mod e2e;
mod flow;
mod protocol;
mod proxy;
mod serve;
mod service;
mod tcp;
mod udp;

use aperio_config::format_bandwidth;
use check::run_check;
use config::{
  CliMode, ClientSettings, build_ws_url, load_file_config, load_home_config, parse_bandwidth,
  parse_cli, resolve_settings, resolve_sources,
};
use protocol::ConfigNote;

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
  // Pin the process-wide rustls provider to ring. The dependency tree pulls
  // rustls with both `ring` and `aws-lc-rs` enabled (workspace feature
  // unification), and with two providers rustls refuses to auto-select one —
  // every wss:// dial would panic at connect time without this.
  let _ = rustls::crypto::ring::default_provider().install_default();

  // Parse CLI first so `--help` and argument errors never emit JSON logs.
  let cli = parse_cli();

  // Initialize logging. Interactive terminals get human-readable output;
  // non-TTY stdout (Docker, pipes, service managers) keeps the structured
  // JSON format (pino.js style). APERIO_LOG_FORMAT=json|pretty overrides
  // the auto-detection.
  // One-shot admin API calls print their JSON answer on stdout, so their logs
  // stay quiet (warnings and above) and go to stderr: a piped `api` call must
  // emit nothing but the response document.
  let api_mode = matches!(cli.mode, CliMode::Api(_));
  // Logging has to be up before the config files are loaded (their own load
  // messages go through it), so the two logging keys are read from the files
  // separately and cheaply here. `RUST_LOG` still wins over everything.
  let log_cfg = config::log_settings(cli.opts.config.as_deref());
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let default = if api_mode { "warn" } else { "info" };
    let level = log_cfg.level.unwrap_or_else(|| default.to_string());
    tracing_subscriber::EnvFilter::new(level)
  });

  let json_logs = match log_cfg.format.as_deref() {
    Some("json") => true,
    Some("pretty") | Some("text") => false,
    _ => {
      use std::io::IsTerminal;
      !std::io::stdout().is_terminal()
    }
  };
  if api_mode {
    tracing_subscriber::fmt()
      .compact()
      .with_target(false)
      .with_writer(std::io::stderr)
      .with_env_filter(log_filter)
      .init();
  } else if json_logs {
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

  if !api_mode {
    info!("Starting Aperio Client...");
  }

  // Configuration layering: CLI > ./aperio.yaml > environment > ~/.aperio.yaml.
  let home_cfg = load_home_config();
  let file_cfg = load_file_config(cli.opts.config.as_deref());
  // At startup an unusable setting is still fatal; only a hot-reload keeps the
  // previous configuration and carries on.
  let mut settings = match resolve_settings(&cli, &home_cfg, &file_cfg) {
    Ok(s) => s,
    Err(e) => {
      error!("CRITICAL ERROR: {}", e);
      std::process::exit(1);
    }
  };

  // Upgrade safety: compare the version the file declares against this build
  // and report every recorded config-format change in between. A change with
  // security consequences stops the client here rather than letting it run
  // under a configuration whose meaning shifted.
  report_config_upgrade(
    settings.config_version.as_deref(),
    api_mode,
    &config::config_keys(cli.opts.config.as_deref()),
  );

  // Fix the server dialing family for the process. Effective at startup only;
  // a hot-reload cannot change it (mirrors other connection-level globals).
  dial::set_ip_family(settings.ip_family);

  // Admin API mode: perform one call, print the JSON answer, exit.
  if let CliMode::Api(ref command) = cli.mode {
    api::run_api(&settings, &cli.opts, command).await;
  }

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

  // The device key is a process identity: resolved from the full layering
  // once, before any connection announces it.
  service::set_device_key_sources(
    settings.device_key.clone(),
    settings.device_key_file.clone(),
  );

  // Static file mode: start one loopback server per served directory and
  // point the target(s) at them. Listeners survive config reloads — a
  // directory seen before reuses its server, a new one gets a fresh server.
  let mut serve_started: std::collections::HashMap<String, (u16, tokio::task::JoinHandle<()>)> =
    std::collections::HashMap::new();
  // Nothing is running yet at startup, so there is nothing to retire.
  if let Err(e) = apply_serve_mode(&mut settings, &mut serve_started).await {
    error!("{}", e);
    std::process::exit(1);
  }

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
    // 0 = nothing served yet, which keeps the idle clock stopped.
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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

  // Idle retirement (`idle_timeout`): once this client has served a request
  // and then stayed quiet for the configured span, it drains and exits, so a
  // scale-to-zero service scales itself back in. The server never stops a
  // client; it only ever asks for more capacity.
  if let Some(idle_secs) = settings.idle_timeout {
    let shutting_down = shared.shutting_down.clone();
    let shutdown_notify = shared.shutdown_notify.clone();
    let last_request_at = shared.last_request_at.clone();
    let inflight_requests = shared.inflight_requests.clone();
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(Duration::from_secs(idle_secs.clamp(1, 30))).await;
        if shutting_down.load(Ordering::SeqCst) {
          return;
        }
        let last = last_request_at.load(Ordering::SeqCst);
        let inflight = inflight_requests.load(Ordering::SeqCst);
        let now = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or_default()
          .as_secs();
        // `should_retire_idle` also holds retirement back while a request is
        // still in flight (a slow backend, a response streaming for longer
        // than the window) and before anything was ever served at all.
        if service::should_retire_idle(last, now, idle_secs, inflight) {
          info!(
            "Idle for {}s (idle_timeout={}s): draining and exiting",
            now.saturating_sub(last),
            idle_secs
          );
          shutting_down.store(true, Ordering::SeqCst);
          shutdown_notify.notify_waiters();
          return;
        }
      }
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
      .and_then(|raw| config::parse_reloaded_config(&raw, &config_path));
    match reloaded {
      Ok(new_file_cfg) => {
        let mut s = match resolve_settings(&cli, &load_home_config(), &new_file_cfg) {
          Ok(s) => s,
          Err(e) => {
            warn!(
              "Config reload from {} produced an invalid configuration ({}); keeping previous",
              config_path, e
            );
            continue;
          }
        };
        let serve_needed = match apply_serve_mode(&mut s, &mut serve_started).await {
          Ok(needed) => needed,
          Err(e) => {
            warn!(
              "Config reload from {} produced an invalid configuration ({}); keeping previous",
              config_path, e
            );
            continue;
          }
        };
        match build_specs(&s, &client_id, cli.target.is_some()) {
          Ok(new_specs) => {
            for (cancel_tx, _) in &running {
              let _ = cancel_tx.send(true);
            }
            for (_, task) in running.drain(..) {
              let _ = task.await;
            }
            specs = new_specs;
            // The new configuration is adopted, so listeners it dropped are
            // now genuinely unused.
            retire_unused_serve_listeners(&serve_needed, &mut serve_started);
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

/// Spawns one task per service connection, each with its own cancel channel.
/// A service with `connections: N` runs as N parallel tunnel connections; the
/// first keeps the service's client id, extras derive `<id>-c2`, `<id>-c3`, …
/// so every connection has a distinct instance id (no shared-id ambiguity for
/// failover or `--bind-tunnels` lookups).
fn spawn_services(
  specs: &[ServiceSpec],
  shared: &Shared,
) -> Vec<(watch::Sender<bool>, tokio::task::JoinHandle<()>)> {
  specs
    .iter()
    .flat_map(|spec| {
      // One shared backend-health state per service: the backend is probed
      // once (by the first connection), not once per parallel connection.
      let health = service::BackendHealth::for_spec(spec);
      (1..=spec.connections).map(move |conn| {
        let mut spec = spec.clone();
        if conn > 1 {
          spec.client_id = format!("{}-c{}", spec.client_id, conn);
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handle = tokio::spawn(run_service(
          spec,
          shared.clone(),
          cancel_rx,
          health.clone(),
          conn == 1,
        ));
        (cancel_tx, handle)
      })
    })
    .collect()
}

/// Static file mode: rewrites every `serve:` directory — the top-level one
/// (single-service mode) or per `services:` entry — into a loopback static
/// server target. One server runs per distinct directory, shared across
/// services and config reloads. Errors on conflicting backend settings.
async fn apply_serve_mode(
  settings: &mut ClientSettings,
  started: &mut std::collections::HashMap<String, (u16, tokio::task::JoinHandle<()>)>,
) -> Result<std::collections::HashSet<String>, String> {
  // Directories the (possibly reloaded) config serves. Returned rather than
  // acted on here: listeners the new config drops may only be retired once
  // that config has been fully validated and adopted, since until then the
  // services still running are the old ones, pointing at these very ports.
  let mut needed: std::collections::HashSet<String> = std::collections::HashSet::new();
  // Resolved once per (re)load and shared by every served directory: the two
  // options are process-wide, not per service.
  let serve_opts = serve::options(settings.serve_spa, settings.serve_404.as_deref());
  if let Some(dir) = settings.serve.clone() {
    if settings.target.is_some() || settings.tcp_target.is_some() {
      return Err(
        "CRITICAL ERROR: serve and target/tcp_target are mutually exclusive — the serve directory IS the backend".to_string(),
      );
    }
    if !settings.services.is_empty() {
      return Err(
        "CRITICAL ERROR: the top-level serve drives single-service mode; move it into the services: entry that should serve the directory".to_string(),
      );
    }
    let port = serve_port(&dir, &serve_opts, started).await?;
    needed.insert(dir.clone());
    settings.target = Some(format!("http://127.0.0.1:{}", port));
  }
  for (i, entry) in settings.services.iter_mut().enumerate() {
    let Some(dir) = entry
      .serve
      .clone()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty())
    else {
      continue;
    };
    let has = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    if has(&entry.target) || has(&entry.tcp_target) {
      return Err(format!(
        "CRITICAL ERROR: service '{}' sets serve together with target/tcp_target — the serve directory IS the backend",
        entry
          .name
          .clone()
          .unwrap_or_else(|| format!("services[{}]", i))
      ));
    }
    let port = serve_port(&dir, &serve_opts, started).await?;
    needed.insert(dir.clone());
    entry.target = Some(format!("http://127.0.0.1:{}", port));
  }
  Ok(needed)
}

/// Stops the static-file listeners no longer named by the adopted config.
///
/// Only safe once the new configuration is live. Doing it as part of
/// [`apply_serve_mode`] meant a reload that failed validation afterwards had
/// already closed the loopback servers the still-running services point at, so
/// the client kept the previous configuration in name only and answered every
/// visitor request with a 502.
fn retire_unused_serve_listeners(
  needed: &std::collections::HashSet<String>,
  started: &mut std::collections::HashMap<String, (u16, tokio::task::JoinHandle<()>)>,
) {
  started.retain(|dir, (_, handle)| {
    if needed.contains(dir) {
      true
    } else {
      handle.abort();
      info!(
        "Static file mode: stopped serving {} (no longer in config)",
        dir
      );
      false
    }
  });
}

/// Compares the declared config version against this build and reports what
/// changed in between, exiting when a change has security consequences.
///
/// Quiet by design: an upgrade that cannot affect the file says nothing at
/// all, so the one time it does speak is worth reading. `quiet` suppresses
/// the informational nudge in `api` mode, whose output is piped.
fn report_config_upgrade(
  declared: Option<&str>,
  quiet: bool,
  keys: &aperio_config::compat::ConfigKeys,
) {
  use aperio_config::compat::{CONFIG_CHANGES, ConfigSurface, check_upgrade, report_lines};

  let report = match check_upgrade(
    declared,
    env!("CARGO_PKG_VERSION"),
    ConfigSurface::Client,
    CONFIG_CHANGES,
    keys,
  ) {
    Ok(report) => report,
    Err(e) => {
      error!("CRITICAL ERROR: {e}");
      std::process::exit(1);
    }
  };
  if report.declared.is_none() {
    if !quiet {
      info!(
        "No `version:` in the configuration, so upgrade checks are off. Add `version: {}` to be warned when a future upgrade changes how this file is read.",
        report.current
      );
    }
    return;
  }
  if report.must_refuse() {
    for line in report_lines(&report) {
      error!("{line}");
    }
    error!(
      "CRITICAL ERROR: refusing to start under a configuration whose security-relevant settings changed meaning. Review the above, then set `version: {}` to acknowledge them.",
      report.current
    );
    std::process::exit(1);
  }
  for line in report_lines(&report) {
    warn!("{line}");
  }
}

/// Returns the loopback port serving `dir`, starting the static server on
/// first use. Directories are keyed by their configured spelling; a reload
/// with the same value reuses the running server.
async fn serve_port(
  dir: &str,
  opts: &serve::ServeOptions,
  started: &mut std::collections::HashMap<String, (u16, tokio::task::JoinHandle<()>)>,
) -> Result<u16, String> {
  if let Some((port, _)) = started.get(dir) {
    return Ok(*port);
  }
  let (port, handle) = serve::start(dir, opts.clone()).await?;
  started.insert(dir.to_string(), (port, handle));
  Ok(port)
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

  // Additional server URLs for cross-server failover (APERIO_SERVER_URLS,
  // comma-separated). The primary (server_addr) is always the first candidate;
  // the reconnect loop rotates to the next after a failed connection.
  let mut ws_urls = vec![ws_url.clone()];
  for extra in &settings.server_urls {
    match build_ws_url(extra) {
      Ok(u) if !ws_urls.contains(&u) => ws_urls.push(u),
      Ok(_) => {}
      Err(e) => tracing::warn!(
        "Ignoring invalid entry '{}' in server.urls / APERIO_SERVER_URLS: {}",
        extra,
        e
      ),
    }
  }
  if ws_urls.len() > 1 {
    info!(
      "Cross-server failover across {} servers configured",
      ws_urls.len()
    );
  }

  let parse_bw = |raw: Option<&str>| {
    raw.and_then(|raw| {
      let parsed = parse_bandwidth(raw);
      if parsed.is_none() {
        warn!("Invalid bandwidth value '{}'; ignoring", raw);
      }
      parsed
    })
  };
  // The top-level value is a budget for the whole client process, not a
  // per-service default: `allocate_bandwidth` divides it up once every spec
  // is built.
  let budget_bps = parse_bw(settings.bandwidth.as_deref());

  let tunnels = validate_tunnels(&settings.tunnels)?;

  // Visitor allowlists fail at startup, not silently on the server.
  let validate_ips = |ips: &[String], what: &str| -> Result<(), String> {
    for entry in ips {
      if !crate::config::valid_ip_entry(entry) {
        return Err(format!(
          "CRITICAL ERROR: {} has an invalid allowed_ips entry '{}'; expected an IP, a CIDR range, or '*'",
          what, entry
        ));
      }
    }
    Ok(())
  };
  validate_ips(&settings.allowed_ips, "the client configuration")?;
  for entry in &settings.services {
    if let Some(ips) = &entry.allowed_ips {
      validate_ips(
        ips,
        &format!(
          "service '{}'",
          entry.name.clone().unwrap_or_else(|| "?".into())
        ),
      )?;
    }
  }

  // Unix socket targets: must carry a path, and only exist on Unix platforms.
  let validate_unix_target = |target: &str, what: &str| -> Result<(), String> {
    if !crate::proxy::unix::is_unix_target(target) {
      return Ok(());
    }
    if cfg!(not(unix)) {
      return Err(format!(
        "CRITICAL ERROR: {} uses a unix:// target, which is not supported on this platform",
        what
      ));
    }
    if crate::proxy::unix::unix_socket_path(target).is_none() {
      return Err(format!(
        "CRITICAL ERROR: {} has a unix:// target without a socket path (expected e.g. unix:///var/run/app.sock)",
        what
      ));
    }
    Ok(())
  };
  if let Some(t) = &settings.target {
    validate_unix_target(t, "the client configuration")?;
  }
  for entry in &settings.services {
    if let Some(t) = &entry.target {
      validate_unix_target(
        t,
        &format!(
          "service '{}'",
          entry.name.clone().unwrap_or_else(|| "?".into())
        ),
      )?;
    }
  }

  // Denied-visitor redirects must be absolute http(s) URLs — anything else
  // would silently degrade to stealth on the server.
  let validate_denied = |denied: Option<&String>, what: &str| -> Result<(), String> {
    if let Some(url) = denied {
      let ok =
        (url.starts_with("http://") || url.starts_with("https://")) && url::Url::parse(url).is_ok();
      if !ok {
        return Err(format!(
          "CRITICAL ERROR: {} has an invalid denied: value '{}'; expected an absolute http(s) URL",
          what, url
        ));
      }
    }
    Ok(())
  };
  validate_denied(settings.denied.as_ref(), "the client configuration")?;
  for entry in &settings.services {
    validate_denied(
      entry.denied.as_ref(),
      &format!(
        "service '{}'",
        entry.name.clone().unwrap_or_else(|| "?".into())
      ),
    )?;
  }

  // Parallel connections per service: bounded so a typo cannot exhaust the
  // server's tunnel slots (it also has its own max_tunnels guard). Defaults to
  // 1; set `connections: N` (or APERIO_CONNECTIONS) to run N parallel
  // connections so a single dropped one (e.g. a CDN recycling a long-lived
  // WebSocket) is covered by a sibling with no visitor-facing gap.
  let clamp_connections = |raw: Option<u32>, what: &str| -> u32 {
    let n = raw.unwrap_or(1).max(1);
    if n > 16 {
      warn!(
        "{} requests {} connections; clamping to the maximum of 16",
        what, n
      );
      16
    } else {
      n
    }
  };
  // A clamped count is announced so the dashboard shows what the config asked
  // for next to what the client is really running.
  let connections_note = |raw: Option<u32>, effective: u32| -> Vec<ConfigNote> {
    let asked = raw.unwrap_or(1).max(1);
    (asked != effective)
      .then(|| ConfigNote {
        field: "connections".to_string(),
        declared: asked.to_string(),
        effective: effective.to_string(),
        reason: "clamped to the maximum of 16 parallel connections".to_string(),
      })
      .into_iter()
      .collect()
  };

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
    let connections = clamp_connections(settings.connections, "the service");
    let mut specs = vec![ServiceSpec {
      name: None,
      client_id: client_id_base.to_string(),
      token,
      instance_group: client_id_base.to_string(),
      server_addr,
      ws_url,
      ws_urls: ws_urls.clone(),
      target,
      hostnames: settings.hostnames.clone(),
      path: settings.path.clone(),
      trim_bind: if settings.path.is_some() {
        settings.trim_bind.unwrap_or(true)
      } else {
        false
      },
      pass_hostname: settings.pass_hostname,
      max_response_body: settings.max_response_body,
      max_request_body: settings.max_request_body,
      response_timeout: settings.response_timeout,
      timeout_secs: settings.timeout_secs,
      max_concurrent: settings.max_concurrent,
      connections,
      priority: settings.priority,
      bandwidth_bps: budget_bps,
      bandwidth_declared: settings.bandwidth.clone(),
      config_notes: connections_note(settings.connections, connections),
      max_message_size: settings.max_message_size,
      max_redirects: settings.max_redirects,
      tcp_target: settings.tcp_target.clone(),
      target_health: settings.target_health.clone(),
      wait_for_backend: settings.wait_for_backend,
      health_interval: settings.health_interval,
      health_timeout: settings.health_timeout,
      health_threshold: settings.health_threshold,
      public: settings.public,
      visitor_auth: settings.visitor_auth.clone(),
      allowed_ips: settings.allowed_ips.clone(),
      resilience: settings.resilience,
      webhook_inbox: settings.webhook_inbox,
      denied: settings.denied.clone(),
      scaling: settings.scaling.clone(),
      tunnels,
      headers: crate::config::merge_security_headers(
        settings.headers.clone(),
        settings.security_headers.as_ref(),
      ),
      cache: settings.cache,
    }];
    allocate_bandwidth(&mut specs, budget_bps);
    return Ok(specs);
  }

  // Multi-service mode: one spec (and one tunnel connection) per entry.
  // Binds (hostname/path/tcp_target/target_health) are strictly per entry;
  // tuning knobs fall back to the top-level resolved values.
  let mut specs: Vec<ServiceSpec> = settings
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
        .ok_or_else(|| {
          format!(
            "CRITICAL ERROR: service '{}' has no target (set target: or serve:)!",
            describe()
          )
        })?;
      let path = entry.path.clone();
      let connections = clamp_connections(
        entry.connections.or(settings.connections),
        &format!("service '{}'", describe()),
      );
      Ok(ServiceSpec {
        name: entry.name.clone(),
        client_id: format!("{}-{}", client_id_base, i),
        token: token.clone(),
        instance_group: client_id_base.to_string(),
        server_addr: server_addr.clone(),
        ws_url: ws_url.clone(),
        ws_urls: ws_urls.clone(),
        target,
        hostnames: entry
          .hostname
          .clone()
          .map(|h| {
            h.into_vec()
              .into_iter()
              .map(|s| s.trim().to_ascii_lowercase())
              .filter(|s| !s.is_empty())
              .collect::<Vec<_>>()
          })
          .filter(|v| !v.is_empty())
          .unwrap_or_default(),
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
        max_request_body: entry.max_request_body.or(settings.max_request_body),
        response_timeout: entry.response_timeout.or(settings.response_timeout),
        timeout_secs: entry.timeout.unwrap_or(settings.timeout_secs),
        max_concurrent: entry
          .max_concurrent
          .or(settings.max_concurrent)
          .filter(|n| *n > 0),
        connections,
        priority: entry.priority.unwrap_or(settings.priority),
        // Only what this entry asked for: the top-level value is the budget
        // these requests are settled against, not a fallback default.
        bandwidth_bps: parse_bw(entry.bandwidth.as_deref()),
        bandwidth_declared: entry.bandwidth.clone(),
        config_notes: connections_note(entry.connections.or(settings.connections), connections),
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
        wait_for_backend: entry.wait_for_backend.unwrap_or(settings.wait_for_backend),
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
        allowed_ips: entry
          .allowed_ips
          .clone()
          .unwrap_or_else(|| settings.allowed_ips.clone()),
        resilience: entry.resilience.unwrap_or(settings.resilience),
        webhook_inbox: entry.webhook_inbox.unwrap_or(settings.webhook_inbox),
        scaling: settings.scaling.clone(),
        denied: entry
          .denied
          .clone()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty())
          .or_else(|| settings.denied.clone()),
        tunnels: tunnels.clone(),
        headers: crate::config::merge_security_headers(
          entry.headers.clone().or_else(|| settings.headers.clone()),
          entry
            .security_headers
            .as_ref()
            .or(settings.security_headers.as_ref()),
        ),
        cache: entry.cache.unwrap_or(settings.cache),
      })
    })
    .collect::<Result<_, String>>()?;
  allocate_bandwidth(&mut specs, budget_bps);
  Ok(specs)
}

/// Settles every service's bandwidth request against the client-wide budget
/// and hands each parallel connection its own share.
///
/// The server shapes each tunnel connection with a token bucket of its own, so
/// N connections all announcing B would let the client be pushed at N*B. On
/// entry `bandwidth_bps` holds what a service asked for (`None` = it asked for
/// nothing); on exit it holds the rate a single connection of that service
/// announces, so the sum over the whole client never exceeds the budget.
///
/// With no top-level budget every service simply keeps what it asked for and
/// the rest stay unlimited. With one:
///
/// - services that named a rate keep it, and whatever is left over is split
///   equally among the services that did not,
/// - if the named rates would leave the unspecified services nothing at all,
///   every named rate is dropped (with a warning) and the budget is split
///   equally, since a service configured to run cannot be given zero,
/// - if every service named a rate and together they overshoot, the rates are
///   scaled down proportionally (with a warning) so the shares keep their
///   relative weight.
///
/// Every difference it introduces is recorded as a `ConfigNote` on the spec, so
/// the dashboard can show the announced rate together with the value the
/// operator actually wrote.
fn allocate_bandwidth(specs: &mut [ServiceSpec], budget_bps: Option<u64>) {
  if specs.is_empty() {
    return;
  }
  // Why a service's rate is not simply what it asked for, filled in by the
  // branch that settled it; the per-connection split appends its own reason.
  let mut settled: Vec<Option<String>> = vec![None; specs.len()];

  if let Some(budget) = budget_bps {
    let asked: u64 = specs.iter().filter_map(|s| s.bandwidth_bps).sum();
    let unspecified = specs.iter().filter(|s| s.bandwidth_bps.is_none()).count();
    if unspecified > 0 && asked >= budget {
      warn!(
        "The per-service bandwidth limits ({} bytes/s) leave nothing of the {} bytes/s budget for the {} service(s) without one; ignoring them and splitting the budget equally",
        asked, budget, unspecified
      );
      let share = budget / specs.len() as u64;
      let reason = format!(
        "the per-service limits left nothing of the {} budget for the {} service(s) without one, so the budget was split equally",
        format_bandwidth(budget),
        unspecified
      );
      for (i, spec) in specs.iter_mut().enumerate() {
        settled[i] = Some(reason.clone());
        spec.bandwidth_bps = Some(share);
      }
    } else if unspecified == 0 && asked > budget {
      warn!(
        "The per-service bandwidth limits add up to {} bytes/s, over the {} bytes/s budget; scaling every limit down proportionally",
        asked, budget
      );
      let reason = format!(
        "the per-service limits added up to {}, over the {} budget, so every limit was scaled down proportionally",
        format_bandwidth(asked),
        format_bandwidth(budget)
      );
      for (i, spec) in specs.iter_mut().enumerate() {
        let want = spec.bandwidth_bps.unwrap_or(0) as u128;
        settled[i] = Some(reason.clone());
        spec.bandwidth_bps = Some((want * budget as u128 / asked as u128) as u64);
      }
    } else if unspecified > 0 {
      let share = (budget - asked) / unspecified as u64;
      let reason = format!(
        "an equal share of what the {} budget leaves the services without a limit of their own",
        format_bandwidth(budget)
      );
      for (i, spec) in specs.iter_mut().enumerate() {
        if spec.bandwidth_bps.is_none() {
          settled[i] = Some(reason.clone());
          spec.bandwidth_bps = Some(share);
        }
      }
    }
  }

  // A service's share is split across its parallel connections, each of which
  // is shaped separately by the server. Never announce 0: the server reads
  // that as unlimited, which is the opposite of what a tiny share means.
  for (i, spec) in specs.iter_mut().enumerate() {
    let per_service = spec.bandwidth_bps;
    if let Some(bps) = per_service {
      spec.bandwidth_bps = Some((bps / spec.connections as u64).max(1));
    }
    let mut reasons: Vec<String> = settled[i].take().into_iter().collect();
    if per_service.is_some() && spec.connections > 1 {
      reasons.push(format!(
        "split across {} parallel connections",
        spec.connections
      ));
    }
    let declared = spec.bandwidth_declared.clone();
    let note = match (declared, spec.bandwidth_bps) {
      // Unparseable: already warned at parse time, reported here as well so
      // it shows up in the dashboard next to the value it failed to become.
      (Some(raw), _) if parse_bandwidth(&raw).is_none() => Some(ConfigNote {
        field: "bandwidth".to_string(),
        declared: raw,
        effective: "unlimited".to_string(),
        reason: "not a valid rate, so it was ignored".to_string(),
      }),
      (declared, Some(effective)) if !reasons.is_empty() => Some(ConfigNote {
        field: "bandwidth".to_string(),
        declared: declared.unwrap_or_default(),
        effective: format_bandwidth(effective),
        reason: reasons.join("; "),
      }),
      _ => None,
    };
    spec.config_notes.extend(note);
  }
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
    if decl.encrypt && protocol != "tcp" {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' sets encrypt: true, which is only supported for tcp tunnels",
        target
      ));
    }
    if decl.psk.is_some() && !decl.encrypt {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' sets a psk without encrypt: true",
        target
      ));
    }
    if let Some(secs) = decl.idle_timeout {
      if protocol != "udp" {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets idle_timeout, which is only supported for udp tunnels",
          target
        ));
      }
      if secs == 0 {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets idle_timeout: 0; it must be at least 1 second",
          target
        ));
      }
    }
    if decl.expose.is_some() {
      if protocol != "tcp" {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets expose, which is only supported for tcp tunnels",
          target
        ));
      }
      if decl.encrypt {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets expose together with encrypt: true; a public port cannot run the client-side encryption handshake",
          target
        ));
      }
    }
    out.push(crate::protocol::TunnelDecl {
      target,
      protocol,
      encrypt: decl.encrypt,
      psk: decl.psk.clone(),
      idle_timeout: decl.idle_timeout,
      expose: decl.expose.clone(),
    });
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
  match spec.hostnames.as_slice() {
    [] => {}
    [one] => info!("- Hostname Bind: {}", one),
    many => info!("- Hostname Binds: {}", many.join(", ")),
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
    if spec.connections > 1 {
      info!(
        "- Announced Bandwidth: {} bytes/s per connection ({} bytes/s across {} connections)",
        bw,
        bw * spec.connections as u64,
        spec.connections
      );
    } else {
      info!("- Announced Bandwidth: {} bytes/s", bw);
    }
  }
  if spec.connections > 1 {
    info!(
      "- Connections: {} parallel tunnel connections (ids {}, {}-c2, ...)",
      spec.connections, spec.client_id, spec.client_id
    );
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
  info!("- Server URL: {}", spec.server_addr);
  info!("- WebSocket URL: {}", spec.ws_url);
  if spec.ws_urls.len() > 1 {
    info!("- Failover servers: {}", spec.ws_urls.len());
  }
}
