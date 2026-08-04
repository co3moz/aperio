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

use aperio_config::format_bandwidth;
use check::run_check;
use config::{
  CliMode, ClientSettings, build_ws_url, load_home_config, parse_bandwidth, parse_cli,
  resolve_settings, resolve_sources,
};
use protocol::ConfigNote;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
use service::{ServiceSpec, Shared, run_service};
use tcp::run_tcp_bridge;

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
  let _ = rustls::crypto::ring::default_provider().install_default();

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
      // One ceiling per service: the first connection learns what the server
      // permits and the rest size themselves from it instead of each finding
      // out by being closed.
      let ceiling = service::ConnectionCeiling::new();
      // An elastic pool runs as a single supervisor task that owns its own
      // connections. That keeps the caller's contract intact, it still holds
      // one cancel channel and one handle per entry, and it puts the decision
      // to open or retire a connection next to the state it is made from.
      if spec.connections_min < spec.connections {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handle = tokio::spawn(run_elastic_pool(
          spec.clone(),
          shared.clone(),
          cancel_rx,
          health,
          ceiling,
        ));
        return vec![(cancel_tx, handle)];
      }
      (1..=spec.connections)
        .map(|conn| spawn_connection(spec, shared, &health, &ceiling, conn))
        .collect::<Vec<_>>()
    })
    .collect()
}

/// Starts connection number `conn` of a service.
fn spawn_connection(
  spec: &ServiceSpec,
  shared: &Shared,
  health: &service::BackendHealth,
  ceiling: &service::ConnectionCeiling,
  conn: u32,
) -> (watch::Sender<bool>, tokio::task::JoinHandle<()>) {
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
    conn,
    ceiling.clone(),
  ));
  (cancel_tx, handle)
}

/// The number to give a pool's next connection: the lowest one not in use.
///
/// Not `len + 1`. Entries do not only leave a pool from the end, a connection
/// past the server's announced ceiling stands down by itself, so after one of
/// those the length and the highest number in use are different things and
/// counting from the length hands out a number a live connection is already
/// answering to. Two clients with one id is exactly the ambiguity the
/// per-connection suffix exists to prevent.
fn next_connection_number(taken: impl IntoIterator<Item = u32>) -> u32 {
  let taken: Vec<u32> = taken.into_iter().collect();
  (1..).find(|n| !taken.contains(n)).unwrap_or(1)
}

/// How often the elastic pool looks at its load.
const POOL_TICK: Duration = Duration::from_secs(2);
/// Requests in flight per connection above which the pool opens another one.
///
/// A tunnel connection multiplexes requests, so this is not a hard capacity,
/// it is the point at which a connection's frames start queueing behind each
/// other rather than going out as they arrive.
const POOL_GROW_PER_CONNECTION: usize = 8;
/// Load per connection below which the pool gives one back.
///
/// Deliberately well under the growth figure: the gap is the hysteresis that
/// stops a service sitting between the two thresholds from opening and closing
/// a connection every few seconds, which costs both ends more than the
/// connection ever saved.
const POOL_SHRINK_PER_CONNECTION: usize = 2;
/// Quiet period after growing before the pool may grow again. One connection
/// at a time, with a pause to see whether it helped.
const POOL_GROW_COOLDOWN: Duration = Duration::from_secs(10);
/// Quiet period before the pool gives a connection back. Much longer than the
/// growth cooldown on purpose: being one connection too many costs a little
/// memory, being one too few costs latency on live traffic, so the pool is
/// eager to grow and reluctant to shrink.
const POOL_SHRINK_COOLDOWN: Duration = Duration::from_secs(120);

