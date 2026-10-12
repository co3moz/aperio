# <img src="docs/assets/aperio-mark.svg" alt="" height="30"> Aperio

Expose a local service through one outbound connection. The client needs no inbound port. Aperio is self-hosted and written in Rust.

```
        Public request                        Outbound WebSocket tunnel
[ Visitor ] ────────────▶ [ aperio-server ] ◀═══════════════════════ [ aperio-client ]
                                 │                                          │
                                 ▼                                          ▼
                        Admin dashboard /aperio                     [ Local backend ]
```

- **It is yours.** Two binaries you run: no account, no third-party relay, nothing priced per tunnel.
- **Nothing else to run.** The server keeps its state in a SQLite file beside it and carries its dashboard inside the binary.
- **One connection out.** The client dials the server, so the machine serving your app can sit behind NAT or a firewall that allows nothing inbound.
- **Small.** Measured on an Apple M-series machine, the server binary is about 14 MB and the client's 6 MB, each using about as much memory at idle.

[![The Aperio admin dashboard](docs/images/dashboard-overview.png)](docs/operations.md)

## Quick start

```bash
# Server, on a public machine
docker run -d -p 8080:8080 -v "$(pwd)/data:/app/data" \
  -e APERIO_SERVER_TOKEN="a-long-random-string" \
  ghcr.io/co3moz/aperio-server:latest

# Client, next to a service on port 3000 (Linux host networking)
docker run -d --network host \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_SERVER_TOKEN="a-long-random-string" \
  -e APERIO_TARGET="http://localhost:3000" \
  -e APERIO_ACCESS=public \
  ghcr.io/co3moz/aperio-client:latest
```

Open `http://your-server-ip:8080`. The dashboard is at `/aperio` (user `aperio`, password: the token). Routes are closed by default, which is why the client says `APERIO_ACCESS=public`.

Without Docker, `curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh`, `brew install --formula https://github.com/co3moz/aperio/releases/latest/download/aperio-client.rb`, or the `.deb`/`.rpm` on every release:

```bash
aperio-client 3000 --server-url https://tunnel.example.com --server-token a-long-random-string --access public
```

[Getting Started](docs/getting-started.md) walks through it.

## What it does

| | |
| --- | --- |
| **Carries** | HTTP/1.1 and HTTP/2, WebSockets, gRPC, static files with no backend, raw TCP and UDP, messages between clients |
| **Routes** | hostname and path binds, round-robin, primary and standby tiers, sticky sessions, canary splits, random subdomains, in-flight failover |
| **Protects** | a visitor gate (passwords, bearer keys, JWT, SSO, your own endpoint), share links, scoped tokens, organizations, passkeys and TOTP for operators |
| **Operates** | live dashboard, request inspector with replay, maintenance mode, response cache, autoscaling hooks, Prometheus, OpenTelemetry, a tamper-evident audit log, webhooks |

Use it to show work in progress on a real URL, give every pull request a preview, publish from behind NAT, reach a private database in an incident, or run a tunnel service for other teams.

## Documentation

- [Getting Started](docs/getting-started.md): a server and a client in five minutes.
- [Configuration](docs/configuration.md): how the client's file and the server's settings work.
- [Reference](docs/reference.md): every setting and every endpoint.
- [Access](docs/access.md): who may visit a site.
- [Tokens and Organizations](docs/tokens-and-organizations.md): who may connect and administer, and tenancy.
- [Traffic](docs/traffic.md): routing, connections, failover, caching and limits.
- [Tunnels](docs/tunnels.md): TCP and UDP tunnels, public listeners, preview environments.
- [Integrations](docs/integrations.md): messages between clients, autoscaling, edge proxies.
- [Operations](docs/operations.md): the dashboard, the admin API and CLI, the server's data.
- [Observability](docs/observability.md): metrics, traces, alerts, logs, audit, webhooks.
- [Deployment](docs/deployment.md): packages, Kubernetes, going live, upgrading.
- [Architecture](docs/architecture.md): the protocol, a request's path, and the trust boundaries.
- [Examples](docs/examples/README.md): runnable configurations, one folder per scenario.

## Security

Put TLS in front of the server, give clients scoped tokens, and treat the master token like a root password. Releases are signed with [Sigstore](https://www.sigstore.dev/) and carry build provenance and an SBOM; see [SECURITY.md](SECURITY.md#verifying-a-release). Report a vulnerability [privately](https://github.com/co3moz/aperio/security/advisories/new), never as a public issue.

## Contributing

Bug reports, reproductions and documentation fixes are as welcome as code. See [CONTRIBUTING.md](CONTRIBUTING.md); ideas live in [GitHub Issues](https://github.com/co3moz/aperio/issues).

## License

Open-source and free to use.
