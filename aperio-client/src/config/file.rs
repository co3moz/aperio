//! Reading `aperio.yaml`, following its `include:`s, and saying what it got
//! wrong.
//!
//! An unknown key is a warning rather than a refusal, because a file written
//! for a newer Aperio should still start on an older one; a malformed one is an
//! error, because guessing what it meant is worse than saying so.

use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

use aperio_config::FileConfig;

/// Loads `./aperio.yaml` (or an explicit `--config` path). A missing default
/// file is fine; an unreadable/invalid explicit file is a fatal error.
/// Folds a freshly parsed file's grouped blocks into the flat fields the
/// resolver reads, and warns about any deprecated flat key it still uses so
/// the operator can move the file over without reading a changelog.
///
/// Refuses a file that describes a service at the top level. Announced since
/// 0.6.0 and carried out in 0.9.0: a file has one shape, `services:`, so
/// there is one place to look for what a client runs. Refused rather than
/// ignored, because a file that says `target:` and is silently not serving it
/// is the worst of the three possible behaviors.
pub(crate) fn fold_and_warn(cfg: &mut FileConfig, path: &str) -> Result<(), String> {
  // Before folding: this reports what the *file* writes, and folding rewrites
  // some of those keys into others.
  let single = cfg.single_service_keys();
  if !single.is_empty() {
    // One key or several: "`hostname` describes a single service" is what the
    // operator actually reads most of the time, since a file usually carries
    // one of these.
    let (verb, subject) = if single.len() == 1 {
      ("describes", "it")
    } else {
      ("describe", "them")
    };
    return Err(format!(
      "`{}` {verb} a single service at the top level, which a config file no longer accepts. \
       Move {subject} into one `services:` entry. Single-service mode is unchanged on the \
       command line and in the environment.",
      single.join("`, `")
    ));
  }
  for key in cfg.fold_groups() {
    warn!(
      "{}: `{}` is deprecated; write it as `{}` instead (the old key still works)",
      path, key.old, key.new
    );
  }
  Ok(())
}

/// How deep an `include:` chain may go. Deep enough for the layouts anyone
/// actually writes (a root file, a per-environment file, a shared fragment),
/// shallow enough that a mistake is reported instead of being followed.
const MAX_INCLUDE_DEPTH: usize = 5;

