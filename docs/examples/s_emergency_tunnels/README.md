# Emergency Tunnels

A break-glass path to services you deliberately do **not** expose (a database, SSH, an internal DNS resolver). The client next to the private service declares them as `tunnels:`, nothing is routed or exposed publicly. When you need one, you bind it from anywhere with a second client, the **same token**, and the declaring client's **id**.

Files in this folder:

- `aperio.yaml`, the **declaring** side, run next to the private services. It pins a `client_id` so the id survives restarts.
- `aperio-binder.yaml`, the **binder** side, run wherever you need access (rename it to `aperio.yaml` on that machine, or start with `--config aperio-binder.yaml`). Plain `aperio-client --bind-tunnels` (no id) binds every entry in the file.
- `aperio-server.yaml`, the shared server.

Each declared tunnel becomes a local `127.0.0.1` listener on the binder, by default on the same port as the declared target, unless an `override` maps it elsewhere. The binder must present exactly the token the declaring client connected with, and tunnel lists are discovered once at binder startup, re-run the binder after changing a peer's `tunnels:` list. See [Emergency Tunnels](../../emergency-tunnels.md).
