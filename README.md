# <img src="docs/assets/aperio-mark.svg" alt="" height="30"> Aperio

Put a local service on the public internet through one outbound connection. No inbound ports, no port-forwarding, no firewall holes. Self-hosted, written in Rust.

```
        Public request                        Outbound WebSocket tunnel
[ Visitor ] ────────────▶ [ aperio-server ] ◀═══════════════════════ [ aperio-client ]
                                 │                                          │
                                 ▼                                          ▼
                        Admin dashboard /aperio                     [ Local backend ]
```

The client always dials **out**, so nothing on your network accepts inbound connections.

[![The Aperio admin dashboard](docs/images/dashboard-overview.png)](docs/dashboard.md)

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

**1. Getting started & configuration**

- [Getting Started](docs/getting-started.md), server and client running in five minutes, Docker or CLI.
- [Configuration](docs/configuration.md), every setting on both sides: env, CLI, or yaml.
- [Client Resilience](docs/client-resilience.md), reconnect backoff, health probes, graceful drain.
- [Behind a Dynamic Edge Proxy](docs/edge-proxy.md), let Traefik or Caddy learn the hostnames that only exist at runtime.

**2. Traffic & routing**

- [Routing & Load Balancing](docs/routing-and-load-balancing.md), hostname/path binds, priority tiers, sticky sessions.
- [Random Subdomains](docs/routing-and-load-balancing.md#random-subdomains), an automatic `a1b2c3.example.com` on a wildcard domain.
- [In-Flight Failover](docs/failover.md), survive a client dying mid-request without the visitor noticing.
- [Response Caching](docs/caching.md), serve GETs without touching the tunnel.

**3. Tunnels & protocols**

- [Tunnel Protocol](docs/tunnel-protocol.md), WebSocket pass-through, chunked bodies, gRPC over h2c/h2.
- [Emergency TCP Tunnels](docs/emergency-tunnels.md), reach a database or SSH through the same tunnel.
- [PR Preview Tunnels](docs/ephemeral-tunnels.md), one ephemeral hostname per pull request, with a GitHub Action.

**4. Security & access control**

- [Visitor Auth & SSO](docs/tokens-and-auth.md#protecting-proxied-traffic), OIDC or a password in front of a proxied site.
- [Access Tokens](docs/tokens-and-auth.md#dynamic-tokens), scoped, revocable, rate-limited, IP-pinned.
- [Share Links](docs/share-links.md), temporary visitor access with no account.

**5. Management & operations**

- [Admin Dashboard](docs/dashboard.md), live traffic, request inspector and replay, kill switch.
- [Admin API from the CLI](docs/cli-api.md), script it all: `aperio-client api share | token | ...`.
- [Autoscaling](docs/autoscaling.md), cold start from zero on the first request, scale out when the pool saturates.
- [Multi-tenancy](docs/organizations.md), isolated organizations on one server.

**6. Observability**

- [Metrics, traces & logs](docs/observability.md), Prometheus, OpenTelemetry, webhooks, access and audit logs.

Full index: **[docs/](docs/README.md)**.

## Security

- Front it with TLS, set `trust_proxy`, use `https://` / `wss://` URLs.
- Prefer scoped dynamic tokens. Treat the master token like a root password.
- The client only talks to its configured targets and caps message sizes.

More: **[Tokens & Authentication](docs/tokens-and-auth.md)**.

## License

Open-source and free to use.
