//! What the server checks before it agrees to exist: a configuration still
//! setting a removed key, the upgrade notes a file's declared `version:` earns,
//! the audit chain's own verification, and binding the listener.

use crate::*;
use tracing::info;

/// Settings that no longer exist, and what to do instead.
///
/// A removed key that is merely ignored is the worst outcome: the file still
/// says the dashboard has its own password, the server no longer agrees, and
/// nobody finds out until someone tries to sign in. Refusing to start turns a
/// silent authentication change into an obvious one, at the only moment the
/// operator is watching.
///
/// [`CONFIG_CHANGES`] covers the same ground for a file that declares a
/// `version:`, and does it with a fuller explanation. This check is what
/// catches the two cases that has no answer for: a file with no `version:`,
/// and an environment-only deployment with no file at all.
const REMOVED_SETTINGS: &[(&str, &str)] = &[(
  "APERIO_DASHBOARD_AUTH",
  "the separate dashboard password was removed. Sign in as `aperio:<APERIO_SERVER_TOKEN>`, \
   or create a dashboard user (Users page) or an organization for anyone who used it. \
   Remove `dashboard_auth:` / `dashboard.auth:` from the configuration to start.",
)];

/// Refuses to start when the configuration still sets a removed key.
///
/// Runs before the runtime exists, so it prints rather than logs, like the
/// upgrade check beside it. Every spelling is covered by checking the
/// environment variable: the file loader materializes both the flat key and
/// the block child into it.
pub(crate) fn refuse_removed_settings() {
  let mut refused = false;
  for (var, guidance) in REMOVED_SETTINGS {
    let set = std::env::var(var)
      .map(|v| !v.trim().is_empty())
      .unwrap_or(false);
    if set {
      eprintln!("aperio-server: {var} is set, but {guidance}");
      refused = true;
    }
  }
  if refused {
    std::process::exit(1);
  }
}

/// The keys `aperio-server.yaml` actually writes, so a change that only
/// reaches files using a particular key is not reported to files that do not.
/// An environment-only deployment has no document and therefore no keys, which
/// is correct: such a change cannot be about a key it never wrote.
pub(crate) fn declared_config_keys() -> aperio_config::compat::ConfigKeys {
  match crate::config_file::document() {
    Some(doc) => aperio_config::compat::ConfigKeys::from_mapping(&doc),
    None => aperio_config::compat::ConfigKeys::default(),
  }
}

/// The Aperio version the configuration declares (`version:` in
/// `aperio-server.yaml`, or `APERIO_VERSION`), if any.
pub(crate) fn declared_config_version() -> Option<String> {
  std::env::var("APERIO_VERSION")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
}

/// Compares the declared config version against this build and reports what
/// changed in between, refusing the start when a change has security
/// consequences.
///
/// Runs before the runtime exists, so it prints rather than logs: tracing is
/// not initialized yet, and an operator watching a container start must see
/// the reason it refused.
pub(crate) fn report_config_upgrade() {
  use aperio_config::compat::{CONFIG_CHANGES, ConfigSurface, check_upgrade, report_lines};

  let declared = declared_config_version();

  let report = match check_upgrade(
    declared.as_deref(),
    env!("CARGO_PKG_VERSION"),
    ConfigSurface::Server,
    CONFIG_CHANGES,
    &declared_config_keys(),
  ) {
    Ok(report) => report,
    Err(e) => {
      eprintln!("aperio-server: {e}");
      std::process::exit(1);
    }
  };
  if report.declared.is_none() {
    eprintln!(
      "aperio-server: no `version:` in the configuration, so upgrade checks are off. Add `version: {}` to be warned when a future upgrade changes how this file is read.",
      report.current
    );
    return;
  }
  for line in report_lines(&report) {
    eprintln!("aperio-server: {line}");
  }
  if report.must_refuse() {
    eprintln!(
      "aperio-server: refusing to start under a configuration whose security-relevant settings changed meaning. Review the above, then set `version: {}` to acknowledge them.",
      report.current
    );
    std::process::exit(1);
  }
}

