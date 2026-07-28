# Emergency Tunnels

> **Concept:** [Emergency Tunnels](../../emergency-tunnels.md).


A break-glass path to services you deliberately do **not** expose (a database, SSH, an internal DNS resolver). The client next to the private service declares them as `tunnels:`, nothing is routed or exposed publicly. When you need one, you bind it from anywhere with a second client, by the tunnel's **name**.

Files in this folder:

- `aperio.yaml`, the **declaring** side, run next to the private services. It pins a `client_id` so the id survives restarts.
- `aperio-binder.yaml`, the **binder** side, run wherever you need access (rename it to `aperio.yaml` on that machine, or start with `--config aperio-binder.yaml`). Plain `aperio-client --bind-tunnels` (no id) binds every entry in the file.
- `aperio-server.yaml`, the shared server.

Each bound tunnel becomes a local `127.0.0.1` listener, by default on the same port as the declared target unless the entry names one (a privileged port falls back to one derived from the name). The binder needs the declaring client's own token, or a token in its organization carrying `allow_bind`; `GET /aperio/tunnels` and the dashboard's Tunnels page list what a token may bind. Tunnel lists are discovered once at binder startup, so re-run the binder after changing a declaring client's `tunnels:` list. See [Tunnels](../../emergency-tunnels.md).
