//! The optional `aperio-server.yaml` configuration file.
//!
//! The server has always been environment-driven; this module adds a file
//! surface without changing any of the ~50 `std::env::var` call sites: at
//! startup — before the async runtime spawns threads — every scalar top-level
//! key of the file is materialized into its corresponding environment
//! variable following the project naming standard (`max_body_size` →
//! `APERIO_MAX_BODY_SIZE`, with `host`/`port`/`log_level` mapping to their
//! bare names). Like the client's `./aperio.yaml`, the file takes precedence
//! over environment variables; dashboard overrides still sit on top of both.
//!
//! Mapping-valued keys are *not* turned into environment variables — they are
//! reserved for structured feature sections and skipped here.

/// Keys that map to bare (un-prefixed) environment variables.
const BARE_KEYS: &[&str] = &["host", "port", "log_level"];

/// Resolves the config file path: `APERIO_SERVER_CONFIG` when set, otherwise
/// `./aperio-server.yaml`.
fn config_path() -> (std::path::PathBuf, bool) {
  match std::env::var("APERIO_SERVER_CONFIG") {
    Ok(p) if !p.trim().is_empty() => (std::path::PathBuf::from(p), true),
    _ => (std::path::PathBuf::from("aperio-server.yaml"), false),
  }
}

/// Maps a yaml key to its environment variable per the naming standard.
fn env_name(key: &str) -> String {
  let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
  if BARE_KEYS.contains(&normalized.as_str()) {
    return normalized.to_ascii_uppercase();
  }
  if normalized.starts_with("aperio_") {
    return normalized.to_ascii_uppercase();
  }
  format!("APERIO_{}", normalized.to_ascii_uppercase())
}

/// Renders a scalar yaml value in the form the env parsers expect
/// (`true`/`false` become `1`/`0`); sequences of scalars join with commas
/// (e.g. `trusted_proxies`). `None` = not representable as an env var.
fn env_value(value: &serde_yaml::Value) -> Option<String> {
  match value {
    serde_yaml::Value::Bool(b) => Some(if *b { "1" } else { "0" }.to_string()),
    serde_yaml::Value::Number(n) => Some(n.to_string()),
    serde_yaml::Value::String(s) => Some(s.clone()),
    serde_yaml::Value::Sequence(items) => {
      let parts: Option<Vec<String>> = items
        .iter()
        .map(|v| match v {
          serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_) => None,
          other => env_value(other),
        })
        .collect();
      parts.map(|p| p.join(","))
    }
    _ => None,
  }
}

/// Loads `aperio-server.yaml` (if present) and materializes its scalar keys
/// into environment variables. Must run before the tokio runtime is built:
/// `std::env::set_var` is only sound while the process is single-threaded.
/// Errors are reported on stderr (tracing is not up yet) and are fatal only
/// for an explicitly configured path.
pub(crate) fn load() {
  let (path, explicit) = config_path();
  let raw = match std::fs::read_to_string(&path) {
    Ok(raw) => raw,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound && !explicit => return,
    Err(err) => {
      eprintln!("aperio-server: cannot read {}: {err}", path.display());
      std::process::exit(1);
    }
  };
  let doc: serde_yaml::Mapping = match serde_yaml::from_str(&raw) {
    Ok(serde_yaml::Value::Mapping(map)) => map,
    Ok(serde_yaml::Value::Null) => serde_yaml::Mapping::new(),
    Ok(_) => {
      eprintln!(
        "aperio-server: {} must be a mapping of settings",
        path.display()
      );
      std::process::exit(1);
    }
    Err(err) => {
      eprintln!("aperio-server: invalid yaml in {}: {err}", path.display());
      std::process::exit(1);
    }
  };

  let mut applied: Vec<String> = Vec::new();
  for (key, value) in &doc {
    let Some(key) = key.as_str() else {
      eprintln!("aperio-server: {}: ignoring non-string key", path.display());
      continue;
    };
    // Mapping values are structured feature sections, read via `structured`.
    if value.is_mapping() {
      continue;
    }
    let Some(rendered) = env_value(value) else {
      eprintln!(
        "aperio-server: {}: ignoring key `{key}` (value not representable as an environment variable)",
        path.display()
      );
      continue;
    };
    let name = env_name(key);
    // SAFETY: called from `main` before the tokio runtime (and any thread)
    // is created, so no concurrent getenv can race this setenv.
    unsafe { std::env::set_var(&name, rendered) };
    applied.push(name);
  }
  if !applied.is_empty() {
    // tracing is initialized later (it reads LOG_LEVEL, possibly from this
    // very file), so announce on stderr like the other pre-init messages.
    eprintln!(
      "aperio-server: loaded {} ({})",
      path.display(),
      applied.join(", ")
    );
  }
}

#[cfg(test)]
#[path = "config_file_tests.rs"]
mod tests;
