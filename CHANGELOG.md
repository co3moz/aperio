# Changelog

All notable changes to Aperio are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows semantic versioning per release tag.

## [Unreleased]

### Added

- **Public services**: a client can declare its service public (`--public`, yaml `public: true`, `APERIO_PUBLIC=1`) and the server skips its visitor password / OIDC gate for routes served exclusively by that client. Gated by a new per-token *may publish public services* permission (off by default; master token always may) and shown as badges in the dashboard.
- The client prints human-readable logs on interactive terminals and keeps JSON when stdout is not a TTY (Docker, pipes); `APERIO_LOG_FORMAT=json|pretty` overrides the auto-detection.
- Global `--target` client flag as an alternative to the positional target, usable with subcommands too — `aperio-client check --target 3000` now works instead of erroring with "unexpected argument".
- **Emergency tunnels**: a client declares normally unexposed local TCP services in a `tunnels:` list (a config with only tunnels is valid); a peer client running `aperio-client --bind-tunnels <client-id>` with the **same token** binds them as local 127.0.0.1 listeners (port = declared target's port, overridable per target via a `bind-tunnels:` yaml section that also supports multiple clients). Port conflicts and already-taken local ports are reported instead of bound. The declaring client only ever dials addresses from its own list; even master-token holders must name an explicit client id. Discovery endpoint: `GET /aperio/tunnels/:client_id`.
- `--client-id` client flag (yaml `client_id`, env `APERIO_CLIENT_ID`) pins the client instance id to a fixed UUID across restarts — useful for failover `wait` mode and `--bind-tunnels`. Invalid (non-UUID) values are rejected at startup. Duplicate ids are allowed but flagged: the dashboard clients table shows a `SHARED ID` badge when two live connections report the same instance id (lookups by that id are ambiguous).
- Test coverage measurement via `cargo-llvm-cov`: a CI `coverage` job puts the per-file summary into the job summary and uploads the HTML/lcov report as a `coverage-report` artifact (reported, not gated).

### Changed

- The experimental TCP tunneling feature (`tcp_target`, `aperio-client tcp <port>`, bare `GET /aperio/tcp`) is no longer documented and the `tcp` subcommand is hidden from `--help`; the API keeps working for existing setups. Emergency tunnels are the supported path for raw TCP access.

### Security

- Bumped the transitive `quinn-proto` dependency to 0.11.16, resolving RUSTSEC-2026-0037 and RUSTSEC-2026-0185 (two high-severity denial-of-service advisories).

## [0.1.1] - 2026-07-07

### Added

- **Multi-service client**: a `services:` list in `aperio.yaml` exposes several targets from one client process — one tunnel connection per entry, each with its own binds, health probe, and tuning knobs. Service names show in client logs and as a badge in the dashboard clients table.
- **Per-token limits**: dynamic tokens accept an optional rate limit (`max_rps`, token bucket) and a daily traffic quota (`daily_max_bytes`); exceeded limits answer `429`. Editable live from the dashboard and the token API.
- **User-level config**: `~/.aperio.yaml` is read as the lowest-precedence layer — keep your shared `server.url`/`server.token` there once.
- **Redirect following**: the client transparently follows same-site backend redirects (`http://x` → `https://x`, hops within the same root domain) up to `max_redirects` jumps (default 5, `0` = old pass-through behavior). Https-to-http downgrades and unrelated hosts still pass through.
- **Random subdomain patterns**: `APERIO_RANDOM_SUBDOMAIN` accepts `example.com` (≡ `*.example.com`) and same-level patterns like `*-test.example.com`, generating `<random>-test.example.com` under the parent wildcard TLS certificate.
- `aperio_request_duration_seconds` Prometheus histogram (5 ms – 30 s buckets) for p95/p99 latency dashboards.
- Size-based audit log rotation (`APERIO_AUDIT_MAX_SIZE`, default 10 MB; `APERIO_AUDIT_MAX_FILES`, default 3).
- `aperio-client check` reports which configuration layer supplied each value and probes every `services:` entry.
- Optional `service` field in the tunnel `Ping` message (backward compatible).
- CI job auditing the dependency tree against the RustSec advisory database (`cargo audit`).

### Changed

- **BREAKING (CLI)**: `aperio-client http <port>` and `run` subcommands are gone — use a positional target (`aperio-client 3000`, `aperio-client example.com`) or plain `aperio-client` with config/env. `tcp` and `check` remain. Old option spellings (`--server`, `--token`, `--host`, `--concurrency`) still work as aliases of the canonical `--server-url`, `--server-token`, `--hostname`, `--max-concurrent`.
- **Configuration layering** is now `CLI > ./aperio.yaml > environment > ~/.aperio.yaml` (the local file previously ranked below the environment).
- Naming is mechanical across surfaces: yaml `server.url`/`server.token` (legacy flat `server:`/`token:` still accepted), env `APERIO_TARGET`, `APERIO_HOSTNAME`, `APERIO_TIMEOUT`, … (legacy `APERIO_CLIENT_*`, `APERIO_HOSTNAME_BIND`, `APERIO_PATH_BIND` remain as aliases).
- Config hot-reload restarts the service(s) with the fully re-resolved configuration — every setting applies now, not just token/server/target/binds/priority.
- Dashboard settings apply to connected clients immediately: changing the random-subdomain pattern re-issues client hostnames on the spot; enabling tunnel compression is offered to connected clients right away.
- CLI parsing moved to clap (proper `--help`, errors, completions groundwork).
- Both crates reorganized into folder-based module hierarchies (`store/`, `api/`, `proxy/`, `tunnel/` on the server; `proxy/` on the client).

### Fixed

- Ephemeral tunnel deletion now emits the `tunnel_deleted` webhook event (was `token_revoked`).
- `client_connected`/`client_disconnected` audit entries and webhooks record the resolved client IP (trust-proxy aware) instead of the raw socket address with port.
- `*-test.example.com` random-subdomain patterns no longer produce the broken `<random>.*-test.example.com` form.

## [0.1.0]

Initial release: HTTP reverse tunneling over a single outbound WebSocket, hostname/path routing with round-robin, primary-standby and sticky strategies, in-flight failover, dynamic scoped tokens, ephemeral tunnels + GitHub Action, share links, OIDC/SSO and visitor password protection, admin dashboard with request inspector/replay and live settings, Prometheus metrics, structured access log, persistent statistics, audit log, webhooks, tunnel compression, chunked body streaming (protocol v2 binary frames), experimental TCP tunneling.
