# Aperio 🛡️

Aperio is a secure, high-performance, and lightweight HTTP reverse tunneling system written in Rust. It exposes local servers behind NATs, firewalls, or private networks to the public internet through a secure, persistent WebSocket connection.

---

## Architecture Overview

Aperio is split into two components:

1. **`aperio-server`**: A public-facing proxy server built with Axum. It handles public HTTP requests and routes them to the connected client(s) via WebSockets. It includes a built-in admin dashboard, rate limiter, and load balancer.
2. **`aperio-client`**: A client agent that runs inside your private network. It connects to the server via WebSockets, receives incoming forwarded HTTP requests, dispatches them to your local server, and pipes the response back.

```
       Public HTTP Request                       Secure WebSocket
[ User ] -----------------> [ aperio-server ] <=====================> [ aperio-client ]
                                     |                                       |
                                     v                                       v
                             Admin Dashboard (Optional)               [ Local Server ]
```

---

## Features

- **Written in Rust**: Blazing fast, memory-safe, and low resource overhead.
- **Robust Connection Liveness**: Employs continuous heartbeat ping/pong signals to detect socket drops and automatically reconnects in seconds.
- **Session-Based Auth Login**: A clean dark-mode login page protects both dashboard and proxied traffic with cookie-based sessions.
- **Per-IP Rate Limiting**: Built-in, high-performance in-memory Token Bucket rate limiter to protect your tunnels from abuse.
- **Concurrency Limiting**: Prevents resource starvation using token-based concurrency controls (semaphores).
- **Round-Robin Load Balancing**: Seamlessly load-balances incoming traffic across multiple active tunnel clients.
- **WebSocket / Socket.io Pass-Through**: Proxies WebSocket upgrade requests end-to-end — public WS connections are tunneled to your local backend in real time, enabling Socket.io, GraphQL subscriptions, and raw WebSocket endpoints.
- **Graceful Shutdowns**: Handles OS signals (`SIGINT`, `SIGTERM`) to release connections cleanly.

---

## Getting Started

### Prerequisites

- **Rust toolchain** (Rust 2024 edition / v1.75+)
- Or **Docker** / **Docker Compose**

### Building from Source

Build the release binaries from the workspace root:

```bash
cargo build --release -p aperio-server
cargo build --release -p aperio-client
```

The compiled binaries will be located at:

- `target/release/aperio-server`
- `target/release/aperio-client`

---

## Server Configuration (`aperio-server`)

The server is configured entirely through environment variables.

### Environment Variables

