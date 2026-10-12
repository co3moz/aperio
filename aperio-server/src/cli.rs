//! The `aperio-server` command line.
//!
//! With no command it starts the server. The others work on the data
//! directory without starting anything: they read and repair the settings
//! that live in the database, apply or check a document, and carry the
//! diagnostics an operator reaches for when the server will not come up.

use clap::{Parser, Subcommand};

use aperio_config::server_settings::ServerSettings;

/// Aperio tunnel server.
#[derive(Parser)]
#[command(name = "aperio-server", version, about, long_about = None)]
pub(crate) struct Cli {
  #[command(subcommand)]
  pub(crate) command: Option<Command>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
  /// Start the server (what running it with no command does).
  Run,
  /// Show or reset the settings stored in the data directory.
  Settings {
    #[command(subcommand)]
    action: SettingsAction,
  },
  /// Store the settings a document (yaml or json) carries, for the next
  /// start. A running server is changed through the dashboard or the API.
  Apply {
    /// The document: a `settings:` section, as an export writes it.
    file: String,
  },
  /// Check a document (yaml or json) without applying it.
  Check {
    /// The document to check.
    file: String,
  },
  /// Print the JSON Schema of the settings document.
  Schema,
  /// Verify the audit log's hash chain.
  Audit {
    #[command(subcommand)]
    action: AuditAction,
  },
  /// Work with backup snapshots.
  Backup {
    #[command(subcommand)]
    action: BackupAction,
  },
}

#[derive(Subcommand)]
pub(crate) enum SettingsAction {
  /// Print the stored settings, only what differs from the defaults unless
  /// --full.
  Show {
    /// Every setting, defaults included.
    #[arg(long)]
    full: bool,
    /// JSON instead of yaml.
    #[arg(long)]
    json: bool,
  },
  /// Put one setting or section back to its default, or every setting when
  /// no path is given. The way back in when a change locked the dashboard
  /// out; restart the server afterwards.
  Reset {
    /// A dotted path, `proxy.trusted` or `security`.
    path: Option<String>,
  },
}

#[derive(Subcommand)]
pub(crate) enum AuditAction {
  /// Verify the tamper-evident hash chain of every audit log generation.
  Verify,
}

#[derive(Subcommand)]
pub(crate) enum BackupAction {
  /// Decrypt a snapshot with APERIO_BACKUP_KEY / APERIO_BACKUP_KEY_FILE.
  Decrypt {
    /// The encrypted snapshot.
    input: String,
    /// Where to write the database. Default: beside the input.
    output: Option<String>,
  },
}

/// Prints the stored settings.
pub(crate) fn settings_show(full: bool, json: bool) -> i32 {
  let store = crate::store::settings::SettingsStore::load(&crate::boot::data_dir());
  let settings = match store.get() {
    Ok(s) => s.unwrap_or_default(),
    Err(e) => {
      eprintln!("{e}");
      return 1;
    }
  };
  let value = if full {
    settings.to_value()
  } else {
    settings.minimal()
  };
  let document = serde_json::json!({ "settings": value });
  let out = if json {
    serde_json::to_string_pretty(&document).unwrap_or_default()
  } else {
    serde_yaml::to_string(&document).unwrap_or_default()
  };
  println!("{}", out.trim_end());
  0
}

/// Resets a path, or everything, to the defaults. Works on a stored document
/// this build cannot read too, which is when it is needed most.
pub(crate) fn settings_reset(path: Option<&str>) -> i32 {
  let mut store = crate::store::settings::SettingsStore::load(&crate::boot::data_dir());
  let current = match store.get() {
    Ok(s) => s.unwrap_or_default(),
    Err(e) if path.is_none() => {
      eprintln!("{e}; resetting every setting");
      ServerSettings::default()
    }
    Err(e) => {
      eprintln!("{e}. Reset every setting with `aperio-server settings reset` (no path).");
      return 1;
    }
  };
  let next = match path {
    None => ServerSettings::default(),
    Some(path) => match reset_path(&current, path) {
      Ok(s) => s,
      Err(e) => {
        eprintln!("{e}");
        return 1;
      }
    },
  };
  if !store.put(&next) {
    eprintln!("the settings could not be saved to the database");
    return 1;
  }
  match path {
    Some(p) => println!("{p} is back to its default. Restart the server to put it in force."),
    None => {
      println!("Every setting is back to its default. Restart the server to put them in force.")
    }
  }
  0
}

