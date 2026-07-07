# Aperio 🛡️

Aperio is a secure, self-hosted reverse tunneling system written in Rust. It exposes HTTP services (and experimentally, raw TCP services) running behind NATs, firewalls, or private networks to the public internet — through a single outbound WebSocket connection, with no inbound ports opened on your network.

It ships with multi-tenant routing, scoped access tokens, SSO protection, and a built-in admin dashboard.

**Highlights**

- Hostname- and path-based routing; round-robin or primary-standby (failover tier) load balancing
- Automatic random subdomains (`a1b2c3.example.com`) under a wildcard domain
- Scoped, revocable API tokens with hostname/path/IP restrictions and TTLs
- Ephemeral tunnels via API + GitHub Action — per-PR preview environments in one step
- Signed share links: temporary, scoped visitor access to protected sites without accounts
- OIDC / SSO protection for proxied traffic (Cloudflare Access style)
- WebSocket & Socket.io pass-through, chunked streaming for large bodies, optional zlib tunnel compression
- Admin dashboard: live traffic, request inspector & replay, client kill switch, maintenance mode, add-client wizard, audit log, webhooks
- Prometheus metrics, structured JSON access log, persistent statistics, backend health probing, graceful drain
- Single static binary per side: one-line installer, prebuilt releases, official multi-arch Docker images

Feature-by-feature articles live in [docs/](docs/README.md); this README is the single-page overview and configuration reference.

---

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

---

## Quick Start

> 📖 Step-by-step walkthrough: [docs/getting-started.md](docs/getting-started.md)

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
  -e APERIO_CLIENT_TARGET="http://localhost:3000" \
  ghcr.io/co3moz/aperio-client:latest

# 3. Open http://your-server-ip:8080 — requests are proxied to localhost:3000
#    Dashboard: http://your-server-ip:8080/aperio  (user: aperio, password: your token)
```

### With the CLI

Install a prebuilt binary (Linux and macOS; Windows zips are on the [Releases page](https://github.com/co3moz/aperio/releases)):

```bash
curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh
# server binary instead: APERIO_BIN=aperio-server curl ... | sh
# pin a version:         APERIO_VERSION=v0.2.0    curl ... | sh
```

```bash
# Expose local port 3000 in one line
aperio-client http 3000 --server https://tunnel.example.com --token apr_xxxxxxxx

# Claim a specific hostname while doing it
aperio-client http 3000 --server https://tunnel.example.com --token apr_xxxxxxxx --host app.example.com
```

### With Docker Compose

```yaml
services:
  server:
    image: ghcr.io/co3moz/aperio-server:latest
    ports:
      - "8080:8080"
    environment:
      - APERIO_SERVER_TOKEN=change-me
      - APERIO_DATA_DIR=/app/data
    volumes:
      # Persist dynamic tokens, stats, audit log and webhooks across restarts.
      - ./data:/app/data
    restart: unless-stopped

  client:
    image: ghcr.io/co3moz/aperio-client:latest
    environment:
      - APERIO_SERVER_TOKEN=change-me
      - APERIO_SERVER_URL=http://server:8080
      - APERIO_CLIENT_TARGET=http://host.docker.internal:3000
    extra_hosts:
      - "host.docker.internal:host-gateway"
    depends_on:
      - server
    restart: unless-stopped
```

See [docker-compose.yml.example](docker-compose.yml.example) for a commented version.

### Releases

Tagging a version (`git tag v0.2.0 && git push --tags`) triggers the release workflow: static binaries for Linux (x86_64/aarch64, musl), macOS (Intel/Apple Silicon), and Windows are built, checksummed, and attached to a GitHub Release — [install.sh](install.sh) always picks up the latest. `aperio-client --version` / `aperio-server --version` print the installed version.

### Building from Source

Requires the Rust toolchain (2024 edition, 1.85+). Building `aperio-server` additionally requires Node.js (with npm): the admin dashboard is a Vite + React app in [`aperio-dashboard/`](aperio-dashboard/) that is built automatically by `build.rs` and embedded into the server binary.

```bash
cargo build --release -p aperio-server -p aperio-client
# binaries: target/release/aperio-server, target/release/aperio-client
```

To skip the frontend build (reusing an existing `aperio-dashboard/dist/`), set `APERIO_SKIP_DASHBOARD_BUILD=1`. For dashboard development, `npm run dev` in `aperio-dashboard/` serves the UI with hot reload and proxies API calls to a local server on port 8080; debug builds of the server read `dist/` from disk at runtime, so a `npm run build` is picked up without recompiling.

---

## Server Guide

The server is configured through environment variables; most settings can also be edited live from the dashboard's *Server Settings* section, where they become persisted overrides on top of the env defaults (see [Dashboard](#dashboard)).

### Core Settings

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_TOKEN` | **Required.** Master token: authenticates tunnel clients and doubles as the dashboard admin password (`aperio:<token>`). | — |
| `HOST` | Bind address. | `0.0.0.0` |
| `PORT` | Listen port. | `8080` |
| `APERIO_DATA_DIR` | Directory for persisted state (tokens, stats, audit log, webhooks). **Mount a volume here in Docker.** | `./data` |
| `LOG_LEVEL` | `error`, `warn`, `info`, `debug`, `trace`. | `info` |