/// Reads one config file and everything it includes, into a single mapping
/// (planned_features #41).
///
/// The merge rule is one sentence: **an included file's keys are used unless
/// the including file sets them, and sequences of mappings concatenate.** The
/// second half is what makes this worth having, since `services:`,
/// `subscribe:` and `expose:` are collections a file adds to, while
/// `allowed_ips:` and the rest are values it sets. Includes are merged in the
/// order written, so a later one wins over an earlier one, and the including
/// file wins over all of them.
///
/// Paths resolve relative to the file that wrote them, not to the working
/// directory: an included fragment has to mean the same thing whichever
/// directory the client is started from.
pub(crate) fn read_with_includes(
  path: &Path,
  depth: usize,
  // The files on the path from the root to here. A cycle is a file that
  // includes itself through this chain, so entries are popped on the way back
  // out; keeping them would call a *diamond* a cycle, and a root that includes
  // two per-environment fragments which both include a shared one is the most
  // ordinary layout there is.
  chain: &mut Vec<PathBuf>,
  // Every file that contributed, for the hot-reload watcher. This one only
  // grows: the watcher needs the whole set, and a shared fragment has to be
  // watched even though it is reached twice.
  visited: &mut Vec<PathBuf>,
) -> Result<serde_yaml::Mapping, String> {
  let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
  if chain.contains(&canonical) {
    return Err(format!(
      "{} is included in a cycle; each file may appear once in a chain",
      path.display()
    ));
  }
  if depth > MAX_INCLUDE_DEPTH {
    return Err(format!(
      "include chain is deeper than {MAX_INCLUDE_DEPTH} files at {}",
      path.display()
    ));
  }
  chain.push(canonical.clone());
  if !visited.contains(&canonical) {
    visited.push(canonical);
  }

  let raw =
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
  // Template variables are expanded before the yaml is parsed, so one file can
  // serve several environments instead of being copied per environment, which
  // is how two files drift. Expanded per file, not after merging, so an
  // included fragment reports its own name when a variable is missing.
  let raw = aperio_config::authoring::expand_vars(&raw, |name| std::env::var(name).ok())
    .map_err(|e| format!("{}: {e}", path.display()))?;
  let mut doc: serde_yaml::Mapping = match serde_yaml::from_str(&raw) {
    Ok(serde_yaml::Value::Mapping(m)) => m,
    Ok(serde_yaml::Value::Null) => serde_yaml::Mapping::new(),
    Ok(_) => return Err(format!("{} must be a mapping of settings", path.display())),
    Err(e) => return Err(format!("invalid yaml in {}: {e}", path.display())),
  };

  let includes = match doc.remove(serde_yaml::Value::String("include".into())) {
    None => Vec::new(),
    Some(serde_yaml::Value::String(one)) => vec![one],
    Some(serde_yaml::Value::Sequence(items)) => {
      let mut out = Vec::with_capacity(items.len());
      for item in items {
        match item {
          serde_yaml::Value::String(p) => out.push(p),
          _ => {
            return Err(format!(
              "{}: every `include:` entry must be a file path",
              path.display()
            ));
          }
        }
      }
      out
    }
    Some(_) => {
      return Err(format!(
        "{}: `include:` must be a file path or a list of them",
        path.display()
      ));
    }
  };

  let base = path.parent().unwrap_or_else(|| Path::new("."));
  let mut merged = serde_yaml::Mapping::new();
  for entry in includes {
    let child = base.join(&entry);
    let child_doc = read_with_includes(&child, depth + 1, chain, visited)?;
    merge_mapping(&mut merged, child_doc);
  }
  merge_mapping(&mut merged, doc);
  chain.pop();
  Ok(merged)
}

/// Merges `overlay` onto `base`: a sequence of mappings is appended, anything
/// else replaces. See [`read_with_includes`] for why the two differ.
pub(crate) fn merge_mapping(base: &mut serde_yaml::Mapping, overlay: serde_yaml::Mapping) {
  for (key, value) in overlay {
    let appended = match (base.get(&key), &value) {
      (Some(serde_yaml::Value::Sequence(existing)), serde_yaml::Value::Sequence(incoming)) => {
        let is_collection =
          |s: &Vec<serde_yaml::Value>| s.iter().any(|v| matches!(v, serde_yaml::Value::Mapping(_)));
        (is_collection(existing) || is_collection(incoming)).then(|| {
          let mut joined = existing.clone();
          joined.extend(incoming.clone());
          serde_yaml::Value::Sequence(joined)
        })
      }
      _ => None,
    };
    match appended {
      Some(joined) => {
        base.insert(key, joined);
      }
      None => {
        base.insert(key, value);
      }
    }
  }
}

/// Parses a config file and its includes into a [`FileConfig`]. The paths that
/// contributed are returned so the hot-reload watcher can watch all of them,
/// not only the root: an edit to an included fragment is a config change like
/// any other.
/// Warns about keys nothing reads, naming the key they were probably meant to
/// be.
///
/// A warning rather than an error: an unknown key has always been ignored, and
/// turning that into a refusal to start would break files that work today,
/// including ones carrying keys for a newer client than the one running. But
/// silence is the wrong answer too, a setting that is silently ignored is the
/// most expensive kind of typo, because the file says the thing is configured
/// and the behavior says it is not.
pub(crate) fn warn_unknown_keys(doc: &serde_yaml::Mapping, path: &str) {
  let (top, service) = aperio_config::authoring::known_keys();
  let report = |key: &str, known: &[String], where_: &str| {
    if known.iter().any(|k| k == key) {
      return;
    }
    match aperio_config::authoring::suggest(key, known.iter().map(String::as_str)) {
      Some(hint) => warn!("{path}: `{key}` in {where_} is not a setting; did you mean `{hint}`?"),
      None => warn!("{path}: `{key}` in {where_} is not a setting and is ignored"),
    }
  };
  for (key, value) in doc {
    let Some(key) = key.as_str() else { continue };
    // `include:` is consumed before the document is deserialized, so it is
    // never a property of the schema and must not be reported as a typo.
    if key == "include" {
      continue;
    }
    report(key, top, "the config file");
    if key == "services"
      && let serde_yaml::Value::Sequence(entries) = value
    {
      for entry in entries {
        let serde_yaml::Value::Mapping(entry) = entry else {
          continue;
        };
        for entry_key in entry.keys().filter_map(|k| k.as_str()) {
          report(entry_key, service, "a services: entry");
        }
      }
    }
  }
}

