//! Which released changelog sections are allowed to be malformed, and why.
//!
//! `CHANGELOG.md` is what an operator reads to decide whether an upgrade is
//! safe, and the grouping under `### Security` / `### Added` / `### Changed` /
//! `### Removed` / `### Fixed` is how they find the part that concerns them. A
//! version that repeats a heading splits one of those groups in two, and
//! somebody scanning for what a release fixed finds one list with no way to
//! learn the others exist. Nothing is lost, no entry is wrong, which is
//! exactly why it survived four times in this file.
//!
//! `changelog_tests.rs` holds the file to it. This module is the part that
//! cannot be derived: the versions that were already released malformed. They
//! are left alone deliberately. Rule 12 freezes a version once its tag exists,
//! and rewriting the shape of a section somebody may have linked to buys less
//! than it costs.
//!
//! Nothing should ever be added here. A new entry means a malformed section
//! was released, which is what the test exists to prevent.

/// A released version whose sections do not follow the shape, left as it
/// shipped.
pub struct Excused {
  /// The version, spelled as the heading writes it.
  pub version: &'static str,
  /// Why it is not being repaired.
  pub reason: &'static str,
}

/// The five that predate the check.
///
/// All of them are tagged releases, so rule 12 has frozen them, and rewriting
/// the shape of a section somebody may have linked to buys less than it costs.
/// The entries inside them are correct; only the grouping is not.
pub const EXCUSED: &[Excused] = &[
  Excused {
    version: "[0.6.0]",
    reason: "shipped with two `### Added` sections",
  },
  Excused {
    version: "[0.4.0]",
    reason: "shipped with its sections in the order they were written,              Changed before Security",
  },
  Excused {
    version: "[0.2.2]",
    reason: "shipped with two `### Added` sections",
  },
  Excused {
    version: "[0.2.0]",
    reason: "shipped with a `### CI` section, which no other version uses and              which is not a thing an operator upgrades into",
  },
  Excused {
    version: "[0.1.2]",
    reason: "shipped with its sections in the order they were written,              Added before Security",
  },
];

/// The sections a version may carry, in the order this file uses them.
///
/// Keep a Changelog puts `Security` last; this file has always put it first,
/// on the reasoning that it is the section an operator most needs to see
/// before deciding to upgrade. The order here follows the file rather than the
/// convention, because consistency within the file is what a reader benefits
/// from.
pub const SECTION_ORDER: &[&str] = &[
  "Security",
  "Added",
  "Changed",
  "Deprecated",
  "Removed",
  "Fixed",
];

#[cfg(test)]
#[path = "changelog_tests.rs"]
mod tests;
