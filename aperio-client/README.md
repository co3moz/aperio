# aperio-client

The tunnel client: runs inside the private network, dials out to an
`aperio-server` over WebSocket, and forwards incoming requests to local
backends. See the [root README](../README.md) for the big picture and
[docs/](../docs/) for user-facing documentation.

## Build & test

```bash
cargo build -p aperio-client            # binary: target/debug/aperio-client
cargo test -p aperio-client             # unit tests
npm --prefix ../tests/e2e test         # full end-to-end suite (needs debug binaries)
```

## Source map

| File | Purpose |
| --- | --- |
| `src/main.rs`, `src/lib.rs` | Entry point: the command line, service startup, signal handling, reload |
| `src/config.rs`, `src/config/` | Configuration from the files, the environment and the command line, merged by one rule; target normalization (`normalize_target`) |
| `src/service.rs`, `src/service/` | One connection's lifecycle: dialling, heartbeats, reconnect with backoff, dispatch, draining |
| `src/client/` | What a client runs: which services share a connection, the elastic pool, bandwidth shares |
| `src/protocol.rs` | `TunnelMessage`, the client-server wire protocol (see [Architecture](../docs/architecture.md#the-tunnel-protocol)) |
| `src/dial.rs`, `src/egress.rs` | The tunnel dial: address family, the egress proxy, the release header |
| `src/proxy.rs`, `src/proxy/` | Forwarding to the backend: HTTP/1.1, HTTP/2, Unix sockets, WebSocket pass-through, header rules, redirects, streaming |
| `src/serve.rs` | The static file server behind `serve:` |
| `src/tcp.rs`, `src/udp.rs` | Raw TCP and UDP relays for declared tunnels and `tcp_target` |
| `src/bind_tunnels.rs` | `aperio-client bind`: discovery and the local listeners |
| `src/e2e.rs` | End-to-end tunnel encryption (X25519 + ChaCha20-Poly1305) between two clients |
| `src/forward_ask.rs` | Answering a `forward` gate asked over the tunnel |
| `src/pubsub.rs`, `src/messages_*.rs` | Messages between clients: subscriptions, `run:` commands, the local HTTP and MQTT faces |
| `src/otel_bridge.rs` | The OTLP receiver that sends a client's telemetry over its tunnel |
| `src/client_id.rs` | The id and device key kept beside a config file |
| `src/check.rs`, `src/init.rs` | `aperio-client check` and `aperio-client init` |
| `src/api.rs`, `src/api/` | `aperio-client api`, `login`, `logout` and `profile` |

Unit tests live next to each module in `<module>_tests.rs`, included via
`#[cfg(test)] #[path = "..."] mod tests;`.

## Configuration

The `aperio.yaml` schema types live in [`aperio-config`](../aperio-config/),
edit them there, not here. A setting has [one name](../docs/configuration.md#one-name-three-spellings),
its yaml key: the environment variable is derived from the schema
(`server.token` is `APERIO_SERVER_TOKEN`), and the common settings have a
flag spelled after the key (`--server-token`).

Related docs: [Configuration](../docs/configuration.md),
[Traffic](../docs/traffic.md) (connections, failover, the client's own resilience),
[Tunnels](../docs/tunnels.md).
