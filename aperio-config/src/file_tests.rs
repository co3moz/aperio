//! The document's own keys: which single-service keys a file writes (the ones
//! removed in 0.9.0, still reported so an upgrade can name them), that the
//! schema marks them deprecated, and how `connections:` reads in both of its
//! spellings.

use super::*;

// ---------------------------------------------------------------------------
// Single-service keys in a config file (deprecated; removed in 0.9.0).
// ---------------------------------------------------------------------------

#[test]
fn a_file_reports_the_single_service_keys_it_writes() {
  let cfg: FileConfig = serde_yaml::from_str(
    r#"
server:
  url: wss://tunnel.example.com
  token: apr_x
target: http://localhost:3000
hostname: app.example.com
path: /api
"#,
  )
  .unwrap();
  // In file order, so the warning reads the way the file does.
  assert_eq!(
    cfg.single_service_keys(),
    vec!["target", "hostname", "path"]
  );
}

#[test]
fn a_services_file_reports_nothing() {
  // The keys that stay legitimately top-level in the multi-service shape are
  // per-entry *fallbacks*, so none of them may trip the deprecation warning.
  let cfg: FileConfig = serde_yaml::from_str(
    r#"
server:
  url: wss://tunnel.example.com
  token: apr_x
max_concurrent: 8
trim_bind: true
pass_hostname: true
serve_spa: true
services:
  - target: http://localhost:3000
    hostname: app.example.com
"#,
  )
  .unwrap();
  assert!(cfg.single_service_keys().is_empty());
}

#[test]
fn an_empty_single_service_key_is_not_written() {
  // `target: ""` is how a value gets cleared in a templated file; reporting
  // it would tell someone to migrate a key they already removed.
  let cfg: FileConfig = serde_yaml::from_str("target: \"  \"\nserve: \"\"\n").unwrap();
  assert!(cfg.single_service_keys().is_empty());
}

#[test]
fn the_schema_marks_the_single_service_keys_deprecated() {
  // The dashboard's config builder hides a deprecated key unless an imported
  // file already writes it, and editors grey it out. Both read this flag, so
  // the form stops offering the shape we want retired.
  let schema = serde_json::to_value(schemars::schema_for!(FileConfig)).unwrap();
  let props = schema["properties"].as_object().unwrap();
  for key in SINGLE_SERVICE_KEYS {
    assert_eq!(
      props[*key].get("deprecated"),
      Some(&serde_json::Value::Bool(true)),
      "`{key}` must be marked deprecated in the emitted schema"
    );
  }
  // The block spelling of the same claim, and only its `endpoint`: the other
  // children stay top-level defaults, so flagging them would be wrong.
  let defs = schema["$defs"].as_object().unwrap();
  let top = defs["TopHealthConfig"]["properties"].as_object().unwrap();
  assert_eq!(
    top["endpoint"].get("deprecated"),
    Some(&serde_json::Value::Bool(true))
  );
  assert_eq!(top["interval"].get("deprecated"), None);
  // And never on a services: entry, which is where it is now supposed to go.
  let entry = defs["HealthConfig"]["properties"].as_object().unwrap();
  assert_eq!(entry["endpoint"].get("deprecated"), None);
  // A key that is still the right way to write something must not be.
  assert_eq!(props["services"].get("deprecated"), None);
  assert_eq!(props["trim_bind"].get("deprecated"), None);
}

#[test]
fn a_top_level_health_endpoint_counts_as_a_single_service_key() {
  // Both spellings, and only the endpoint: the rest of the block is a real
  // per-entry default and reporting it would be advice to delete a working key.
  let block: FileConfig =
    serde_yaml::from_str("health:\n  endpoint: /health\n  interval: 30\n").unwrap();
  assert_eq!(block.single_service_keys(), vec!["target_health"]);

  let flat: FileConfig = serde_yaml::from_str("target_health: /health\n").unwrap();
  assert_eq!(flat.single_service_keys(), vec!["target_health"]);

  let defaults_only: FileConfig =
    serde_yaml::from_str("health:\n  interval: 30\n  wait_for_backend: true\n").unwrap();
  assert!(defaults_only.single_service_keys().is_empty());
}

