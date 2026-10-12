# Getting Started

Aperio publishes a service through one **outbound** connection: the client connects to the server, and visitors' requests come back through it. Two pieces:

- **`aperio-server`** on a machine with a public address, usually behind a TLS-terminating proxy.
- **`aperio-client`** next to the service you are exposing.

## 1. Start the server

```bash
docker run -d --name aperio-server -p 8080:8080 \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -v "$(pwd)/data:/app/data" \
  ghcr.io/co3moz/aperio-server:latest
```

The master token authenticates clients and is the dashboard password of the built-in `aperio` user. `data` keeps the settings, tokens and statistics across restarts. That is the whole configuration of a first run: everything else is changed from the dashboard at `http://your-server-ip:8080/aperio`, under **Settings**, without a restart. To start a server with settings already in place, see [The first start](configuration.md#the-first-start).

## 2. Connect a client

```bash
docker run -d --name aperio-client --network host \
  -e APERIO_SERVER_URL="http://your-server-ip:8080" \
  -e APERIO_SERVER_TOKEN="change-me-to-a-long-random-string" \
  -e APERIO_TARGET="http://localhost:3000" \
  -e APERIO_ACCESS=public \
  ghcr.io/co3moz/aperio-client:latest
```

`--network host` lets a Linux container reach the host's `localhost`; on Docker Desktop, `host.docker.internal` reaches the host instead. `APERIO_ACCESS=public` opens the route to everyone, since routes are closed by default; see [Access](access.md) to put a login in front instead.

Or install the client (`curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh`) and run it directly:

```bash
aperio-client 3000 --server-url https://tunnel.example.com \
  --server-token change-me-to-a-long-random-string --access public
```

## 3. Check it

Open `http://your-server-ip:8080` to reach the service on port 3000, and `/aperio` for the dashboard. If something is wrong, `aperio-client check` tests every hop (the server, its release, the token, the target) and says which one failed.

## More than one service

Write an `aperio.yaml` with a `services:` map, one entry per backend, each with its own hostname or path. The services share one connection to the server. See [Configuration](configuration.md#a-file).

## Next

- Put the server behind TLS and tell it which proxy to trust: [Deployment](deployment.md#in-front-of-the-server).
- Give each client its own scoped token instead of the master token: [Tokens and Organizations](tokens-and-organizations.md).
- Give each service its own hostname: [Traffic](traffic.md#routing).

Runnable examples start with [`simple`](examples/simple/).
