use super::*;

/// A stand-in history, so the tests exercise the mechanism rather than
/// whatever the real table happens to hold (which is empty by design).
static CHANGES: &[ConfigChange] = &[
  ConfigChange {
    version: "0.6.0",
    surface: ConfigSurface::Server,
    severity: ChangeSeverity::Migration,
    fields: &["cache_max_bytes"],
    summary: "Moved into the cache: block.",
    action: "Rewrite it as cache.max_bytes.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Client,
    severity: ChangeSeverity::Breaking,
    fields: &["allowed_ips"],
    summary: "No longer accepts a bare string.",
    action: "Write it as a list.",
  },
  ConfigChange {
    version: "0.7.0",
    surface: ConfigSurface::Both,
    severity: ChangeSeverity::Security,
    fields: &["public"],
    summary: "Now defaults to off.",
    action: "Set it explicitly if you relied on the old default.",
  },
];

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
  let r = check_upgrade(Some("0.5.0"), "0.5.9", ConfigSurface::Server, CHANGES).unwrap();
  assert!(r.is_quiet());
  assert!(!r.must_refuse());
  assert!(report_lines(&r).is_empty());
}

#[test]
fn a_change_in_the_range_is_reported_with_its_fields() {
  let r = check_upgrade(Some("0.5.0"), "0.6.0", ConfigSurface::Server, CHANGES).unwrap();
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
  let r = check_upgrade(Some("0.6.0"), "0.6.5", ConfigSurface::Server, CHANGES).unwrap();
  assert!(r.changes.is_empty());
  // A change that ships in a version *newer* than this build is not yet real
  // for it, so it stays quiet until the binary is actually upgraded.
  let r = check_upgrade(Some("0.5.0"), "0.6.9", ConfigSurface::Client, CHANGES).unwrap();
  assert!(r.changes.is_empty(), "0.7.0 has not been reached yet");
}

#[test]
fn a_security_change_refuses_the_start() {
  let r = check_upgrade(Some("0.5.0"), "0.7.0", ConfigSurface::Client, CHANGES).unwrap();
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
  let server = check_upgrade(Some("0.6.0"), "0.7.0", ConfigSurface::Server, CHANGES).unwrap();
  assert_eq!(server.changes.len(), 1);
  assert_eq!(server.changes[0].severity, ChangeSeverity::Security);

  let client = check_upgrade(Some("0.6.0"), "0.7.0", ConfigSurface::Client, CHANGES).unwrap();
  assert_eq!(client.changes.len(), 2);
}

#[test]
fn an_undeclared_version_checks_nothing_and_a_typo_is_an_error() {
  // Nothing to compare against: no report, no noise, no refusal.
  let r = check_upgrade(None, "0.7.0", ConfigSurface::Client, CHANGES).unwrap();
  assert!(r.is_quiet());
  assert!(r.declared.is_none());
  let r = check_upgrade(Some("   "), "0.7.0", ConfigSurface::Client, CHANGES).unwrap();
  assert!(r.is_quiet());

  // A misspelled version must not silently disable the safety net.
  let err = check_upgrade(Some("0.5.x"), "0.7.0", ConfigSurface::Client, CHANGES).unwrap_err();
  assert!(err.contains("version:"), "{err}");
}

#[test]
fn a_config_from_a_newer_aperio_is_called_out() {
  // The rollback case: the binary went back, the config did not.
  let r = check_upgrade(Some("0.9.0"), "0.7.0", ConfigSurface::Client, CHANGES).unwrap();
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
  // It is empty today, but every entry added later must parse and name at
  // least one field, or the report would be useless at the moment it fires.
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
  }
}