| Variable Name                            | Description                                                                                                                   | Default Value     | Required | Type    |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | ----------------- | -------- | ------- |
| `APERIO_SERVER_TOKEN`                    | Secret security token required for websocket clients to connect. Used as a Bearer Token.                                      | _(None)_          | **Yes**  | String  |
| `HOST`                                   | The network address the server binds to.                                                                                      | `0.0.0.0`         | No       | String  |
| `PORT`                                   | The TCP port the proxy server listens on.                                                                                     | `8080`            | No       | u16     |
| `APERIO_DASHBOARD`                       | Set to `0` or `false` to disable the admin dashboard. Enabled by default.                                                     | `true`            | No       | Boolean |
| `APERIO_SERVER_GATEWAY_TIMEOUT`          | Time (in seconds) to wait for a tunnel client to connect if a request comes in while offline (grace period for reconnecting). | `10`              | No       | u64     |
| `APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT` | Maximum time (in seconds) to wait for a connected client to process a request and reply.                                      | `30`              | No       | u64     |
| `APERIO_MAX_BODY_SIZE`                   | Maximum request body payload size allowed in bytes to protect against OOM attacks.                                            | `10485760` (10MB) | No       | usize   |
| `APERIO_MAX_CONCURRENT_REQUESTS`         | Limit on max concurrent in-flight requests processed across all tunnels.                                                      | `100`             | No       | usize   |
| `APERIO_MAX_TUNNELS`                     | Maximum number of concurrent active tunnel client connections.                                                                | `10`              | No       | usize   |
| `APERIO_IP_LIMIT_MAX`                    | The burst size capacity for the per-IP Token Bucket rate limiter.                                                             | `100.0`           | No       | f64     |
| `APERIO_IP_LIMIT_REFILL`                 | Token bucket refill rate (tokens per second) for rate limiting (e.g. `5.0` allows average 300 req/min).                       | `5.0`             | No       | f64     |
| `APERIO_SERVER_AUTH`                      | If set to `username:password`, requires login via web form before proxied requests are allowed.                              | _(None)_          | No       | String  |
| `APERIO_DASHBOARD_AUTH`                  | Password for dashboard-only login with username `aperio`. Falls back to `APERIO_SERVER_TOKEN` if not set.                   | _(None)_          | No       | String  |
| `APERIO_TRUST_PROXY`                     | Set to `1` or `true` to trust `X-Forwarded-For` / `X-Real-IP` headers for client IP resolution.                             | `false`           | No       | Boolean |
| `APERIO_SECURE_COOKIES`                  | Set to `1` or `true` to add the `Secure` flag to session cookies (HTTPS-only). Defaults to `APERIO_TRUST_PROXY` value.      | `false`           | No       | Boolean |
| `APERIO_REQUIRE_HOSTNAME_BIND`           | Set to `1` or `true` to require hostname binds: clients without a hostname bind are excluded from load balancing entirely.  | `false`           | No       | Boolean |
| `APERIO_METRICS`                         | Set to `1` or `true` to enable the Prometheus metrics endpoint at `/aperio/metrics`.                                        | `false`           | No       | Boolean |
| `APERIO_METRICS_TOKEN`                   | Optional bearer token required to scrape `/aperio/metrics` (`Authorization: Bearer <token>`). Unset = no auth on metrics.    | _(None)_          | No       | String  |
| `LOG_LEVEL`                               | Log verbosity. Use instead of `RUST_LOG` for a simpler interface. Values: `error`, `warn`, `info`, `debug`, `trace`.          | `info`            | No       | String  |

### Endpoints

- **`/*` (Fallback)**: Any path not matching `/aperio` routes is proxied to the active tunnel clients.
- **`GET /aperio/ws`**: Secure WebSocket endpoint where the client connects. Requires authentication (HTTP `Authorization: Bearer <token>` or `x-auth-token: <token>`).
- **`GET /aperio`**: HTML admin dashboard interface (available when `APERIO_DASHBOARD` is enabled).
- **`GET /aperio/api/stats`**: JSON stats endpoint displaying connection counters, byte counters, and uptime info (available when `APERIO_DASHBOARD` is enabled).
- **`GET /aperio/api/logs`**: JSON endpoint returning the last 100 request logs (available when `APERIO_DASHBOARD` is enabled).
- **`POST /aperio/api/clients/:id/override`**: Applies a temporary (in-memory) hostname/path bind overrule to a connected client (available when `APERIO_DASHBOARD` is enabled).
- **`GET /aperio/metrics`**: Prometheus text-format metrics (available when `APERIO_METRICS` is enabled; optionally protected with `APERIO_METRICS_TOKEN`).
- **`GET /aperio/health`**: Simple server health verification endpoint (always available, no auth required).
- **`GET /POST /aperio/auth`**: Login page and authentication endpoint. Always available regardless of dashboard setting.

---

## Client Configuration (`aperio-client`)

The client receives requests from the server and forwards them to a local backend server.

### Environment Variables

