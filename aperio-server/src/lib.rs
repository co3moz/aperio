mod access_log;
mod alert_rules;
mod alerts;
mod api;
mod auth;
mod backup;
mod backup_crypto;
mod cache;
mod capacity;
mod check_config;
mod config_file;
mod consumers;
mod deny_list;
mod error_pages;
mod expose;
mod fallbacks;
mod forward_auth;
mod headers;
mod jwt;
mod limits;
mod maintenance_windows;
mod metrics_labels;
mod oidc;
mod otlp_identity;
mod outbound;
mod print_config;
mod protocol;
mod protocol_profile;
mod proxy;
mod redact;
mod relay_log;
mod retention;
mod route_limits;
mod routing;
mod scaling;
mod settings;
mod share;
mod state;
mod static_routes;
mod store;
mod supervise;
mod telemetry;
mod totp;
mod tunnel;
mod visitor_auth;
mod waf;
mod webauthn;

// What the process does, in the order it does it. `run` and `async_main` stay
// in this file because they are the sequence; each stage is its own module.
//
// `state_build` is one function and stays whole: it is a single pass over
// roughly two hundred settings that ends in one struct literal, and staging it
// would mean half-built structs handed between the stages.
mod server {
  pub(crate) mod background;
  pub(crate) mod router;
  pub(crate) mod shutdown;
  pub(crate) mod startup;
  pub(crate) mod state_build;
}

pub(crate) use server::background::*;
pub(crate) use server::router::*;
pub(crate) use server::shutdown::*;
pub(crate) use server::startup::*;
pub(crate) use server::state_build::*;

use crate::state::AppState;

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

/// Entry point for the Aperio server, called by the thin binary in
/// `main.rs`. Handles the diagnostic subcommands, loads `aperio-server.yaml`
/// into the environment while still single-threaded, then hands over to the
/// async server on a multi-thread runtime.
pub fn run() {
  // Route every panic through structured logging before the runtime contains
  // it (see `install_panic_logger`), so a panic in a spawned task or a
  // background thread is visible in the server's own log stream instead of
  // only reaching stderr with no task context.
  install_panic_logger();

  // Pin the process-wide rustls provider to ring. The dependency tree pulls
  // rustls with both `ring` and `aws-lc-rs` enabled (workspace feature
  // unification), and with two providers rustls refuses to auto-select one,
  // the first outbound TLS call (webhooks, OIDC, OTLP) would panic without
  // this.
  ensure_crypto_provider();

  // `aperio-server --version` must print and exit instead of starting the
  // server (used by installers and packaging).
  if matches!(
    std::env::args().nth(1).as_deref(),
    Some("--version" | "-V" | "version")
  ) {
    println!("aperio-server {}", env!("CARGO_PKG_VERSION"));
    return;
  }

  // `aperio-server --print-schema` prints the JSON Schema for
  // `aperio-server.yaml` (the file is the primary configuration surface; env
  // vars are the fallback) and exits. Point an editor's `yaml.schemas` at the
  // output for autocompletion and validation. Needs no config load.
  if std::env::args().nth(1).as_deref() == Some("--print-schema") {
    println!("{}", aperio_config::server_schema_json());
    return;
  }

  // Must happen before the runtime exists: the loader writes environment
  // variables, which is only sound while no other thread can read them.
  config_file::load();

  // `aperio-server --decrypt-backup <in.db.enc> [out.db]` turns an encrypted
  // snapshot back into a database, using the same key configuration the
  // server writes them with. This is not a convenience: an encrypted backup
  // nobody can open is worse than no backup, and the moment somebody needs it
  // is the moment they are least able to write a decryption script.
  //
  // **Before the upgrade check, unlike every other subcommand**, and
  // deliberately: that check exits on a security-relevant config change, and
  // this is the one command that must still run during an emergency. Turning
  // ciphertext back into bytes does not depend on what a config key means, so
  // there is nothing for the verdict to protect here and a restore it blocked
  // would be the worst possible time to find out. It runs after the file is
  // loaded, so a key written in the `backup:` block still works.
  if std::env::args().nth(1).as_deref() == Some("--decrypt-backup") {
    std::process::exit(decrypt_backup());
  }

  // Upgrade safety: compare the version the file declares against this build
  // and report every recorded config-format change in between. Runs before
  // the diagnostic subcommands so they inherit the same verdict, and before
  // anything binds a port, since a security-relevant change must stop the
  // start rather than be noticed afterwards.
  report_config_upgrade();
  refuse_removed_settings();

  // `aperio-server --check-config` lints the layered configuration (file +
  // environment) and exits without starting the server.
  if std::env::args().nth(1).as_deref() == Some("--check-config") {
    std::process::exit(check_config::run());
  }

  // `aperio-server --print-config` prints the effective configuration, which
  // `APERIO_*` values are set and whether each came from the environment, the
  // `aperio-server.yaml` file, or a persisted dashboard override, and exits
  // without starting the server.
  if std::env::args().nth(1).as_deref() == Some("--print-config") {
    std::process::exit(print_config::run());
  }

  // `aperio-server --verify-audit` verifies the tamper-evident hash chain of
  // the audit log and exits without starting the server.
  if std::env::args().nth(1).as_deref() == Some("--verify-audit") {
    std::process::exit(verify_audit());
  }

  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("failed to build the tokio runtime")
    .block_on(async_main());
}