/// Runs a service whose `connections:` is a range, opening `min` connections
/// and growing towards `max` while the pool is busy.
///
/// Growth is driven by requests in flight rather than by a request *rate*: a
/// thousand requests a second that all answer in a millisecond need one
/// connection, and ten slow uploads need room to run in parallel. In flight is
/// the quantity that tells those apart.
async fn run_elastic_pool(
  spec: ServiceSpec,
  shared: Shared,
  mut cancel_rx: watch::Receiver<bool>,
  health: service::BackendHealth,
  ceiling: service::ConnectionCeiling,
) {
  // The connection number is carried alongside the handle rather than implied
  // by the position, because entries do not only leave from the end: a
  // connection past the server's ceiling stands down on its own, and deriving
  // the next number from the length would then hand out a number a live
  // connection is already using.
  let mut pool: Vec<(u32, watch::Sender<bool>, tokio::task::JoinHandle<()>)> = Vec::new();
  for conn in 1..=spec.connections_min {
    let (cancel_tx, handle) = spawn_connection(&spec, &shared, &health, &ceiling, conn);
    pool.push((conn, cancel_tx, handle));
  }
  spec.pool_load.set_open(spec.connections_min);
  info!(
    "[{}] Elastic pool: {} connection(s) open, growing to {} under load",
    spec.client_id, spec.connections_min, spec.connections
  );
  let mut grew_at = tokio::time::Instant::now();
  let mut shrank_at = tokio::time::Instant::now();
  let mut ticker = tokio::time::interval(POOL_TICK);
  ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
  loop {
    tokio::select! {
      changed = cancel_rx.changed() => {
        if changed.is_err() || *cancel_rx.borrow() {
          break;
        }
      }
      _ = ticker.tick() => {
        // A connection whose task has ended is not open, whatever the pool
        // spawned. The server announces a per-service ceiling and a
        // connection above it stands down by returning, so a pool told to
        // start more than the server allows was counting connections that had
        // never opened: the dashboard and the Ping reported them, and the
        // growth arithmetic divided by them.
        let before = pool.len();
        pool.retain(|(_, _, handle)| !handle.is_finished());
        if pool.len() != before {
          warn!(
            "[{}] {} connection(s) of this pool are not running (the server's \
             ceiling, or a connection that gave up); the pool is {} deep",
            spec.client_id,
            before - pool.len(),
            pool.len()
          );
          spec.pool_load.set_open(pool.len() as u32);
        }
        let peak = spec.pool_load.take_peak();
        let open = pool.len() as u32;
        let now = tokio::time::Instant::now();
        // The server's announced ceiling wins over the file: asking for a
        // connection it will refuse just burns a handshake.
        let permitted = ceiling.permitted().unwrap_or(spec.connections).min(spec.connections);
        if open < permitted
          && peak >= open as usize * POOL_GROW_PER_CONNECTION
          && now.duration_since(grew_at) >= POOL_GROW_COOLDOWN
        {
          let conn = next_connection_number(pool.iter().map(|(c, _, _)| *c));
          info!(
            "[{}] {} request(s) in flight over {} connection(s); opening connection {}",
            spec.client_id, peak, open, conn
          );
          let (cancel_tx, handle) = spawn_connection(&spec, &shared, &health, &ceiling, conn);
          pool.push((conn, cancel_tx, handle));
          spec.pool_load.set_open(pool.len() as u32);
          grew_at = now;
          shrank_at = now;
          continue;
        }
        if open > spec.connections_min
          && peak <= (open as usize - 1) * POOL_SHRINK_PER_CONNECTION
          && now.duration_since(shrank_at) >= POOL_SHRINK_COOLDOWN
        {
          if let Some((conn, cancel_tx, handle)) = pool.pop() {
            info!(
              "[{}] Load dropped to {} request(s) in flight over {} connection(s); \
               retiring connection {} (pool floor is {})",
              spec.client_id, peak, open, conn, spec.connections_min
            );
            let _ = cancel_tx.send(true);
            // Awaited rather than detached: the retired connection's client id
            // is `<id>-c<open>`, and the pool hands that same number out again
            // the next time it grows. Letting a draining connection overlap
            // with its replacement would put two clients with one id in front
            // of the server, which is exactly the ambiguity the per-connection
            // suffix exists to prevent.
            let _ = handle.await;
            spec.pool_load.set_open(pool.len() as u32);
          }
          shrank_at = tokio::time::Instant::now();
          grew_at = shrank_at;
        }
      }
    }
  }
  for (_, cancel_tx, _) in &pool {
    let _ = cancel_tx.send(true);
  }
  for (_, _, handle) in pool {
    let _ = handle.await;
  }
}

