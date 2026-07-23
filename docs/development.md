# Development & Releases

## Building from source

Requires the Rust toolchain (2024 edition, 1.85+). Building `aperio-server` additionally requires Node.js (with npm): the admin dashboard is a Vite + React app in [`aperio-dashboard/`](../aperio-dashboard/) that is built automatically by `build.rs` and embedded into the server binary.

```bash
cargo build --release -p aperio-server -p aperio-client
# binaries: target/release/aperio-server, target/release/aperio-client
```

To skip the frontend build (reusing an existing `aperio-dashboard/dist/`), set `APERIO_SKIP_DASHBOARD_BUILD=1`.

## Dashboard development

`npm run dev` in `aperio-dashboard/` serves the UI with hot reload and proxies API calls to a local server on port 8080. Debug builds of the server read `dist/` from disk at runtime, so a `npm run build` is picked up without recompiling the server.

Dashboard tests: `npm run test` runs the [vitest](https://vitest.dev) unit suite (pure lib functions; scans `src/` only), which CI runs alongside the i18n check. `npm run test:e2e` runs the [Playwright](https://playwright.dev) shell smoke test against a static `vite preview` build (one-time `npx playwright install chromium` first); it is not wired into CI because full API-backed journeys need a running server.

## Tests & end-to-end suite

`cargo test --all` runs the unit tests. `bash tests/e2e.sh` runs the end-to-end suite — a real `aperio-server`, several `aperio-client` processes, and stdlib-only Python mock backends, exercised phase by phase (proxying, dashboard APIs, auth, failover, load balancing, WebSocket pass-through, emergency tunnels, ...). CI runs both on every push and pull request, plus `cargo clippy -D warnings`, `cargo fmt --check`, and a `cargo audit` scan of the dependency tree.

### Protocol fuzzing

The tunnel wire protocol — the main corruption/attack surface — has [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) targets under [`fuzz/`](../fuzz): `binary_frame` (the v2 binary frame parser, asserting the `id.len() <= 255` prefix invariant) and `tunnel_message` (zlib inflate + `TunnelMessage` JSON decode). Run them on a nightly toolchain:

```console
cargo +nightly fuzz run binary_frame
cargo +nightly fuzz run tunnel_message
```

CI runs a short smoke pass of each. The `fuzz/` crate is a standalone workspace, so it never affects the main `cargo build`/`test`.

### Benchmarks & load

[criterion](https://github.com/bheisler/criterion.rs) micro-benchmarks for the cache hot paths live in [`aperio-server/benches/hot_paths.rs`](../aperio-server/benches/hot_paths.rs) (`cargo bench -p aperio-server --bench hot_paths`); CI runs them with a short measurement window and reports the timings (a hard regression gate would need a persisted baseline, which is out of scope). For sustained load, [`tests/soak.js`](../tests/soak.js) is a [k6](https://k6.io) soak test with error-rate and p95-latency thresholds — run manually against a live stack, not in CI. (Windows e2e is intentionally not run: development and the primary target are Unix; Windows issues are handled via feedback when they arise.)

## Test coverage

Coverage is measured with [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) (`cargo install cargo-llvm-cov` + `rustup component add llvm-tools-preview`):

```bash
cargo llvm-cov --workspace          # per-file summary table
cargo llvm-cov --workspace --open   # line-by-line HTML report in the browser
```

CI goes further and merges the E2E integration run into the same report (instrumented binaries driven by `tests/e2e.sh`), publishing it as a `coverage-report` artifact on every run — that merged number is the real one, since the tunnel/proxy runtime paths are mostly exercised end-to-end rather than by unit tests. Note that the e2e merge relies on graceful SIGTERM handling to flush profile data, so it only works on Unix (CI/WSL); a local Windows run reports the unit-test-only number.

## Releases

Tagging a version (`git tag v0.2.0 && git push --tags`) triggers the release workflow: static binaries for Linux (x86_64/aarch64, musl), macOS (Intel/Apple Silicon), and Windows are built, checksummed, and attached to a GitHub Release — [install.sh](../install.sh) always picks up the latest. `aperio-client --version` / `aperio-server --version` print the installed version. The versioned `aperio.yaml` and `aperio-server.yaml` JSON Schemas (`aperio-client.<tag>.json`, `aperio-server.<tag>.json`) are attached to the release too.

The same run also publishes the multi-arch (amd64+arm64) Docker images `ghcr.io/co3moz/aperio-server` and `ghcr.io/co3moz/aperio-client`, tagged with the version (`v0.2.0`, `0.2.0`, `0.2`, `0`); `latest` tracks the most recent **stable** release (a pre-release tag such as `v1.0.0-rc1` publishes only its exact tag). The images are assembled from the **same Linux musl binaries** the release build just produced: the release uses a runtime-only `Dockerfile.workflow` per crate that just copies the prebuilt binary in, so the Rust code is never built twice and there is no in-container cross-compilation to go wrong. The original from-source `Dockerfile` (compiles the crate and embeds the dashboard) is kept for local, air-gapped, or reproducible-from-source builds. Docker images are not built on ordinary pushes — the CI workflow (`ci.yml`) only builds, lints, tests, audits, and runs the e2e tunnel test.

## Conventions

- Configuration naming follows the [one-name-three-surfaces standard](configuration.md#the-standard-one-name-three-surfaces) — CLI `--kebab-case` ↔ yaml `snake_case` ↔ env `APERIO_SNAKE_CASE`. Never rename across surfaces; keep legacy spellings as aliases.
- Unit tests live next to the module in a `<module>_tests.rs` file, included with `#[cfg(test)] #[path = "..."] mod tests;`.
- Every feature, fix, or behavior change updates `CHANGELOG.md` (`[Unreleased]` section, [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format) in the same commit.

## Zero-downtime restarts

With `APERIO_REUSEPORT=1` (yaml `reuseport`) the server binds its listener with `SO_REUSEPORT`, so a second process can bind the same `host:port` while the first is still running. A rolling restart is then:

1. Start the new process (same `PORT`, `APERIO_REUSEPORT=1`). The kernel begins load-balancing new connections across both.
2. Send `SIGTERM` to the old process. It broadcasts a `ServerShutdown` to its connected clients (so they reconnect immediately instead of waiting out their backoff) and drains in-flight requests before exiting.

Tunnels re-establish on the surviving process, so visitor traffic keeps flowing across the swap. `SO_REUSEPORT` is a Unix feature (Linux/BSD/macOS); on other platforms the flag is ignored and a plain listener is used.
