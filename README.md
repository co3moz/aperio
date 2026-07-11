# Aperio 🛡️

Aperio is a secure, self-hosted reverse tunneling system written in Rust. It exposes HTTP services running behind NATs, firewalls, or private networks to the public internet — through a single outbound WebSocket connection, with no inbound ports opened on your network.

It ships with multi-tenant routing, scoped access tokens, SSO protection, and a built-in admin dashboard.

**Highlights**

- Hostname- and path-based routing; round-robin or primary-standby (failover tier) load balancing
- Automatic random subdomains (`a1b2c3.example.com`) under a wildcard domain
- Scoped, revocable API tokens with hostname/path/IP restrictions and TTLs
- Ephemeral tunnels via API + GitHub Action — per-PR preview environments in one step
- Signed share links: temporary, scoped visitor access to protected sites without accounts
- OIDC / SSO protection for proxied traffic (Cloudflare Access style)
- WebSocket & Socket.io pass-through, chunked streaming for large bodies, optional zlib tunnel compression
- gRPC / HTTP/2 backends via `h2c://` and `h2://` targets, with end-to-end trailer relay (`grpc-status`)
- Emergency tunnels: reach normally unexposed TCP services (a database, SSH) in a pinch
- Admin dashboard: live traffic, request inspector & replay, client kill switch, maintenance mode, add-client wizard, audit log, webhooks
- Prometheus metrics, structured JSON access log, persistent statistics, backend health probing, graceful drain
- Single static binary per side: one-line installer, prebuilt releases, official multi-arch Docker images

## How It Works

Aperio has two components:

- **`aperio-server`** — the public-facing side. It terminates public HTTP(S) traffic (usually behind a TLS-terminating proxy such as Traefik, Caddy, or nginx) and forwards requests over persistent WebSocket tunnels to connected clients.
- **`aperio-client`** — runs inside your private network. It dials out to the server, keeps the tunnel alive with heartbeats, and forwards incoming requests to your local backend.

```
        Public request                        Outbound WebSocket tunnel
[ Visitor ] ────────────▶ [ aperio-server ] ◀═══════════════════════ [ aperio-client ]
                                 │                                          │
                                 ▼                                          ▼
                        Admin dashboard /aperio                     [ Local backend ]
```

Because the client always dials *out*, nothing on your private network needs to accept inbound connections.

## Quick Start

### With Docker

```bash
# 1. Start the server (public side)
docker run -d --name aperio-server \
  -p 8080:8080 \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -v ./data:/app/data \
  ghcr.io/co3moz/aperio-server:latest

# 2. Start a client next to the service you want to expose
docker run -d --name aperio-client \
  --network host \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_TARGET="http://localhost:3000" \
  ghcr.io/co3moz/aperio-client:latest

# 3. Open http://your-server-ip:8080 — requests are proxied to localhost:3000
#    Dashboard: http://your-server-ip:8080/aperio  (user: aperio, password: your token)
```

A commented Docker Compose setup lives in [docker-compose.yml.example](docker-compose.yml.example).

### With the CLI

Install a prebuilt binary (Linux and macOS; Windows zips are on the [Releases page](https://github.com/co3moz/aperio/releases)):

```bash
curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh
```

```bash
# Expose local port 3000 in one line
aperio-client 3000 --server-url https://tunnel.example.com --server-token apr_xxxxxxxx

# Claim a specific hostname while doing it
aperio-client 3000 --server-url https://tunnel.example.com --server-token apr_xxxxxxxx --hostname app.example.com
```

## Documentation

Everything else lives in [docs/](docs/README.md) as short, focused articles:

| | |
| --- | --- |
| [Getting Started](docs/getting-started.md) | Step-by-step first tunnel, with Docker or the CLI. |
| [Configuration Reference](docs/configuration.md) | **Every setting on both sides** — the env/CLI/yaml naming standard, precedence, full tables, HTTP endpoints. |
| [Routing & Load Balancing](docs/routing-and-load-balancing.md) | Hostname/path binds, strategies, random subdomains, overrules. |
| [In-Flight Failover](docs/failover.md) | Surviving a client death mid-request. |
| [Tokens & Authentication](docs/tokens-and-auth.md) | Master/dynamic tokens, visitor password, OIDC/SSO, hardening advice. |
| [Share Links](docs/share-links.md) | Temporary visitor access without accounts. |
| [Ephemeral Tunnels](docs/ephemeral-tunnels.md) | Per-PR preview environments via the API and the GitHub Action. |
| [Emergency Tunnels](docs/emergency-tunnels.md) | Reaching unexposed TCP services with `--bind-tunnels`. |
| [The Dashboard](docs/dashboard.md) | Live traffic, inspector & replay, kill switch, maintenance, live settings. |
| [Observability](docs/observability.md) | Prometheus metrics, access log, audit trail, webhooks, statistics. |
| [Client Resilience](docs/client-resilience.md) | Backoff, health probing, hot-reload, graceful drain. |
| [Tunnel Protocol & Advanced Features](docs/tunnel-protocol.md) | WS pass-through, chunked streaming, binary frames, compression. |
| [Development & Releases](docs/development.md) | Building from source, tests & coverage, release process, conventions. |

## Security Notes

- Always front the server with TLS (Traefik/Caddy/nginx) and set `APERIO_TRUST_PROXY=1` behind it; clients should use `https://`/`wss://` URLs so tokens never travel in plaintext.
- Prefer **dynamic tokens** over sharing the master token: scope them to a hostname, pin them to source IPs, give them a TTL. Treat the master token as root.
- The client deliberately does not fully trust the server: it only connects to its configured targets (SSRF guards), caps tunnel message sizes, bounds decompression output, and enforces its own concurrency limit.

More in [docs/tokens-and-auth.md](docs/tokens-and-auth.md).

## License

This project is open-source and free to use.
