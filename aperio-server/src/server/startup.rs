//! What the server checks before it agrees to exist: an environment still
//! carrying a setting that moved into the database, the audit chain's own
//! verification, and binding the listener.

use crate::*;
use tracing::info;

/// Refuses to start while the environment still sets a variable whose
/// setting moved into the database, naming where it went.
///
/// A variable that is silently ignored is the worst outcome: a container
/// definition still says `APERIO_ADMIN_ALLOWED_IPS`, the server no longer
/// reads it, and the dashboard is open to everyone while the operator
/// believes otherwise. Refusing to start makes the move impossible to miss at
/// the one moment somebody is watching. Runs before the runtime exists, so it
/// prints rather than logs.
pub(crate) fn refuse_removed_settings() {
  let mut refused: Vec<String> = Vec::new();
  for (name, value) in std::env::vars() {
    if value.trim().is_empty() {
      continue;
    }
    if let Some(hint) = aperio_config::removed::server_env_hint(&name) {
      refused.push(format!("{name} is set, but it was {hint}"));
    }
  }
  if refused.is_empty() {
    return;
  }
  refused.sort();
  for line in &refused {
    eprintln!("aperio-server: {line}");
  }
  eprintln!(
    "aperio-server: refusing to start until {} environment variable(s) are removed. The environment now holds only: {}.",
    refused.len(),
    aperio_config::server_settings::BOOT_ENV
      .iter()
      .map(|(k, _)| *k)
      .collect::<Vec<_>>()
      .join(", ")
  );
  std::process::exit(1);
}

/// `aperio-server --verify-audit`: verifies the tamper-evident hash chain of
/// the audit log, the active `audit.jsonl` plus every rotated generation,
/// and returns the process exit code (0 = intact, 1 = a broken/tampered line
/// was found). Each file is checked independently; its first line is a
/// rotation boundary and is not checkable against a rotated-away predecessor.
pub(crate) fn verify_audit() -> i32 {
  let data_dir = crate::boot::data_dir();
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

/// The asynchronous server proper: reads the boot environment and the
/// settings, sets up logging, registers paths/middleware, and binds the TCP
/// listener.
pub(crate) async fn async_main() {
  // The boot environment and the settings come first: the tracing layers
  // below read the `otel` section, so the document has to be in force before
  // logging starts. Failures here are printed, since nothing logs yet.
  let boot = match crate::boot::Boot::from_env() {
    Ok(boot) => boot,
    Err(e) => {
      eprintln!("aperio-server: {e}");
      std::process::exit(1);
    }
  };
  let (settings, init) = match crate::boot::load_settings(&boot) {
    Ok(loaded) => loaded,
    Err(e) => {
      eprintln!(
        "aperio-server: refusing to start: {e}. `aperio-server settings reset` puts a setting, or all of them, back to the default."
      );
      std::process::exit(1);
    }
  };
  crate::live::set(settings.clone());

  // Structured JSON logs, plus the optional OpenTelemetry layer (`otel` in
  // the settings). The returned guard flushes buffered spans on shutdown.
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });
  let otel_guard = telemetry::init(log_filter);

  info!("Starting Aperio Server...");

  let Some(StartupBundle {
    state,
    metrics_enabled,
  }) = build_state(boot, settings, init).await
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

  // Restore public listeners before requests can mutate their desired state.
  state.exposes.lock().await.initialize(&state).await;
  spawn_background(&state);

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
