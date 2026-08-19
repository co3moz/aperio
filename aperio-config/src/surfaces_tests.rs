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

/// Settings with no `APERIO_<KEY>` to be documented by, so their own spelling
/// is all a table can show.
///
/// Derived from the schema rather than listed, because the rule is derivable:
/// rule 16's stated exception is a structured value, and the server only
/// materializes *scalar* keys into the environment, so an object or an array
/// has no variable by construction. `host`, `port` and `log_level` are the
/// three the rule names as taking bare environment names instead of the
/// prefix, which is the same thing for this check.
fn no_env_spelling(schema: &str) -> Vec<String> {
  let v: serde_json::Value = serde_json::from_str(schema).expect("the schema is valid JSON");
  let mut out = vec![
    "host".to_string(),
    "port".to_string(),
    "log_level".to_string(),
  ];
  if let Some(props) = v["properties"].as_object() {
    for (k, spec) in props {
      let structured = match &spec["type"] {
        serde_json::Value::String(s) => s == "object" || s == "array",
        serde_json::Value::Array(list) => list.iter().any(|t| t == "object" || t == "array"),
        _ => spec.get("properties").is_some() || spec.get("items").is_some(),
      };
      if structured {
        out.push(fold(k));
      }
    }
  }
  out
}

fn doc(relative: &str) -> String {
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("..")
    .join(relative);
  fold(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

/// The parts of a page that are spelling a key, rather than talking about one.
///
/// The check used to be `contains` over the whole page, and that cannot fail
/// for a short key: `name` is inside `hostname`, `custom_name` and
/// `service_name`, so a setting called `name` was documented by any page that
/// mentioned a hostname, which is all of them. It passed before a row for it
/// existed, which is how it was found.
///
/// Documentation spells a setting in one of four places, and all four have to
/// count or the check demands rows that are already there: inline code and
/// fenced blocks in Markdown, and `\code{}` and the verbatim environments in
/// LaTeX. The book documents `maintenance_windows` and `fallbacks` only inside
/// a `codecard`, which a stricter first attempt reported as undocumented.
fn documented_spans(text: &str) -> String {
  let mut out = String::new();
  // Markdown: inline spans and fenced blocks both fall out of splitting on
  // backticks, since a fence is three of them and its body is the odd part.
  for (i, part) in text.split('`').enumerate() {
    if i % 2 == 1 {
      out.push_str(part);
      out.push('\n');
    }
  }
  // LaTeX: the argument of `\code{...}`, which does not nest.
  let mut rest = text;
  while let Some(at) = rest.find("\\code{") {
    rest = &rest[at + 6..];
    if let Some(end) = rest.find('}') {
      out.push_str(&rest[..end]);
      out.push('\n');
      rest = &rest[end..];
    }
  }
  // LaTeX: verbatim listings, which is where a yaml block is written out.
  for env in ["codecard", "lstlisting"] {
    let open = format!("\\begin{{{env}}}");
    let close = format!("\\end{{{env}}}");
    let mut rest = text;
    while let Some(at) = rest.find(&open) {
      rest = &rest[at + open.len()..];
      if let Some(end) = rest.find(&close) {
        out.push_str(&rest[..end]);
        out.push('\n');
        rest = &rest[end..];
      } else {
        break;
      }
    }
  }
  out
}

/// Does `spans` spell `key` as a whole key, rather than inside another word?
///
/// Two boundaries are deliberately not boundaries, both learned by breaking
/// them. A leading `aperio_` is one, because both tables document most
/// settings in their environment spelling, so `APERIO_BACKUP_DIR` is how
/// `backup_dir` appears. A trailing `_` is the other, because a grouped block
/// like `circuit_breaker` is documented through its children.
fn mentions(spans: &str, key: &str) -> bool {
  let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
  let bytes = spans.as_bytes();
  let mut from = 0;
  while let Some(rel) = spans[from..].find(key) {
    let at = from + rel;
    let before_ok = at == 0 || !ident(bytes[at - 1] as char) || spans[..at].ends_with("aperio_");
    let end = at + key.len();
    let after_ok = end >= bytes.len() || !ident(bytes[end] as char) || bytes[end] == b'_';
    if before_ok && after_ok {
      return true;
    }
    from = at + 1;
  }
  false
}

fn check(table: &str, text: &str, exempt: &[Exempt]) {
  let excused: Vec<String> = exempt.iter().map(|e| fold(e.key)).collect();
  let spans = documented_spans(text);
  let mut missing: Vec<String> = Vec::new();
  for schema in [crate::schema_json(), crate::server_schema_json()] {
    let bare = no_env_spelling(&schema);
    for key in keys(&schema) {
      // The environment spelling is what a table actually writes, and it is
      // far more specific than the bare key: `name: web` in a yaml example
      // is not documentation of a setting called `name`, while
      // `APERIO_NAME` can only be one. A key with no environment variable
      // falls back to its own spelling, which is all it has.
      let env = format!("aperio_{key}");
      // The bare key is only good enough for a setting that genuinely has no
      // environment variable, which `CLIENT_ENV_EXEMPT` is the list of. For
      // everything else the variable is required, and that is what makes this
      // check able to fail: `name: web` in a yaml example is not
      // documentation of a setting called `name`, and used to satisfy it.
      let env_exempt = crate::surfaces::CLIENT_ENV_EXEMPT
        .iter()
        .any(|e| fold(e.key) == key)
        || bare.contains(&key);
      let found = mentions(&spans, &env) || (env_exempt && mentions(&spans, &key));
      if !found && !excused.contains(&key) {
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
       `node scripts/dump-schemas.mjs` from aperio-dashboard/"
    );
  }
}
