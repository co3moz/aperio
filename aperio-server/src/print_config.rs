//! `aperio-server --print-config`: a read-only report of the effective
//! configuration, printed without starting the server (no port is bound, no
//! runtime is created). It answers "what is actually configured, and where did
//! each value come from?" — the everyday question behind config sprawl.
//!
//! Because [`crate::config_file::load`] has already folded every scalar
//! `aperio-server.yaml` key into its `APERIO_*` environment variable by the
//! time this runs, the *set* environment variables are the effective values.
//! Each is attributed to its origin — the file or the real environment — and
//! the persisted dashboard overrides (which win at runtime) are listed too.
//! Unset knobs keep their built-in defaults; `--print-schema` lists the full
//! catalogue with those defaults.

use std::fmt::Write;

/// Masks a secret-looking value, summarizes an over-long one, and passes
/// everything else through unchanged.
fn display_value(key: &str, value: &str) -> String {
  if crate::redact::config_key_is_secret(key) {
    return crate::redact::mask().to_string();
  }
  let len = value.chars().count();
  if len > 80 {
    format!("<{len} chars>")
  } else {
    value.to_string()
  }
}

/// Renders the effective-configuration report as text (the body of [`run`],
/// split out so it can be asserted on in tests without capturing stdout).
pub(crate) fn render() -> String {
  let mut out = String::new();
  let data_dir = std::env::var("APERIO_DATA_DIR").unwrap_or_else(|_| "./data".to_string());

  out.push_str("Effective Aperio server configuration\n");
  out.push_str("=====================================\n");
  match crate::config_file::watched_path() {
    Some(p) if p.exists() => {
      let _ = writeln!(out, "config file : {}", p.display());
    }
    Some(p) => {
      let _ = writeln!(out, "config file : {} (not present)", p.display());
    }
    None => out.push_str("config file : (none — using environment and defaults)\n"),
  }
  let _ = writeln!(out, "data dir    : {data_dir}");
  out.push('\n');

  // Set APERIO_* variables, each attributed to the file or the environment.
  let from_file = crate::config_file::materialized_env_names();
  let mut rows: Vec<(String, String, &'static str)> = std::env::vars()
    .filter(|(k, v)| k.starts_with("APERIO_") && !v.trim().is_empty())
    .map(|(k, v)| {
      let source = if from_file.contains(&k) {
        "aperio-server.yaml"
      } else {
        "env"
      };
      let shown = display_value(&k, &v);
      (k, shown, source)
    })
    .collect();
  rows.sort();

  let _ = writeln!(out, "Settings ({} set, the rest use defaults):", rows.len());
  if rows.is_empty() {
    out.push_str("  (none set — every setting uses its built-in default)\n");
  }
  let width = rows.iter().map(|(k, ..)| k.len()).max().unwrap_or(0);
  for (k, v, source) in &rows {
    let _ = writeln!(out, "  {k:<width$} = {v}  [{source}]");
  }
  out.push('\n');

  // Structured YAML sections are not environment variables.
  let sections = crate::config_file::structured_keys();
  if !sections.is_empty() {
    let _ = writeln!(
      out,
      "Structured aperio-server.yaml sections: {}",
      sections.join(", ")
    );
    out.push('\n');
  }

  // Persisted dashboard overrides win over the environment and the file.
  let settings_path = std::path::PathBuf::from(&data_dir).join("settings.json");
  match std::fs::read_to_string(&settings_path) {
    Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
      Ok(serde_json::Value::Object(map)) => {
        let set: Vec<(&String, &serde_json::Value)> =
          map.iter().filter(|(_, v)| !v.is_null()).collect();
        let _ = writeln!(
          out,
          "Dashboard overrides ({}) — these win over env/yaml at runtime:",
          settings_path.display()
        );
        if set.is_empty() {
          out.push_str("  (none)\n");
        }
        let width = set.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, v) in set {
          let raw = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
          };
          let _ = writeln!(out, "  {k:<width$} = {}", display_value(k, &raw));
        }
      }
      _ => {
        let _ = writeln!(
          out,
          "Dashboard overrides: {} is present but not a JSON object (ignored)",
          settings_path.display()
        );
      }
    },
    Err(_) => {
      let _ = writeln!(
        out,
        "Dashboard overrides: none ({} not present)",
        settings_path.display()
      );
    }
  }

  out
}

/// Prints the effective-configuration report and returns the process exit
/// code (always 0 — printing the configuration cannot fail).
pub(crate) fn run() -> i32 {
  print!("{}", render());
  0
}

#[cfg(test)]
#[path = "print_config_tests.rs"]
mod tests;