/// Decrypts one snapshot, reading the key the same way the backup task does.
///
/// The key is taken from `APERIO_BACKUP_KEY` / `APERIO_BACKUP_KEY_FILE`, or
/// from the `backup:` block of the configured server file, so restoring uses
/// the configuration that produced the file rather than a second place to get
/// it wrong. The "inside the backup directory" refusal applies here too: if
/// the arrangement was never safe, saying so while restoring is the last
/// chance to hear it.
fn decrypt_backup() -> i32 {
  let mut args = std::env::args().skip(2);
  let Some(input) = args.next() else {
    eprintln!(
      "usage: aperio-server --decrypt-backup <snapshot{}> [output.db]",
      backup_crypto::ENCRYPTED_SUFFIX
    );
    return 2;
  };
  let input = std::path::PathBuf::from(input);
  let output = args
    .next()
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|| {
      // `aperio-1700000000.db.enc` -> `aperio-1700000000.db`
      let name = input.file_name().map(|n| n.to_string_lossy().into_owned());
      match name.and_then(|n| {
        n.strip_suffix(backup_crypto::ENCRYPTED_SUFFIX)
          .map(str::to_string)
      }) {
        Some(stem) => input.with_file_name(format!("{stem}.db")),
        None => input.with_extension("db"),
      }
    });

  // The backup directory only matters for the refusal, and defaults to the
  // snapshot's own directory, which is the right answer when someone is
  // restoring from a copy rather than from where it was written.
  let dir = std::env::var("APERIO_BACKUP_DIR")
    .ok()
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|| {
      input
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
    });

  let key = match backup_crypto::load_key(
    std::env::var("APERIO_BACKUP_KEY").ok().as_deref(),
    std::env::var("APERIO_BACKUP_KEY_FILE").ok().as_deref(),
    &dir,
  ) {
    Ok(Some(key)) => key,
    Ok(None) => {
      eprintln!(
        "No backup key configured. Set APERIO_BACKUP_KEY or APERIO_BACKUP_KEY_FILE \
         to the key this snapshot was written with."
      );
      return 2;
    }
    Err(e) => {
      eprintln!("{e}");
      return 2;
    }
  };

  match backup_crypto::decrypt_file(&key, &input, &output) {
    Ok(size) => {
      println!("{} ({} bytes)", output.display(), size);
      0
    }
    Err(e) => {
      eprintln!("{e}");
      1
    }
  }
}

