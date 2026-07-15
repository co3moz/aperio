# tests

End-to-end integration suite. It starts a real `aperio-server`, several
`aperio-client` processes, and stdlib-only Python mock backends, then
exercises the system over HTTP with curl — phase by phase, each phase with
its own server configuration.

## Layout

- `e2e.sh` — the runner: sources the harness, then each phase file in order.
- `lib/harness.sh` — shared configuration, process lifecycle (`start_server` /
  `start_client` / `stop_server`), assertion helpers (`assert_status`,
  `assert_contains`, `retry`, ...), and mock backends. Sourced, never run.
- `phases/<letter>-<name>.sh` — one file per phase, sourced after the harness.
- `mock-h2/` — the HTTP/2 (`h2c`) echo backend and client used by phase I.

| Phase | File | Covers |
| --- | --- | --- |
| A. base | `phases/a-base.sh` | health, 504, proxying, dashboard APIs, tunnels API, maintenance mode, settings, access log, metrics, inspector & replay (with the request timeline and secret redaction), webhooks + delivery log, stage stats, audit, token lifecycle, client control |
| B. auth | `phases/b-auth.sh` | visitor password: login redirect + share-link flow |
| C. failover | `phases/c-failover.sh` | retry-wait re-dispatch after a mid-request client kill |
| D. lb | `phases/d-lb.sh` | primary-standby tiers, sticky sessions |
| E. features | `phases/e-features.sh` | positional-target CLI, `check`, redirect following, multi-service client, `~/.aperio.yaml` layer, per-token rate limit |
| F. ws | `phases/f-ws.sh` | WebSocket pass-through (upgrade + frame echo + close) |
| G. tunnels | `phases/g-tunnels.sh` | emergency tunnels (`tunnels:` + `--bind-tunnels`) and the legacy tcp bridge |
| H. subdomain | `phases/h-subdomain.sh` | same-level random subdomain pattern (`*-suffix`) |
| I. h2 | `phases/i-h2.sh` | `h2c://` backend (HTTP/2 prior knowledge) with gRPC-style trailer relay, driven by the [`mock-h2`](mock-h2/) helper |
| J. sessions | `phases/j-sessions.sh` | dashboard sessions survive a server restart; active session management; usernameless passkey endpoints |
| K. cache | `phases/k-cache.sh` | response cache hits, ETag/304 conditional answers, serve-stale for resilient services during an outage |
| L. health | `phases/l-health.sh` | `target_health` probes: unhealthy reporting + routing exclusion, recovery, immediate first probe against a dead backend |
| M. multihost | `phases/m-multihost.sh` | one service claiming several hostnames |

## Running

```bash
cargo build -p aperio-server -p aperio-client   # debug binaries
bash tests/e2e.sh                # every phase
bash tests/e2e.sh cache health   # only these phases (by name)
bash tests/e2e.sh k l            # same, by phase letter
```

Requires bash, curl, and Python 3 on `PATH`. Binaries can be overridden with
`APERIO_SERVER_BIN` / `APERIO_CLIENT_BIN`. Ports 18100+ must be free.

CI runs this suite on every push/PR and merges its coverage into the
`cargo-llvm-cov` report — most tunnel/proxy runtime paths are covered here
rather than by unit tests, so **new features should add an e2e phase or
extend an existing one**. Unit tests, by contrast, live in the crates next to
their modules (`<module>_tests.rs`).

See [docs/development.md](../docs/development.md) for the full
test/coverage/release workflow.