#[test]
fn the_top_level_health_block_still_parses_every_field() {
  // The top level has its own type now so `endpoint` can be marked withdrawn
  // there and not on a services: entry. Same fields, so a file written either
  // way must load identically, a schema-only split must not become a parse
  // change.
  let cfg: FileConfig = serde_yaml::from_str(
    "health:\n  endpoint: /h\n  interval: 7\n  timeout: 3\n  threshold: 4\n  wait_for_backend: true\n",
  )
  .unwrap();
  let health = cfg.health.clone().unwrap();
  assert_eq!(health.endpoint.as_deref(), Some("/h"));
  assert_eq!(health.interval, Some(7));
  assert_eq!(health.timeout, Some(3));
  assert_eq!(health.threshold, Some(4));
  assert_eq!(health.wait_for_backend, Some(true));

  let mut folded = cfg;
  folded.fold_groups();
  assert_eq!(folded.target_health.as_deref(), Some("/h"));
  assert_eq!(folded.health_interval, Some(7));
}

// ---------------------------------------------------------------------------
// connections: fixed or elastic (planned_features #48)
// ---------------------------------------------------------------------------

#[test]
fn connections_accepts_a_scalar_and_a_range() {
  let fixed: Connections = serde_yaml::from_str("4").unwrap();
  // The scalar spelling is unchanged: four connections, opened and kept, with
  // no elasticity anybody has to think about.
  assert_eq!((fixed.min(), fixed.max()), (4, 4));
  assert!(!fixed.is_elastic());

  let range: Connections = serde_yaml::from_str("{min: 2, max: 8}").unwrap();
  assert_eq!((range.min(), range.max()), (2, 8));
  assert!(range.is_elastic());
}

#[test]
fn connections_defaults_each_half_of_a_range() {
  // `min` alone: a floor with no headroom, which is a fixed pool.
  let floor: Connections = serde_yaml::from_str("{min: 3}").unwrap();
  assert_eq!((floor.min(), floor.max()), (3, 3));

  // `max` alone: grows from one, which is the "start small" case.
  let ceiling: Connections = serde_yaml::from_str("{max: 6}").unwrap();
  assert_eq!((ceiling.min(), ceiling.max()), (1, 6));
  assert!(ceiling.is_elastic());
}

#[test]
fn connections_reads_an_inverted_range_as_the_floor() {
  // A range written the wrong way round is a typo. Honoring `max` literally
  // would open fewer connections than the file's own `min` promises, so the
  // floor wins and the pool is simply fixed at it.
  let inverted: Connections = serde_yaml::from_str("{min: 6, max: 2}").unwrap();
  assert_eq!((inverted.min(), inverted.max()), (6, 6));
  assert!(!inverted.is_elastic());
}

#[test]
fn connections_never_reads_as_zero() {
  // Zero connections is a service that cannot serve anything, and it is far
  // likelier to be a mistake than a way of turning a service off.
  let zero: Connections = serde_yaml::from_str("0").unwrap();
  assert_eq!((zero.min(), zero.max()), (1, 1));
  let zero_range: Connections = serde_yaml::from_str("{min: 0, max: 0}").unwrap();
  assert_eq!((zero_range.min(), zero_range.max()), (1, 1));
}

/// Parses a `auth:` value the way a config file would carry it.
fn auth_of(yaml: &str) -> AuthSetting {
  serde_yaml::from_str(yaml).expect("a valid auth: value")
}