### Routing & Load Balancing

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_REQUIRE_HOSTNAME_BIND` | `1` = clients without a hostname bind never receive traffic (strict multi-tenant mode). | `0` |
| `APERIO_RANDOM_SUBDOMAIN` | Pattern with a `*` placeholder in the leftmost label — every connecting client gets the pattern with `*` replaced by a random label, in addition to its other binds. `example.com` ≡ `*.example.com`; `*-test.example.com` yields `<random>-test.example.com` (stays on the same subdomain level, so one wildcard TLS cert covers it). | — |
| `APERIO_CLIENT_DOWN_THRESHOLD` | Seconds without a heartbeat before a client is dropped from the routing pool (it rejoins on the next ping). | `15` |
| `APERIO_LB_STRATEGY` | Load-balancing strategy: `round-robin`, `primary-standby` (client priority tiers), or `sticky` (visitor affinity via cookie). See [Routing](#routing). | `round-robin` |
| `APERIO_FAILOVER` | What to do when a client dies mid-request: `fail`, `retry`, `wait`, or `retry-wait`. See [In-Flight Failover](#in-flight-failover). | `fail` |
| `APERIO_FAILOVER_MAX_JUMPS` | Max re-dispatch attempts per request. | `2` |
| `APERIO_FAILOVER_WINDOW` | Total seconds the `wait`/`retry-wait` modes may spend waiting for a candidate, across all jumps. | `15` |
| `APERIO_FAILOVER_ALL_METHODS` | `1` = also fail over non-idempotent methods (POST/PATCH). Off by default because a re-dispatched request may reach a backend twice. | `0` |

### Limits & Protection

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_MAX_BODY_SIZE` | Max request body size in bytes. | `10485760` (10 MB) |
| `APERIO_MAX_CONCURRENT_REQUESTS` | Max in-flight proxied requests across all tunnels. | `100` |
| `APERIO_MAX_TUNNELS` | Max simultaneously connected tunnel clients. | `10` |
| `APERIO_IP_LIMIT_MAX` | Per-IP token bucket burst capacity. | `100` |
| `APERIO_IP_LIMIT_REFILL` | Per-IP refill rate (requests/second). | `5` |
| `APERIO_SERVER_GATEWAY_TIMEOUT` | Seconds to wait for a client to (re)connect before failing a request. | `10` |
| `APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT` | Seconds to wait for a client to answer a dispatched request. | `30` |
| `APERIO_TRUST_PROXY` | `1` = trust `X-Forwarded-For` / `X-Real-IP` for client IPs. Enable **only** behind a trusted reverse proxy. | `0` |
| `APERIO_REAL_IP_HEADER` | Header consulted **before** `X-Forwarded-For` for the real client IP (with `APERIO_TRUST_PROXY=1`). Needed behind CDN→proxy chains where the proxy resets XFF to the CDN edge — e.g. set `CF-Connecting-IP` behind Cloudflare, or configure the proxy's `forwardedHeaders.trustedIPs` instead. | — |
| `APERIO_SECURE_COOKIES` | `1` = set the `Secure` flag on session cookies. Defaults to the `APERIO_TRUST_PROXY` value. | — |
| `APERIO_TUNNEL_COMPRESSION` | `1` = offer per-message zlib compression to clients (enabled per connection once acknowledged; old clients keep plain frames). | `0` |
| `APERIO_504_PAGE` | Path to an HTML file served on 504 gateway-timeout responses instead of the plain-text default. | — |
| `APERIO_503_PAGE` | Path to an HTML file served while a hostname is in maintenance mode instead of the plain-text default. | — |
| `APERIO_ACCESS_LOG` | File path for the structured access log: one JSON line per proxied request (`request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, `error`) — directly ingestible by Loki/ClickHouse. The same data is always emitted to stdout as structured `aperio_access` tracing events. | — |

### Authentication Layers

> 📖 In depth: [docs/tokens-and-auth.md](docs/tokens-and-auth.md)

Aperio has several independent auth layers; use the ones you need:

1. **Master token** (`APERIO_SERVER_TOKEN`) — full access: tunnel connections, dashboard login, TCP endpoint.
2. **Dynamic tokens** — created from the dashboard, scoped and revocable. See [Dynamic API Tokens](#dynamic-api-tokens).
3. **Visitor password** (`APERIO_SERVER_AUTH=user:password`) — a login form in front of all proxied traffic.
4. **OIDC / SSO** — identity-provider login in front of all proxied traffic. See [OIDC / SSO Protection](#oidc--sso-protection).
5. **Dashboard password** (`APERIO_DASHBOARD_AUTH`) — a separate dashboard-only password (username `aperio`), so you don't have to share the master token with dashboard users. Set `APERIO_DASHBOARD=0` to disable the dashboard entirely.
6. **Share links** — signed, expiring URLs that grant visitors scoped access to a protected site without an account. See [Share Links](#share-links).

### OIDC / SSO Protection

Put an identity-provider login (Google, Keycloak, Authentik, ...) in front of everything the tunnel serves:

```bash
APERIO_OIDC_ISSUER=https://accounts.google.com
APERIO_OIDC_CLIENT_ID=xxxx.apps.googleusercontent.com
APERIO_OIDC_CLIENT_SECRET=xxxx
APERIO_OIDC_ALLOWED_EMAILS=me@corp.com,*@team.example.com
```

Unauthenticated visitors are redirected to the provider. After login, the verified email (fetched from the issuer's `userinfo` endpoint over TLS) is checked against the allowlist — exact addresses, `*@domain`, or `*`. Sessions last 24h and reuse the standard `aperio_session` cookie.

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_OIDC_ISSUER` | Issuer URL. Setting it enables SSO enforcement. | — |
| `APERIO_OIDC_CLIENT_ID` / `APERIO_OIDC_CLIENT_SECRET` | OAuth client registered at the issuer. Redirect URI: `https://<your-host>/aperio/oidc/callback`. | — |
| `APERIO_OIDC_ALLOWED_EMAILS` | Comma-separated allowlist (required with issuer). | — |
| `APERIO_OIDC_SCOPES` | Requested scopes. | `openid email profile` |
| `APERIO_OIDC_REDIRECT_URL` | Fixed callback URL; otherwise derived from the request `Host` (and `X-Forwarded-Proto` when `APERIO_TRUST_PROXY=1`). Recommended to set explicitly. | derived |

Discovery is fetched from `<issuer>/.well-known/openid-configuration` at startup. A misconfigured SSO setup is a **fatal error** — the server refuses to start rather than silently serving an unprotected proxy. Grants and denials are audit-logged.

### Metrics (Prometheus)

Enable with `APERIO_METRICS=1`. The endpoint always requires a token: set `APERIO_METRICS_TOKEN`, or let the server generate a random one on first start (persisted in `APERIO_DATA_DIR/metrics_token`, printed to the log once).

```yaml
# prometheus.yml
scrape_configs:
  - job_name: aperio
    metrics_path: /aperio/metrics
    params:
      token: ["<your-metrics-token>"]
    static_configs:
      - targets: ["tunnel.example.com"]
```

Exposed metrics include `aperio_requests_total`, `aperio_requests_success_total`, `aperio_requests_failed_total`, `aperio_bytes_transferred_total`, `aperio_connected_clients`, `aperio_pending_requests`, `aperio_ws_streams_active`, `aperio_uptime_seconds`, the `aperio_request_duration_seconds` histogram (p95/p99-ready), and per-client `aperio_client_requests_total{client_id=...}`.

### HTTP Endpoints

| Endpoint | Description | Auth |
| --- | --- | --- |
| `/*` (fallback) | Proxied to tunnel clients. | visitor password / OIDC if configured |
| `GET /aperio/ws` | Tunnel endpoint for clients. | master or dynamic token (Bearer / `x-auth-token`) |
| `GET /aperio/tcp` | Experimental TCP tunnel endpoint (WebSocket, binary frames = raw bytes). | master or dynamic token |
| `GET /aperio` | Admin dashboard. | dashboard session |
| `GET /aperio/api/stats`, `/api/logs`, `/api/audit` | Live stats, request log, audit events. | dashboard session |
| `GET/POST /aperio/api/tokens`, `PUT/DELETE /aperio/api/tokens/:id` | Dynamic token management. | dashboard session |
| `GET/POST /aperio/api/webhooks`, `DELETE /aperio/api/webhooks/:id` | Webhook management. | dashboard session |
| `GET /aperio/api/requests/:id`, `POST /aperio/api/requests/:id/replay` | Request inspector & replay. | dashboard session |
| `POST /aperio/api/clients/:id/override`, `POST /aperio/api/clients/:id/enabled` | Temporary bind overrule / enable-disable toggle. | dashboard session |
| `GET/POST /aperio/api/maintenance` | List / toggle per-hostname maintenance mode. | dashboard session |
| `POST /aperio/api/share` | Generate a signed share link (see [Share Links](#share-links)). | dashboard session |
| `GET/PUT /aperio/api/settings` | Read / edit runtime server settings (persisted overrides on top of env defaults). | dashboard session |
| `POST /aperio/api/tunnels`, `DELETE /aperio/api/tunnels/:id` | Programmatic ephemeral tunnel provisioning. See [Ephemeral Tunnels](#ephemeral-tunnels-ci--preview-environments). | master token (Bearer) or dashboard session |
| `GET/POST /aperio/auth` | Login page / login API. | — |
| `GET /aperio/oidc/login`, `/aperio/oidc/callback` | OIDC flow. | — |
| `GET /aperio/metrics` | Prometheus metrics. | metrics token |
| `GET /aperio/health` | Liveness probe (status, client count, uptime). | none |

---

## Client Guide

> 📖 Resilience features (backoff, health probing, hot-reload, drain): [docs/client-resilience.md](docs/client-resilience.md)

The client can be configured three ways, with this precedence:

**CLI arguments  >  environment variables  >  `aperio.yaml`**

With no CLI arguments the client is fully environment-driven — existing Docker setups keep working unchanged.

On connection loss the client reconnects with **exponential backoff and jitter** (1 s doubling up to 60 s, randomized) so a restarted server is not stampeded by its whole client fleet at once; the backoff resets after a connection stays up for 30 s.

When a config file is present, the client **hot-reloads** it: edits to `aperio.yaml` (or the `--config` path) are detected within ~5 s, the current connection is dropped gracefully, and the client reconnects with the freshly resolved `token`, `server`, `target`, `hostname`, `path`, and `priority`. CLI arguments and environment variables keep their precedence; a file that no longer parses is ignored with a warning.

### CLI

```
aperio-client                          Run with environment variables (Docker mode)
aperio-client http <port> [options]    Expose http://localhost:<port>
aperio-client run [--config FILE]      Run from aperio.yaml
aperio-client tcp <local_port>         Bridge a local TCP port to the server's /aperio/tcp endpoint
aperio-client check                    Diagnose configuration and connectivity
aperio-client --version
aperio-client --help
```

`aperio-client check` resolves the configuration with the usual precedence and verifies every hop: server health endpoint (including a client/server version and protocol comparison), token validity (a real tunnel handshake), the local target, and its health endpoint when configured. Exit code 0 = all green — handy in support requests and provisioning scripts.

| Option | Meaning |
| --- | --- |
| `--server URL` | Aperio server URL |
| `--token TOKEN` | Tunnel token (master or dynamic) |
| `--host HOSTNAME` | Hostname bind (e.g. `app.example.com`) |
| `--path PREFIX` | Path bind (e.g. `/api`) |
| `--concurrency N` | Local max concurrent requests |
| `--priority N` | Load-balancing priority tier: 0 = primary (default), higher = standby |
| `--pass-hostname` | Forward the original `Host` header to the backend |
| `--config FILE` | Config file path (default: `./aperio.yaml`) |

### Configuration Reference

| Env variable | CLI | yaml key | Description | Default |
| --- | --- | --- | --- | --- |
| `APERIO_SERVER_TOKEN` | `--token` | `token` | **Required.** Tunnel token. | — |
| `APERIO_SERVER_URL` | `--server` | `server` | **Required.** Server URL (`http/https/ws/wss`). | — |
| `APERIO_CLIENT_TARGET` | `http <port>` | `target` | **Required.** Local backend to forward to. | — |
| `APERIO_HOSTNAME_BIND` | `--host` | `hostname` | Hostname this client serves. | — |
| `APERIO_PATH_BIND` | `--path` | `path` | Path prefix this client serves. | — |
| `APERIO_CLIENT_TRIM_BIND` | — | `trim_bind` | Strip the path bind prefix before forwarding. | `1` when a path bind is set |
| `APERIO_CLIENT_PASS_HOSTNAME` | `--pass-hostname` | `pass_hostname` | Forward the original `Host` header instead of the target's. | `0` |
| `APERIO_CLIENT_PRIORITY` | `--priority` | `priority` | Load-balancing priority tier announced to the server (0 = primary, higher = standby; effective with `APERIO_LB_STRATEGY=primary-standby`). | `0` |
| `APERIO_CLIENT_BANDWIDTH` | — | `bandwidth` | Link capacity of this client's network, e.g. `8mbit`, `500kbit`, `2MB`, or plain bytes/second. The server paces outgoing tunnel frames (token bucket, 1 s burst) so this client is never pushed faster than its network can drain. | unlimited |
| `APERIO_CLIENT_MAX_CONCURRENT` | `--concurrency` | `max_concurrent` | Max concurrent requests; announced to the server, which queues the excess instead of flooding the backend. Also enforced locally. | unlimited |
| `APERIO_CLIENT_TCP_TARGET` | — | `tcp_target` | `host:port` for experimental TCP tunneling. The client only ever connects to this exact address. | — |
| `APERIO_CLIENT_TARGET_HEALTH` | — | `target_health` | Health endpoint of the local target (path like `/health`, or a full URL). When set, the client probes it independently and reports the result to the server: a failing backend takes the client **out of routing without dropping the tunnel**; it rejoins automatically when the probe recovers. The dashboard shows a `BACKEND DOWN` badge meanwhile. | — |
| `APERIO_CLIENT_HEALTH_INTERVAL` | — | `health_interval` | Seconds between backend health probes. | `10` |
| `APERIO_CLIENT_HEALTH_TIMEOUT` | — | `health_timeout` | Per-probe timeout (seconds). | `5` |
| `APERIO_CLIENT_HEALTH_THRESHOLD` | — | `health_threshold` | Consecutive probe failures before the backend is reported unhealthy. | `2` |
| `APERIO_CLIENT_TIMEOUT` | — | `timeout` | Per-request backend timeout (seconds). | `30` |
| `APERIO_CLIENT_MAX_RESPONSE_BODY` | — | `max_response_body` | Max backend response size in bytes; bodies over 256 KB are streamed through the tunnel in chunks, larger than this limit are truncated. | 50 MB |
| `APERIO_CLIENT_MAX_MESSAGE_SIZE` | — | `max_message_size` | Max size of one tunnel message accepted from the server (memory protection). | 32 MB |
| `LOG_LEVEL` | — | — | Log verbosity. | `info` |

### aperio.yaml

If an `aperio.yaml` exists in the working directory (or is passed with `--config`), its values are used as defaults:

```yaml
# Aperio client configuration
server: https://tunnel.example.com
token: apr_xxxxxxxxxxxxxxxx
target: http://localhost:3000

# optional
hostname: app.example.com
path: /api
trim_bind: true
pass_hostname: false
max_concurrent: 8
priority: 0                # 0 = primary, higher = standby tier
target_health: /health     # probe the backend; report unhealthy without dropping the tunnel
health_interval: 10
tcp_target: localhost:5432
```

The file is hot-reloaded: edits are applied within ~5 s via a graceful reconnect.

### Graceful Shutdown

On `SIGINT`/`SIGTERM` the client tells the server it is **draining**: the server immediately stops routing new requests to it, in-flight requests finish (up to 30 s), then the process exits. This plays well with `docker stop` and rolling deployments.

---

## Routing

> 📖 In depth: [docs/routing-and-load-balancing.md](docs/routing-and-load-balancing.md) · [docs/failover.md](docs/failover.md)

When a request arrives, the server picks a client in this order:

1. **Eligibility** — clients that are unhealthy (no heartbeat within `APERIO_CLIENT_DOWN_THRESHOLD`), whose own backend health probe failed, draining, or disabled from the dashboard are skipped. In-flight requests always finish.
2. **Hostname** — clients whose hostname binds contain the request's `Host` (case-insensitive, port ignored) win. If none match, clients *without* any hostname bind act as the fallback pool — unless `APERIO_REQUIRE_HOSTNAME_BIND=1`, in which case the request fails with 504.
3. **Path** — within the hostname pool, the longest matching path bind wins. Binds match on segment boundaries: `/api` matches `/api` and `/api/v1`, never `/apixyz`. Clients without a path bind are the fallback.
4. **Strategy** — how a client is picked from the final pool, set by `APERIO_LB_STRATEGY`:
   - `round-robin` (default) — clients with identical binds share traffic evenly.
   - `primary-standby` — only the clients with the **lowest announced priority** (`--priority` / `APERIO_CLIENT_PRIORITY`, 0 = primary) receive traffic; standby tiers take over automatically when every more-primary client is unhealthy, draining, disabled, or gone. Rotation still applies within a tier. The dashboard marks standby clients with a `standby N` badge.
   - `sticky` — round-robin for first-time visitors, then an `aperio_affinity` cookie (HttpOnly, 24 h) pins each visitor to the client that served them — including their WebSockets. Affinity keys on the client's instance ID, so it survives reconnects of the same client process; if that client leaves the pool the visitor falls back to rotation and gets a fresh cookie. Use this when backends hold per-visitor state (PHP sessions, in-memory carts, ...). The cookie is stripped before requests reach backends.

A client can hold several hostname binds at once: its declared `--host`, hostnames granted by its token, and a random subdomain.

### In-Flight Failover

By default, a request that is already dispatched to a client fails with **502** if that client's connection drops before it answers. `APERIO_FAILOVER` changes this — failover only ever triggers when **no response bytes have reached the visitor yet**, so a re-dispatch is transparent:

- `fail` *(default)* — answer 502 immediately.
- `retry` — re-dispatch to another currently available candidate for the same route; 502 when none exists.
- `wait` — wait for the **same client** to reconnect (recognized by its self-reported instance ID, which survives reconnects) and re-dispatch to it; when the instance is unknown, any candidate counts.
- `retry-wait` — re-dispatch to another candidate right away; if none exists, wait for one to appear. The most available option.

Two limits bound the behavior: `APERIO_FAILOVER_MAX_JUMPS` caps how many times one request may be re-dispatched (default 2), and `APERIO_FAILOVER_WINDOW` caps the total seconds the waiting modes may spend (default 15, starting at the first failure).

Only **idempotent methods** (GET, HEAD, OPTIONS, PUT, DELETE, TRACE) fail over by default: a POST may have already reached the backend before the client died, and re-dispatching it could execute the operation twice. Set `APERIO_FAILOVER_ALL_METHODS=1` if your backends handle duplicate deliveries. Every jump is logged with the old and new client IDs.

### Hostname Binding

Run the server behind a wildcard domain (e.g. Traefik routing `*.example.com` to it) and let each client claim a subdomain:

```
a.example.com  ──▶  client A (--host a.example.com)
b.example.com  ──▶  client B (--host b.example.com)
c.example.com  ──▶  client C (no hostname bind — fallback)
```

### Path Binding & Prefix Trimming

```
/api/v1/users  ──▶  client bound to /api   (forwarded as /v1/users, trim_bind default)
/app/index.js  ──▶  client bound to /app
/about.html    ──▶  fallback client
```

Set `APERIO_CLIENT_TRIM_BIND=0` to forward the full original path.

### Random Subdomains

With `APERIO_RANDOM_SUBDOMAIN="*.example.com"` on the server, every connecting client is automatically assigned a hostname like `a1b2c3d4e5.example.com`. The client logs it on connect and the dashboard shows it. Assignments are per-connection (a reconnect gets a fresh one) and *additive* — token-granted and declared binds keep working alongside.

The value is a pattern whose leftmost label contains a `*` placeholder, replaced with a random label on assignment:

- `example.com` — shorthand for `*.example.com`
- `*.example.com` — `<random>.example.com`
- `*-test.example.com` — `<random>-test.example.com`: stays on the same subdomain level, so the parent domain's wildcard TLS certificate still covers the generated hostnames

### Dashboard Overrule

The dashboard can temporarily override any client's hostname/path binds ("Overrule" button) — useful for redirecting a hostname live or giving binds to a client that connected without them. Overrides live only in server memory: a client reconnect or server restart reverts to the client's own configuration.

---

## Dynamic API Tokens

> 📖 In depth: [docs/tokens-and-auth.md](docs/tokens-and-auth.md)

Besides the master token, you can mint scoped tokens from the dashboard (*API Tokens* section). Each token carries permissions:

- **Hostnames** — which hostname binds the token may claim. `*` = any. Specific entries are **auto-bound** on connect (the client doesn't even need `--host`).
- **Paths** — which path binds it may claim. `*` = any.
- **Allowed IPs** — source IPs/CIDRs that may connect with this token (`0.0.0.0/0` = any, the default).
- **Lifetime** — optional TTL; expired tokens are rejected at connect time.

A client declaring a bind its token doesn't permit gets the declaration ignored (logged). Tokens can be **edited in place** (scope, IPs, expiry — the secret never changes) or revoked; revocation rejects new connections while existing tunnels stay up until they drop.

Secrets are stored as SHA-256 hashes in `APERIO_DATA_DIR/tokens.json` and shown exactly once at creation.

> **Docker note:** dynamic tokens (plus stats, audit log, and webhooks) live in `APERIO_DATA_DIR`. Without a volume (`- ./data:/app/data`) they are lost when the container is recreated.

---

## Share Links

> 📖 In depth: [docs/share-links.md](docs/share-links.md)

When a proxied site is protected (`APERIO_SERVER_AUTH` or OIDC), you can hand out **temporary access** without creating accounts: the dashboard's *Share Links* section generates a URL like

```
https://app.example.com/docs?aperio_share=eyJob3N0IjoiYXBwLuKApiJ9.9f2c…
```

The token is JWT-style — `base64url(claims).base64url(HMAC-SHA256)` — carrying the hostname, an optional path prefix, and an expiry (default 3 days; the dashboard offers presets from 30 minutes up to 1 month, plus a never-expires option). Opening the link validates the token, answers with a redirect to the clean URL, and sets an `aperio_share` cookie (`HttpOnly`, `SameSite=Lax`, expiring with the token) that authorizes subsequent requests — including the page's WebSockets. Out-of-scope paths still redirect to the login page.

Links are **stateless**: the signing key is derived from the master token, nothing is stored server-side, and links simply expire (rotating `APERIO_SERVER_TOKEN` invalidates all of them at once). Creation is audited (`share_created`) and emitted to webhooks; the internal cookie is stripped before requests are forwarded to backends.

---

## Ephemeral Tunnels (CI / Preview Environments)

> 📖 In depth: [docs/ephemeral-tunnels.md](docs/ephemeral-tunnels.md)

`POST /aperio/api/tunnels` mints a **short-lived, hostname-scoped token** in one call — designed for automation such as per-PR preview environments. It authenticates with the master token in a header (no browser login), and works even when the dashboard is disabled:

```bash
curl -X POST https://tunnel.example.com/aperio/api/tunnels \
  -H "Authorization: Bearer $APERIO_SERVER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "pr-123", "hostname": "pr-123.example.com", "ttl_seconds": 3600}'
# → {"id": "…", "hostname": "pr-123.example.com", "url": "https://pr-123.example.com",
#    "token": "apr_…", "expires_at": 1700000000}
```

- Omit `hostname` to get a **random subdomain** (requires `APERIO_RANDOM_SUBDOMAIN`).
- `ttl_seconds` defaults to 1 hour, capped at 7 days; `allowed_ips` restricts who may connect.
- The minted token's hostname is **auto-bound** on connect — run the client with just the server URL, token, and target.
- `DELETE /aperio/api/tunnels/:id` revokes the token (same auth), e.g. from a CI cleanup step.
- Events appear in the audit log and are delivered to webhooks as `tunnel_created` / `tunnel_deleted`.

### GitHub Action

[`aperio-tunnel-action`](aperio-tunnel-action/) wraps the flow for GitHub Actions: it provisions a tunnel, runs the client container for the rest of the job, and revokes the token afterwards.

```yaml
- name: Open tunnel
  id: tunnel
  uses: co3moz/aperio/aperio-tunnel-action@master
  with:
    server-url: https://tunnel.example.com
    server-token: ${{ secrets.APERIO_SERVER_TOKEN }}
    port: 3000
    hostname: pr-${{ github.event.number }}.example.com

- run: echo "Preview at ${{ steps.tunnel.outputs.url }}"
```

See [aperio-tunnel-action/README.md](aperio-tunnel-action/README.md) for all inputs and outputs.

---

## Dashboard

> 📖 In depth: [docs/dashboard.md](docs/dashboard.md)

Available at `/aperio` (login: `aperio` / master token, or `APERIO_DASHBOARD_AUTH`):

- **Live overview** — connected clients, request rate chart, lifetime average response time, today's traffic (persisted across restarts).
- **Clients table** — binds, health dot, last heartbeat, client version (with a warning badge on tunnel protocol mismatch), standby tier, `BACKEND DOWN` badge when the client's own health probe fails, announced concurrency limit, per-client **Enable/Disable** kill switch (disabled clients stay connected but receive no new traffic), and bind overrule.
- **Request inspector** — click any row in the traffic table to see full request/response headers and body previews (up to 64 KB per direction, last 50 requests), and **replay** the request through the tunnel with one click.
- **API Tokens / Webhooks** — create, edit, revoke.
- **Add Client wizard** — pick a token strategy (placeholder or mint a scoped token on the spot), describe the local service, and copy a ready-to-run `docker run` / CLI / `aperio.yaml` snippet.
- **Maintenance mode** — put a hostname (or `*` for everything) into maintenance: visitors get a 503 page (customizable via `APERIO_503_PAGE`, with `Retry-After`) while the tunnel clients stay connected. In-memory like bind overrides; cleared on server restart. Toggles are audited and emitted as `maintenance_on`/`maintenance_off` webhook events.
- **Share links** — generate signed, expiring visitor-access URLs. See [Share Links](#share-links).
- **Server settings** — edit almost every runtime setting (timeouts, limits, LB strategy, failover, compression, random subdomains, visitor password, custom 503/504 HTML) live from the dashboard. Environment variables stay the defaults; edits become **persisted overrides** (`APERIO_DATA_DIR/settings.json`) that survive restarts and can be reset per field. The master token, `HOST`/`PORT`, proxy trust and OIDC remain env-only. Changes are audited (`settings_updated`) and emitted to webhooks.
- **Audit log** — the last 200 administrative/security events.

---

## Observability & Events

> 📖 In depth: [docs/observability.md](docs/observability.md)

### Audit Log

Logins (password and OIDC), token create/update/revoke, ephemeral tunnel provisioning, share link creation, maintenance toggles, client connect/disconnect/drain, kill-switch toggles, overrules, replays and TCP streams are appended to `APERIO_DATA_DIR/audit.jsonl` with timestamp, actor IP, and details — and shown in the dashboard.

### Webhooks

Define webhooks from the dashboard (name, URL, subscribed events — `*` for all). Events are delivered as fire-and-forget JSON POSTs with a 10 s timeout:

```json
{ "event": "client_connected", "timestamp": "2026-07-06T15:16:37+03:00", "data": { "client_id": "…", "ip": "…", "token": "tenant-a" } }
```

Available events: `client_connected`, `client_disconnected`, `client_draining`, `token_created`, `token_revoked`, `tunnel_created`, `tunnel_deleted`, `share_created`, `maintenance_on`, `maintenance_off`.

### Access Log

Every proxied request is emitted as a structured `aperio_access` tracing event on stdout (JSON with `request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, `error` as top-level fields). Set `APERIO_ACCESS_LOG=/path/to/access.jsonl` to additionally append the same data as raw JSON lines, unaffected by `LOG_LEVEL` — ready to be tailed into Loki or ClickHouse.

### Persistent Statistics

Lifetime counters (total requests, success/failure, bytes sent/received, summed duration) and daily/weekly/monthly/yearly buckets survive restarts in `APERIO_DATA_DIR/stats.json` (flushed every 30 s and on shutdown; pruned to 60 days / 26 weeks / 24 months / 10 years).

Traffic is additionally attributed **per token** (`master` for the master token) and **per request hostname** — the dashboard's *Traffic Breakdown* section shows the top consumers of each. Up to 200 distinct labels are tracked per dimension; overflow folds into an `(other)` bucket so unbounded hostname cardinality cannot grow the stats file.

---

## Advanced

> 📖 In depth: [docs/tunnel-protocol.md](docs/tunnel-protocol.md)

### WebSocket / Socket.io Pass-Through

WebSocket upgrade requests are detected automatically and proxied end-to-end — the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints all work with zero configuration. The same hostname/path routing rules apply.

### Large Bodies & Compression

Bodies over 256 KB are streamed through the tunnel in chunks with backpressure **in both directions** — responses since v1, and request bodies (uploads) with tunnel protocol v2 — so memory usage stays bounded on both sides regardless of size. v2 peers additionally exchange body chunks as **raw binary WebSocket frames** instead of base64+JSON, removing the ~33% base64 overhead. Both features negotiate automatically via the heartbeat protocol version: older peers transparently fall back to buffered bodies and base64 frames. Streamed uploads cannot fail over or be replayed from the inspector (the body is consumed as it is forwarded).

With `APERIO_TUNNEL_COMPRESSION=1` the server offers per-message zlib compression for JSON frames; clients that support it acknowledge and both directions switch to compressed frames (older clients keep working uncompressed).

### Experimental TCP Tunneling

Expose a raw TCP service (database, SSH, ...) through the same tunnel port:

```bash
# Private network side: allow TCP streams to exactly one target
APERIO_CLIENT_TCP_TARGET=localhost:5432 aperio-client http 3000 --server ... --token ...

# Consumer side (your laptop): bridge a local port through the server
aperio-client tcp 15432 --server https://tunnel.example.com --token apr_xxxxxxxx
psql -h 127.0.0.1 -p 15432
```

Consumers authenticate against `GET /aperio/tcp` with any valid tunnel token (dynamic-token IP allowlists apply). The client only ever connects to its configured `tcp_target`, regardless of what the server asks — the TCP analogue of the HTTP SSRF guard. No extra public ports are opened.

### Custom Error Pages

`APERIO_504_PAGE=/app/error_504.html` serves your own HTML (loaded once at startup) on gateway-timeout responses — e.g. a branded "tunnel is offline, check back soon" page. `APERIO_503_PAGE` does the same for the maintenance-mode response.

---

## Security Notes

- Always front the server with TLS (Traefik/Caddy/nginx) and set `APERIO_TRUST_PROXY=1` behind it; clients should use `https://`/`wss://` URLs so tokens never travel in plaintext.
- Prefer **dynamic tokens** over sharing the master token: scope them to a hostname, pin them to source IPs, give them a TTL. Treat the master token as root.
- The client deliberately does not fully trust the server: it only connects to its configured HTTP/TCP targets (SSRF guards), caps tunnel message sizes, bounds decompression output, and enforces its own concurrency limit.
- Constant-time comparison is used for all secrets; dashboard sessions are `HttpOnly` + `SameSite=Lax` cookies; query strings are stripped from logs.
- Share links are HMAC-signed and scoped to a hostname (and optional path); anyone holding a link has access until it expires, so scope them tightly — rotating `APERIO_SERVER_TOKEN` revokes all of them at once.
- The metrics endpoint is never public — it always requires a token.

---

## License

This project is open-source and free to use.