pub(crate) fn parse_config_tree(path: &Path) -> Result<(FileConfig, Vec<PathBuf>), String> {
  let mut chain = Vec::new();
  let mut seen = Vec::new();
  let merged = read_with_includes(path, 0, &mut chain, &mut seen)?;
  warn_unknown_keys(&merged, &path.display().to_string());
  let mut cfg: FileConfig = serde_yaml::from_value(serde_yaml::Value::Mapping(merged))
    .map_err(|e| format!("{}: {e}", path.display()))?;
  fold_and_warn(&mut cfg, &path.display().to_string())
    .map_err(|e| format!("{}: {e}", path.display()))?;
  Ok((cfg, seen))
}

/// The config file plus every file it includes, with the list of paths that
/// contributed so the hot-reload watcher can watch all of them.
pub(crate) fn load_file_config_tree(explicit: Option<&str>) -> (FileConfig, Vec<PathBuf>) {
  let path = explicit.unwrap_or("aperio.yaml");
  if !Path::new(path).exists() {
    if explicit.is_some() {
      error!("Failed to read config file {}: not found", path);
      std::process::exit(1);
    }
    return (FileConfig::default(), Vec::new());
  }
  match parse_config_tree(Path::new(path)) {
    Ok((cfg, files)) => {
      if files.len() > 1 {
        info!(
          "Loaded configuration from {} (+{} included)",
          path,
          files.len() - 1
        );
      } else {
        info!("Loaded configuration from {}", path);
      }
      (cfg, files)
    }
    Err(e) => {
      error!("Failed to parse {}: {}", path, e);
      std::process::exit(1);
    }
  }
}

/// Path of the user-level config (`~/.aperio.yaml`).
pub(crate) fn home_config_path() -> Option<PathBuf> {
  let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
  std::env::var_os(var).map(|home| PathBuf::from(home).join(".aperio.yaml"))
}

/// Loads `~/.aperio.yaml`, the lowest-precedence layer, shared across
/// projects (typically holding `server.url` and `server.token`). Missing is
/// fine; an unparseable file is skipped with a warning rather than being
/// fatal, since it may belong to another aperio version.
pub(crate) fn load_home_config() -> FileConfig {
  let Some(path) = home_config_path() else {
    return FileConfig::default();
  };
  match std::fs::read_to_string(&path) {
    Ok(raw) => match serde_yaml::from_str::<FileConfig>(&raw) {
      Ok(mut cfg) => {
        // The user-level file is a layer of defaults, not the deployment, so a
        // service described in it is dropped with a warning rather than
        // stopping a client whose own file is fine.
        if let Err(e) = fold_and_warn(&mut cfg, &path.to_string_lossy()) {
          warn!("Ignoring {:?}: {}", path, e);
          return FileConfig::default();
        }
        info!("Loaded user configuration from {:?}", path);
        cfg
      }
      Err(e) => {
        warn!("Ignoring unparseable {:?}: {}", path, e);
        FileConfig::default()
      }
    },
    Err(_) => FileConfig::default(),
  }
}

// --- Layered resolution -----------------------------------------------------
