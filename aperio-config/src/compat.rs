//! Upgrade safety for the configuration files.
//!
//! A config file may declare the Aperio version it was written for
//! (`version: 0.5.0`). On startup each binary compares that against its own
//! version and looks up every recorded change to the configuration format
//! that landed in between. Nothing in the range means the upgrade is
//! config-safe and nothing is said; a change in the range is reported with
//! the exact fields it touched, and a change marked `Security` refuses the
//! start outright rather than running under a configuration whose meaning
//! quietly shifted.
//!
//! The point is to make a blind upgrade survivable: `docker pull` on a
//! Friday should either behave exactly as before, or tell the operator
//! precisely what to look at, or refuse — never silently do something
//! different from what the file says.
//!
//! [`CONFIG_CHANGES`] is deliberately empty of history: it starts at the
//! version that introduced this mechanism, because entries written after the
//! fact could only be guesses. Every *future* change to the config format is
//! recorded there as part of the change itself.

use std::cmp::Ordering;
use std::fmt;

/// A `MAJOR.MINOR.PATCH` version, tolerant of a missing patch (`0.5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
  pub major: u64,
  pub minor: u64,
  pub patch: u64,
}

impl Version {
  /// Parses `1.2.3`, `1.2` or `1`, ignoring a leading `v` and any
  /// pre-release/build suffix. Anything else is an error, so a typo is
  /// reported rather than silently disabling the check.
  pub fn parse(raw: &str) -> Result<Version, String> {
    let cleaned = raw.trim().trim_start_matches(['v', 'V']);
    let core = cleaned
      .split(['-', '+'])
      .next()
      .unwrap_or_default()
      .trim_end_matches('.');
    if core.is_empty() {
      return Err(format!("'{raw}' is not a version"));
    }
    let mut parts = [0u64; 3];
    for (i, part) in core.split('.').enumerate() {
      if i >= 3 {
        return Err(format!("'{raw}' has more than three components"));
      }
      parts[i] = part
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a version: '{part}' is not a number"))?;
    }
    Ok(Version {
      major: parts[0],
      minor: parts[1],
      patch: parts[2],
    })
  }
}

impl fmt::Display for Version {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
  }
}

impl Ord for Version {
  fn cmp(&self, other: &Self) -> Ordering {
    (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
  }
}

impl PartialOrd for Version {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

/// Which file a change affects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSurface {
  Client,
  Server,
  Both,
}

impl ConfigSurface {
  fn covers(self, other: ConfigSurface) -> bool {
    self == ConfigSurface::Both || other == ConfigSurface::Both || self == other
  }
}

/// How badly a change can hurt a file written before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeSeverity {
  /// The old spelling still works and was translated automatically. Worth
  /// reviewing, nothing is broken.
  Migration,
  /// The old spelling no longer does what it says: the setting is ignored,
  /// renamed away, or means something different now.
  Breaking,
  /// The change alters what the configuration *protects*. Starting under it
  /// could expose something the file was written to keep closed, so the
  /// binary refuses until the operator has looked.
  Security,
}

impl ChangeSeverity {
  pub fn as_str(self) -> &'static str {
    match self {
      ChangeSeverity::Migration => "migration",
      ChangeSeverity::Breaking => "breaking",
      ChangeSeverity::Security => "security",
    }
  }
}

/// Who a change reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
  /// Only a file that actually writes one of `fields`. A key that was removed
  /// or given a new meaning harms exactly the people who set it; reporting it
  /// to everyone else is noise, and refusing their start is an outage for a
  /// change that cannot touch them.
  WhenSet,
  /// Every file in the range, whether or not it mentions the fields. This is
  /// the case for a changed *default*: the people affected are precisely the
  /// ones who left the key alone, so presence tells us nothing.
  Always,
}

/// The keys a configuration file actually writes, flattened so a block child
/// is addressable as `dashboard.auth` alongside the flat `dashboard_auth`.
#[derive(Debug, Clone, Default)]
pub struct ConfigKeys(std::collections::BTreeSet<String>);

