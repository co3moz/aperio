# Emergency Tunnels (bind-tunnels)

An emergency fallback path for reaching services you deliberately do **not** expose: a database, an internal admin port, an SSH daemon. A running client declares them as *tunnels*; nothing about them is routed or exposed publicly. When you need one — a broken deployment, a dead VPN, a 3 a.m. incident — you bind it from anywhere with a second client, the **same token**, and that client's **id**.

This is not designed as a SaaS building block or a high-load proxy. It is a break-glass tool.

## Declaring tunnels

On the machine next to the private service, the client's `aperio.yaml` gains a `tunnels:` list:

```yaml
server:
  url: https://tunnel.example.com
  token: apr_xxxxxxxxxxxxxxxx
target: http://localhost:3000     # optional — a client may declare only tunnels
tunnels:
  - target: 127.0.0.1:27017       # MongoDB, never exposed
    protocol: tcp                 # only tcp is supported for now
  - target: 127.0.0.1:22
```

The client announces the list to the server via its heartbeat and logs its **client id** at startup (`- Client ID: ...`). Make the id survive restarts with `--client-id <uuid>` / `APERIO_CLIENT_ID` / yaml `client_id` — otherwise a new random UUID is generated per run and your bind configuration goes stale. A config with only `tunnels:` (no `target`, no `services:`) is valid: the connection then exists purely for emergencies.

## Binding tunnels

From any machine, start a client in bind mode:

```bash
aperio-client --bind-tunnels <client-id> \
  --server-url https://tunnel.example.com \
  --server-token <the SAME token that client connected with>
```

Every declared tunnel becomes a local `127.0.0.1` listener — by default on the **same port** as the declared target (`127.0.0.1:27017` above listens on local port 27017). Connections are relayed through the server to the declaring client, which dials its local target:

```
mongosh → 127.0.0.1:27017 → aperio-server → declaring client → 127.0.0.1:27017
```

### Local configuration & port overrides

Several clients (and per-target port overrides) are configured in the binder's own `aperio.yaml`; plain `aperio-client --bind-tunnels` (no id) binds every entry:

```yaml
server:
  url: https://tunnel.example.com
bind-tunnels:
  '018f3c1e-...-client-a':
    token: apr_client_a_token
    override:
      '127.0.0.1:27017': 15000    # listen on 127.0.0.1:15000 instead
  '018f3c1e-...-client-b':
    token: apr_client_b_token
```

- If two bound clients would claim the same local port, neither listener is opened and the error names both — define an `override` rule for one of them.
- If a local port is already taken by something else, that bind fails and is skipped (the rest keep working).
- A peer that is not connected yet is retried every 15 seconds, so the binder can be started first.

## The rules

- **Same token.** The binder must present exactly the token the declaring client connected with. A different valid token gets `403`. The reasoning: whoever operates that client already holds its token; nothing new is granted.
- **Explicit client id, always.** Even the master token must name a client id — there is no "list all tunnels" call. (The master token may bind any client's tunnels; dynamic tokens only clients using the very same token.)
- **The declaring client dials only what it declared.** A `TcpOpen` for an address outside its own `tunnels:` list is refused — the TCP analogue of the HTTP SSRF guard. A compromised server cannot turn the client into a generic port scanner.
- Streams are audited on the server (`tcp_stream_opened`, with client and target).

## Limitations

- `protocol: tcp` only for now; `udp` declarations are rejected at startup.
- Tunnel lists are discovered once when the binder starts (or when the peer first appears); re-run the binder after changing a peer's `tunnels:` list.
- Client ids are self-reported by clients. Tokens gate everything, but treat ids as identifiers, not secrets.
