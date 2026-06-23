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
- **Basic Auth Protected Admin Dashboard**: A clean, responsive dark-mode dashboard showing server health, active connections, total traffic, and real-time request logs.
- **Per-IP Rate Limiting**: Built-in, high-performance in-memory Token Bucket rate limiter to protect your tunnels from abuse.
- **Concurrency Limiting**: Prevents resource starvation using token-based concurrency controls (semaphores).
- **Round-Robin Load Balancing**: Seamlessly load-balances incoming traffic across multiple active tunnel clients.
- **Graceful Shutdowns**: Handles OS signals (`SIGINT`, `SIGTERM`) to release connections cleanly.

---

## Getting Started

### Prerequisites

- **Rust toolchain** (Rust 2024 edition / v1.75+)
- Or **Docker** / **Docker Compose**

### Building from Source

Build the release binaries for both server and client:

```bash
# Build Server
cd aperio-server
cargo build --release

# Build Client
cd ../aperio-client
cargo build --release
```

The compiled binaries will be located at:

- `aperio-server/target/release/aperio-server`
- `aperio-client/target/release/aperio-client`

---

## Server Configuration (`aperio-server`)

The server is configured entirely through environment variables.

### Environment Variables

| Variable Name                            | Description                                                                                                                   | Default Value     | Required | Type    |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | ----------------- | -------- | ------- |
| `APERIO_SERVER_TOKEN`                    | Secret security token required for websocket clients to connect. Used as a Bearer Token.                                      | _(None)_          | **Yes**  | String  |
| `HOST`                                   | The network address the server binds to.                                                                                      | `0.0.0.0`         | No       | String  |
| `PORT`                                   | The TCP port the proxy server listens on.                                                                                     | `8080`            | No       | u16     |
| `APERIO_DASHBOARD`                       | Enables the built-in admin web dashboard, statistics, and log APIs. Set to `1` or `true`.                                     | `false`           | No       | Boolean |
| `APERIO_SERVER_GATEWAY_TIMEOUT`          | Time (in seconds) to wait for a tunnel client to connect if a request comes in while offline (grace period for reconnecting). | `10`              | No       | u64     |
| `APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT` | Maximum time (in seconds) to wait for a connected client to process a request and reply.                                      | `30`              | No       | u64     |
| `APERIO_MAX_BODY_SIZE`                   | Maximum request body payload size allowed in bytes to protect against OOM attacks.                                            | `10485760` (10MB) | No       | usize   |
| `APERIO_MAX_CONCURRENT_REQUESTS`         | Limit on max concurrent in-flight requests processed across all tunnels.                                                      | `100`             | No       | usize   |
| `APERIO_MAX_TUNNELS`                     | Maximum number of concurrent active tunnel client connections.                                                                | `10`              | No       | usize   |
| `APERIO_IP_LIMIT_MAX`                    | The burst size capacity for the per-IP Token Bucket rate limiter.                                                             | `100.0`           | No       | f64     |
| `APERIO_IP_LIMIT_REFILL`                 | Token bucket refill rate (tokens per second) for rate limiting (e.g. `5.0` allows average 300 req/min).                       | `5.0`             | No       | f64     |

### Endpoints

- **`/*` (Fallback)**: Any path not matching `/aperio` routes is proxied to the active tunnel clients.
- **`GET /aperio/ws`**: Secure WebSocket endpoint where the client connects. Requires authentication (HTTP `Authorization: Bearer <token>` or `x-auth-token: <token>`).
- **`GET /aperio`**: HTML admin dashboard interface (available when `APERIO_DASHBOARD` is enabled).
- **`GET /aperio/api/stats`**: JSON stats endpoint displaying connection counters, byte counters, and uptime info (available when `APERIO_DASHBOARD` is enabled).
- **`GET /aperio/api/logs`**: JSON endpoint returning the last 100 request logs (available when `APERIO_DASHBOARD` is enabled).
- **`GET /aperio/health`**: Simple server health verification endpoint (available when `APERIO_DASHBOARD` is enabled).

---

## Client Configuration (`aperio-client`)

The client receives requests from the server and forwards them to a local backend server.

### Environment Variables

| Variable Name                 | Description                                                                                                           | Default Value           | Required | Type           |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------- | ----------------------- | -------- | -------------- |
| `APERIO_SERVER_TOKEN`         | Secret security token matching the server's token.                                                                    | _(None)_                | **Yes**  | String         |
| `APERIO_SERVER`               | Address of your public-facing `aperio-server`. Supports `http`/`https` or `ws`/`wss` protocols.                       | `http://localhost:8080` | No       | String         |
| `APERIO_CLIENT_TARGET`        | Address of the local target backend to forward proxy traffic to.                                                      | _(None)_                | **Yes**  | String         |
| `APERIO_CLIENT_PASS_HOSTNAME` | If set to `1`, passes the original request `Host` header through. Otherwise, overrides it with the local target host. | `0` (default)           | No       | Boolean/String |

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
   $env:APERIO_SERVER="http://localhost:8080"
   $env:APERIO_CLIENT_TARGET="http://localhost:3000"
   ./aperio-client

   # On Linux/macOS
   APERIO_SERVER_TOKEN="super-secret-token" APERIO_SERVER="http://localhost:8080" APERIO_CLIENT_TARGET="http://localhost:3000" ./aperio-client
   ```

3. **Access the Dashboard:**
   Open a browser at `http://localhost:8080/aperio`. Log in using user `aperio` and password `dashboard-password`.

---

## Docker Usage

Both server and client include Docker support.

### Server Docker Run

```bash
docker build -t aperio-server ./aperio-server
docker run -d \
  -p 8080:8080 \
  -e APERIO_SERVER_TOKEN="your-secure-token" \
  -e APERIO_DASHBOARD="1" \
  aperio-server
```

### Client Docker Run

```bash
docker build -t aperio-client ./aperio-client
docker run -d \
  --network="host" \
  -e APERIO_SERVER_TOKEN="your-secure-token" \
  -e APERIO_SERVER="http://your-server-ip:8080" \
  -e APERIO_CLIENT_TARGET="http://localhost:3000" \
  aperio-client
```

---

## License

This project is open-source and free to use.
