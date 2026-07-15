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

## Tests & end-to-end suite

`cargo test --all` runs the unit tests. `bash tests/e2e.sh` runs the end-to-end suite — a real `aperio-server`, several `aperio-client` processes, and stdlib-only Python mock backends, exercised phase by phase (proxying, dashboard APIs, auth, failover, load balancing, WebSocket pass-through, emergency tunnels, ...). CI runs both on every push and pull request, plus `cargo clippy -D warnings`, `cargo fmt --check`, and a `cargo audit` scan of the dependency tree.

## Test coverage

Coverage is measured with [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) (`cargo install cargo-llvm-cov` + `rustup component add llvm-tools-preview`):

```bash
cargo llvm-cov --workspace          # per-file summary table
cargo llvm-cov --workspace --open   # line-by-line HTML report in the browser
```

CI goes further and merges the E2E integration run into the same report (instrumented binaries driven by `tests/e2e.sh`), publishing it as a `coverage-report` artifact on every run — that merged number is the real one, since the tunnel/proxy runtime paths are mostly exercised end-to-end rather than by unit tests. Note that the e2e merge relies on graceful SIGTERM handling to flush profile data, so it only works on Unix (CI/WSL); a local Windows run reports the unit-test-only number.

## Releases

Tagging a version (`git tag v0.2.0 && git push --tags`) triggers the release workflow: static binaries for Linux (x86_64/aarch64, musl), macOS (Intel/Apple Silicon), and Windows are built, checksummed, and attached to a GitHub Release — [install.sh](../install.sh) always picks up the latest. `aperio-client --version` / `aperio-server --version` print the installed version. The versioned `aperio.yaml` and `aperio-server.yaml` JSON Schemas (`aperio-client.<tag>.json`, `aperio-server.<tag>.json`) are attached to the release too.

The same tag also builds the multi-arch (amd64+arm64) Docker images `ghcr.io/co3moz/aperio-server` and `ghcr.io/co3moz/aperio-client`, tagged with the version (`v0.2.0`, `0.2.0`, `0.2`, `0`); `latest` tracks the most recent **stable** release (a pre-release tag such as `v1.0.0-rc1` publishes only its exact tag). Docker images are not built on ordinary pushes — the CI workflow (`ci.yml`) only builds, lints, tests, audits, and runs the e2e tunnel test.

## Conventions

- Configuration naming follows the [one-name-three-surfaces standard](configuration.md#the-standard-one-name-three-surfaces) — CLI `--kebab-case` ↔ yaml `snake_case` ↔ env `APERIO_SNAKE_CASE`. Never rename across surfaces; keep legacy spellings as aliases.
- Unit tests live next to the module in a `<module>_tests.rs` file, included with `#[cfg(test)] #[path = "..."] mod tests;`.
- Every feature, fix, or behavior change updates `CHANGELOG.md` (`[Unreleased]` section, [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format) in the same commit.
