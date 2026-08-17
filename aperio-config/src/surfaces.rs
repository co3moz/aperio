//! Which settings are deliberately absent from a documentation table, and why.
//!
//! Rules 15 to 17 in `CLAUDE.md` say a setting reaches yaml, an env read, the
//! table in `docs/configuration.md` and the reference table in
//! `docs/book/aperio.tex`, all in the same commit. Four surfaces kept in step
//! by whoever remembers, over two hundred and fourteen keys, and the drift is
//! invisible by eye: the one that had happened by 2026-08-18 was a single key
//! missing from the book.
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

/// Keys absent from `docs/configuration.md`.
///
/// Rule 16 exempts a list of mappings from having an environment variable,
/// and the same shape is what keeps these out of a table whose rows are
/// `key | description | default`: a rule with five fields of its own does not
/// fit in a cell, so it is written up where it can be shown as yaml.
pub const NOT_IN_CONFIGURATION_MD: &[Exempt] = &[
  Exempt {
    key: "alert_rules",
    why: "a list of mappings, each with a metric, a bound and a window; written up with examples in docs/observability.md",
  },
  Exempt {
    key: "maintenance_windows",
    why: "a list of mappings, each with a schedule and a scope; written up with examples in docs/dashboard.md",
  },
];

/// Keys absent from the book's reference table (`docs/book/aperio.tex`).
///
/// Empty, and worth keeping that way. The book is the surface that drifts
/// first, because it is the one nobody opens while editing Rust.
pub const NOT_IN_BOOK: &[Exempt] = &[];

/// Client settings with no `APERIO_*` read, or one under another name.
///
/// Rule 16: env is the secondary surface, needed by containers and secret
/// injection, so a variable should exist unless the value genuinely cannot be
/// a flat scalar. A list of mappings is the stated exception; a scalar, a list
/// of scalars, or a grouped block's child is not.
///
/// The server needs no such list: `config_file.rs` materializes every scalar
/// key it reads into `APERIO_<KEY>` generically, so the surface is satisfied by
/// construction there. The client reads each variable by name, which is where
/// a key can quietly have none, and where `depends_on` did.
pub const CLIENT_ENV_EXEMPT: &[Exempt] = &[
  Exempt {
    key: "services",
    why: "a list of mappings, rule 16's stated exception",
  },
  Exempt {
    key: "tunnels",
    why: "a list of mappings, rule 16's stated exception",
  },
  Exempt {
    key: "headers",
    why: "a list of mappings, named in rule 16 itself",
  },
  Exempt {
    key: "bind-tunnels",
    why: "a map of mappings, which is the same shape as the exception and worse to flatten",
  },
  Exempt {
    key: "include",
    why: "resolved relative to the directory of the file that writes it, and a variable has no file to be relative to; the merge order is a property of the file tree",
  },
  Exempt {
    key: "log_level",
    why: "read as the bare LOG_LEVEL, which rule 16 names as one of the three exceptions to the APERIO_ prefix",
  },
  Exempt {
    key: "auth",
    why: "read as APERIO_VISITOR_AUTH, which says what it gates; the key is `auth:` because it sits on a service",
  },
  Exempt {
    key: "token",
    why: "the tunnel token, read as APERIO_SERVER_TOKEN because `token:` is the short spelling of `server.token:` and the variable names the thing rather than the shorthand",
  },
  Exempt {
    key: "circuit_breaker",
    why: "its two children are read as APERIO_BREAKER_FAILURES and APERIO_BREAKER_OPEN_FOR, shortened before rule 16 wrote the naming down; renaming them would break files that work, for a prefix",
  },
];

/// The environment variable rule 16 expects for a yaml key.
pub fn env_name(key: &str) -> String {
  format!("APERIO_{}", key.replace('-', "_").to_ascii_uppercase())
}

/// Normalizes a key or a document for comparison.
///
/// Three spellings of the same setting exist and all three are correct where
/// they appear: the book escapes underscores for LaTeX (`request\_id\_header`),
/// `docs/configuration.md` writes a grouped key with a dot
/// (`request_id.header`) and names the environment variable in upper case
/// (`APERIO_REQUEST_ID_HEADER`), and a key with a serde rename is written with
/// a dash where a person sees it (`bind-tunnels`).
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