/// Installs a process-wide panic hook that logs every panic through `tracing`
/// (message, source location, thread, and a backtrace when `RUST_BACKTRACE` is
/// set) before the runtime contains it.
///
/// This changes observability, not control flow. Under the default `unwind`
/// strategy a panic still only unwinds its own task/connection (or is turned
/// into a 500 by the catch-panic layer), the process keeps running. But such a
/// contained panic is otherwise easy to miss: its `JoinHandle` is never
/// awaited, so the only trace is an unstructured stderr line with no task
/// context. Routing it through `tracing` puts it in the server's normal log
/// pipeline (including JSON logs and any OTLP export) so it can be alerted on.
fn install_panic_logger() {
  std::panic::set_hook(Box::new(|info| {
    let message = info
      .payload()
      .downcast_ref::<&str>()
      .map(|s| (*s).to_string())
      .or_else(|| info.payload().downcast_ref::<String>().cloned())
      .unwrap_or_else(|| "<non-string panic payload>".to_string());
    let location = info
      .location()
      .map(|l| l.to_string())
      .unwrap_or_else(|| "<unknown>".to_string());
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>").to_string();
    let backtrace = std::backtrace::Backtrace::capture();
    tracing::error!(
      target: "aperio_server::panic",
      panic_message = %message,
      panic_location = %location,
      panic_thread = %thread,
      panic_backtrace = %backtrace,
      "panic caught, the task/connection is unwound; the process continues"
    );
  }));
}

/// Snapshot of every service entity's availability, keyed by service name or
/// stable client id: `up` when at least one connection is heartbeat-healthy,
/// routable, and its backend probe passes; `degraded` when connected but not
/// serving (backend unhealthy, draining, or disabled); absent entities are
/// treated as `down` by the uptime store.
pub(crate) async fn observe_service_availability(
  state: &AppState,
) -> std::collections::HashMap<String, (crate::store::uptime::Availability, Option<String>)> {
  use crate::store::uptime::Availability;
  let down_threshold = state.config().client_down_threshold;
  let clients = state.clients.read().await;
  let mut out: std::collections::HashMap<String, (Availability, Option<String>)> =
    std::collections::HashMap::new();
  for (conn_id, handle) in clients.iter() {
    // One record per service, not per connection. Uptime is asked about a
    // service by name, and a connection carrying several reported only the
    // first of them: the rest had no history at all, which reads as a service
    // that was never up rather than one nothing was watching.
    for service in &handle.services {
      let key = service
        .service_name
        .clone()
        .or_else(|| handle.reported_instance_id.clone())
        .unwrap_or_else(|| conn_id.clone());
      // The connection's own health gates every service on it, because a
      // socket that is not answering is not serving any of them; past that
      // each service is up or degraded on its own backend.
      let status = if !handle.is_healthy(down_threshold) {
        Availability::Down
      } else if service.backend_healthy && service.admin_enabled && !handle.draining {
        Availability::Up
      } else {
        Availability::Degraded
      };
      // Several connections may serve one entity; the best state wins. All
      // connections of one entity share its organization.
      let entry = out
        .entry(key)
        .or_insert((Availability::Down, handle.perms.org_id.clone()));
      let rank = |s: &Availability| match s {
        Availability::Up => 2,
        Availability::Degraded => 1,
        Availability::Down => 0,
      };
      if rank(&status) > rank(&entry.0) {
        entry.0 = status;
      }
      entry.1 = handle.perms.org_id.clone();
    }
  }
  out
}

/// In-process composition facade for the integration tests in `tests/`.
///
/// Hidden rather than private: an integration test is its own crate, so the
/// only way to hand it the composed server is a `pub` item, and the only
/// honest way to say "this is not API" is `#[doc(hidden)]` plus this notice.
/// Nothing here is stable, nothing here is for embedding Aperio.
#[doc(hidden)]
pub mod testkit {
  use std::sync::Arc;

  /// The composed server: the state behind it stays opaque.
  pub struct Composed {
    state: Arc<crate::state::AppState>,
    pub router: axum::Router,
  }

