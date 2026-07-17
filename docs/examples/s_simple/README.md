# Simple

The minimal working pair: a server with just a master token, and a client that forwards all traffic to a single local backend.

Run the server, then start the client next to its `aperio.yaml`:

```bash
aperio-server            # reads ./aperio-server.yaml
aperio-client            # reads ./aperio.yaml
```

Requests reaching `https://tunnel.example.com` are proxied to `http://localhost:3000`.
