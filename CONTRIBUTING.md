# Contributing

Thanks for looking. Bug reports, reproductions and documentation fixes are as welcome as code, and a good issue is often worth more than a patch. Security bugs do **not** go here: see [SECURITY.md](SECURITY.md). Ideas and the backlog live in [GitHub Issues](https://github.com/co3moz/aperio/issues).

## Building

Rust 1.88+ and Node.js: `build.rs` builds the dashboard (`aperio-dashboard/`, Vite and React) and embeds it in the server, running `npm ci` first when `node_modules` is missing.

```bash
cargo build --release -p aperio-server -p aperio-client
```

`APERIO_SKIP_DASHBOARD_BUILD=1` reuses an existing `aperio-dashboard/dist/`. For dashboard work, `npm run dev` in `aperio-dashboard/` serves the UI with hot reload against a local server on port 8080; a debug server reads `dist/` from disk, so `npm run build` is picked up without recompiling it.

## Before you open a pull request

Run what CI runs, locally. CI minutes cost money and a failure caught here costs none.

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

If the change touches the dashboard:

```bash
cd aperio-dashboard && npm run check-i18n && npx tsc -b && npm run lint && npm run test && npm run build
```

If it touches the Rust, protocol or configuration side, run the end-to-end suite once, at the end: it brings up real servers, clients and backends, phase by phase, and is slow.

```bash
npm --prefix tests/e2e test
```

`tests/e2e/specs/timing/budget.test.ts` holds a server start, a client connect and a reconnect to 300 ms each; a change that makes one slower is fixed rather than given a bigger budget.

## What a change carries

Each of these exists because its absence caused a concrete problem before.

- **A changelog entry** in `CHANGELOG.md`, in the same commit, for anything a user or operator would notice: behaviour, configuration, flags, variables, endpoints, CLI, defaults, security. Internal work (refactors, tests, CI) skips it.
- **A setting is a doc comment.** A new key goes on its struct in `aperio-config` with a doc comment and an example; the comment is the JSON Schema's description and the row in [`docs/reference.md`](docs/reference.md). Regenerate the page with `APERIO_BLESS=1 cargo test -p aperio-config reference`, and the dashboard's schema snapshot with `node scripts/dump-schemas.mjs` in `aperio-dashboard/`.
- **A removed or renamed key leaves a hint** in `aperio-config/src/removed.rs` naming what replaced it, and a **Breaking** line in the changelog. There is no `version:` and no translation layer, so the hint is what an operator reads after an upgrade.
- **The protocol is frozen within a minor release.** A client and a server pair only on the same minor (`aperio-config/src/pairing.rs`), so a patch must not change a frame, a handshake header or the Ping's shape. On a minor release it may change freely, and code kept only for an older peer is deleted.
- **Tests beside the module**, in a sibling `<module>_tests.rs` wired in with `#[cfg(test)] #[path = "..."] mod tests;`, never inline.

## Style

- **English everywhere**: code, comments, commit messages, docs.
- **No em dashes.** A comma, or two sentences. This holds in UI strings and their translations too.
- Commit messages say *why*, in the imperative, with the reasoning in the body.
- Comments explain the decision, not the syntax: which alternative was rejected and why is what nobody can reconstruct later.

## Testing deeper

**Fuzzing.** The tunnel's frame parser and message decoder have `cargo-fuzz` targets in [`tools/fuzz/`](tools/fuzz), a standalone workspace; CI runs a short pass of each.

```bash
cargo +nightly fuzz run --fuzz-dir tools/fuzz binary_frame
cargo +nightly fuzz run --fuzz-dir tools/fuzz tunnel_message
```

**Coverage.** `cargo llvm-cov --workspace` for the unit tests. CI merges the end-to-end run into the same report, which is the real number, since the tunnel and proxy paths are mostly exercised end to end.

**Mutation testing.** Coverage says a line ran, not that anything checked what it did. [cargo-mutants](https://mutants.rs) over the security-relevant modules listed in [`.cargo/mutants.toml`](.cargo/mutants.toml) runs in the **Chore** workflow, started by hand before a release and sharded across runners. Do not run it locally: it grows `target/` past tens of gigabytes. Read the survivors, not the score.

**Benchmarks and soak.** Criterion benchmarks in `aperio-server/benches/`, and a k6 soak profile in [`tests/soak/`](tests/soak/) that fails on a growth trend in either binary's memory. Both are load: ask before running them on a shared machine, and a number measured beside a build is not a number.

**Screenshots.** After `cargo build --workspace`, `npm run capture:docs` in `aperio-dashboard/` starts a temporary server, generates traffic and recaptures the images in `README.md` and `docs/`. A new figure gets a `tab` and a `ready` selector in its `SHOTS` list.

## Releases

Tagging `vX.Y.Z` runs the release workflow: static binaries for Linux (x86_64 and aarch64, musl), macOS and Windows, `.deb` and `.rpm` packages, a Homebrew formula and a Scoop manifest, the JSON Schemas, and multi-arch images on `ghcr.io`, built from the same binaries. A `SHA256SUMS` manifest is signed with Sigstore, archives and images carry build provenance, and an SPDX SBOM is attached; see [SECURITY.md](SECURITY.md#verifying-a-release).

**Rehearse first.** The same workflow runs from its Run button with the tag it is about to be given and does everything except publish, uploading the result as a `release-rehearsal-<tag>` artifact. Tag only after it is green.

**Push the version bump before the tag** and let CI finish: the bump changes `Cargo.lock`, and the `warm-release-cache` job on that push is what fills the cache the tag build restores.

**A Homebrew tap** is optional: with `HOMEBREW_TAP_REPO` set as a repository variable and `HOMEBREW_TAP_TOKEN` as a secret, the release pushes the formulae there. `tools/packaging/render-manifests.sh` renders them from a release's own checksums.
