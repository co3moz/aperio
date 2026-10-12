//! Which settings are deliberately absent from the book's reference table,
//! and why.
//!
//! `docs/reference.md` is generated from the schemas (`reference.rs`), so it
//! cannot miss a setting. The book's table is written by hand, and the drift
//! there is invisible by eye: the one that had happened by 2026-08-18 was a
//! single key missing from it.
//!
//! `surfaces_tests.rs` holds them to it. This module is the part that cannot
//! be derived, the handful of keys that are absent on purpose. It is
//! deliberately short and deliberately annoying to add to: a new entry here is
//! a decision that something stays undocumented, which is the kind of decision
//! that should cost a sentence explaining itself.

/// A key the check does not expect to find in a table, with the reason.
pub struct Exempt {
  /// The yaml key, spelled as the JSON Schema spells it.
  pub key: &'static str,
  /// Why it is not in that table. Read by a person, not by the test.
  pub why: &'static str,
}

/// Keys absent from the book's reference table (`docs/book/aperio.tex`).
///
/// Empty, and worth keeping that way. The book is the surface that drifts
/// first, because it is the one nobody opens while editing Rust.
pub const NOT_IN_BOOK: &[Exempt] = &[];

/// Normalizes a key or a document for comparison.
///
/// Three spellings of the same setting exist and all three are correct where
/// they appear: the book escapes underscores for LaTeX (`request\_id\_header`),
/// `docs/configuration.md` writes a grouped key with a dot
/// (`request_id.header`) and names the environment variable in upper case
/// (`APERIO_REQUEST_ID_HEADER`), and a key may be written with a dash where a
/// person sees it.
///
/// Comparing without folding those together is why the first three passes of
/// this measurement reported over a hundred missing keys, every one of them a
/// false positive. A checker that cries wolf on that scale is deleted within
/// the week, so the folding is the feature.
pub fn fold(s: &str) -> String {
  s.replace("\\_", "_")
    .replace(['-', '.'], "_")
    .to_lowercase()
}

#[cfg(test)]
#[path = "surfaces_tests.rs"]
mod tests;
