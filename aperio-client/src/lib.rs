use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

mod adaptive;
mod api;
mod bind_tunnels;
mod check;
mod config;
mod dial;
mod e2e;
mod egress;
mod flow;
mod health_report;
mod messages_http;
mod messages_mqtt;
mod messages_run;
mod otel_bridge;
mod protocol;
mod proxy;
mod proxy_protocol;
mod pubsub;
mod serve;
mod service;
mod tcp;
mod udp;

// What `run` does, in the order it does it: resolve the file into specs, stand
// up the process-wide facilities, then spawn a connection per service and
// supervise them.
mod client {
  pub(crate) mod bandwidth;
  pub(crate) mod facilities;
  pub(crate) mod pool;
  pub(crate) mod serve_mode;
  pub(crate) mod specs;
}

pub(crate) use client::bandwidth::*;
pub(crate) use client::facilities::*;
pub(crate) use client::pool::*;
pub(crate) use client::serve_mode::*;
pub(crate) use client::specs::*;

use aperio_config::format_bandwidth;
use check::run_check;
use config::{
  CliMode, build_ws_url, load_home_config, parse_bandwidth, parse_cli, resolve_settings,
  resolve_sources,
};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
use service::Shared;
use tcp::run_tcp_bridge;

/// Install the process-wide rustls crypto provider, once, before anything
/// builds a TLS client.
///
/// Two separate things need this and both are load-bearing. rustls itself is
/// pulled with `ring` *and* `aws-lc-rs` enabled by workspace feature
/// unification, and with two providers it refuses to auto-select. And reqwest
/// is on `rustls-no-provider`, which does not fall back at all: it panics
/// inside `ClientBuilder::build()` with "No rustls crypto provider is
/// configured".
///
/// So the provider is not merely preferred, it has to exist before the first
/// client is built. Doing it only in `run()` made that an ordering rule held
/// up by a panic, which is how the whole unit suite went down the first time
/// reqwest moved to 0.13: tests build clients without ever entering `run()`.
/// Every builder in this crate calls this first instead.
pub(crate) fn ensure_crypto_provider() {
  static ONCE: std::sync::Once = std::sync::Once::new();
  ONCE.call_once(|| {
    let _ = rustls::crypto::ring::default_provider().install_default();
  });
}

/// A reqwest client for tests.
///
/// `reqwest` is on `rustls-no-provider`, so a bare `Client::new()` panics
/// unless a crypto provider is already installed, and a unit test never
/// enters `run()`, which is where the binary installs it. Tests build their
/// clients through here so that guarantee holds without each of them having
/// to know about it.
#[cfg(test)]
pub(crate) fn test_http_client() -> reqwest::Client {
  ensure_crypto_provider();
  reqwest::Client::new()
}

