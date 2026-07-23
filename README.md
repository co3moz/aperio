# Aperio 🛡️

Put a local service on the public internet through one outbound connection. No inbound ports, no port-forwarding, no firewall holes. Self-hosted, written in Rust.

```
        Public request                        Outbound WebSocket tunnel
[ Visitor ] ────────────▶ [ aperio-server ] ◀═══════════════════════ [ aperio-client ]
                                 │                                          │
                                 ▼                                          ▼
                        Admin dashboard /aperio                     [ Local backend ]
```

The client always dials **out**, so nothing on your network accepts inbound connections.

## Quick start

```bash
# Server (public box)
docker run -d -p 8080:8080 -v ./data:/app/data \
  -e APERIO_SERVER_TOKEN="a-long-random-string" \
  ghcr.io/co3moz/aperio-server:latest

# Client (next to your service)
docker run -d --network host \
  -e APERIO_SERVER_TOKEN="a-long-random-string" \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_TARGET="http://localhost:3000" \
  ghcr.io/co3moz/aperio-client:latest
```

Or one line with the CLI:

```bash
curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh
aperio-client 3000 --server-url https://tunnel.example.com --server-token apr_xxxx
```

Dashboard at `/aperio` (user `aperio`, password = your token). Full walkthrough: **[Getting Started](docs/getting-started.md)**.

## What it does

Click a feature for the details.

| Feature | In short |
| --- | --- |
| [Routing & load balancing](docs/routing-and-load-balancing.md) | hostname/path binds, failover tiers, sticky sessions |
| [Random subdomains](docs/routing-and-load-balancing.md) | auto `a1b2c3.example.com` on a wildcard domain |
| [Access tokens](docs/tokens-and-auth.md) | scoped, revocable, rate-limited, IP-pinned |
| [Visitor auth & SSO](docs/tokens-and-auth.md) | OIDC or a password in front of a site |
| [Share links](docs/share-links.md) | temporary visitor access, no account |
| [PR preview tunnels](docs/ephemeral-tunnels.md) | one per pull request |
| [Emergency TCP tunnels](docs/emergency-tunnels.md) | reach a DB or SSH in a pinch |
| [Failover](docs/failover.md) | survive a client dying mid-request |
| [WebSocket, streaming, gRPC](docs/tunnel-protocol.md) | pass-through, chunked bodies, h2c/h2 |
| [Response cache](docs/caching.md) | serve GETs without the tunnel |
| [Multi-tenancy](docs/organizations.md) | isolated organizations on one server |
| [Admin dashboard](docs/dashboard.md) | live traffic, inspector, replay, kill switch |
| [Observability](docs/observability.md) | Prometheus, OpenTelemetry, access log, webhooks |
| [Client resilience](docs/client-resilience.md) | reconnect, health probes, graceful drain |
| [Configuration](docs/configuration.md) | every setting: env, CLI, or yaml |

Full index: **[docs/](docs/README.md)**.

## Security

- Front it with TLS, set `trust_proxy`, use `https://` / `wss://` URLs.
- Prefer scoped dynamic tokens. Treat the master token like a root password.
- The client only talks to its configured targets and caps message sizes.

More: **[Tokens & Authentication](docs/tokens-and-auth.md)**.

## License

Open-source and free to use.
