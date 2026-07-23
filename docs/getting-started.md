# Getting Started

Aperio exposes a service running on your machine (or inside a private network) to the public internet through a single **outbound** WebSocket connection. Nothing on your side accepts inbound traffic, the client dials out to the server and requests flow back through that tunnel.

You need two pieces:

- **`aperio-server`** on a machine with a public address (usually behind a TLS-terminating proxy such as Traefik, Caddy, or nginx).
- **`aperio-client`** next to the service you want to expose.

## 1. Start the server

```bash
docker run -d --name aperio-server \
  -p 8080:8080 \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -v ./data:/app/data \
  ghcr.io/co3moz/aperio-server:latest
```

The token is the master credential: it authenticates tunnel clients and doubles as the dashboard admin password. The `./data` volume persists dynamic tokens, statistics, the audit log, and webhooks across restarts, don't skip it.

## 2. Connect a client

With Docker:

```bash
docker run -d --name aperio-client \
  --network host \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_TARGET="http://localhost:3000" \
  ghcr.io/co3moz/aperio-client:latest
```

Or with the CLI (installed via `curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh`):

```bash
aperio-client 3000 --server-url https://tunnel.example.com --server-token apr_xxxxxxxx
```

## 3. Verify

Open `http://your-server-ip:8080`, requests are proxied to your local port 3000. The admin dashboard lives at `/aperio` (user `aperio`, password: your token).

If something doesn't work, run `aperio-client check`: it verifies the server's health endpoint, compares client/server versions, performs a real token handshake, and probes your local target, exit code 0 means every hop is green.

## More than one service?

A single client process can expose several targets, put a `services:` list in `aperio.yaml` (each entry with its own `target`, `hostname`/`path`, and health probe) and the client opens one tunnel per entry. See [Multiple services](configuration.md#multiple-services) in the configuration reference.

## Next steps

- Put the server behind TLS and set `APERIO_TRUST_PROXY=1` (yaml `trust_proxy`), see [Tokens & Authentication](tokens-and-auth.md) for why the master token should never travel in plaintext.
- Give each client its own hostname, see [Routing & Load Balancing](routing-and-load-balancing.md).
- Mint scoped tokens instead of sharing the master token, see [Tokens & Authentication](tokens-and-auth.md).
- Browse every setting on both sides, see the [Configuration Reference](configuration.md).

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`s_simple`](examples/s_simple/): minimal one-target pair
