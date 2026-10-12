//! That every setting reaches both documentation tables, or says why not:
//! the client's keys, and every path of the server's settings document.
//!
//! The check rules 15 to 17 describe but nothing enforced. It works off the
//! JSON Schema rather than the Rust structs, which matters more than it
//! sounds: the schema carries the yaml spelling after serde's renames, so a
//! key is compared as an operator writes it rather than as its field name,
//! which nothing in the docs ever calls it.

use super::*;

/// How each client setting is spelled in a table, folded: its environment
/// variable, which names exactly one setting, and for the keys that have none
/// (a map of entries, `include`, a field of a tunnel or a bind) the key
/// itself.
fn client_spellings() -> Vec<String> {
  let mut out: Vec<String> = crate::layers::client_env()
    .iter()
    .map(|s| fold(&s.var))
    .collect();
  let schema = crate::layers::client_schema();
  for key in [
    "defaults",
    "services",
    "tunnels",
    "binds",
    "include",
    "depends_on",
  ] {
    out.push(fold(key));
  }
  for def in ["TunnelEntry", "BindEntry", "SubscribeEntry"] {
    for key in schema["$defs"][def]["properties"]
      .as_object()
      .into_iter()
      .flatten()
      .map(|(k, _)| k)
    {
      out.push(fold(key));
    }
  }
  out.sort();
  out.dedup();
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
/// LaTeX. The book documented `maintenance_windows` and `fallbacks` only inside
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
/// settings in their environment spelling, so `APERIO_HEALTH_INTERVAL` is how
/// `health_interval` appears. A trailing `_` is the other, because a grouped block
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

/// Every setting of the server's document, folded: `section.field` for a
/// field of a section, the bare key for a list or block at the top. A nested
/// block (`security.login_lockout`) is one setting, documented as a whole.
fn setting_paths() -> Vec<String> {
  let v: serde_json::Value =
    serde_json::from_str(&crate::server_schema_json()).expect("the schema is valid JSON");
  let mut out = Vec::new();
  for (key, prop) in v["properties"]
    .as_object()
    .expect("the schema has properties")
  {
    let fields = prop["$ref"]
      .as_str()
      .and_then(|r| r.rsplit('/').next())
      .and_then(|name| v["$defs"][name]["properties"].as_object());
    match fields {
      Some(fields) => out.extend(fields.keys().map(|f| fold(&format!("{key}.{f}")))),
      None => out.push(fold(key)),
    }
  }
  out
}

fn check(table: &str, text: &str, exempt: &[Exempt]) {
  let excused: Vec<String> = exempt.iter().map(|e| fold(e.key)).collect();
  let spans = documented_spans(text);
  let mut missing: Vec<String> = Vec::new();
  for spelling in client_spellings() {
    if !mentions(&spans, &spelling) && !excused.contains(&spelling) {
      missing.push(spelling);
    }
  }
  // The server's settings have no environment spelling: the dotted path is
  // what an operator writes, in the dashboard, the API and a document alike.
  for path in setting_paths() {
    if !mentions(&spans, &path) && !excused.contains(&path) {
      missing.push(path);
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
  let all: Vec<String> = client_spellings()
    .into_iter()
    .chain(setting_paths())
    .collect();
  for exempt in NOT_IN_BOOK {
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
