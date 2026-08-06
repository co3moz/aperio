# Contributing

Thanks for looking. Bug reports, reproductions and documentation fixes are as
welcome as code, and a good issue is often worth more than a patch.

Security bugs do **not** go here: see [SECURITY.md](SECURITY.md).

## Getting a build running

Rust 1.87+ and Node.js (the dashboard is a Vite + React app that `build.rs`
compiles into the server binary).

```bash
cargo build --release -p aperio-server -p aperio-client
```

[docs/development.md](docs/development.md) is the real reference: dashboard hot
reload, the e2e suite, fuzzing, coverage, the book, screenshots, and how a
release is cut. Read it before a first change of any size.

## Before you open a pull request

Run what CI runs, locally. CI minutes cost real money and a failure caught here
costs none:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

If the change touches the dashboard (`aperio-dashboard/`), also:

```bash
cd aperio-dashboard && npm run check-i18n && npx tsc -b && npm run lint && npm run test && npm run build
```

If it touches anything on the Rust, protocol or config side, run the
end-to-end suite once, at the end, rather than after each step, it brings up
real servers and clients and is slow:

```bash
npm --prefix tests/e2e test
```

## What a change is expected to carry

Not rules for their own sake: each of these exists because its absence caused a
concrete problem before.

- **A changelog entry**, in `CHANGELOG.md`, in the *same commit*, for anything a
  user or operator would notice: behavior, config, flags, env vars, endpoints,
  CLI, defaults, security. Purely internal work (refactors, tests, CI) skips it.
- **All of a setting's surfaces, together.** A new setting is a yaml key on the
  config struct (its doc comment becomes the JSON Schema description), an
  environment variable, a row in `docs/configuration.md`, and a row in the
  book's reference table (`docs/book/aperio.tex`). A setting that reaches only
  one surface is unfinished, not "to be completed later". See
  [the naming standard](docs/configuration.md#the-standard-one-name-three-surfaces).
- **A `CONFIG_CHANGES` entry** (`aperio-config/src/compat.rs`) whenever a config
  change can alter how an *existing* file behaves: a key renamed, removed or
  given a new meaning, a default that changes what a file does. This is what
  lets an operator upgrade blind. Purely additive keys need none.
- **Backward compatibility on the tunnel protocol.** Design changes so an older
  peer keeps working. A change that genuinely cannot be made compatibly is a
  discussion to open in the issue *before* the code, not a surprise in a diff:
  the server is one box and the clients are a fleet, and they are never
  upgraded at the same moment.
- **Tests next to the module.** Rust unit tests live in a sibling
  `<module>_tests.rs`, wired in with `#[cfg(test)] #[path = "..."] mod tests;`,
  never inline. A module and its tests grow at different rates.

## Style

- **English everywhere**: code, comments, commit messages, docs.
- **No em dashes.** A comma, or two sentences. This holds in UI strings and
  their translations too.
- Commit messages say *why*, in the imperative, with the reasoning in the body.
  The existing log is the reference for the voice.
- Comments explain the decision, not the syntax. A comment restating the line
  below it is noise; a comment saying which alternative was rejected and why is
  the thing nobody can reconstruct later.

## Ideas and the backlog

Feature ideas live in [planned_features.md](planned_features.md) with stable
`#N` ids that are never renumbered or reused. An entry moves between *Future
ideas*, *Withdrawn* and *Completed* exactly once, and a withdrawn one carries
the reason it was dropped, so a decision is on record rather than re-argued
from memory a year later. If you want to propose something, an issue is fine;
if you want to record it, that file is where it goes.
