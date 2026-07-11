# aperio-server

The public-facing side: an Axum HTTP server that terminates visitor traffic
(usually behind a TLS proxy), routes it to connected clients over persistent
WebSocket tunnels, and serves the embedded admin dashboard at `/aperio`. See
the [root README](../README.md) for the big picture and [docs/](../docs/) for
user-facing documentation.

## Build & test

```bash
cargo build -p aperio-server            # also builds + embeds the dashboard (needs Node.js)
APERIO_SKIP_DASHBOARD_BUILD=1 cargo build -p aperio-server   # reuse aperio-dashboard/dist/
cargo test -p aperio-server
bash ../tests/e2e.sh
```

Debug builds read `aperio-dashboard/dist/` from disk at runtime, so a
`npm run build` there is picked up without recompiling the server.

## Source map

| File | Purpose |
| --- | --- |
| `src/main.rs` | Entry point: router assembly, listeners, graceful shutdown |
| `src/state.rs` | `AppState`: connected clients, sessions, live stats, event emission |
| `src/routing.rs` | Hostname/path routing, load balancing, failover tiers, client IP resolution |
| `src/proxy.rs`, `src/proxy/ws.rs` | Visitor request → tunnel dispatch (HTTP + WebSocket pass-through) |
| `src/tunnel/ws.rs` | Client tunnel WebSocket endpoint: heartbeats, protocol negotiation, health verdicts |
| `src/tunnel/tcp.rs` | Emergency TCP tunnels and the UDP relay endpoint |
| `src/protocol.rs` | `TunnelMessage` wire protocol (server copy — keep in sync with the client; see [docs/tunnel-protocol.md](../docs/tunnel-protocol.md)) |
| `src/auth.rs` | Dashboard/visitor auth: sessions, login lockout, IP/CIDR helpers |
| `src/oidc.rs` | OIDC / SSO protection for proxied traffic and dashboard login |
| `src/share.rs` | Signed share links |
| `src/settings.rs` | Live-editable server settings with env-var baselines |
| `src/cache.rs` | Response cache |
| `src/access_log.rs` | Structured JSON access log |
| `src/telemetry.rs` | Prometheus metrics + optional OpenTelemetry OTLP export |
| `src/api/` | Dashboard REST API: `clients`, `tunnels`, `tokens`, `users`, `webhooks`, `metrics`, `settings`, `inspector`, `maintenance`, `openapi` |
| `src/store/` | SQLite persistence (`<data_dir>/aperio.db`): `stats`, `tokens`, `users`, `webhooks`, `audit` |

Unit tests live next to each module in `<module>_tests.rs`, included via
`#[cfg(test)] #[path = "..."] mod tests;`.

## Dashboard

The admin UI is a separate Vite + React app in
[`aperio-dashboard/`](../aperio-dashboard/), built by `build.rs` and embedded
into the binary with `rust-embed`.

Related docs: [routing & load balancing](../docs/routing-and-load-balancing.md),
[tokens & auth](../docs/tokens-and-auth.md),
[share links](../docs/share-links.md),
[observability](../docs/observability.md),
[dashboard](../docs/dashboard.md).
