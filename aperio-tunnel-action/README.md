# Aperio Tunnel Action

Exposes a port of your GitHub Actions runner to the public internet through an
[Aperio](https://github.com/co3moz/aperio) tunnel — the building block for
per-PR preview environments.

The action calls `POST /aperio/api/tunnels` on your Aperio server to mint an
**ephemeral, hostname-scoped token**, runs the `aperio-client` container for
the rest of the job, and automatically **revokes the token** when the job
finishes (a TTL acts as a safety net if cleanup never runs).

## Usage

```yaml
jobs:
  preview:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Start the app
        run: docker compose up -d   # listens on 127.0.0.1:3000

      - name: Open tunnel
        id: tunnel
        uses: co3moz/aperio/aperio-tunnel-action@master
        with:
          server-url: https://tunnel.example.com
          server-token: ${{ secrets.APERIO_SERVER_TOKEN }}
          port: 3000
          hostname: pr-${{ github.event.number }}.example.com

      - name: Comment preview URL
        run: echo "Preview ready at ${{ steps.tunnel.outputs.url }}"

      - name: Run smoke tests against the public URL
        run: curl --fail ${{ steps.tunnel.outputs.url }}/health
```

Omit `hostname` to get a random subdomain — requires
`APERIO_RANDOM_SUBDOMAIN="*.example.com"` on the server.

## Inputs

| Input | Required | Default | Description |
|---|---|---|---|
| `server-url` | ✔ | — | Base URL of the Aperio server |
| `server-token` | ✔ | — | Master token (store as a secret) used to provision the tunnel |
| `port` | one of | — | Local port to expose (`http://127.0.0.1:<port>`) |
| `target` | one of | — | Full target URL; overrides `port` |
| `hostname` | | random | Hostname to bind, e.g. `pr-123.example.com` |
| `name` | | `<repo>-run-<id>` | Token label shown in the dashboard |
| `ttl-seconds` | | `3600` | Token lifetime (safety net) |
| `client-image` | | `ghcr.io/co3moz/aperio-client:latest` | Tunnel client image |
| `wait-timeout` | | `60` | Seconds to wait for the client to connect |

## Outputs

| Output | Description |
|---|---|
| `url` | Public URL of the tunnel (`https://<hostname>`) |
| `hostname` | Hostname bound to the tunnel |
| `tunnel-id` | ID of the provisioned tunnel token |

## Requirements

- Linux runner with Docker (standard `ubuntu-latest` works)
- An Aperio server reachable from the runner; the tunnels API works even when
  the dashboard is disabled
