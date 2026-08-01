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
//! precisely what to look at, or refuse, never silently do something
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
    version: "0.8.0",
    surface: ConfigSurface::Server,
    // Breaking rather than Migration: nothing was translated for anyone, and
    // the effective value of a setting can change at upgrade. Found by the
    // release audit (CLAUDE.md rule 19) rather than in the commit that made
    // the change, which is exactly the case that rule exists for.
    //
    // WhenSet, because only a file that writes one of these keys can be
    // affected: a file that mentions none of them keeps behaving identically,
    // whatever is stored in the dashboard.
    severity: ChangeSeverity::Breaking,
    applies: Applies::WhenSet,
    fields: &[
      "gateway_timeout",
      "gateway_response_timeout",
      "max_body_size",
      "max_tunnels",
      "max_connections_per_service",
      "inspector",
      "access_events",
      "require_hostname_bind",
      "lb_strategy",
      "failover",
      "failover_max_jumps",
      "failover_window",
      "failover_all_methods",
      "client_down_threshold",
      "ip_limit_max",
      "ip_limit_refill",
      "tunnel_compression",
      "server_auth",
      "cache",
      "cache_max_bytes",
      "cache_max_stale",
      "stream_pause_bytes",
      "stream_resume_bytes",
      "stream_backlog_limit",
      "max_concurrent_requests",
      "login_lockout_threshold",
      "login_lockout_secs",
      "audit_max_size",
      "audit_max_files",
      "ui_language",
      "preview_noindex",
      "random_subdomain",
    ],
    summary: "aperio-server.yaml now outranks a setting saved from the dashboard. Until 0.8.0 the order was environment < file < dashboard, so a value changed once from the dashboard won over the file forever, and the file said one thing while the server did another. A key your file sets that also carries a stored override therefore takes effect on this upgrade, possibly changing what the server does; the override is dropped at startup, with a warning naming each key and an audit event.",
    action: "Compare the settings pane against your file before upgrading, or read the startup warning after: it names every override that was dropped. To keep a dashboard-set value, write it into the file. Settings your file does not mention are unaffected and stay editable from the dashboard.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Client,
    // Announced early, on purpose. Nothing is ignored yet and nothing is
    // renamed: a file written this way still starts and still behaves
    // identically, which is why this is a Migration and not Breaking. The
    // point of saying it is that an operator who only ever reads this report
    // should hear about the removal while there is still nothing to fix.
    //
    // `version` stays at the release the *announcement* shipped in, so anyone
    // upgrading across it still sees the notice. The removal itself was first
    // aimed at 0.7.0, did not land there, and is now aimed at 0.9.0; the
    // release audit for 0.8.0 caught the text still naming a version that had
    // already shipped.
    severity: ChangeSeverity::Migration,
    applies: Applies::WhenSet,
    fields: &[
      "target",
      "serve",
      "hostname",
      "path",
      "tcp_target",
      "target_health",
      "health.endpoint",
    ],
    summary: "Describing a single service at the top level of `aperio.yaml` is deprecated; from 0.9.0 a config file will only accept `services:`. Single-service mode is unaffected on the command line and in the environment.",
    action: "Move the top-level `target:`/`serve:`/`hostname:`/`path:`/`tcp_target:`/`target_health:` (or `health.endpoint`) into one `services:` entry. The file behaves the same either way until 0.9.0.",
  },
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
    // surface they think is gated, which is precisely what `Security` is for.
    severity: ChangeSeverity::Security,
    applies: Applies::WhenSet,
    fields: &["dashboard_auth", "dashboard.auth"],
    summary: "The separate dashboard password is gone; the dashboard is entered as aperio:<master token>, or as a named user.",
    action: "Remove the key. Anyone who signed in with it needs a dashboard user (Users page), or their own organization.",
  },
  ConfigChange {
    version: "0.6.0",
    surface: ConfigSurface::Client,
    // The key still parses and still shapes the tunnel, but the number now
    // means something else: it used to be announced per connection and per
    // service, so the effective ceiling was the value times both counts. A
    // file tuned against the old arithmetic now allows a fraction of what its
    // author measured, which is a break even though nothing errors.
    severity: ChangeSeverity::Breaking,
    applies: Applies::WhenSet,
    fields: &["bandwidth"],
    summary: "`bandwidth` is now a total budget divided across a service's connections, and at the top level across the services: entries, instead of being announced in full by each of them.",
    action: "Recheck the number against what you intend in total. A file that relied on the old multiplication needs it raised by roughly connections x services. Applies to a `bandwidth:` inside a services: entry too.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Client,
    // The keys still parse and still bind. What changed is what a *key* means:
    // it is read as a tunnel name first and only then as a peer's client id.
    // Tunnel names shaped like a UUID are refused precisely so the two spaces
    // cannot collide, which is what keeps every existing file resolving the
    // way it did. Worth a look because the local ports may move: a privileged
    // or unparseable declared port now falls back to a derived one instead of
    // failing to bind at all.
    severity: ChangeSeverity::Migration,
    applies: Applies::WhenSet,
    fields: &["bind-tunnels", "bind_tunnels"],
    summary: "`bind-tunnels:` keys are read as tunnel names first, falling back to a peer's client id; a bare port is the short form, and a tunnel whose declared port is privileged now binds a derived local port instead of failing.",
    action: "Nothing to do for an entry keyed by a client id. Check the startup log for the local ports chosen, and prefer naming tunnels (`name:` on the declaration) over addressing peers by id.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Client,
    // A name is an identifier now: a-z, 0-9 and `_`. A file that named a
    // service or a tunnel anything else no longer starts, which is the whole
    // point, the alternative is two people writing what they think is the
    // same name (`Postgres`, `pg-main`, `pg_main`) and reaching different
    // things. Everything human moves to `custom_name:`, which is free text.
    severity: ChangeSeverity::Breaking,
    applies: Applies::WhenSet,
    fields: &["name"],
    summary: "`name:` on a service or a tunnel must be an identifier: a-z, 0-9 and `_`. Capitals, `-`, `.` and non-English letters are refused; `custom_name:` carries the label.",
    action: "Rename what the file declares (`pg-main` becomes `pg_main`) and update the `bind-tunnels:` keys and `expose:` rules that address it. Put the old spelling in `custom_name:` if it was there to be read. An unnamed tunnel's derived handle changed spelling too (`127-0-0-1-5432-tcp` is now `127_0_0_1_5432_tcp`).",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Client,
    // The key still parses and still produces a header. What changed is that
    // a value browsers do not recognize is no longer sent as written: it
    // becomes the strict default. Someone whose `ALLOW-FROM ...` was ignored
    // by every browser had framing allowed in practice, and now has it
    // denied, the same file, a different site.
    severity: ChangeSeverity::Migration,
    applies: Applies::WhenSet,
    fields: &[
      "security_headers",
      "security_headers.frame_options",
      "security_headers.referrer_policy",
    ],
    summary: "`frame_options:` is sent as SAMEORIGIN or DENY and `referrer_policy:` as one of the eight values browsers act on; anything else becomes the strict default instead of going out verbatim.",
    action: "Only affects a file whose value is not one a browser recognizes (a typo, or `ALLOW-FROM`, which no browser supports). Set one of the accepted values, or drop the key.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Server,
    // The server used to start and bind whatever the OS handed out, which
    // nothing could then reach; it refuses to start now. Breaking rather than
    // Migration because the file no longer runs at all.
    severity: ChangeSeverity::Breaking,
    applies: Applies::WhenSet,
    fields: &["expose"],
    summary: "An `expose:` entry with `port: 0` is refused at startup instead of binding a port the OS picks, which nothing could reach.",
    action: "Give the entry the port you meant. Only a file that literally writes `port: 0` is affected.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Server,
    // The rule still parses and still matches the client it always matched:
    // `token:` alone is read exactly as before. What changed is that the
    // right way to own a port is now the organization, because a token name
    // is not unique and a rule naming one can match a client of another
    // organization, with the winner decided by hash map order. Worth a look
    // rather than a break, since nothing an operator wrote stopped working.
    severity: ChangeSeverity::Migration,
    applies: Applies::WhenSet,
    fields: &["expose", "expose.token"],
    summary: "An `expose:` port is claimed by organization (`org:`, or `tunnel: <org>@<name>`); `token:` still works but cannot be unambiguous, since a token name may exist in several organizations.",
    action: "Replace `token: <name>` with `org: <name>` (or fold it into `tunnel: <org>@<name>`) on each entry. Only entries that rely on a token name shared across organizations behave differently today.",
  },
  ConfigChange {
    version: "0.6.0",
    surface: ConfigSurface::Client,
    // Nothing the operator wrote stopped working; something that never worked
    // started. That is not `Breaking`, but it is worth a look, because a list
    // left in the home file from an old experiment now actually exposes
    // whatever it names.
    severity: ChangeSeverity::Migration,
    applies: Applies::WhenSet,
    fields: &["services", "tunnels", "bind-tunnels"],
    summary: "`services:`, `tunnels:` and `bind-tunnels:` written in ~/.aperio.yaml are honored now; they were read from the project file only and silently ignored in the home one.",
    action: "Only affects a home config that declares them. Check ~/.aperio.yaml still describes what you want exposed, since a project file that does not mention a section now inherits it.",
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
  /// The keys this file writes, so the report can name the ones an entry
  /// actually touches instead of every key the entry lists. An entry may
  /// cover thirty settings and reach a file through one of them; printing all
  /// thirty is how a warning stops being read.
  pub keys: Vec<String>,
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
      keys: Vec::new(),
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
    keys: keys.0.iter().cloned().collect(),
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
    // The keys this file writes, when the entry names any: an operator reads
    // "Affected" to find out whether their file is one of the affected ones,
    // and a list of thirty keys they mostly do not use answers a different
    // question. An `Always` entry (a changed default) has nothing to
    // intersect, so it keeps naming the keys it is about.
    let touched: Vec<&str> = change
      .fields
      .iter()
      .copied()
      .filter(|f| report.keys.iter().any(|k| k == f))
      .collect();
    let affected = if touched.is_empty() {
      change.fields.join(", ")
    } else {
      touched.join(", ")
    };
    lines.push(format!(
      "  [{}] {} (since {}): {} Affected: {}. {}",
      change.severity.as_str(),
      change.surface_label(),
      change.version,
      change.summary,
      affected,
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
