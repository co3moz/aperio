# Public Expose

> **Concept:** [Emergency Tunnels](../../emergency-tunnels.md), [Public exposes](../../public-exposes.md).

An expose listener cuts the binder out of the tunnel picture: the server itself opens a public TCP (or UDP) port and relays every accepted connection into a declared tunnel, useful for exposing SSH or a game server without running `aperio-client bind` anywhere.

The listener names the tunnel and the organization whose client may claim it (`master` here, the organization the client below connects as), so the claim is settled by identity rather than a secret copied into two places. Revoking the tokens of that organization closes the port's source. Listeners are managed resources: create this one from the dashboard's Tunnels page, or with the API using [`expose.json`](expose.json):

```bash
APERIO_SERVER_TOKEN=apr_public_expose_change_me aperio-server
aperio-client api expose create --file expose.json   # with an admin key, see the linked guide
aperio-client                                        # reads ./aperio.yaml
```

The connection goes to a healthy client that declares the tunnel, and `encrypt: true` tunnels are excluded, because a raw public socket cannot run the client-side handshake. The exposed port is **public**, so keep the real authentication (SSH keys, database passwords) on the backend itself, and narrow `allowed_ips` in the listener where you can.

With this running, `ssh -p 2222 user@tunnel.example.com` lands on the declaring machine's local sshd.

> **Before running:** Start SSH on the declaring machine, replace the example hostname, and allow inbound TCP port 2222 to the Aperio server. See [Public exposes](../../public-exposes.md).
