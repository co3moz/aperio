# Development & Releases

## Building from source

Requires the Rust toolchain (2024 edition, **1.87+**, declared as `rust-version` in each crate, so an older toolchain says so instead of failing inside a dependency). Building `aperio-server` additionally requires Node.js (with npm): the admin dashboard is a Vite + React app in [`aperio-dashboard/`](../aperio-dashboard/) that is built automatically by `build.rs` and embedded into the server binary.

```bash
cargo build --release -p aperio-server -p aperio-client
# binaries: target/release/aperio-server, target/release/aperio-client
```

To skip the frontend build (reusing an existing `aperio-dashboard/dist/`), set `APERIO_SKIP_DASHBOARD_BUILD=1`.

## Dashboard development

`npm run dev` in `aperio-dashboard/` serves the UI with hot reload and proxies API calls to a local server on port 8080. Debug builds of the server read `dist/` from disk at runtime, so a `npm run build` is picked up without recompiling the server.

Dashboard tests: `npm run test` runs the [vitest](https://vitest.dev) unit suite (pure lib functions; scans `src/` only), which CI runs alongside the i18n check. `npm run test:e2e` runs the [Playwright](https://playwright.dev) shell smoke test against a static `vite preview` build (one-time `npx playwright install chromium` first); it is not wired into CI because full API-backed journeys need a running server.

**The book.** `docs/book/aperio.tex` builds with `tectonic aperio.tex` (or `pdflatex` twice). The release workflow builds it too and attaches `aperio-guide.pdf` to every release, with a versioned copy beside it; that job fails if the version on the title page disagrees with the tag, so the bump in step 2 below is not optional.

**Brand lockup.** `npm run export:brand` renders the mark and the APERIO wordmark, in the dashboard's own Michroma webfont, to `docs/images/aperio-lockup.png`, which is what the book's title page includes. Re-run it if the mark or the wordmark font changes.

**Screenshots.** The images in `README.md`, the [docs pages](dashboard.md) and the [guide](book/aperio.tex) are re-captured with `npm run capture:docs` (in `aperio-dashboard/`, after `cargo build --workspace`). It brings up a throwaway instance on its own temp directory, drives demo traffic through it so the screens have something real to show, captures each page at 1440x900 @2x, and stops everything. Run it whenever the UI changes shape, the first set went stale within two releases because refreshing them was a manual afternoon. Adding a figure means adding an entry to `SHOTS`: a `tab`, and a `ready` selector that only the *populated* screen has. That last part is the whole discipline, a screen captured before its data arrives is an empty state, and an empty state looks exactly like a successful capture until somebody opens the PDF.

## Tests & end-to-end suite

`cargo test --all` runs the unit tests. `npm --prefix tests/e2e test` runs the end-to-end suite: a real `aperio-server`, several `aperio-client` processes, and mock backends, exercised phase by phase (proxying, dashboard APIs, auth, failover, load balancing, WebSocket pass-through, emergency tunnels, ...). Each phase gets its own server on its own port, and so does each file within a phase, so they run four at a time and any one of them, phase or file, can be run alone. CI runs both on every push and pull request, plus `cargo clippy -D warnings`, `cargo fmt --check`, a `cargo audit` scan of the dependency tree, and `cargo deny check licenses bans sources` ([deny.toml](../deny.toml)) for the questions `cargo audit` does not answer: which licenses end up in the shipped binaries, whether anything is fetched from outside crates.io, and whether a wildcard dependency is letting an arbitrary future version in.

### Protocol fuzzing

The tunnel wire protocol, the main corruption/attack surface, has [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) targets under [`tools/fuzz/`](../tools/fuzz): `binary_frame` (the v2 binary frame parser, asserting the `id.len() <= 255` prefix invariant) and `tunnel_message` (zlib inflate + `TunnelMessage` JSON decode). Run them on a nightly toolchain:

```console
cargo +nightly fuzz run --fuzz-dir tools/fuzz binary_frame
cargo +nightly fuzz run --fuzz-dir tools/fuzz tunnel_message
```

CI runs a short smoke pass of each. The `tools/fuzz/` crate is a standalone workspace, so it never affects the main `cargo build`/`test`.

### Benchmarks & load

[criterion](https://github.com/bheisler/criterion.rs) micro-benchmarks for the cache hot paths live in [`aperio-server/benches/hot_paths.rs`](../aperio-server/benches/hot_paths.rs) (`cargo bench -p aperio-server --bench hot_paths`); CI runs them with a short measurement window and reports the timings (a hard regression gate would need a persisted baseline, which is out of scope). For sustained load, [`tests/soak/`](../tests/soak/) holds a [k6](https://k6.io) soak profile with error-rate and p95-latency thresholds. `node tests/soak/run.mjs` brings a whole stack up, holds the load and watches both binaries' RSS over the plateau, failing on a growth *trend* rather than a threshold; a weekly workflow runs it, and `node --test 'tests/soak/*.test.mjs'` checks the rule that decides without generating any load. (Windows e2e is intentionally not run: development and the primary target are Unix; Windows issues are handled via feedback when they arise.)

## Test coverage

Coverage is measured with [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) (`cargo install cargo-llvm-cov` + `rustup component add llvm-tools-preview`):

```bash
cargo llvm-cov --workspace          # per-file summary table
cargo llvm-cov --workspace --open   # line-by-line HTML report in the browser
```

CI goes further and merges the E2E integration run into the same report (instrumented binaries driven by `tests/e2e`), publishing it as a `coverage-report` artifact on every run, that merged number is the real one, since the tunnel/proxy runtime paths are mostly exercised end-to-end rather than by unit tests. Note that the e2e merge relies on graceful SIGTERM handling to flush profile data, so it only works on Unix (CI/WSL); a local Windows run reports the unit-test-only number.

### Mutation testing

Coverage says a line ran. It does not say anything checked what the line did, and this repo has been caught by that difference three times: a sweep found four tests that had silently lost their `#[test]` attribute while the suite stayed green, a test turned out to describe a mechanism that does not exist, and a check passed on a configuration variable that was documented and never read. All three were found by breaking the behaviour and looking, which [cargo-mutants](https://mutants.rs) does mechanically.

```bash
cargo install cargo-mutants
cargo mutants --in-place        # the scoped set below; see the note on disk
```

**Use `--in-place`, one job, and clean around it.** Without it the tool copies
the entire build tree once per job, which is about 6 GB each; with it `target/`
grows instead, to tens of gigabytes over a full run. Either way the disk is the
binding constraint on a laptop, and a run that dies partway still writes a
survivor list that reads exactly like a complete one. Scope a run with
`--config` pointing at a cut-down file rather than `-f`, which `examine_globs`
overrides.

[`.cargo/mutants.toml`](../.cargo/mutants.toml) scopes it to the five modules where a wrong answer is a security answer: `visitor_auth.rs`, `proxy/gate.rs` and `auth.rs` (who is let in), `state/admission.rs` (what is refused) and `redact.rs` (what is kept out of the logs). That is roughly 315 mutants, and the cost is one full test suite per mutant, so the whole set is hours rather than minutes. It is deliberately not on the pull-request path, nor on any automatic one: the workflow is `workflow_dispatch` only, started by hand before a release, and it shards the set thirty-two ways so the wall-clock is minutes of runner time rather than an hour. It takes a glob for when you have just changed one of these modules and want to know.

**Read the survivors, not the score.** A surviving mutant is a change to the code that no test objected to, which is either an assertion nobody wrote or a line that does nothing; both are worth a look. A percentage is worth nothing and invites gaming. The workflow puts the list in the job summary and the whole run in an artifact. A mutant that *timed out* is unproven rather than caught, and is listed separately for the same reason.

## Releases

Tagging a version (`git tag v0.2.0 && git push --tags`) triggers the release workflow: static binaries for Linux (x86_64/aarch64, musl), macOS (Intel/Apple Silicon), and Windows are built, checksummed, and attached to a GitHub Release, [install.sh](../install.sh) always picks up the latest. `aperio-client --version` / `aperio-server --version` print the installed version. The versioned `aperio.yaml` and `aperio-server.yaml` JSON Schemas (`aperio-client.<tag>.json`, `aperio-server.<tag>.json`) are attached to the release too, along with [The Complete Guide](book/aperio.tex) as a PDF (`aperio-guide.<tag>.pdf`, plus a stable `aperio-guide.pdf` for `releases/latest/download`).

Every release is also **signed and attested**. A `SHA256SUMS` manifest covering all assets is signed with [Sigstore](https://www.sigstore.dev/) keyless signing (`SHA256SUMS.sigstore.json` next to it), the archives and the container images carry [build provenance attestations](https://docs.github.com/actions/security-guides/using-artifact-attestations), and an SPDX SBOM (`aperio.spdx.json`) is attached. None of it needs a repository secret: the workflow's own OIDC identity signs, so there is no key to rotate or leak. The verification commands are in [SECURITY.md](../SECURITY.md#verifying-a-release).

The Windows leg of that matrix compiles OpenSSL from source, which is minutes of the run, so CI keeps it warm: a `warm-release-cache` job on the default branch builds the Windows target whenever `Cargo.lock`/`Cargo.toml` changed and saves the cache the tag build restores. Two things keep that useful, and a CI step (`tools/scripts/check-release-cache.py`) enforces the first:

- **The warm job must run the same cargo invocations as the release**, split the same way. Features are unified per invocation, so building both crates together and building them separately give every shared dependency a different fingerprint, and the release recompiles despite a warm cache.
- **Push the version bump before the tag**, and let CI finish. The bump changes `Cargo.lock`, which changes the cache key; the warm job on that push is what fills the new one. Tagging the same commit at the same moment races it, and the release build starts against a key nothing has filled yet.

The same run also publishes the multi-arch (amd64+arm64) Docker images `ghcr.io/co3moz/aperio-server` and `ghcr.io/co3moz/aperio-client`, tagged with the version (`v0.2.0`, `0.2.0`, `0.2`, `0`); `latest` tracks the most recent **stable** release (a pre-release tag such as `v1.0.0-rc1` publishes only its exact tag). The images are assembled from the **same Linux musl binaries** the release build just produced: the release uses a runtime-only `Dockerfile.workflow` per crate that just copies the prebuilt binary in, so the Rust code is never built twice and there is no in-container cross-compilation to go wrong. The original from-source `Dockerfile` (compiles the crate and embeds the dashboard) is kept for local, air-gapped, or reproducible-from-source builds. Docker images are not built on ordinary pushes, the CI workflow (`ci.yml`) only builds, lints, tests, audits, and runs the e2e tunnel test.

## Conventions

- Configuration naming follows the [one-name-three-surfaces standard](configuration.md#the-standard-one-name-three-surfaces), CLI `--kebab-case` ↔ yaml `snake_case` ↔ env `APERIO_SNAKE_CASE`. Never rename across surfaces; keep legacy spellings as aliases.
- Unit tests live next to the module in a `<module>_tests.rs` file, included with `#[cfg(test)] #[path = "..."] mod tests;`.
- Every feature, fix, or behavior change updates `CHANGELOG.md` (`[Unreleased]` section, [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format) in the same commit.

## Zero-downtime restarts

With `APERIO_REUSEPORT=1` (yaml `reuseport`) the server binds its listener with `SO_REUSEPORT`, so a second process can bind the same `host:port` while the first is still running. A rolling restart is then:

1. Start the new process (same `PORT`, `APERIO_REUSEPORT=1`). The kernel begins load-balancing new connections across both.
2. Send `SIGTERM` to the old process. It broadcasts a `ServerShutdown` to its connected clients (so they reconnect immediately instead of waiting out their backoff) and drains in-flight requests before exiting.

Tunnels re-establish on the surviving process, so visitor traffic keeps flowing across the swap. `SO_REUSEPORT` is a Unix feature (Linux/BSD/macOS); on other platforms the flag is ignored and a plain listener is used.