impl ConfigKeys {
  /// Collects the keys of a parsed yaml mapping, one level of nesting deep,
  /// which is as far as the config format goes for scalars.
  pub fn from_mapping(doc: &serde_yaml::Mapping) -> Self {
    let mut out = std::collections::BTreeSet::new();
    for (key, value) in doc {
      let Some(key) = key.as_str() else { continue };
      out.insert(key.to_string());
      if let serde_yaml::Value::Mapping(children) = value {
        for child in children.keys() {
          if let Some(child) = child.as_str() {
            out.insert(format!("{key}.{child}"));
          }
        }
      }
    }
    ConfigKeys(out)
  }

  /// Builds the set from an iterator of already-flattened key names.
  pub fn from_names<I: IntoIterator<Item = String>>(names: I) -> Self {
    ConfigKeys(names.into_iter().collect())
  }

  pub fn contains(&self, key: &str) -> bool {
    self.0.contains(key)
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

/// One recorded change to the configuration format.
#[derive(Debug, Clone, Copy)]
pub struct ConfigChange {
  /// The version the change shipped in. A file declaring an *older* version
  /// is affected; one declaring this version or newer is not.
  pub version: &'static str,
  pub surface: ConfigSurface,
  pub severity: ChangeSeverity,
  /// Whether the change reaches every file in the range or only those that
  /// write one of `fields`.
  pub applies: Applies,
  /// The config keys involved, as an operator would write them.
  pub fields: &'static [&'static str],
  /// What changed, in one sentence.
  pub summary: &'static str,
  /// What the operator should do about it.
  pub action: &'static str,
}

/// Every recorded configuration-format change, oldest first.
///
/// Empty on purpose: the mechanism starts here, and reconstructing history
/// after the fact would mean guessing at which past releases moved which
/// keys. From now on, a change that can alter how an existing file behaves is
/// recorded here in the same commit that makes it (see CLAUDE.md).
pub const CONFIG_CHANGES: &[ConfigChange] = &[
  ConfigChange {
    version: "0.6.0",
    surface: ConfigSurface::Server,
    // Nothing is ignored and nothing is renamed: the endpoint is read the way
    // it always was, and its port now also picks the transport. On the two
    // conventional ports that changes nothing, and on 4317 it replaces a
    // configuration that silently dropped every span. Only an HTTP collector
    // deliberately placed on 4317 needs to act, which is what the action says.
    severity: ChangeSeverity::Migration,
    applies: Applies::WhenSet,
    fields: &["otel.endpoint", "otel_endpoint"],
    summary: "Traces can now be exported over OTLP/gRPC, and with `otel.protocol` unset the endpoint's port picks the transport: 4317 is gRPC, anything else HTTP.",
    action: "Nothing to do for a collector on a conventional port. Set `otel.protocol: http` if an OTLP/HTTP collector listens on 4317.",
  },
  ConfigChange {
    version: "0.6.0",
    surface: ConfigSurface::Server,
    // The file claims the dashboard is behind its own password. It is not any
    // more, and an operator who believes otherwise has published an admin
    // surface they think is gated — which is precisely what `Security` is for.
    severity: ChangeSeverity::Security,
    applies: Applies::WhenSet,
    fields: &["dashboard_auth", "dashboard.auth"],
    summary: "The separate dashboard password is gone; the dashboard is entered as aperio:<master token>, or as a named user.",
    action: "Remove the key. Anyone who signed in with it needs a dashboard user (Users page), or their own organization.",
  },
];

/// What the version check concluded.
#[derive(Debug, Clone)]
pub struct UpgradeReport {
  /// The version the file declared, if any.
  pub declared: Option<Version>,
  /// The version of the running binary.
  pub current: Version,
  /// Changes that landed after `declared`, up to and including `current`.
  pub changes: Vec<&'static ConfigChange>,
  /// True when the file declares a version newer than the binary, which
  /// usually means a rollback nobody rolled the config back for.
  pub from_the_future: bool,
}

impl UpgradeReport {
  /// Whether the binary must refuse to start.
  pub fn must_refuse(&self) -> bool {
    self
      .changes
      .iter()
      .any(|c| c.severity == ChangeSeverity::Security)
  }