/// `settings` with the value at `path` replaced by the default's.
pub(crate) fn reset_path(settings: &ServerSettings, path: &str) -> Result<ServerSettings, String> {
  let mut value = settings.to_value();
  let defaults = ServerSettings::default().to_value();
  let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
  if parts.is_empty() {
    return Err("an empty path".to_string());
  }
  let mut target = &mut value;
  let mut default = Some(&defaults);
  for part in &parts[..parts.len() - 1] {
    target = target
      .get_mut(*part)
      .filter(|v| v.is_object())
      .ok_or_else(|| format!("`{path}` is not a setting"))?;
    default = default.and_then(|d| d.get(*part));
  }
  let last = parts[parts.len() - 1];
  let map = target
    .as_object_mut()
    .ok_or_else(|| format!("`{path}` is not a setting"))?;
  let known = map.contains_key(last) || default.is_some_and(|d| d.get(last).is_some());
  if !known && !is_optional_field(path) {
    return Err(format!("`{path}` is not a setting"));
  }
  match default.and_then(|d| d.get(last)) {
    Some(d) => {
      map.insert(last.to_string(), d.clone());
    }
    None => {
      map.remove(last);
    }
  }
  serde_json::from_value(value).map_err(|e| format!("`{path}`: {e}"))
}

/// Whether `path` names an optional field (absent from the default tree,
/// since an unset optional is not written).
fn is_optional_field(path: &str) -> bool {
  let schema: serde_json::Value =
    serde_json::from_str(&aperio_config::server_schema_json()).unwrap_or_default();
  let mut node = &schema;
  for part in path.split('.') {
    let props = node.get("properties").or_else(|| {
      node
        .get("$ref")
        .and_then(|r| r.as_str())
        .and_then(|r| r.rsplit('/').next())
        .and_then(|name| schema.get("$defs").and_then(|d| d.get(name)))
        .and_then(|d| d.get("properties"))
    });
    match props.and_then(|p| p.get(part)) {
      Some(next) => node = next,
      None => return false,
    }
  }
  true
}

/// Reads a document and checks its settings the way a change is checked.
fn checked_document(file: &str) -> Result<crate::api::export::ImportDump, String> {
  let document = crate::boot::read_document(file)?;
  if let Some(settings) = &document.settings {
    let mut probe = settings.clone();
    // A check must not leave a generated metrics token behind.
    if probe.metrics.token.is_none() {
      probe.metrics.token = Some("check".to_string());
    }
    let scratch = std::env::temp_dir().to_string_lossy().into_owned();
    crate::server::state_build::config::build_config(&probe, "check", &scratch)
      .map_err(|e| format!("{file}: {e}"))?;
  }
  Ok(document)
}

/// `aperio-server check <file>`.
pub(crate) fn check(file: &str) -> i32 {
  match checked_document(file) {
    Ok(document) => {
      let sections = sections_present(&document);
      println!(
        "{file} is valid ({})",
        if sections.is_empty() {
          "empty".to_string()
        } else {
          sections.join(", ")
        }
      );
      0
    }
    Err(e) => {
      eprintln!("{e}");
      1
    }
  }
}

/// `aperio-server apply <file>`: stores the document's settings.
pub(crate) fn apply(file: &str) -> i32 {
  let document = match checked_document(file) {
    Ok(d) => d,
    Err(e) => {
      eprintln!("{e}");
      return 1;
    }
  };
  let others: Vec<String> = sections_present(&document)
    .into_iter()
    .filter(|s| s != "settings")
    .collect();
  if !others.is_empty() {
    eprintln!(
      "{file} also carries {}: apply those with POST /aperio/api/import, or name the file in APERIO_INIT_FILE for a first start. Nothing was applied.",
      others.join(", ")
    );
    return 1;
  }
  let Some(settings) = document.settings else {
    eprintln!("{file} has no settings section; nothing to apply");
    return 1;
  };
  let mut store = crate::store::settings::SettingsStore::load(&crate::boot::data_dir());
  if !store.put(&settings) {
    eprintln!("the settings could not be saved to the database");
    return 1;
  }
  println!("Applied the settings from {file}. Restart the server to put them in force.");
  0
}

fn sections_present(document: &crate::api::export::ImportDump) -> Vec<String> {
  let mut out = Vec::new();
  let mut has = |present: bool, name: &str| {
    if present {
      out.push(name.to_string());
    }
  };
  has(document.settings.is_some(), "settings");
  has(document.tokens.is_some(), "tokens");
  has(document.webhooks.is_some(), "webhooks");
  has(document.users.is_some(), "users");
  has(document.organizations.is_some(), "organizations");
  has(document.scaling.is_some(), "scaling");
  has(document.exposes.is_some(), "exposes");
  has(document.statistics.is_some(), "statistics");
  has(document.uptime.is_some(), "uptime");
  has(document.activity.is_some(), "activity");
  has(document.inbox.is_some(), "inbox");
  has(document.admin_keys.is_some(), "admin_keys");
  out
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
