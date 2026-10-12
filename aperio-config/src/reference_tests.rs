//! That `docs/reference.md` says what the schemas say: every generated region
//! is exactly what this crate renders today.
//!
//! The schema is the side whose change invalidates the page (rule 25), so the
//! check is here, where a renamed field or a reworded doc comment runs it.

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
