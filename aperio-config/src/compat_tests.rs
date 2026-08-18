//! The upgrade notices a config file gets: which entries a version range selects,
//! which of them a given file actually uses, that a `Security` change refuses the
//! start, and that the shipped table itself is well formed.

use super::*;

/// A stand-in history, so the tests exercise the mechanism rather than
/// whatever the real table happens to hold (which is empty by design).
static CHANGES: &[ConfigChange] = &[
  ConfigChange {
    version: "0.6.0",
    surface: ConfigSurface::Server,
    severity: ChangeSeverity::Migration,
    applies: Applies::Always,
    fields: &["cache_max_bytes"],
    summary: "Moved into the cache: block.",
    action: "Rewrite it as cache.max_bytes.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Client,
    severity: ChangeSeverity::Breaking,
    applies: Applies::Always,
    fields: &["allowed_ips"],
    summary: "No longer accepts a bare string.",
    action: "Write it as a list.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Both,
    severity: ChangeSeverity::Security,
    applies: Applies::Always,
    fields: &["public"],
    summary: "Now defaults to off.",
    action: "Set it explicitly if you relied on the old default.",
  },
];

/// A change that can only reach a file writing the key it names.
static WHEN_SET: &[ConfigChange] = &[ConfigChange {
  version: "0.6.0",
  surface: ConfigSurface::Server,
  severity: ChangeSeverity::Security,
  applies: Applies::WhenSet,
  fields: &["dashboard_auth", "dashboard.auth"],
  summary: "The separate dashboard password is gone.",
  action: "Remove the key and use a named user.",
}];

/// Keys a file writes, for the `WhenSet` cases.
fn keys(names: &[&str]) -> ConfigKeys {
  ConfigKeys::from_names(names.iter().map(|s| s.to_string()))
}

/// No document at all, an environment-only server, say.
fn no_keys() -> ConfigKeys {
  ConfigKeys::default()
}

#[test]
fn version_parsing_accepts_the_shapes_people_write() {
  assert_eq!(
    Version::parse("1.2.3").unwrap(),
    Version {
      major: 1,
      minor: 2,
      patch: 3
    }
  );
  // A missing patch or minor reads as zero, and a `v` prefix is tolerated.
  assert_eq!(Version::parse("0.5").unwrap().to_string(), "0.5.0");
  assert_eq!(Version::parse("v2").unwrap().to_string(), "2.0.0");
  // Pre-release and build suffixes are ignored rather than rejected.
  assert_eq!(Version::parse("1.2.3-rc1").unwrap().to_string(), "1.2.3");
  assert_eq!(Version::parse("1.2.3+build9").unwrap().to_string(), "1.2.3");
  // A typo is an error: it must not look like a clean upgrade.
  assert!(Version::parse("").is_err());
  assert!(Version::parse("latest").is_err());
  assert!(Version::parse("0.5.x").is_err());
  assert!(Version::parse("1.2.3.4").is_err());
}

#[test]
fn versions_order_by_component() {
  let v = |s: &str| Version::parse(s).unwrap();
  assert!(v("0.6.0") > v("0.5.9"));
  assert!(v("0.5.10") > v("0.5.9"));
  assert!(v("1.0.0") > v("0.99.99"));
  assert_eq!(v("0.5"), v("0.5.0"));
}