  /// Runs the real startup path (environment, stores, settings layering,
  /// router assembly) inside the calling process. `None` = the same refusals
  /// `build_state` logs. Spawns no background loops: the test decides what
  /// runs beside it.
  pub async fn compose() -> Option<Composed> {
    let bundle = crate::build_state().await?;
    let router = crate::build_router(bundle.state.clone(), bundle.metrics_enabled);
    Some(Composed {
      state: bundle.state,
      router,
    })
  }

  impl Composed {
    /// Runs the real serve loop, graceful shutdown included: it returns only
    /// once the shutdown signal has run. The caller owns the process-global
    /// consequences (the signal handlers, and shutdown_signal's ten-second
    /// force-exit fallback), which is why only a single-test integration
    /// binary should call this.
    pub async fn serve_until_shutdown(self) {
      crate::serve_until_shutdown(self.state, self.router).await;
    }

    /// Inserts a minimal connected-client record and returns the receiving
    /// end of its tunnel channel, so a test can observe what the server
    /// writes to clients (the shutdown notice, for one).
    pub async fn insert_probe_client(
      &self,
    ) -> tokio::sync::mpsc::Receiver<axum::extract::ws::Message> {
      let (tx, rx) = tokio::sync::mpsc::channel(16);
      let handle = crate::state::ClientHandle {
        tx,
        disconnect: Arc::new(tokio::sync::Notify::new()),
        connected_at: std::time::Instant::now(),
        client_ip: "127.0.0.1".to_string(),
        declared_client_id: None,
        drain_secs: None,
        last_ping_at: Some(std::time::Instant::now()),
        perms: crate::state::ClientPerms::master(),
        draining: false,
        client_version: None,
        client_protocol: None,
        cpu_percent: None,
        rss_bytes: None,
        rtt_ms: None,
        jitter_ms: None,
        reconnects: None,
        reported_instance_id: None,
        instance_group: None,
        subscriptions: Vec::new(),
        services: vec![crate::state::ServiceState {
          server_side_target: None,
          server_side_refused: None,
          service_custom_name: None,
          request_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
          declared_path: None,
          assigned_path: None,
          declared_hostname: Some("probe.example.com".to_string()),
          declared_hostnames: vec!["probe.example.com".to_string()],
          assigned_hostnames: Vec::new(),
          random_hostname: None,
          override_path_bind: None,
          override_hostname_binds: Vec::new(),
          capture: true,
          connections: None,
          connections_min: None,
          connections_max: None,
          config_notes: Vec::new(),
          metrics_labels: Vec::new(),
          max_concurrent: None,
          max_concurrent_ceiling: None,
          inflight_limiter: None,
          admin_enabled: true,
          tcp_enabled: false,
          backend_healthy: true,
          backend_probed: true,
          priority: 0,
          bandwidth_bps: Arc::new(std::sync::atomic::AtomicU64::new(0)),
          service_name: None,
          public: false,
          public_denied_warned: false,
          visitor_auth: None,
          visitor_auth_policy: None,
          visitor_auth_denied_warned: false,
          ungated_warned: false,
          allowed_ips: Vec::new(),
          allowed_ips_invalid_warned: false,
          scaling_invalid_warned: false,
          tunnels: Vec::new(),
          cache: false,
          cache_ignored_warned: false,
          resilience: false,
          max_request_body: None,
          response_timeout: None,
          webhook_inbox: false,
          denied: None,
          recent_failures: std::collections::VecDeque::new(),
          ejected_until: None,
        }],
      };
      self
        .state
        .clients
        .write()
        .await
        .insert("probe-client".to_string(), handle);
      rx
    }

    /// Serves the composed app on an ephemeral loopback port and returns the
    /// address; the serve task runs until the returned handle is aborted.
    pub async fn serve_ephemeral(&self) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();
      let app = self.router.clone();
      let handle = tokio::spawn(async move {
        axum::serve(
          listener,
          app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
      });
      (addr, handle)
    }

    /// How many live tunnel clients the state currently tracks, so a test
    /// can assert on the state the HTTP surface is serving from.
    pub async fn connected_clients(&self) -> usize {
      self.state.clients.read().await.len()
    }
  }
}

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