  /// Whether there is anything at all to tell the operator.
  pub fn is_quiet(&self) -> bool {
    self.changes.is_empty() && !self.from_the_future
  }

  /// The affected fields, deduplicated, in the order encountered.
  pub fn affected_fields(&self) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for change in &self.changes {
      for field in change.fields {
        if !out.contains(field) {
          out.push(field);
        }
      }
    }
    out
  }
}

/// Compares a declared config version against the running binary and collects
/// the changes that landed in between.
///
/// `declared` is what the file says (`None` = it says nothing, so there is
/// nothing to compare and the report is empty). An unparseable declaration is
/// an error rather than a silent skip: a typo must not look like a clean
/// upgrade.
pub fn check_upgrade(
  declared: Option<&str>,
  current: &str,
  surface: ConfigSurface,
  changes: &'static [ConfigChange],
  keys: &ConfigKeys,
) -> Result<UpgradeReport, String> {
  let current = Version::parse(current)?;
  let Some(raw) = declared.map(str::trim).filter(|s| !s.is_empty()) else {
    return Ok(UpgradeReport {
      declared: None,
      current,
      changes: Vec::new(),
      from_the_future: false,
    });
  };
  let declared = Version::parse(raw)
    .map_err(|e| format!("{e} (the `version:` key names the Aperio version this file targets)"))?;

  let applicable = changes
    .iter()
    .filter(|c| c.surface.covers(surface))
    .filter(|c| match Version::parse(c.version) {
      // A change is relevant when it shipped after the file was written and
      // is present in the binary now running it.
      Ok(v) => v > declared && v <= current,
      // A malformed entry in our own table must not be swallowed; treating
      // it as relevant surfaces it in testing rather than in production.
      Err(_) => true,
    })
    // A `WhenSet` change cannot reach a file that never writes the key, so it
    // is not reported to one. Without this the severity ladder is unusable:
    // `Security` refuses the start, and refusing every file in the version
    // range would turn a change that harms a few into an outage for everyone.
    .filter(|c| match c.applies {
      Applies::Always => true,
      Applies::WhenSet => c.fields.iter().any(|f| keys.contains(f)),
    })
    .collect();

  Ok(UpgradeReport {
    declared: Some(declared),
    current,
    changes: applicable,
    from_the_future: declared > current,
  })
}

/// Renders the report as the lines a binary should log, most severe first.
/// Empty when there is nothing to say.
pub fn report_lines(report: &UpgradeReport) -> Vec<String> {
  let mut lines = Vec::new();
  if report.from_the_future {
    lines.push(format!(
      "This configuration declares version {}, newer than this build ({}). It may use settings this binary does not know; check that you meant to run an older Aperio.",
      report.declared.map(|v| v.to_string()).unwrap_or_default(),
      report.current
    ));
  }
  if report.changes.is_empty() {
    return lines;
  }
  let declared = report
    .declared
    .map(|v| v.to_string())
    .unwrap_or_else(|| "unknown".to_string());
  lines.push(format!(
    "The configuration format changed between the version this file declares ({}) and this build ({}); {} change(s) affect it:",
    declared,
    report.current,
    report.changes.len()
  ));
  let mut sorted: Vec<&&ConfigChange> = report.changes.iter().collect();
  sorted.sort_by_key(|c| std::cmp::Reverse(c.severity));
  for change in sorted {
    lines.push(format!(
      "  [{}] {} (since {}): {} Affected: {}. {}",
      change.severity.as_str(),
      change.surface_label(),
      change.version,
      change.summary,
      change.fields.join(", "),
      change.action
    ));
  }
  lines.push(format!(
    "Review these, then set `version: {}` in the file to acknowledge them.",
    report.current
  ));
  lines
}

impl ConfigChange {
  fn surface_label(&self) -> &'static str {
    match self.surface {
      ConfigSurface::Client => "aperio.yaml",
      ConfigSurface::Server => "aperio-server.yaml",
      ConfigSurface::Both => "both files",
    }
  }
}

#[cfg(test)]
#[path = "compat_tests.rs"]
mod tests;