/// `aperio-server --verify-audit`: verifies the tamper-evident hash chain of
/// the audit log, the active `audit.jsonl` plus every rotated generation,
/// and returns the process exit code (0 = intact, 1 = a broken/tampered line
/// was found). Each file is checked independently; its first line is a
/// rotation boundary and is not checkable against a rotated-away predecessor.
pub(crate) fn verify_audit() -> i32 {
  let data_dir = std::env::var("APERIO_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
  let base = std::path::PathBuf::from(&data_dir).join("audit.jsonl");

  // Collect rotated generations (.1 newest .. .N oldest), then order oldest →
  // active so the output reads chronologically.
  let mut generations: Vec<std::path::PathBuf> = Vec::new();
  let mut n = 1usize;
  loop {
    let gen_path = std::path::PathBuf::from(format!("{}.{}", base.display(), n));
    if gen_path.exists() {
      generations.push(gen_path);
      n += 1;
    } else {
      break;
    }
  }
  generations.reverse();
  generations.push(base);

  println!("Verifying audit log hash chain ({data_dir})");
  let mut broken = 0usize;
  let mut checked = 0usize;
  for f in &generations {
    if !f.exists() {
      continue;
    }
    checked += 1;
    match crate::store::audit::verify_chain(f) {
      Ok(None) => println!("  ok    {}", f.display()),
      Ok(Some(line)) => {
        broken += 1;
        println!(
          "  FAIL  {}, hash chain breaks at line {}",
          f.display(),
          line
        );
      }
      Err(e) => {
        broken += 1;
        println!("  FAIL  {}, cannot read: {}", f.display(), e);
      }
    }
  }

  println!();
  if checked == 0 {
    println!("No audit log found in {data_dir} (nothing to verify)");
    return 0;
  }
  if broken > 0 {
    println!("Audit verification FAILED: {broken} file(s) with a broken chain");
    1
  } else {
    println!("Audit verification OK ({checked} file(s) intact)");
    0
  }
}

/// Binds the main TCP listener. With `reuseport`, the socket is created with
/// `SO_REUSEADDR` + `SO_REUSEPORT` so multiple server processes can share the
/// same port for zero-downtime restarts; otherwise a plain listener is used.
pub(crate) async fn bind_listener(
  host: &str,
  port: u16,
  reuseport: bool,
) -> std::io::Result<tokio::net::TcpListener> {
  if !reuseport {
    return tokio::net::TcpListener::bind(format!("{host}:{port}")).await;
  }
  use socket2::{Domain, Protocol, Socket, Type};
  use std::net::ToSocketAddrs;
  let addr = format!("{host}:{port}")
    .to_socket_addrs()?
    .next()
    .ok_or_else(|| {
      std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("could not resolve {host}:{port}"),
      )
    })?;
  let domain = if addr.is_ipv6() {
    Domain::IPV6
  } else {
    Domain::IPV4
  };
  let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
  socket.set_reuse_address(true)?;
  #[cfg(unix)]
  socket.set_reuse_port(true)?;
  socket.set_nonblocking(true)?;
  socket.bind(&addr.into())?;
  socket.listen(1024)?;
  tokio::net::TcpListener::from_std(socket.into())
}

/// The asynchronous server proper: sets up logging, reads env config,
/// registers paths/middleware, and binds the TCP listener.
pub(crate) async fn async_main() {
  // Initialize tracing with structured JSON output (pino.js style), plus the
  // optional OpenTelemetry OTLP export layer (APERIO_OTEL). The returned guard
  // flushes buffered spans on graceful shutdown.
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });
  let otel_guard = telemetry::init(log_filter);

  info!("Starting Aperio Server...");

  let Some(StartupBundle {
    state,
    metrics_enabled,
  }) = build_state().await
  else {
    // The refusal has been logged by build_state with its reason.
    return;
  };

  // Once, at startup, before anything is served: what the file asks for
  // against what this machine can give. It never changes a setting, only says
  // so, because a number that silently changes because the host changed is
  // exactly what the configuration work was spent on preventing.
  {
    let cfg = state.config();
    crate::capacity::warn_if_beyond_the_machine(
      cfg.max_ws_connections,
      cfg.max_tunnels,
      cfg.cache_max_bytes,
    );
  }

  let app = build_router(state.clone(), metrics_enabled);

  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
  spawn_background(&state, &host);

  serve_until_shutdown(state.clone(), app).await;

  // Final stats flush so nothing recorded since the last tick is lost.
  state.persistent_stats.lock().await.save_if_dirty();
  state.uptime.lock().await.save_if_dirty();
  state.activity.lock().await.save_if_dirty();

  // Flush any buffered OTLP spans before exit.
  otel_guard.shutdown();
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