#[test]
fn a_config_safe_upgrade_says_nothing() {
  // Nothing landed between 0.5.0 and 0.5.9, so the operator hears nothing.
  let r = check_upgrade(
    Some("0.5.0"),
    "0.5.9",
    ConfigSurface::Server,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert!(r.is_quiet());
  assert!(!r.must_refuse());
  assert!(report_lines(&r).is_empty());
}

#[test]
fn a_change_in_the_range_is_reported_with_its_fields() {
  let r = check_upgrade(
    Some("0.5.0"),
    "0.6.0",
    ConfigSurface::Server,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert_eq!(r.changes.len(), 1);
  assert_eq!(r.affected_fields(), vec!["cache_max_bytes"]);
  assert!(!r.must_refuse(), "a migration is a warning, not a refusal");
  let text = report_lines(&r).join("\n");
  assert!(text.contains("cache_max_bytes"), "{text}");
  assert!(text.contains("migration"), "{text}");
  // The operator is told how to acknowledge it.
  assert!(text.contains("version: 0.6.0"), "{text}");
}

#[test]
fn the_boundaries_are_exclusive_below_and_inclusive_above() {
  // A file already declaring the version a change shipped in is not affected.
  let r = check_upgrade(
    Some("0.6.0"),
    "0.6.5",
    ConfigSurface::Server,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert!(r.changes.is_empty());
  // A change that ships in a version *newer* than this build is not yet real
  // for it, so it stays quiet until the binary is actually upgraded.
  let r = check_upgrade(
    Some("0.5.0"),
    "0.6.9",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert!(r.changes.is_empty(), "0.7.0 has not been reached yet");
}

#[test]
fn a_security_change_refuses_the_start() {
  let r = check_upgrade(
    Some("0.5.0"),
    "0.7.0",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  // The client-side breaking change and the both-sides security one; the
  // server-only migration is filtered out.
  assert_eq!(r.changes.len(), 2);
  assert!(r.must_refuse());
  let text = report_lines(&r).join("\n");
  // Most severe first, so the reason for the refusal leads.
  let security = text.find("[security]").expect("security line");
  let breaking = text.find("[breaking]").expect("breaking line");
  assert!(security < breaking, "{text}");
}

#[test]
fn changes_are_filtered_by_which_file_is_being_checked() {
  // The client-only entry must not fire for the server file, but the
  // both-sides one must fire for either.
  let server = check_upgrade(
    Some("0.6.0"),
    "0.7.0",
    ConfigSurface::Server,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert_eq!(server.changes.len(), 1);
  assert_eq!(server.changes[0].severity, ChangeSeverity::Security);

  let client = check_upgrade(
    Some("0.6.0"),
    "0.7.0",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert_eq!(client.changes.len(), 2);
}

#[test]
fn an_undeclared_version_checks_nothing_and_a_typo_is_an_error() {
  // Nothing to compare against: no report, no noise, no refusal.
  let r = check_upgrade(None, "0.7.0", ConfigSurface::Client, CHANGES, &no_keys()).unwrap();
  assert!(r.is_quiet());
  assert!(r.declared.is_none());
  let r = check_upgrade(
    Some("   "),
    "0.7.0",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert!(r.is_quiet());

  // A misspelled version must not silently disable the safety net.
  let err = check_upgrade(
    Some("0.5.x"),
    "0.7.0",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap_err();
  assert!(err.contains("version:"), "{err}");
}

#[test]
fn a_config_from_a_newer_aperio_is_called_out() {
  // The rollback case: the binary went back, the config did not.
  let r = check_upgrade(
    Some("0.9.0"),
    "0.7.0",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert!(r.from_the_future);
  assert!(!r.is_quiet());
  assert!(
    !r.must_refuse(),
    "unknown-but-newer is a warning, not a refusal"
  );
  let text = report_lines(&r).join("\n");
  assert!(text.contains("newer than this build"), "{text}");
}

#[test]
fn the_shipped_table_is_well_formed() {
  // Every entry must parse and name at least one field, or the report would
  // be useless at the moment it fires.
  for change in CONFIG_CHANGES {
    Version::parse(change.version)
      .unwrap_or_else(|e| panic!("CONFIG_CHANGES entry '{}': {e}", change.summary));
    assert!(
      !change.fields.is_empty(),
      "CONFIG_CHANGES entry '{}' names no fields",
      change.summary
    );
    assert!(
      !change.action.trim().is_empty(),
      "CONFIG_CHANGES entry '{}' tells the operator nothing to do",
      change.summary
    );
    // A Security entry refuses the start, so it must be the kind of change
    // that can only reach the files it names. `Always` + `Security` would
    // stop every server in the version range, affected or not.
    assert!(
      change.severity != ChangeSeverity::Security || change.applies == Applies::WhenSet,
      "CONFIG_CHANGES entry '{}' refuses the start for every file in range; \
       a Security entry must be WhenSet, or its severity is wrong",
      change.summary
    );
  }
}

#[test]
fn a_when_set_change_reaches_only_the_files_that_use_the_key() {
  // The point of the distinction: removing a credential harms exactly the
  // operators who configured it. Reporting it to everyone else is noise, and
  // since this one is Security, refusing their start would be an outage for a
  // change that cannot touch them.
  let used = check_upgrade(
    Some("0.5.0"),
    "0.6.0",
    ConfigSurface::Server,
    WHEN_SET,
    &keys(&["server_token", "dashboard_auth"]),
  )
  .unwrap();
  assert_eq!(used.changes.len(), 1);
  assert!(used.must_refuse());

  let unaffected = check_upgrade(
    Some("0.5.0"),
    "0.6.0",
    ConfigSurface::Server,
    WHEN_SET,
    &keys(&["server_token", "max_body_size"]),
  )
  .unwrap();
  assert!(unaffected.is_quiet(), "an unaffected file hears nothing");
  assert!(!unaffected.must_refuse(), "and is certainly not refused");
}

#[test]
fn the_block_spelling_of_a_key_counts_as_using_it() {
  // `dashboard: { auth: … }` is the same setting as `dashboard_auth:`, and an
  // entry naming both must fire for either spelling.
  let r = check_upgrade(
    Some("0.5.0"),
    "0.6.0",
    ConfigSurface::Server,
    WHEN_SET,
    &keys(&["dashboard", "dashboard.auth"]),
  )
  .unwrap();
  assert_eq!(r.changes.len(), 1);
  assert!(r.must_refuse());
}

#[test]
fn a_default_that_changed_reaches_everyone_in_range() {
  // The mirror case, and the reason `Always` exists: when a *default* moves,
  // the people affected are the ones who never wrote the key, so presence
  // proves nothing and filtering on it would silence the report entirely.
  let r = check_upgrade(
    Some("0.5.0"),
    "0.7.0",
    ConfigSurface::Client,
    CHANGES,
    &no_keys(),
  )
  .unwrap();
  assert_eq!(r.changes.len(), 2);
}

#[test]
fn config_keys_flattens_one_level_of_blocks() {
  let doc: serde_yaml::Mapping =
    serde_yaml::from_str("server_token: x\ndashboard:\n  auth: y\n  enabled: true\n").unwrap();
  let k = ConfigKeys::from_mapping(&doc);
  assert!(k.contains("server_token"));
  assert!(k.contains("dashboard"));
  assert!(k.contains("dashboard.auth"));
  assert!(k.contains("dashboard.enabled"));
  assert!(!k.contains("auth"), "a child is not addressable on its own");
  assert!(ConfigKeys::default().is_empty());
}

#[test]
fn the_single_service_deprecation_reaches_a_file_that_writes_one() {
  // The real table, not a fixture: this is the entry an operator upgrading
  // past 0.6.0 with a single-service file has to be told about, and the point
  // of announcing it a release early is that they hear it while there is
  // still nothing to fix.
  let keys = ConfigKeys::from_names(["target".to_string(), "hostname".to_string()]);
  let r = check_upgrade(
    Some("0.6.0"),
    "0.7.0",
    ConfigSurface::Client,
    CONFIG_CHANGES,
    &keys,
  )
  .unwrap();
  assert!(
    r.affected_fields().contains(&"target"),
    "{:?}",
    r.affected_fields()
  );
  // Nothing may refuse the start: the file still behaves exactly as it did.
  assert!(!r.must_refuse());
}

#[test]
fn the_single_service_deprecation_is_silent_for_a_services_file() {
  // A file already written the recommended way must hear nothing at all,
  // a warning nobody can act on is what teaches people to ignore warnings.
  let keys = ConfigKeys::from_names(["services".to_string(), "max_concurrent".to_string()]);
  let r = check_upgrade(
    Some("0.6.0"),
    "0.7.0",
    ConfigSurface::Client,
    CONFIG_CHANGES,
    &keys,
  )
  .unwrap();
  assert!(
    r.changes.is_empty(),
    "reported {:?}",
    r.changes.iter().map(|c| c.fields).collect::<Vec<_>>()
  );
}

#[test]
fn the_report_names_the_keys_this_file_writes_not_the_whole_entry() {
  // An entry can cover thirty settings and reach a file through one of them.
  // Printing all thirty is how a warning stops being read, and the operator's
  // question is "which of mine".
  static WIDE: &[ConfigChange] = &[ConfigChange {
    version: "0.8.0",
    surface: ConfigSurface::Server,
    severity: ChangeSeverity::Breaking,
    applies: Applies::WhenSet,
    fields: &["cache", "tunnel_compression", "max_tunnels"],
    summary: "Something moved.",
    action: "Look at it.",
  }];
  let keys = keys(&["tunnel_compression"]);
  let report = check_upgrade(Some("0.7.0"), "0.8.0", ConfigSurface::Server, WIDE, &keys).unwrap();
  let text = report_lines(&report).join("\n");
  assert!(
    text.contains("Affected: tunnel_compression."),
    "the report should name only the key this file writes: {text}"
  );
  assert!(!text.contains("max_tunnels"), "{text}");

  // An `Always` entry has nothing to intersect (the people it affects are the
  // ones who left the key alone), so it still names what it is about.
  static ALWAYS: &[ConfigChange] = &[ConfigChange {
    version: "0.8.0",
    surface: ConfigSurface::Server,
    severity: ChangeSeverity::Migration,
    applies: Applies::Always,
    fields: &["cache_max_bytes"],
    summary: "A default changed.",
    action: "Set it if you relied on the old one.",
  }];
  let report = check_upgrade(
    Some("0.7.0"),
    "0.8.0",
    ConfigSurface::Server,
    ALWAYS,
    &no_keys(),
  )
  .unwrap();
  let text = report_lines(&report).join("\n");
  assert!(text.contains("Affected: cache_max_bytes."), "{text}");
}

/// No entry names a version this build could not plausibly be leading up to.
///
/// The table is written one change at a time, and an entry for a release that
/// has not been cut has to guess its number. That guess is the failure mode
/// worth catching, because it fails in the quietest possible way: an entry
/// naming a version that never ships simply never fires. No test goes red, no
/// operator is warned, and the upgrade note is missing for exactly the people
/// it was written for. The `depends_on` entry is one of these today, stamped
/// `0.11.0` against a crate at 0.10.0.
///
/// What can be checked is the distance. One minor ahead is the legitimate
/// mid-cycle window, since `CARGO_PKG_VERSION` is the last release until rule
/// 11's bump moves it, and a planned major is the other honest case. Anything
/// further is a typo or a guess that has gone stale while the releases moved
/// past it. What cannot be checked here is a guess that is merely *wrong*,
/// `0.11.0` written for what turns out to be a `0.10.1` release, which is what
/// rule 19's release audit is for; this narrows the window that audit has to
/// cover rather than replacing it.
#[test]
fn no_entry_names_a_version_far_ahead_of_this_build() {
  let build = Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version parses");
  for change in CONFIG_CHANGES {
    let v = Version::parse(change.version).expect("checked by the well-formed test");
    let plausible = (v.major == build.major && v.minor <= build.minor + 1)
      || (v.major == build.major + 1 && v.minor == 0);
    assert!(
      plausible,
      "CONFIG_CHANGES entry '{}' is stamped {} against a build at {}. \
       Either it is a typo, or the releases have moved past a guess written \
       before the number was known; rule 19 says correct it to the version \
       actually being cut.",
      change.summary,
      change.version,
      env!("CARGO_PKG_VERSION")
    );
  }
}