/// Entry point for the Aperio client, called by the thin binary in
/// `main.rs`. Resolves the layered configuration, spawns one service task per
/// exposed target, and supervises them: a config-file change re-resolves
/// everything and respawns the services, so every setting takes effect on
/// hot-reload.
#[tokio::main]
pub async fn run() {
  // Pin the process-wide rustls provider to ring. The dependency tree pulls
  // rustls with both `ring` and `aws-lc-rs` enabled (workspace feature
  // unification), and with two providers rustls refuses to auto-select one,
  // every wss:// dial would panic at connect time without this.
  ensure_crypto_provider();

  // Parse CLI first so `--help` and argument errors never emit JSON logs.
  let cli = parse_cli();

  // Completion scripts print and exit before logging is even initialized.
  // The script goes to stdout, and this client logs to stdout too, so a
  // single startup line would be pasted into the middle of a shell function
  // and break it for whoever sourced it. It also has to be as cheap and as
  // side-effect-free as `--help`: a shell may run it at every new prompt.
  if let CliMode::Completions(shell) = cli.mode {
    config::print_completions(shell);
    return;
  }

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
  let (file_cfg, config_files) = crate::config::load_file_config_tree(cli.opts.config.as_deref());
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
  dial::set_tls_policy(settings.tls_policy.clone());
  if let Some(ref proxy) = settings.egress_proxy {
    // Worth a line at startup: an operator debugging a connection needs to
    // know the dial is not going where the config's `server:` says. Redacted,
    // because the value may carry a credential.
    info!(
      "Dialing the tunnel server through the proxy {}{}",
      proxy.redacted(),
      if proxy.has_credentials() {
        " (with a credential)"
      } else {
        ""
      }
    );
  }
  dial::set_egress_proxy(settings.egress_proxy.clone());

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

  if let Some(path) = settings.pid_file.clone() {
    write_pid_file(&path);
  }

  // Static file mode: start one loopback server per served directory and
  // point the target(s) at them. Listeners survive config reloads, a
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
  // The OTel bridge: a local OTLP receiver whose exports travel to the server
  // and on to its collector. Started before the services so an exporter that
  // comes up with them is never refused, and its queue lives in `Shared` so
  // whichever tunnel connection is live can carry the exports.
  let (otel_exports, otel_credentials) = start_otel_bridge(&settings).await;

  let shared = Shared {
    otel_exports,
    shutting_down: Arc::new(AtomicBool::new(false)),
    shutdown_notify: Arc::new(tokio::sync::Notify::new()),
    inflight_requests: Arc::new(AtomicUsize::new(0)),
    ready_services: watch::channel(std::collections::HashMap::new()).0,
    // 0 = nothing served yet, which keeps the idle clock stopped.
    last_request_at: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    messages: crate::pubsub::MessageBus::new(
      settings.subscribe.iter().map(|e| e.topic.clone()).collect(),
    ),
  };

  // The local face, if the operator asked for one. Started before the
  // services so an application attaching immediately does not race the first
  // connection: the bus exists either way, and a publish before any tunnel is
  // up is refused with a reason rather than silently dropped.
  // The message faces and the subscription runners. Started before the
  // services so a message arriving on the first connection already has
  // somewhere to go, and kept in `facilities` so a reload can apply changes
  // to them: they used to be started once here and never looked at again, so
  // a listener removed from the file went on listening and a new `subscribe:`
  // entry needed a restart, while the documentation said every setting
  // applies on reload.
  let mut facilities = ProcessFacilities {
    otel_credentials,
    ..Default::default()
  };
  if let Err(e) = facilities.apply(&settings, &shared, true).await {
    error!("CRITICAL ERROR: {}", e);
    std::process::exit(1);
  }
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
  // Re-read on every pass rather than captured, so a reload can change it, and
  // 0 means "not configured", which is also what a reload that removed the
  // setting has to be able to say.
  let idle_timeout = Arc::new(std::sync::atomic::AtomicU64::new(
    settings.idle_timeout.unwrap_or(0),
  ));
  {
    let idle_timeout = idle_timeout.clone();
    let shutting_down = shared.shutting_down.clone();
    let shutdown_notify = shared.shutdown_notify.clone();
    let last_request_at = shared.last_request_at.clone();
    let inflight_requests = shared.inflight_requests.clone();
    tokio::spawn(async move {
      loop {
        // Unset polls slowly and decides nothing: it is a live cell now, and
        // a reload that turns the setting on has to be picked up without a
        // restart, so the loop cannot simply not exist.
        let poll = match idle_timeout.load(Ordering::SeqCst) {
          0 => 30,
          secs => secs.clamp(1, 30),
        };
        tokio::time::sleep(Duration::from_secs(poll)).await;
        if shutting_down.load(Ordering::SeqCst) {
          return;
        }
        // Read after the sleep, so a reload during it is already in effect.
        let idle_secs = idle_timeout.load(Ordering::SeqCst);
        if idle_secs == 0 {
          continue;
        }
        let last = last_request_at.load(Ordering::SeqCst);
        // Read after `last`, and read again below: a request that starts
        // between the two loads bumps the timestamp we already have and the
        // counter we are about to read, so taking the counter last is what
        // makes "idle" mean idle at one moment rather than across two.
        let inflight = inflight_requests.load(Ordering::SeqCst);
        let now = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or_default()
          .as_secs();
        // `should_retire_idle` also holds retirement back while a request is
        // still in flight (a slow backend, a response streaming for longer
        // than the window) and before anything was ever served at all.
        if service::should_retire_idle(last, now, idle_secs, inflight)
          && inflight_requests.load(Ordering::SeqCst) == 0
          && last_request_at.load(Ordering::SeqCst) == last
        {
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
    // Every file that contributed, so editing an included fragment is a
    // configuration change like any other. The set is re-read on each tick
    // because an edit can add or remove an include, and watching only what
    // the first parse found would then miss the very file being worked on.
    let mtimes = |paths: &[std::path::PathBuf]| -> Vec<Option<std::time::SystemTime>> {
      paths
        .iter()
        .map(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()))
        .collect()
    };
    let mut watched = config_files.clone();
    let mut last_mtime = mtimes(&watched);
    if watched.len() > 1 {
      info!(
        "- Watching {} and {} included file(s) for configuration changes",
        watch_path,
        watched.len() - 1
      );
    } else {
      info!("- Watching {} for configuration changes", watch_path);
    }
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mtime = mtimes(&watched);
        if mtime != last_mtime {
          last_mtime = mtime;
          // Re-read which files make up the configuration now.
          if let Ok((_, files)) =
            crate::config::parse_config_tree(std::path::Path::new(&watch_path))
          {
            watched = files;
            last_mtime = mtimes(&watched);
          }
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
    // Includes are followed on reload exactly as on the first load: a
    // configuration split across files has to reload as the same
    // configuration, not as its root fragment.
    let reloaded =
      config::parse_config_tree(std::path::Path::new(&config_path)).map(|(cfg, _)| cfg);
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
            // The process-wide facilities follow the same configuration the
            // services are about to be rebuilt from, and only once it has
            // been validated: a reload that is going to be rejected must not
            // have moved a listener on its way to being rejected.
            if let Err(e) = facilities.apply(&s, &shared, false).await {
              warn!("Config reload from {}: {}", config_path, e);
            }
            idle_timeout.store(s.idle_timeout.unwrap_or(0), Ordering::SeqCst);
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
  // Removed only on the way out of a clean run: a pid file left behind by a
  // crash is a stale pid, and an init system reading it would signal whatever
  // process now holds that number.
  remove_pid_file();
}