#[test]
fn the_three_spellings_of_a_visitor_gate_mean_the_same_thing() {
  // The scalar predates the grammar and has to keep meaning exactly what it
  // meant, or every file written before this change quietly changes behaviour.
  let scalar = auth_of("\"admin:s3cret\"");
  let block = auth_of("{method: basic, users: \"admin:s3cret\"}");
  let list = auth_of("[{method: basic, users: [\"admin:s3cret\"]}]");
  for policy in [&scalar, &block, &list] {
    assert_eq!(policy.as_single_credential(), Some("admin:s3cret"));
    let methods = policy.methods();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].method, "basic");
    assert!(validate_auth_setting(policy).is_ok());
  }
}

#[test]
fn a_policy_the_scalar_cannot_carry_says_so_rather_than_losing_half_of_itself() {
  // `as_single_credential` is what travels to a server that predates the
  // grammar. Anything it cannot express must answer None, or the far side
  // would be handed a gate weaker than the one written.
  assert_eq!(
    auth_of("{method: none}").as_single_credential(),
    None,
    "an open gate is not a credential"
  );
  assert_eq!(
    auth_of("{method: basic, users: [\"a:b\", \"c:d\"]}").as_single_credential(),
    None,
    "two credentials are not one"
  );
  assert_eq!(
    auth_of("[{method: basic, users: \"a:b\"}, {method: basic, users: \"c:d\"}]")
      .as_single_credential(),
    None,
    "two methods are not one"
  );
}

#[test]
fn a_gate_nobody_could_open_is_refused_where_it_is_written() {
  // Each of these parses. The point of validation is that none of them
  // reaches a visitor as "the password does not work".
  let cases = [
    ("[]", "empty list"),
    ("{method: ldap}", "not a method"),
    ("{method: basic}", "basic without users"),
    ("{method: basic, users: []}", "basic with an empty list"),
    ("{method: basic, users: \"nocolon\"}", "no separator"),
    ("{method: basic, users: \"user:\"}", "empty password"),
    ("{method: basic, users: \":pw\"}", "empty user"),
    (
      "{method: none, users: \"a:b\"}",
      "an open gate with credentials",
    ),
    (
      "[{method: none}, {method: basic, users: \"a:b\"}]",
      "none beside another method",
    ),
  ];
  for (yaml, why) in cases {
    let err =
      validate_auth_setting(&auth_of(yaml)).expect_err(&format!("{why} should be refused: {yaml}"));
    assert!(!err.is_empty(), "the refusal has to say something");
  }

  // The message names the methods that do exist, so the fix is in the error.
  let err = validate_auth_setting(&auth_of("{method: ldap}")).unwrap_err();
  for method in AUTH_METHODS {
    assert!(
      err.contains(method),
      "the refusal should list `{method}`: {err}"
    );
  }
}

#[test]
fn case_and_whitespace_around_a_method_name_do_not_change_it() {
  for spelling in ["Basic", " basic ", "BASIC"] {
    let policy = auth_of(&format!("{{method: \"{spelling}\", users: \"a:b\"}}"));
    assert!(validate_auth_setting(&policy).is_ok(), "{spelling}");
    assert_eq!(policy.as_single_credential(), Some("a:b"), "{spelling}");
  }
}

