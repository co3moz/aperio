# Simple

> **Concept:** [Getting Started](../../getting-started.md).

The smallest server and client configuration: one master token and one local backend.

Run the server, then start the client next to its `aperio.yaml`:

```bash
aperio-server            # reads ./aperio-server.yaml
aperio-client            # reads ./aperio.yaml
```

> **Before running:** Start a backend on port 3000, replace the token in both files, and point `server.url` to your reachable Aperio server. `tunnel.example.com` is only a placeholder. See [Getting Started](../../getting-started.md).

Requests reaching the configured server are forwarded to `http://localhost:3000`.
