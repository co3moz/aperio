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
