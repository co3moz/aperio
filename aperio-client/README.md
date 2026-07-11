# aperio-client

The tunnel client: runs inside the private network, dials out to an
`aperio-server` over WebSocket, and forwards incoming requests to local
backends. See the [root README](../README.md) for the big picture and
[docs/](../docs/) for user-facing documentation.

## Build & test

```bash
cargo build -p aperio-client            # binary: target/debug/aperio-client
cargo test -p aperio-client             # unit tests
bash ../tests/e2e.sh                    # full end-to-end suite (needs debug binaries)
```

## Source map

| File | Purpose |
| --- | --- |
| `src/main.rs` | Entry point: CLI parsing, service startup, signal handling |
| `src/config.rs` | Configuration from CLI / env / `aperio.yaml` (layered); target normalization (`normalize_target`) |
| `src/service.rs` | Per-service lifecycle: connect, heartbeats, reconnect with backoff, backend health probes |
| `src/protocol.rs` | `TunnelMessage` — the client↔server wire protocol (see [docs/tunnel-protocol.md](../docs/tunnel-protocol.md)) |
| `src/proxy.rs`, `src/proxy/http.rs` | HTTP request forwarding to the local backend (reqwest), header rules, redirects, body streaming |
| `src/proxy/ws.rs` | WebSocket upgrade pass-through to the backend |
| `src/tcp.rs` | Raw TCP tunneling: `tcp_target` bridge and emergency-tunnel data path |
| `src/udp.rs` | UDP datagram relay for `protocol: udp` tunnels (idle timeout, session tracking) |
| `src/bind_tunnels.rs` | Consumer side of emergency tunnels (`--bind-tunnels`): discovery + local listeners |
| `src/e2e.rs` | End-to-end tunnel encryption (X25519 + ChaCha20-Poly1305) between two clients |
| `src/check.rs` | `aperio-client check`: config validation and connectivity diagnostics |

Unit tests live next to each module in `<module>_tests.rs`, included via
`#[cfg(test)] #[path = "..."] mod tests;`.

## Configuration

The `aperio.yaml` schema types live in [`aperio-config`](../aperio-config/) —
edit them there, not here. Every option follows the
[one-name-three-surfaces standard](../docs/configuration.md#the-standard-one-name-three-surfaces):
CLI `--kebab-case` ↔ yaml `snake_case` ↔ env `APERIO_SNAKE_CASE`.

Related docs: [configuration](../docs/configuration.md),
[client resilience](../docs/client-resilience.md),
[emergency tunnels](../docs/emergency-tunnels.md),
[failover](../docs/failover.md).
