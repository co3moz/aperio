//! Static file mode: standing a local server in front of each `serve:`
//! directory, retiring the ones a reload removed, and the port each one gets.

use tracing::info;

use crate::config::ClientSettings;
use crate::*;

pub(crate) type ServeKey = (String, serve::ServeOptions);
pub(crate) type ServeListeners =
  std::collections::HashMap<ServeKey, (u16, tokio::task::JoinHandle<()>)>;

/// Static file mode: rewrites every `serve:` directory, the top-level one
/// (single-service mode) or per `services:` entry, into a loopback static
/// server target. One server runs per distinct directory, shared across
/// services and config reloads. Errors on conflicting backend settings.
pub(crate) async fn apply_serve_mode(
  settings: &mut ClientSettings,
  started: &mut ServeListeners,
) -> Result<std::collections::HashSet<ServeKey>, String> {
  // Directories the (possibly reloaded) config serves. Returned rather than
  // acted on here: listeners the new config drops may only be retired once
  // that config has been fully validated and adopted, since until then the
  // services still running are the old ones, pointing at these very ports.
  let mut needed: std::collections::HashSet<ServeKey> = std::collections::HashSet::new();
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
    needed.insert((dir.clone(), serve_opts.clone()));
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
    needed.insert((dir.clone(), serve_opts.clone()));
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
pub(crate) fn retire_unused_serve_listeners(
  needed: &std::collections::HashSet<ServeKey>,
  started: &mut ServeListeners,
) {
  started.retain(|dir, (_, handle)| {
    if needed.contains(dir) {
      true
    } else {
      handle.abort();
      info!(
        "Static file mode: stopped serving {} (no longer in config)",
        dir.0
      );
      false
    }
  });
}

/// Returns the loopback port serving `dir`, starting the static server on
/// first use. Options are part of the key so a reload can stage a new
/// listener while old connections finish using their original options.
pub(crate) async fn serve_port(
  dir: &str,
  opts: &serve::ServeOptions,
  started: &mut ServeListeners,
) -> Result<u16, String> {
  let key = (dir.to_string(), opts.clone());
  if let Some((port, _)) = started.get(&key) {
    return Ok(*port);
  }
  let (port, handle) = serve::start(dir, opts.clone()).await?;
  started.insert(key, (port, handle));
  Ok(port)
}
