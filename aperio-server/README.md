# aperio-server

The public-facing side: an Axum HTTP server that terminates visitor traffic
(serving HTTPS itself, or behind a TLS proxy), routes it to connected clients
over persistent WebSocket tunnels, and serves the embedded admin dashboard at
`/aperio`. See the [root README](../README.md) for the big picture and
[docs/](../docs/) for user-facing documentation.

## Build & test

```bash
cargo build -p aperio-server            # also builds + embeds the dashboard (needs Node.js)
APERIO_SKIP_DASHBOARD_BUILD=1 cargo build -p aperio-server   # reuse aperio-dashboard/dist/
cargo test -p aperio-server
npm --prefix ../tests/e2e test
```

Debug builds read `aperio-dashboard/dist/` from disk at runtime, so a
`npm run build` there is picked up without recompiling the server.

## Source map

| File | Purpose |
| --- | --- |
| `src/main.rs`, `src/cli.rs` | The binary and its subcommands (`run`, `settings`, `check`, `apply`, `schema`, `audit`, `backup`, `admin`) |
| `src/server/` | Startup, the router, background tasks, graceful shutdown, and the state built from the settings |
| `src/boot.rs`, `src/live.rs` | The boot environment and the first-start document; the settings document in force |
| `src/state.rs`, `src/state/` | `AppState`: connected clients, sessions, live stats, event emission |
| `src/routing.rs`, `src/routing/` | Hostname/path routing, load balancing, failover tiers, client IP resolution |
| `src/sites.rs`, `src/sites/` | What the server does for a hostname by itself: redirects, fixed answers, rate limits, canaries, the firewall, maintenance windows |
| `src/proxy.rs`, `src/proxy/` | Visitor request to tunnel dispatch (HTTP and WebSocket), the visitor gate, the cache path |
| `src/visitor_auth.rs`, `src/jwt.rs`, `src/forward_auth.rs` | The `access` methods a gate is compiled into |
| `src/tunnel.rs`, `src/tunnel/` | The tunnel endpoints: the upgrade and its release check, heartbeats and declarations, the TCP and UDP relays |
| `src/protocol.rs`, `src/protocol_profile.rs` | `TunnelMessage`, the wire protocol (server copy; keep in sync with the client; see [Architecture](../docs/architecture.md#the-tunnel-protocol)), and the [embedded profile](../docs/embedded-profile.md)'s classification of it |
| `src/auth.rs`, `src/auth/` | Dashboard sign-in: sessions, roles and grants, login lockout, OIDC, and the visitor `sso` flow |
| `src/tls.rs`, `src/tls/` | The built-in HTTPS listener: certificate files, and ACME over TLS-ALPN-01 |
| `src/share.rs` | Signed share links |
| `src/cache.rs` | Response cache |
| `src/access_log.rs`, `src/telemetry.rs` | Structured JSON access log; Prometheus metrics and OpenTelemetry export |
| `src/expose_manager.rs` | Public TCP and UDP listeners |
| `src/api/` | The admin REST API under `/aperio/api/` |
| `src/store/` | SQLite persistence (`<data_dir>/aperio.db`): settings, tokens, users, sessions, webhooks, statistics |

Unit tests live next to each module in `<module>_tests.rs`, included via
`#[cfg(test)] #[path = "..."] mod tests;`.

## Dashboard

The admin UI is a separate Vite + React app in
[`aperio-dashboard/`](../aperio-dashboard/), built by `build.rs` and embedded
into the binary with `rust-embed`.

Related docs: [Traffic](../docs/traffic.md),
[Access](../docs/access.md),
[Tokens and organizations](../docs/tokens-and-organizations.md),
[Observability](../docs/observability.md),
[Operations](../docs/operations.md).
