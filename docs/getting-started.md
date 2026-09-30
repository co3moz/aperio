# Getting Started

Aperio exposes a local or private-network service through one **outbound** WebSocket connection. The client connects to the public server, and visitor requests return through that tunnel. Your network does not need an inbound port.

You need two pieces:

- **`aperio-server`** on a machine with a public address (usually behind a TLS-terminating proxy such as Traefik, Caddy, or nginx).
- **`aperio-client`** next to the service you want to expose.

## 1. Start the server

```bash
# on the public box
docker run -d --name aperio-server \
  -p 8080:8080 \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -v "$(pwd)/data:/app/data" \
  ghcr.io/co3moz/aperio-server:latest
```

The master token authenticates clients and also serves as the dashboard password for the built-in `aperio` admin. The `data` directory in your current working directory keeps tokens, statistics, audit events, and webhooks across restarts.

## 2. Connect a client

With Docker on Linux:

```bash
# on the machine next to the service you are exposing
docker run -d --name aperio-client \
  --network host \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_TARGET="http://localhost:3000" \
  -e APERIO_PUBLIC=1 \
  ghcr.io/co3moz/aperio-client:latest
```

`--network host` lets this Linux container reach a backend on the host through
`localhost`. On another Docker setup, use an address the container can reach
and adjust its network settings. On Docker Desktop, `host.docker.internal` is
one way to reach a backend on the host.

`APERIO_PUBLIC=1` makes the route public. Since 0.10.0, routes are closed by
default. Remove this setting and configure visitor authentication when the
site should require a login; see [Tokens & Authentication](tokens-and-auth.md).

Or run the client directly after installing it with `curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh`. The HTTPS URL below requires DNS and a TLS proxy in front of the server. Use the same token as step 1, or mint a scoped token first:

```bash
# on the machine next to the service you are exposing
aperio-client 3000 --server-url https://tunnel.example.com \
  --server-token change-me-to-a-long-random-string --public
```

## 3. Verify

Open `http://your-server-ip:8080` to reach the service on local port 3000.
The dashboard is at `/aperio` (user `aperio`, password: your master token).

If something doesn't work, run `aperio-client check`. It checks server health,
compares client/server versions, performs a token handshake, and probes the
local target. Exit code `0` means all checks passed. See [Configuration](configuration.md#cli)
for details.

## More than one service?

A single client process can expose several targets. Add a `services:` list to
`aperio.yaml`; each entry can set its target, hostname or path, and health
probe. The client opens a tunnel for each service. See [Multiple services](configuration.md#multiple-services).

## Next steps

- Put the server behind TLS before using it across an untrusted network. Configure proxy trust for the reverse proxy you actually use; see [Production Hardening](production-hardening.md#transport--network).
- Give each client its own hostname, see [Routing & Load Balancing](routing-and-load-balancing.md).
- Mint scoped tokens instead of sharing the master token, see [Tokens & Authentication](tokens-and-auth.md).
- Browse every setting on both sides, see the [Configuration Reference](configuration.md).

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`simple`](examples/simple/): minimal one-target pair
