# tests

End-to-end integration suite. `e2e.sh` starts a real `aperio-server`,
several `aperio-client` processes, and stdlib-only Python mock backends, then
exercises the system over HTTP with curl — phase by phase, each phase with
its own server configuration:

| Phase | Covers |
| --- | --- |
| A. base | health, 504, proxying, dashboard APIs, tunnels API, maintenance mode, settings, access log, metrics, inspector & replay, webhooks, audit, token lifecycle, client control |
| B. auth | visitor password: login redirect + share-link flow |
| C. failover | retry-wait re-dispatch after a mid-request client kill |
| D. lb | primary-standby tiers, sticky sessions |
| E. features | positional-target CLI, `check`, redirect following, multi-service client, `~/.aperio.yaml` layer, per-token rate limit |
| F. ws | WebSocket pass-through (upgrade + frame echo + close) |
| G. tunnels | emergency tunnels (`tunnels:` + `--bind-tunnels`) and the legacy tcp bridge |
| H. subdomain | same-level random subdomain pattern (`*-suffix`) |

## Running

```bash
cargo build -p aperio-server -p aperio-client   # debug binaries
bash tests/e2e.sh
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
