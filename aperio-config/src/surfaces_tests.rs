//! That every setting reaches both documentation tables, or says why not.
//!
//! The check rules 15 to 17 describe but nothing enforced. It works off the
//! JSON Schema rather than the Rust structs, which matters more than it
//! sounds: the schema carries the yaml spelling after serde's renames, so
//! `bind-tunnels` and `504_page` are compared as an operator writes them
//! rather than as `bind_tunnels` and `error_page_504`, which is what the field
//! names say and what nothing in the docs ever calls them.

use super::*;

/// Every yaml key the schema describes, folded for comparison.
fn keys(schema: &str) -> Vec<String> {
  let v: serde_json::Value = serde_json::from_str(schema).expect("the schema is valid JSON");
  v["properties"]
    .as_object()
    .expect("the schema has properties")
    .keys()
    .map(|k| fold(k))
    .collect()
}

fn doc(relative: &str) -> String {
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("..")
    .join(relative);
  fold(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

fn check(table: &str, text: &str, exempt: &[Exempt]) {
  let excused: Vec<String> = exempt.iter().map(|e| fold(e.key)).collect();
  let mut missing: Vec<String> = Vec::new();
  for schema in [crate::schema_json(), crate::server_schema_json()] {
    for key in keys(&schema) {
      if !text.contains(&key) && !excused.contains(&key) {
        missing.push(key);
      }
    }
  }
  missing.sort();
  missing.dedup();
  assert!(
    missing.is_empty(),
    "{} does not document: {}.\n\nRule 17: a setting reaches the yaml struct, the env read, \
     `docs/configuration.md` and `docs/book/aperio.tex` in the same commit. Add the row. \
     If it genuinely belongs nowhere in that table, add it to `surfaces.rs` with the reason, \
     which is a decision worth writing down rather than a line worth deleting.",
    table,
    missing.join(", ")
  );
}

#[test]
fn every_setting_is_in_the_configuration_table() {
  check(
    "docs/configuration.md",
    &doc("docs/configuration.md"),
    NOT_IN_CONFIGURATION_MD,
  );
}

#[test]
fn every_setting_is_in_the_books_reference_table() {
  // The surface that drifts first: it is the one nobody has open while
  // editing Rust. `depends_on` was missing from it when this check was
  // written, and had been for as long as the key existed.
  check(
    "docs/book/aperio.tex",
    &doc("docs/book/aperio.tex"),
    NOT_IN_BOOK,
  );
}

/// An exemption names a key that exists, and says why.
///
/// Both halves rot on their own. A key renamed leaves an exemption excusing
/// nothing, which then silently excuses nothing forever while the real key
/// goes unchecked; and an exemption without a reason is indistinguishable
/// from someone having deleted a failing assertion.
#[test]
fn every_exemption_is_for_a_key_that_exists_and_gives_a_reason() {
  let all: Vec<String> = [crate::schema_json(), crate::server_schema_json()]
    .iter()
    .flat_map(|s| keys(s))
    .collect();
  for exempt in NOT_IN_CONFIGURATION_MD.iter().chain(NOT_IN_BOOK) {
    assert!(
      all.contains(&fold(exempt.key)),
      "`{}` is exempted from a documentation table but is not a setting; \
       it was probably renamed, and the exemption now excuses nothing while \
       the real key goes unchecked",
      exempt.key
    );
    assert!(
      exempt.why.len() > 20,
      "`{}` is exempted with no real reason given",
      exempt.key
    );
  }
}

// ---------------------------------------------------------------------------
// Rule 16: an environment variable wherever one can exist
// ---------------------------------------------------------------------------

/// Strips line comments, so only what the code does counts.
///
/// Without this the check passes on a variable that is *documented* and never
/// read, because `ClientSettings` names each variable in the doc comment above
/// its field. Verified the only way that means anything: deleting the
/// `APERIO_DEPENDS_ON` read while the doc comment stayed left the test green.
fn code_only(src: &str) -> String {
  src
    .lines()
    .map(|l| match l.find("//") {
      Some(i) => &l[..i],
      None => l,
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Every client setting the schema describes is reachable from the environment.
///
/// Reads the source for the variable rather than the documentation table for
/// its name. `docs/configuration.md` already lists a variable per setting, so
/// checking the doc would pass on a variable that is documented and not read,
/// which is the failure mode this is for: `depends_on` was in the schema, so
/// editors completed it and `--check-config` accepted it, and no code read it
/// anywhere, at the top level or from the environment.
#[test]
fn every_client_setting_can_be_set_from_the_environment() {
  let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
  let mut src = String::new();
  for dir in ["aperio-client/src", "aperio-config/src"] {
    let mut stack = vec![root.join(dir)];
    while let Some(d) = stack.pop() {
      for entry in std::fs::read_dir(&d)
        .expect("the crate source is readable")
        .flatten()
      {
        let path = entry.path();
        if path.is_dir() {
          stack.push(path);
        } else if path.extension().is_some_and(|e| e == "rs")
          // Production reads only: a name that appears solely in a test is a
          // variable nothing honours.
          && !path.to_string_lossy().ends_with("_tests.rs")
        {
          src.push_str(&code_only(
            &std::fs::read_to_string(&path).expect("a source file is readable"),
          ));
        }
      }
    }
  }
  let excused: Vec<String> = CLIENT_ENV_EXEMPT.iter().map(|e| fold(e.key)).collect();
  let schema: serde_json::Value =
    serde_json::from_str(&crate::schema_json()).expect("the schema is valid JSON");
  let mut missing: Vec<String> = Vec::new();
  for key in schema["properties"]
    .as_object()
    .expect("the schema has properties")
    .keys()
  {
    if excused.contains(&fold(key)) {
      continue;
    }
    let var = env_name(key);
    if !src.contains(&var) {
      missing.push(format!("{key} (expected {var})"));
    }
  }
  missing.sort();
  assert!(
    missing.is_empty(),
    "no environment variable is read for: {}.\n\nRule 16: env is the secondary \
     surface and a container has nothing else. Add the read. If the value \
     genuinely cannot be a flat scalar, or is read under another name, add it \
     to `CLIENT_ENV_EXEMPT` in `surfaces.rs` with the reason.",
    missing.join(", ")
  );
}

/// The env exemptions are held to the same standard as the doc ones.
#[test]
fn every_env_exemption_is_for_a_key_that_exists_and_gives_a_reason() {
  let schema: serde_json::Value =
    serde_json::from_str(&crate::schema_json()).expect("the schema is valid JSON");
  let all: Vec<String> = schema["properties"]
    .as_object()
    .expect("the schema has properties")
    .keys()
    .map(|k| fold(k))
    .collect();
  for exempt in CLIENT_ENV_EXEMPT {
    assert!(
      all.contains(&fold(exempt.key)),
      "`{}` is exempted from needing an environment variable but is not a \
       client setting; it was probably renamed, and the real key now goes \
       unchecked",
      exempt.key
    );
    assert!(
      exempt.why.len() > 20,
      "`{}` is exempted with no real reason given",
      exempt.key
    );
  }
}

// ---------------------------------------------------------------------------
// The dashboard's config form, which is a config surface too
// ---------------------------------------------------------------------------

/// Compares two schemas the way the snapshot survives being written.
///
/// The snapshot is produced by piping this crate's output through
/// `JSON.stringify`, which writes `10.0` as `10`. `serde_json` keeps the two
/// apart, so a byte-faithful comparison fails on a difference that exists only
/// because the file passed through Node and that nobody can fix. Numbers are
/// therefore compared by value; everything else exactly.
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
  use serde_json::Value;
  match (a, b) {
    (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
      (Some(x), Some(y)) => x == y,
      _ => x == y,
    },
    (Value::Object(x), Value::Object(y)) => {
      x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
    }
    (Value::Array(x), Value::Array(y)) => {
      x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w))
    }
    _ => a == b,
  }
}

/// The schema snapshot the dashboard's form tests run against is current.
///
/// `configSchema.live.test.ts` exists so a new setting is proven to reach the
/// config editor rather than degrading to an `unsupported` row in silence. It
/// runs against `__schemas.json`, a checked-in dump, and its own generator says
/// what that costs: "Run it after changing aperio-config, or the test keeps
/// answering for the old schema."
///
/// Nobody ran it. Measured 2026-08-18, the snapshot was from 2026-08-05 and had
/// been answering for a schema without `multiplex`, `egress_proxy`,
/// `tls_min_version` or `tls_cipher_suites` for two weeks, which is precisely
/// the four settings somebody would have wanted the form proven for.
///
/// This assertion is here rather than in the dashboard because the change that
/// invalidates the snapshot is a change to *this crate*, and a check belongs on
/// the side whose test suite the breaking change actually runs. The same
/// reasoning put the explain-code parity check into `aperio-server`.
#[test]
fn the_dashboards_schema_snapshot_is_not_stale() {
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../aperio-dashboard/src/lib/__schemas.json");
  let snapshot: serde_json::Value = serde_json::from_str(
    &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
  )
  .expect("the snapshot is valid JSON");

  for (side, live) in [
    ("client", crate::schema_json()),
    ("server", crate::server_schema_json()),
  ] {
    let live: serde_json::Value = serde_json::from_str(&live).expect("the schema is valid JSON");
    let snap = &snapshot[side];
    let keys = |v: &serde_json::Value| -> Vec<String> {
      v["properties"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
    };
    let (live_keys, snap_keys) = (keys(&live), keys(snap));
    let missing: Vec<&String> = live_keys
      .iter()
      .filter(|k| !snap_keys.contains(k))
      .collect();
    let extra: Vec<&String> = snap_keys
      .iter()
      .filter(|k| !live_keys.contains(k))
      .collect();
    assert!(
      missing.is_empty() && extra.is_empty(),
      "the {side} schema snapshot is stale: {} missing, {} left over.\n\n\
       Regenerate it with `node scripts/dump-schemas.mjs` from aperio-dashboard/ \
       and commit the result; until then the dashboard's form tests are \
       answering for a schema that no longer exists.\n  missing: {:?}\n  extra: {:?}",
      missing.len(),
      extra.len(),
      missing,
      extra
    );
    assert!(
      same(snap, &live),
      "the {side} schema snapshot has the right keys but differs in shape \
       (a type, an enum, a description); regenerate it with \
       `node scripts/dump-schemas.mjs`"
    );
  }
}
