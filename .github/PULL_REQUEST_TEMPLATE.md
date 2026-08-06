<!-- Thanks for the patch. The list below is short on purpose: each item is
     something whose absence has actually broken something here before. Delete
     what genuinely does not apply, rather than ticking it. -->

## What this changes

<!-- What it does and, more usefully, why. Which alternative did you reject? -->

## Why it is correct

<!-- How you know: the test that fails without it, the reproduction that stops
     reproducing, the measurement. "It builds" is not this section. -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
      and `cargo test --workspace` all pass locally
- [ ] `npm --prefix tests/e2e test` passes (skip only for a dashboard-only change)
- [ ] Dashboard change: `npm run check-i18n`, `tsc -b`, `lint`, `test`, `build` pass
- [ ] `CHANGELOG.md` updated in this same commit, or the change is purely internal
- [ ] A new setting reaches **all** its surfaces: yaml field with a doc comment,
      environment variable, `docs/configuration.md`, `docs/book/aperio.tex`
- [ ] A config change that can affect an existing file has a `CONFIG_CHANGES`
      entry with an honest severity (`aperio-config/src/compat.rs`)
- [ ] The tunnel protocol still accepts an older peer, or the break is agreed
      in an issue first and refuses the connection with a message that says so
- [ ] New Rust tests live in a sibling `<module>_tests.rs`, not inline
- [ ] English throughout, and no em dashes
