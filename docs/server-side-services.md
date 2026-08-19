# Serving From the Server (`server_side:`)

Every service is relayed by default. A request arrives at the server, goes out
over the tunnel, the client connects to the target, and the answer comes back
the same way. That is what a tunnel is for: the target is somewhere only the
client can reach.

Sometimes it is not. A client can sit on the same network as the server, or
the target can be a service the server already talks to for other reasons. The
relay is then two hops bought for nothing, the one to the client and the one
from the client to the target.

`server_side: true` removes both.

```yaml
# aperio.yaml, on the client
services:
  - name: internal_api
    target: http://10.0.0.5:8080
    hostname: api.example.com
    server_side: true
```

The service is still *declared* by the client, and everything the server does
before the last hop is unchanged: it is routed by the same binds, gated by the
same `auth:` and `allowed_ips:`, counted in the same metrics, captured by the
inspector, and written to the same access log. What moves is where the request
goes at the end.

## It takes two permissions, and neither is enough alone

**The operator names the destinations.** Nothing is reachable this way until
the server says so:

```yaml
# aperio-server.yaml
server_side_targets:
  - 10.0.0.0/8
  - "*.internal.example.com"
```

Unset permits nothing. That is the opposite of `outbound.allowlist`'s
empty-means-permissive default, and it is deliberate: that list governs
callbacks the server makes on its own initiative, while this one opens a
request and response channel that a tenant steers, so an operator who never
configured it cannot be pointed anywhere.

**The token permits the asking.** `allow_server_side` on the tunnel token,
separate from the list, so you can allow a destination without allowing every
tenant to reach it.

## What it refuses, and why refusing is the point

A service that asks for a target the list does not name is **refused, and left
out of routing**. It does not quietly fall back to the tunnel.

That looks harsh until you notice why a service asks for this in the first
place: usually because the client *cannot* reach the target itself. Relaying
would not produce a slower service, it would produce connection errors from a
backend nobody can see from either side. The refusal names the target and the
setting instead.

The same reasoning covers the older-server case. A server that predates this
feature does not refuse the request, it ignores the field and relays, so the
client checks what the server announced on the handshake and holds the service
back rather than asking hopefully. The log says which side has to move.

Two more refusals, both at client startup where the message can name the file:

- **`serve:` cannot be combined with it.** Those files are on the client, and
  a server reaching the target cannot serve them.
- The single-service (top-level) config shape does not offer the key at all.
  It is a `services:` key, because the top-level spellings are the deprecated
  form.

## WebSockets

They work, and they are spliced rather than relayed: the server opens its own
socket to the target and passes frames both ways. The same gates run first,
and the scheme follows the target's, so an `https://` target gets a `wss://`
socket rather than a silently plaintext one. The upgrade is accepted only
after the target has answered, so an unreachable backend gives the visitor a
`502` it can read instead of a socket that opens and closes at once.

## What you give up

- **Failover and load balancing across connections** do not apply to the last
  hop. There is no second client to fail over to, because no client is
  involved: if the target is down, it is down.
- **Backend health as the client sees it** is not what decides here. The
  client's probe reaches the target from the client's network, which is not
  the network the request now takes.
- **The client's own bandwidth pacing** governs the tunnel, and this traffic
  does not use it.

## Security

This is the boundary the [threat model](threat-model.md#6-server--a-clients-target-server_side)
calls *Server → a client's target*, and it is worth reading before turning it
on in a multi-tenant deployment. The short version: the allowlist is judged on
the target as written and never on what a name resolves to, nothing a visitor
sends can move the request to another host, and the hop-by-hop header strip
the relayed path performs applies here too, so this is not a way around it.

Outbound TLS here verifies against the host's certificate store and leaves
through the same egress proxy as the server's other outbound calls, which
[Architecture](architecture.md#outbound-tls-and-what-verifies-it) explains.