| Variable Name                 | Description                                                                                                           | Default Value           | Required | Type           |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------- | ----------------------- | -------- | -------------- |
| `APERIO_SERVER_TOKEN`         | Secret security token matching the server's token.                                                                    | _(None)_                | **Yes**  | String         |
| `APERIO_SERVER_URL`           | Public URL of the Aperio proxy server. Supports `http`/`https` or `ws`/`wss` protocols.                                | _(None)_                | **Yes**  | String         |
| `APERIO_CLIENT_TARGET`        | Address of the local target backend to forward proxy traffic to.                                                      | _(None)_                | **Yes**  | String         |
| `APERIO_CLIENT_PASS_HOSTNAME` | If set to `1`, passes the original request `Host` header through. Otherwise, overrides it with the local target host. | `0` (default)           | No       | Boolean/String |
| `APERIO_PATH_BIND`           | Path prefix to bind this client to (e.g. `/api`). Unbound clients serve as fallback.                                   | _(None)_                | No       | String         |
| `APERIO_HOSTNAME_BIND`       | Hostname to bind this client to (e.g. `a.example.com`). The server routes requests whose `Host` header matches.        | _(None)_                | No       | String         |
| `APERIO_CLIENT_TRIM_BIND`    | If `1`, strips the path bind prefix from the URI before forwarding. Defaults to `1` when `APERIO_PATH_BIND` is set.     | `1` (if bind set)       | No       | Boolean        |
| `APERIO_CLIENT_MAX_RESPONSE_BODY` | Maximum response body size in bytes accepted from the backend. Protects against OOM.                                  | `52428800` (50MB)       | No       | usize          |
| `APERIO_CLIENT_TIMEOUT`     | Per-request timeout in seconds for calls to the target backend.                                                           | `30`                    | No       | u64            |
| `LOG_LEVEL`                 | Log verbosity. Values: `error`, `warn`, `info`, `debug`, `trace`.                                                    | `info`                  | No       | String         |

---

## Client Path Binding & Routing

Aperio supports advanced path-based routing, allowing you to direct public traffic to different clients depending on the URL path prefix of the incoming request. This is controlled via two environment variables on the client side: `APERIO_PATH_BIND` and `APERIO_CLIENT_TRIM_BIND`.

```
           Public HTTP Requests
                 |
                 +---> GET /api/v1/users  --> [ Client A (Path Bind: /api) ]  --> Local Target A (Forwarded as: /v1/users)
                 |
                 +---> GET /app/index.js  --> [ Client B (Path Bind: /app) ]  --> Local Target B (Forwarded as: /app/index.js)
                 |
                 +---> GET /about.html    --> [ Client C (No Path Bind) ]     --> Local Target C (Fallback client)
```

### How Routing Decisions are Made

1. **Specific Matches (Boundary-Aware)**:
   When a request comes in, the server evaluates the path prefix against all registered path-bound clients. Binds are matched on segment boundaries, meaning a bind of `/api` will match `/api` or `/api/v1`, but will **not** match `/apixyz`.
2. **Round-Robin Load Balancing**:
   If multiple clients register the *exact same* `APERIO_PATH_BIND` prefix, the server automatically distributes traffic between them using round-robin load balancing.
3. **Fallback to Unbound Clients**:
   If no path-bound client matches the incoming request, the server routes the request to any connected clients that **do not** have a path bind set (acting as catch-all / fallback handlers).
4. **Gateway Timeout**:
   If there are no matching path-bound clients and no unbound fallback clients connected, the server returns a `504 Gateway Timeout` response.

### Hostname Binding (`APERIO_HOSTNAME_BIND`)

When you expose the Aperio server behind a wildcard domain (e.g. Traefik routing `*.example.com` to it), each client can claim a specific hostname:

```
           Public HTTP Requests (Host header)
                 |
                 +---> a.example.com  --> [ Client A (Hostname Bind: a.example.com) ]
                 |
                 +---> b.example.com  --> [ Client B (Hostname Bind: b.example.com) ]
                 |
                 +---> c.example.com  --> [ Client C (No Hostname Bind, fallback) ]
```