/// Static file mode: rewrites every `serve:` directory, the top-level one
/// (single-service mode) or per `services:` entry, into a loopback static
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
        "CRITICAL ERROR: serve and target/tcp_target are mutually exclusive, the serve directory IS the backend".to_string(),
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
        "CRITICAL ERROR: service '{}' sets serve together with target/tcp_target, the serve directory IS the backend",
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

  // Denied-visitor redirects must be absolute http(s) URLs, anything else
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
  // A sanity bound, not a policy. The policy is the server's
  // `max_connections_per_service` (lowered further by a token's own
  // `max_connections`), announced on the handshake and applied per connection;
  // this only stops `connections: 100000` from spawning a hundred thousand
  // tasks before the first one has connected to find out.
  const CONNECTIONS_SANITY_BOUND: u32 = 256;
  // Returns (floor, ceiling): equal for a fixed count, a range for an elastic
  // pool.
  let clamp_connections = |raw: Option<&aperio_config::Connections>, what: &str| -> (u32, u32) {
    let (min, max) = match raw {
      None => (1, 1),
      Some(c) => (c.min(), c.max()),
    };
    if max > CONNECTIONS_SANITY_BOUND {
      warn!(
        "{} requests {} connections; clamping to {}. The server decides the real \
         ceiling and announces it on connect.",
        what, max, CONNECTIONS_SANITY_BOUND
      );
      return (min.min(CONNECTIONS_SANITY_BOUND), CONNECTIONS_SANITY_BOUND);
    }
    (min, max)
  };
  // A clamped count is announced so the dashboard shows what the config asked
  // for next to what the client is really running.
  let connections_note =
    |raw: Option<&aperio_config::Connections>, effective: u32| -> Vec<ConfigNote> {
      let asked = raw.map(|c| c.max()).unwrap_or(1).max(1);
      (asked != effective)
        .then(|| ConfigNote {
          field: "connections".to_string(),
          declared: asked.to_string(),
          effective: effective.to_string(),
          reason: format!("clamped to {CONNECTIONS_SANITY_BOUND} parallel connections"),
        })
        .into_iter()
        .collect()
    };

  if !settings.services.is_empty() && !cli_target_given {
    // The services: list wins, and the keys that describe a single service at
    // the top level are read by nothing. Saying so is the point: they were
    // written to have an effect, they have none, and until now the client
    // dropped them without a word.
    let shadowed: Vec<&str> = [
      ("target", settings.target.is_some()),
      ("serve", settings.serve.is_some()),
      ("hostname", !settings.hostnames.is_empty()),
      ("path", settings.path.is_some()),
      ("tcp_target", settings.tcp_target.is_some()),
      ("target_health", settings.target_health.is_some()),
    ]
    .into_iter()
    .filter(|(_, set)| *set)
    .map(|(key, _)| key)
    .collect();
    if !shadowed.is_empty() {
      warn!(
        "Ignoring the top-level `{}`: a services: list is present and it is what runs. Move them into the entry they belong to.",
        shadowed.join("`, `")
      );
    }
  }

  if settings.services.is_empty() || cli_target_given {
    if cli_target_given && !settings.services.is_empty() {
      warn!(
        "A positional target was given on the command line; ignoring the {} entry/entries of the services: list",
        settings.services.len()
      );
    }
    // A client may run with nothing exposed at all. The connection then
    // serves no HTTP target and exists purely to carry something else: the
    // `tunnels:` a peer binds in an emergency, or the messages this client
    // publishes and subscribes to. Refusing to start would mean a
    // publish-only client had to invent a service it does not have.
    let carries_messages = !settings.subscribe.is_empty()
      || settings.messages_listen.is_some()
      || settings.messages_mqtt_listen.is_some();
    let target = match settings.target.clone() {
      Some(t) => t,
      None if !tunnels.is_empty() || carries_messages => String::new(),
      None => {
        return Err(
          "CRITICAL ERROR: there is nothing for this client to do (give a positional target, APERIO_TARGET, or a services:/tunnels:/subscribe: list)!".to_string(),
        );
      }
    };
    let (connections_min, connections) =
      clamp_connections(settings.connections.as_ref(), "the service");
    let mut specs = vec![ServiceSpec {
      name: None,
      custom_name: settings.custom_name.clone(),
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
      reload_drain_secs: settings.reload_drain_secs,
      retry_attempts: settings.retry_attempts,
      retry_backoff_ms: settings.retry_backoff_ms,
      retry_all_methods: settings.retry_all_methods,
      breaker_failures: settings.breaker_failures,
      breaker_open_for_secs: settings.breaker_open_for_secs,
      max_request_body: settings.max_request_body,
      response_timeout: settings.response_timeout,
      timeout_secs: settings.timeout_secs,
      max_concurrent: settings.max_concurrent,
      adaptive_concurrency: settings.adaptive_concurrency,
      connections,
      connections_min,
      metrics_labels: settings.metrics_labels.clone(),
      startup_delay: settings.startup_delay.unwrap_or(0),
      // A single service has nothing in the same file to depend on.
      depends_on: Vec::new(),
      connect_timeout: settings.connect_timeout,
      min_tls_version: settings.min_tls_version.clone(),
      pool_load: std::sync::Arc::new(service::PoolLoad::default()),
      priority: settings.priority,
      bandwidth_bps: budget_bps,
      bandwidth_declared: settings.bandwidth.clone(),
      config_notes: connections_note(settings.connections.as_ref(), connections),
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
      capture: settings.capture,
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
    validate_tls_floors(&specs)?;
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
      // A service name is an identifier the dashboard, the logs and a future
      // address form all share, so it is settled here rather than wherever it
      // is first displayed. `custom_name:` is where anything human goes.
      if let Some(name) = entry.name.as_deref() {
        aperio_config::validate_name("service", name)
          .map_err(|e| format!("CRITICAL ERROR: {e}"))?;
      }
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
      let declared_connections = entry.connections.as_ref().or(settings.connections.as_ref());
      let (connections_min, connections) =
        clamp_connections(declared_connections, &format!("service '{}'", describe()));
      Ok(ServiceSpec {
        name: entry.name.clone(),
        custom_name: entry
          .custom_name
          .clone()
          .map(|n| n.trim().to_string())
          .filter(|n| !n.is_empty()),
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
        reload_drain_secs: settings.reload_drain_secs,
        retry_attempts: entry
          .retry
          .as_ref()
          .and_then(|r| r.attempts)
          .map(|n| n.clamp(1, 10))
          .unwrap_or(settings.retry_attempts),
        retry_backoff_ms: entry
          .retry
          .as_ref()
          .and_then(|r| r.backoff)
          .unwrap_or(settings.retry_backoff_ms),
        retry_all_methods: entry
          .retry
          .as_ref()
          .and_then(|r| r.all_methods)
          .unwrap_or(settings.retry_all_methods),
        breaker_failures: entry
          .circuit_breaker
          .as_ref()
          .and_then(|b| b.failures)
          .unwrap_or(settings.breaker_failures),
        breaker_open_for_secs: entry
          .circuit_breaker
          .as_ref()
          .and_then(|b| b.open_for)
          .map(|s| s.max(1))
          .unwrap_or(settings.breaker_open_for_secs),
        max_request_body: entry.max_request_body.or(settings.max_request_body),
        response_timeout: entry.response_timeout.or(settings.response_timeout),
        timeout_secs: entry.timeout.unwrap_or(settings.timeout_secs),
        max_concurrent: entry
          .max_concurrent
          .or(settings.max_concurrent)
          .filter(|n| *n > 0),
        adaptive_concurrency: entry
          .adaptive_concurrency
          .unwrap_or(settings.adaptive_concurrency),
        connections,
        connections_min,
        metrics_labels: entry
          .metrics_labels
          .clone()
          .unwrap_or_else(|| settings.metrics_labels.clone()),
        startup_delay: entry.startup_delay.or(settings.startup_delay).unwrap_or(0),
        depends_on: entry.depends_on.clone().unwrap_or_default(),
        connect_timeout: entry.connect_timeout.or(settings.connect_timeout),
        min_tls_version: entry
          .min_tls_version
          .clone()
          .or_else(|| settings.min_tls_version.clone()),
        pool_load: std::sync::Arc::new(service::PoolLoad::default()),
        priority: entry.priority.unwrap_or(settings.priority),
        // Only what this entry asked for: the top-level value is the budget
        // these requests are settled against, not a fallback default.
        bandwidth_bps: parse_bw(entry.bandwidth.as_deref()),
        bandwidth_declared: entry.bandwidth.clone(),
        config_notes: connections_note(declared_connections, connections),
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
        capture: entry.capture.unwrap_or(settings.capture),
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
  validate_depends_on(&specs)?;
  validate_tls_floors(&specs)?;
  allocate_bandwidth(&mut specs, budget_bps);
  Ok(specs)
}

/// Rejects a `min_tls_version` this build cannot honour, here rather than
/// where it is used.
///
/// It used to be parsed inside the running service task, which had no way to
/// refuse a configuration and so called `process::exit(1)`: a hot-reload that
/// introduced one typo took the whole client down, including every other
/// service in the file, when the contract is that a bad reload is warned
/// about and the previous configuration kept. `--check-config` could not see
/// it either, because nothing on the validation path ever parsed the value.
fn validate_tls_floors(specs: &[ServiceSpec]) -> Result<(), String> {
  for spec in specs {
    if let Err(e) = crate::proxy::http::tls_floor(spec.min_tls_version.as_deref()) {
      return Err(match spec.name.as_deref() {
        Some(name) => format!("CRITICAL ERROR: service '{name}': {e}"),
        None => format!("CRITICAL ERROR: {e}"),
      });
    }
  }
  Ok(())
}

/// Rejects a `depends_on` that cannot be satisfied.
///
/// All three of these end the same way at runtime, everybody waiting out the
/// grace period and then starting anyway, so the failure is invisible unless
/// it is caught here. A name that is not in the file is almost always a typo;
/// a cycle is always a mistake, since no member of it can ever come up first.
fn validate_depends_on(specs: &[ServiceSpec]) -> Result<(), String> {
  use std::collections::{HashMap, HashSet};
  let names: HashSet<&str> = specs.iter().filter_map(|s| s.name.as_deref()).collect();
  let mut deps: HashMap<&str, &[String]> = HashMap::new();
  for spec in specs {
    let Some(name) = spec.name.as_deref() else {
      if !spec.depends_on.is_empty() {
        return Err(
          "CRITICAL ERROR: depends_on needs services with names; the entry that declares it has none"
            .to_string(),
        );
      }
      continue;
    };
    for dep in &spec.depends_on {
      if dep == name {
        return Err(format!(
          "CRITICAL ERROR: service '{name}' depends_on itself"
        ));
      }
      if !names.contains(dep.as_str()) {
        return Err(format!(
          "CRITICAL ERROR: service '{name}' depends_on '{dep}', which is not a service in this configuration"
        ));
      }
    }
    deps.insert(name, &spec.depends_on);
  }
  // Depth-first walk; `visiting` is the current chain, so re-entering it is a
  // cycle rather than merely a diamond.
  fn walk<'a>(
    node: &'a str,
    deps: &HashMap<&'a str, &'a [String]>,
    visiting: &mut Vec<&'a str>,
    done: &mut HashSet<&'a str>,
  ) -> Result<(), String> {
    if done.contains(node) {
      return Ok(());
    }
    if let Some(at) = visiting.iter().position(|n| *n == node) {
      let mut chain: Vec<&str> = visiting[at..].to_vec();
      chain.push(node);
      return Err(format!(
        "CRITICAL ERROR: depends_on forms a cycle ({}); no service in it can ever start first",
        chain.join(" -> ")
      ));
    }
    visiting.push(node);
    for dep in deps.get(node).copied().unwrap_or(&[]) {
      walk(dep, deps, visiting, done)?;
    }
    visiting.pop();
    done.insert(node);
    Ok(())
  }
  let mut done = HashSet::new();
  for name in deps.keys() {
    walk(name, &deps, &mut Vec::new(), &mut done)?;
  }
  Ok(())
}

/// Starts the OTel bridge's receivers, if configured, and returns the queue
/// the tunnel transport drains.
///
/// `https` needs no queue on this side: the forwarder owns the receiver and
/// posts to the server itself, so nothing has to reach into a service task.
/// One running local face: its address, the switch that ends it, and the
/// acceptor's handle.
///
/// The switch reaches the accepted sessions as well as the acceptor. Ending
/// only the acceptor left every connection the face had already taken serving
/// a face the configuration no longer asks for, for as long as its client
/// cared to hold it open.
struct Face {
  addr: String,
  cancel: tokio::sync::watch::Sender<bool>,
  task: tokio::task::JoinHandle<()>,
}

impl Face {
  async fn stop(self, what: &str) {
    let _ = self.cancel.send(true);
    // Bounded: a face that will not wind up must not hold a reload, let
    // alone a shutdown, and its listener is released by the drop either way.
    let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
    info!("{what} on {} stopped", self.addr);
  }
}

/// The process-wide facilities a reload has to be able to change: the two
/// local message faces and the subscription runners.
///
/// They were started once, before the supervisor loop, from the settings of
/// the first load. A reload rebuilt the services and nothing else, so a face
/// the operator removed from the file kept listening, one whose address moved
/// kept the old port, and an edited `subscribe:` block needed a restart, all
/// while `docs/configuration.md` promised that every setting applies.
///
/// A face whose address is unchanged is deliberately left alone rather than
/// rebound: rebinding drops the connections it is serving, and a reload that
/// changed something else entirely has no business doing that.
#[derive(Default)]
struct ProcessFacilities {
  http_face: Option<Face>,
  mqtt_face: Option<Face>,
  runners: Option<tokio::task::JoinHandle<()>>,
  /// What `otel_bridge:` said at startup, to notice a reload that changes the
  /// part of it that cannot be applied.
  otel_bridge: Option<aperio_config::OtelBridge>,
  /// The server and token the https transport posts with, when that transport
  /// is in use. Updated on reload: they are read per export, so this is one
  /// part of the bridge that does follow the file.
  otel_credentials: Option<OtelCredentials>,
}

impl ProcessFacilities {
  /// Brings the facilities in line with `settings`. On the first call a
  /// listener that cannot bind is fatal, as it always was; on a reload it is
  /// reported and the previous configuration for that face is kept, which is
  /// what the rest of the reload path does.
  async fn apply(
    &mut self,
    settings: &ClientSettings,
    shared: &Shared,
    first: bool,
  ) -> Result<(), String> {
    // The two faces. The address the file now asks for is bound *before* the
    // running one is stopped, so a bind that fails leaves the old face
    // serving: stopping first and then failing left the process with no face
    // at all, under a log line that claimed the previous configuration had
    // been kept.
    let bus = shared.messages.clone();
    self.http_face = swap_face(
      self.http_face.take(),
      settings.messages_listen.clone(),
      "Message face",
      first,
      |addr, cancel| {
        let bus = bus.clone();
        Box::pin(async move { crate::messages_http::serve(&addr, bus, cancel).await })
      },
    )
    .await?;

    let bus = shared.messages.clone();
    self.mqtt_face = swap_face(
      self.mqtt_face.take(),
      settings.messages_mqtt_listen.clone(),
      "MQTT face",
      first,
      |addr, cancel| {
        let bus = bus.clone();
        Box::pin(async move { crate::messages_mqtt::serve(&addr, bus, cancel).await })
      },
    )
    .await?;

    // Subscription filters. Replaced wholesale; a filter a local subscriber
    // is still holding survives, because that subscriber is still there.
    let topics: Vec<String> = settings.subscribe.iter().map(|e| e.topic.clone()).collect();
    if shared.messages.set_filters(topics).await && !first {
      shared.messages.resubscribe_all().await;
      info!("Subscriptions reloaded");
    }

    // The runners. Restarted rather than diffed: a Runner owns its
    // concurrency counter, and carrying one over from a command that changed
    // would mean the new command inherits the old one's in-flight count.
    let runners: Vec<crate::messages_run::Runner> = settings
      .subscribe
      .iter()
      .filter_map(|entry| {
        entry.run.as_deref().map(|command| {
          crate::messages_run::Runner::new(
            entry.topic.clone(),
            command.to_string(),
            entry.timeout,
            entry.max_concurrent,
            entry
              .env
              .iter()
              .map(|(k, v)| (k.clone(), v.clone()))
              .collect(),
          )
        })
      })
      .collect();
    // Subscribe the replacement before stopping the incumbent, so nothing
    // delivered in between falls between the two: a broadcast receiver only
    // sees what is sent after it exists, and `spawn` takes its receiver on
    // the calling thread for exactly this reason. The overlap is the safe
    // direction, since a message the old dispatcher is already handling is
    // not re-delivered to the new one.
    let replacement = crate::messages_run::spawn(shared.messages.clone(), runners);
    if let Some(task) = self.runners.take() {
      task.abort();
    }
    self.runners = replacement;

    // The server and token the https transport posts with are read per
    // export, so a reload that moves the server or rotates the token reaches
    // them. Without this the tunnel followed the change and the telemetry did
    // not: exports kept going to the old address, or were refused by a token
    // that had been replaced, and the earlier warning did not fire either,
    // because it only watched the `otel_bridge:` block.
    if let Some(credentials) = &self.otel_credentials
      && let (Some(server), Some(token)) = (settings.server.clone(), settings.token.clone())
    {
      credentials.send_if_modified(|current| {
        let next = (server, token);
        if *current == next {
          return false;
        }
        if !first {
          info!("OTel bridge: exports will now be posted to {}", next.0);
        }
        *current = next;
        true
      });
    }

    // The rest of the bridge is the one facility a reload cannot rebuild: the
    // receiving end of its queue is held by whichever tunnel connection is
    // live, and moving that would mean handing every service a different
    // queue mid-flight. Saying so is better than ignoring the edit.
    let unappliable = |cfg: &Option<aperio_config::OtelBridge>| {
      cfg.as_ref().map(|c| {
        (
          c.listen.clone(),
          c.listen_grpc.clone(),
          c.queue,
          c.transport.clone(),
        )
      })
    };
    if !first && unappliable(&self.otel_bridge) != unappliable(&settings.otel_bridge) {
      warn!(
        "otel_bridge: the listeners, queue or transport changed, and those cannot be rebuilt \
         while the client runs; restart to apply them"
      );
    }
    self.otel_bridge = settings.otel_bridge.clone();
    Ok(())
  }
}

/// Brings one face in line with what the configuration now asks for.
///
/// The order is the whole point: bind first, stop second. Only an address
/// that actually changed gets here, so the two never contend for the same
/// port, and a failure to bind the new one leaves the old one serving rather
/// than leaving the process with nothing.
async fn swap_face<F>(
  running: Option<Face>,
  want: Option<String>,
  what: &str,
  first: bool,
  start: F,
) -> Result<Option<Face>, String>
where
  F: FnOnce(
    String,
    tokio::sync::watch::Receiver<bool>,
  ) -> std::pin::Pin<
    Box<dyn Future<Output = Result<tokio::task::JoinHandle<()>, String>> + Send>,
  >,
{
  if running.as_ref().map(|f| f.addr.clone()) == want {
    return Ok(running);
  }
  let Some(addr) = want else {
    if let Some(face) = running {
      face
        .stop(&format!("{what} (the configuration no longer asks for it)"))
        .await;
    }
    return Ok(None);
  };
  let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
  match start(addr.clone(), cancel_rx).await {
    Ok(task) => {
      if let Some(face) = running {
        face.stop(&format!("{what} (moved to {addr})")).await;
      }
      Ok(Some(Face { addr, cancel, task }))
    }
    Err(e) if first => Err(e),
    Err(e) => {
      match &running {
        Some(face) => warn!("{e}; the {what} on {} keeps serving", face.addr),
        None => warn!("{e}; no {what} is running"),
      }
      Ok(running)
    }
  }
}

/// What the bridge's https transport needs kept current, when it is in use.
type OtelCredentials = tokio::sync::watch::Sender<(String, String)>;

async fn start_otel_bridge(
  settings: &ClientSettings,
) -> (Option<otel_bridge::Queue>, Option<OtelCredentials>) {
  let Some(cfg) = settings.otel_bridge.as_ref() else {
    return (None, None);
  };
  let http = cfg
    .listen
    .clone()
    .or_else(|| Some("127.0.0.1:4318".to_string()));
  let grpc = cfg.listen_grpc.clone();
  let (tx, rx) = otel_bridge::channel(cfg.queue.unwrap_or(256));
  tokio::spawn(otel_bridge::run(http, grpc, tx));
  tokio::spawn(otel_bridge::report_drops());

  let over_tunnel = cfg
    .transport
    .as_deref()
    .map(str::trim)
    .map(|t| !t.eq_ignore_ascii_case("https"))
    .unwrap_or(true);
  if over_tunnel {
    info!("OTel bridge: exports will travel on the tunnel");
    return (Some(std::sync::Arc::new(tokio::sync::Mutex::new(rx))), None);
  }
  match (settings.server.clone(), settings.token.clone()) {
    (Some(server), Some(token)) => {
      info!("OTel bridge: exports will be posted to the server over https");
      let (credentials, rx_credentials) = tokio::sync::watch::channel((server, token));
      tokio::spawn(otel_bridge::run_https_forwarder(rx, rx_credentials));
      (None, Some(credentials))
    }
    _ => {
      error!(
        "OTel bridge: transport https needs a server URL and a tunnel token; exports will be dropped"
      );
      (None, None)
    }
  }
}

/// Writes the process id where an init system can find it.
///
/// Best effort by design: a pid file it cannot write is worth a warning, not a
/// refusal to start. The tunnel is the job, and a supervisor that wanted the
/// file will notice its absence long before a visitor does.
fn write_pid_file(path: &str) {
  match std::fs::write(path, std::process::id().to_string()) {
    Ok(()) => {
      info!("Wrote pid {} to {}", std::process::id(), path);
      let _ = PID_FILE.set(path.to_string());
    }
    Err(e) => warn!("Could not write the pid file {path}: {e}"),
  }
}

/// The pid file this process wrote, if any.
///
/// Recorded process-wide because the shutdown path does not come back here:
/// a service that has finished draining ends the process where it stands, so
/// the removal at the end of `async_main` was only ever reached by a run that
/// ended some other way. A clean SIGTERM left a stale pid file behind, and a
/// stale pid file is a number an init system will signal, whatever process
/// now holds it.
static PID_FILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Removes the pid file, if this process wrote one. Called on every path that
/// ends the process deliberately.
pub(crate) fn remove_pid_file() {
  if let Some(path) = PID_FILE.get()
    && let Err(e) = std::fs::remove_file(path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    warn!("Could not remove the pid file {path}: {e}");
  }
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
  let mut names = std::collections::HashSet::new();
  let mut out = Vec::with_capacity(raw.len());
  for decl in raw {
    let target = decl.target.trim().to_string();
    // `udp/tcp` is normalized to the one spelling everything else compares
    // against, so a file may write it either way round.
    let protocol = match decl.protocol.trim().to_ascii_lowercase().as_str() {
      "udp/tcp" => aperio_config::PROTOCOL_BOTH.to_string(),
      other => other.to_string(),
    };
    if !matches!(
      protocol.as_str(),
      "tcp" | "udp" | aperio_config::PROTOCOL_BOTH
    ) {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' declares protocol '{}'; use tcp, udp, or tcp/udp for a service that is both",
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
        "CRITICAL ERROR: tunnel '{}' sets encrypt: true, which is only supported for tcp tunnels (a tcp/udp tunnel would leave its udp half in the clear)",
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
      // Applies to the datagram half, so a combined tunnel may set it.
      if !aperio_config::protocol_serves(&protocol, "udp") {
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
      // A public port relays TCP; a combined tunnel qualifies for its tcp half.
      if !aperio_config::protocol_serves(&protocol, "tcp") {
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
    // The name is the handle a binder and an `expose:` entry address, so it is
    // settled here and announced, rather than being re-derived by whoever
    // needs it. An explicit name is validated; a derived one cannot fail.
    if let Some(name) = decl.name.as_deref() {
      aperio_config::validate_tunnel_name(name).map_err(|e| format!("CRITICAL ERROR: {e}"))?;
    }
    let normalized = crate::protocol::TunnelDecl {
      name: decl.name.as_ref().map(|n| n.trim().to_string()),
      custom_name: decl
        .custom_name
        .as_ref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty()),
      target,
      protocol,
      encrypt: decl.encrypt,
      psk: decl.psk.clone(),
      proxy_protocol: decl.proxy_protocol,
      idle_timeout: decl.idle_timeout,
      expose: decl.expose.clone(),
    };
    let name = aperio_config::tunnel_name(&normalized);
    if !names.insert(name.clone()) {
      return Err(format!(
        "CRITICAL ERROR: two tunnels resolve to the name '{name}'; give one of them a distinct `name:`"
      ));
    }
    out.push(normalized);
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
    if spec.tunnels.is_empty() {
      info!("- Target: (none, this connection carries messages)");
    } else {
      info!("- Target: (none, tunnels only)");
    }
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
      "- Tunnel: {} ({}), bindable by a peer client with this token and client id",
      t.target, t.protocol
    );
  }
  info!("- Server URL: {}", spec.server_addr);
  info!("- WebSocket URL: {}", spec.ws_url);
  if spec.ws_urls.len() > 1 {
    info!("- Failover servers: {}", spec.ws_urls.len());
  }
}
