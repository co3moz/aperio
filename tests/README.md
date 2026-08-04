# tests

End-to-end integration suite. It starts a real `aperio-server`, several
`aperio-client` processes, and mock backends, then exercises the system over
HTTP from the outside, phase by phase, each phase with its own server on its
own port.

## Layout

- `e2e/`, the suite. See [its README](e2e/README.md) for how it is put
  together and the two things that will bite you.
- `mock-h2/`, the HTTP/2 (`h2c`) echo backend and client used by the h2 phase.
  Built on demand; it is not a workspace default.
- `soak.js`, a k6 load profile. Not run in CI, and not part of the suite.

## Phases

Each is a directory under `e2e/specs/`, and each runs on its own, as does each file within one.

| Phase | Covers |
| --- | --- |
| `base` | health, 504, proxying, dashboard APIs, tunnels API, maintenance mode, settings, access log, metrics, inspector & replay (with the request timeline and secret redaction), webhooks + delivery log, organizations and their isolation, activity rings, export, audit, roles, TOTP, token lifecycle, client control |
| `auth` | visitor password: login redirect, share-link flow, public opt-out, token rotation |
| `failover` | retry-wait re-dispatch after a mid-request client kill |
| `lb` | primary-standby tiers, sticky sessions |
| `features` | positional-target CLI, `check`, redirect following, request-id correlation, multi-service client, webhook inbox, unix-socket target, `~/.aperio.yaml` layer, per-token rate limit, connection ceiling, visitor allowlist |
| `ws` | WebSocket pass-through (upgrade, frame echo, close) and a backend that speaks first |
| `tunnels` | emergency tunnels (`tunnels:` + `--bind-tunnels`) and the legacy tcp bridge |
| `subdomain` | same-level random subdomain pattern (`*-suffix`), passkey surface |
| `h2` | `h2c://` backend (HTTP/2 prior knowledge) with gRPC-style trailer relay and gRPC health checking, driven by the [`mock-h2`](mock-h2/) helper |
| `sessions` | dashboard sessions survive a server restart; active session management; usernameless passkey endpoints |
| `cache` | cache hits, ETag/304, serve-stale during an outage, single-flight coalescing, stale-while-revalidate, ranged reads, purge |
| `health` | `target_health` probes: unhealthy reporting and routing exclusion, recovery, immediate first probe, `wait_for_backend` |
| `multihost` | one service claiming several hostnames; per-service static `serve:` |
| `config` | `aperio-server.yaml` hot-reload, `denied_ips`, per-hostname error pages, `--print-schema` / `--print-config` / `--check-config`, tunnel compression |
| `api-cli` | the `aperio-client api …` admin commands |
| `scaling` | cold start from zero, single-flight scaling calls, the SSRF fence |
| `messages` | publish/subscribe between clients, the local HTTP and MQTT faces, QoS 1, subscription commands, messaging metrics |

## Running

```bash
cargo build -p aperio-server -p aperio-client   # debug binaries
npm --prefix tests/e2e ci                       # once
npm --prefix tests/e2e test                     # every phase, four at a time
npm --prefix tests/e2e run test:serial          # one at a time, clearer log
```

One phase on its own:

```bash
cd tests/e2e && npx nole './specs/cache/**/*.test.ts'
```

Requires Node 22+. Binaries can be overridden with `APERIO_SERVER_BIN` /
`APERIO_CLIENT_BIN`. No port is pinned, so none has to be free.

CI runs this suite on every push/PR and merges its coverage into the
`cargo-llvm-cov` report, most tunnel/proxy runtime paths are covered here
rather than by unit tests, so **new features should add a phase or extend an
existing one**. Unit tests, by contrast, live in the crates next to their
modules (`<module>_tests.rs`).

See [docs/development.md](../docs/development.md) for the full
test/coverage/release workflow.