#[test]
fn a_bearer_gate_is_refused_when_it_could_not_hold_the_whole_of_itself() {
  let good = auth_of("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  assert!(validate_auth_setting(&good).is_ok());

  let cases = [
    ("{method: bearer}", "no secret at all"),
    ("{method: bearer, secret: []}", "an empty list"),
    ("{method: bearer, secret: \"   \"}", "a blank secret"),
    (
      "{method: bearer, secret: \"short\"}",
      "below the length floor",
    ),
    (
      "{method: bearer, users: \"a:b\"}",
      "credentials, which bearer has no half for",
    ),
    (
      "{method: basic, users: \"a:b\", secret: \"0123456789abcdef\"}",
      "a secret on basic",
    ),
    (
      "{method: none, secret: \"0123456789abcdef\"}",
      "a secret on the open gate",
    ),
  ];
  for (yaml, why) in cases {
    validate_auth_setting(&auth_of(yaml)).expect_err(&format!("{why} should be refused: {yaml}"));
  }

  // The length floor says the number, so the fix does not need the source.
  let err = validate_auth_setting(&auth_of("{method: bearer, secret: \"short\"}")).unwrap_err();
  assert!(err.contains(&MIN_BEARER_SECRET_LEN.to_string()), "{err}");
}

#[test]
fn a_bearer_gate_is_not_expressible_as_the_one_scalar_the_old_surfaces_carry() {
  // Whatever else changes, this is what keeps a gate from travelling as
  // something weaker than it is.
  let p = auth_of("{method: bearer, secret: \"0123456789abcdef-secret\"}");
  assert_eq!(p.as_single_credential(), None);
}

#[test]
fn a_jwt_gate_needs_exactly_one_way_of_knowing_who_signed_a_token() {
  assert!(
    validate_auth_setting(&auth_of(
      "{method: jwt, jwks_url: \"https://accounts.example.com/jwks\", issuer: \"https://accounts.example.com\"}"
    ))
    .is_ok()
  );
  assert!(
    validate_auth_setting(&auth_of(
      "{method: jwt, hmac_secret: \"0123456789abcdef-secret\"}"
    ))
    .is_ok()
  );

  let cases = [
    ("{method: jwt}", "neither key source"),
    (
      "{method: jwt, jwks_url: \"https://x/jwks\", hmac_secret: \"0123456789abcdef\"}",
      "both key sources",
    ),
    (
      "{method: jwt, jwks_url: \"not-a-url\"}",
      "a jwks_url that is not one",
    ),
    (
      "{method: jwt, hmac_secret: \"short\"}",
      "a secret below the floor",
    ),
    (
      "{method: jwt, jwks_url: \"https://x/jwks\", users: \"a:b\"}",
      "users on jwt",
    ),
    (
      "{method: jwt, jwks_url: \"https://x/jwks\", secret: \"0123456789abcdef\"}",
      "secret on jwt",
    ),
    (
      "{method: jwt, jwks_url: \"https://x/jwks\", issuer: \"  \"}",
      "a blank issuer",
    ),
    (
      "{method: none, jwks_url: \"https://x/jwks\"}",
      "a key source on the open gate",
    ),
  ];
  for (yaml, why) in cases {
    validate_auth_setting(&auth_of(yaml)).expect_err(&format!("{why} should be refused: {yaml}"));
  }
}

#[test]
fn every_sibling_test_file_says_what_it_pins_down() {
  // Project rule: a module's tests live in a sibling `<file>_tests.rs` that
  // opens with a `//!` saying what about that module they hold down. The rule
  // exists because a test file is the one place a reader can find out what a
  // module is *supposed* to guarantee, and a file that starts straight into
  // `use super::*` makes them read four hundred assertions to find out.
  //
  // Checked here, beside the other cross-crate source walks, because it is
  // exactly the kind of thing that is true when written and quietly stops
  // being true one new file at a time.
  fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        walk(&path, out);
      } else if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("_tests.rs"))
      {
        out.push(path);
      }
    }
  }

  let mut files = Vec::new();
  for crate_dir in ["../aperio-server/src", "../aperio-client/src", "src"] {
    walk(std::path::Path::new(crate_dir), &mut files);
  }
  assert!(
    files.len() > 50,
    "the walk found only {} test files, so it is looking in the wrong place",
    files.len()
  );

  let missing: Vec<String> = files
    .iter()
    .filter(|p| {
      std::fs::read_to_string(p)
        .map(|t| !t.trim_start().starts_with("//!"))
        .unwrap_or(false)
    })
    .map(|p| p.display().to_string())
    .collect();
  assert!(
    missing.is_empty(),
    "these test files do not open with a `//!` saying what they pin down:\n  {}",
    missing.join("\n  ")
  );
}