Routing order: the server first selects the hostname group (exact match on the request's `Host` header, case-insensitive, port ignored), then applies path-bind routing *within* that group. Clients without a hostname bind act as the fallback pool for unmatched hosts.

With `APERIO_REQUIRE_HOSTNAME_BIND=1` on the server, the fallback is disabled: clients that did not declare a hostname bind never receive proxied traffic. Use this in strict multi-tenant setups where every client must claim its own subdomain.

### Dashboard Overrule (Temporary Bind Overrides)

The dashboard's *Active Tunnel Connections* table shows each client's hostname bind, path bind, and last heartbeat. The **Overrule** button lets you set a temporary hostname/path bind for a connected client — useful to route traffic to a client that connected without binds, or to redirect a hostname live. Overrides live only in server memory: they disappear when the client reconnects or the server restarts, and the client's own configuration is never modified.

### Prefix Trimming (`APERIO_CLIENT_TRIM_BIND`)

By default, when a client has a path bind configured, it strips the bind prefix from the URL path before forwarding the request to your local backend server.

- **With `APERIO_CLIENT_TRIM_BIND=1` (Default)**:
  * Public Request: `GET /api/v1/users`
  * Client Bind: `/api`
  * Request received by Local Target: `GET /v1/users`

- **With `APERIO_CLIENT_TRIM_BIND=0`**:
  * Public Request: `GET /api/v1/users`
  * Client Bind: `/api`
  * Request received by Local Target: `GET /api/v1/users`

---

## WebSocket / Socket.io Pass-Through

Aperio automatically detects and proxies WebSocket upgrade requests. When a public client sends an HTTP request with `Connection: Upgrade` and `Upgrade: websocket` headers, the server performs the upgrade handshake and establishes a persistent bidirectional relay between the public client, the tunnel, and your local backend.

```
    Public WS Client                    Tunnel                   Local Backend
[ Browser (socket.io) ] ---WSS---> [ aperio-server ] <==WS==> [ aperio-client ] ---WS---> [ localhost:3000 ]
         |                               |                          |                        |
         +------ bidirectional frames ---+---- WsData frames ------+--- bidirectional frames -+
```

### How It Works

1. Public client sends `GET /ws HTTP/1.1` with `Upgrade: websocket` headers.
2. Server detects the upgrade, selects a tunnel client (same path-binding rules apply), and sends an `UpgradeRequest` through the tunnel.
3. Client connects to the local backend via WebSocket, preserving the original `Sec-WebSocket-*` headers.
4. Once the backend accepts the upgrade, a `101 Switching Protocols` response flows back, and the server upgrades the public connection.
5. All subsequent WebSocket frames (text, binary, close) are relayed bidirectionally through the tunnel in real time.

### Supported Use Cases

| Technology            | Works? | Notes                                   |
| --------------------- | ------ | --------------------------------------- |
| Raw WebSocket (ws://) | Yes    | Full pass-through of text/binary frames |
| Socket.io             | Yes    | WebSocket transport mode is supported   |
| GraphQL Subscriptions | Yes    | If backend uses WS transport            |
| HTTP Long-Polling     | Yes    | Regular HTTP request/response (unchanged) |

No additional configuration is required — the server and client handle upgrade detection automatically.

---

## Run and Usage Examples

### Quick Start (Local Development)

1. **Start the Server:**

   ```bash
   # On Windows (PowerShell)
   $env:APERIO_SERVER_TOKEN="super-secret-token"
   $env:APERIO_DASHBOARD="1"
   ./aperio-server

   # On Linux/macOS
   APERIO_SERVER_TOKEN="super-secret-token" APERIO_DASHBOARD="1"  ./aperio-server
   ```

2. **Start the Client (forwarding to local port 3000):**

   ```bash
   # On Windows (PowerShell)
   $env:APERIO_SERVER_TOKEN="super-secret-token"
   $env:APERIO_SERVER_URL="http://localhost:8080"
   $env:APERIO_CLIENT_TARGET="http://localhost:3000"
   ./aperio-client

   # On Linux/macOS
   APERIO_SERVER_TOKEN="super-secret-token" APERIO_SERVER_URL="http://localhost:8080" APERIO_CLIENT_TARGET="http://localhost:3000" ./aperio-client
   ```

3. **Access the Dashboard:**
   Open a browser at `http://localhost:8080/aperio`. Log in using `aperio` as the username and your `APERIO_SERVER_TOKEN` as the password (or the value of `APERIO_DASHBOARD_AUTH` if configured).

---

## Docker Usage

Both server and client include Docker support.

### Server Docker Run

```bash
docker build -t aperio-server -f aperio-server/Dockerfile .
docker run -d \
  -p 8080:8080 \
  -e APERIO_SERVER_TOKEN="your-secure-token" \
  aperio-server
```

### Client Docker Run

```bash
docker build -t aperio-client -f aperio-client/Dockerfile .
docker run -d \
  --network="host" \
  -e APERIO_SERVER_TOKEN="your-secure-token" \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_CLIENT_TARGET="http://localhost:3000" \
  aperio-client
```

---

## License

This project is open-source and free to use.
