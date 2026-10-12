//! That what is generated from the schemas is current: every region of
//! `docs/reference.md`, and the snapshot the dashboard's form tests run
//! against.
//!
//! The schema is the side whose change invalidates both (rule 25), so the
//! checks are here, where a renamed field or a reworded doc comment runs them.

use super::*;

fn page_path() -> std::path::PathBuf {
  std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/reference.md")
}

#[test]
fn the_reference_page_is_rendered_from_the_current_schemas() {
  let path = page_path();
  let page = std::fs::read_to_string(&path)
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    .replace("\r\n", "\n");
  let mut want = page.clone();
  for (name, render) in REGIONS {
    want = splice(&want, name, &render())
      .unwrap_or_else(|| panic!("docs/reference.md has no `{name}` region"));
  }
  if want == page {
    return;
  }
  if std::env::var_os("APERIO_BLESS").is_some() {
    std::fs::write(&path, &want).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    return;
  }
  panic!(
    "docs/reference.md is not what the schemas say. It is generated: change the doc \
     comment on the field, then regenerate the page with \
     `APERIO_BLESS=1 cargo test -p aperio-config reference` and commit it."
  );
}

#[test]
fn a_sentence_stating_the_default_is_left_to_the_default_column() {
  assert_eq!(
    without_default("Tunnel connections at once. Default `64`."),
    "Tunnel connections at once."
  );
  // A doc comment wraps wherever its line ran out, including right after
  // the word.
  assert_eq!(
    without_default("Requests the window must hold. Default\n`20`."),
    "Requests the window must hold."
  );
  // A full stop inside a code span does not end the sentence.
  assert_eq!(
    without_default("Share of traffic, `0.5` is half. Default: `0.0`; off."),
    "Share of traffic, `0.5` is half."
  );
  // A sentence that only mentions the word is kept.
  assert_eq!(
    without_default("The default route answers it."),
    "The default route answers it."
  );
}

#[test]
fn a_cell_keeps_to_one_line_and_cannot_end_early() {
  assert_eq!(
    cell("a\nwrapped  line | with a pipe"),
    "a wrapped line \\| with a pipe"
  );
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
