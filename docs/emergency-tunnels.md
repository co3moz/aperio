# Tunnels (bind-tunnels & public expose)

Reaching services you deliberately do **not** expose: a database, an internal admin port, an SSH daemon. A running client declares them as *tunnels*; nothing about them is routed or exposed publicly. When you need one, a broken deployment, a dead VPN, a 3 a.m. incident, you bind it from anywhere with a second client authorized to do so.

A tunnel is addressed by its **name** within an organization. The name is the only handle you write: the server-side connection id is a fresh UUID per connection and a client suffixes its own id per service, so neither survives in a configuration file.

This is not designed as a high-load proxy. It is a break-glass path, and a deliberately narrow one.

## Declaring tunnels

On the machine next to the private service, the client's `aperio.yaml` gains a `tunnels:` list:

```yaml
server:
  url: https://tunnel.example.com
  token: apr_xxxxxxxxxxxxxxxx
services:                         # optional, a client may declare only tunnels
  - target: http://localhost:3000
tunnels:
  - name: mongo                   # the handle binders address
    target: 127.0.0.1:27017       # MongoDB, never exposed
    protocol: tcp                 # tcp (default) or udp
  - name: ssh
    target: 127.0.0.1:22
  - name: dns
    target: 127.0.0.1:53          # e.g. an internal DNS resolver
    protocol: tcp/udp             # one tunnel, both transports
    idle_timeout: 10              # the udp half: relay expiry in seconds (default 60)
```

`protocol:` takes `tcp` (the default), `udp`, or `tcp/udp` for a service that is genuinely both, DNS being the obvious one. A combined tunnel is **one** tunnel: one name, one entry in the binder, one local port, with a listener opened on each transport (they are separate port spaces, so the number is shared without conflict). Declaring the same target twice, once per protocol, still works and gives you two independently addressable tunnels; use it when you want them bound to different local ports. `encrypt: true` is refused on a combined tunnel, since the handshake is TCP-only and accepting it would leave the datagram half in the clear under a flag that says otherwise.

Names are unique within the organization and may contain letters, digits, `-`, `_` and `.`. A name shaped like a UUID is refused, so names can never be confused with client ids. Leaving `name:` out derives one from the target and protocol (`127.0.0.1:5432` tcp becomes `127-0-0-1-5432-tcp`), which keeps older files working and still gives every tunnel a stable handle; two tunnels that would resolve to the same name are a startup error.

A config with only `tunnels:` (no `target`, no `services:`) is valid: the connection then exists purely for emergencies.

## Finding what you can bind

`GET /aperio/tunnels`, with a tunnel token, lists every tunnel that token may bind: name, protocol, target, the declaring client, how many connections can serve it, and whether any of them can right now. The dashboard's **Tunnels** page is the same list, and its copy button hands you a ready `bind-tunnels:` block.

This is what makes a name usable as an address. Without it you had to already know a client id before you could ask anything at all.

## Binding tunnels

From any machine, start a client in bind mode:

```bash
aperio-client --bind-tunnels mongo \
  --server-url https://tunnel.example.com \
  --server-token apr_xxxxxxxxxxxxxxxx
```

Connections are relayed through the server to the declaring client, which dials its local target:

```
mongosh → 127.0.0.1:27017 → aperio-server → declaring client → 127.0.0.1:27017
```

### Local configuration

The binder's own `aperio.yaml` says what to bind and where. A bare number is the short form, since naming the local port is the only thing most entries do:

```yaml
server:
  url: https://tunnel.example.com
  token: apr_xxxxxxxxxxxxxxxx     # in the declaring client's organization, with allow_bind
bind-tunnels:
  mongo: 15000                    # listen on 127.0.0.1:15000
  dns: 5300
  pg-main:
    port: 15432
    address: 127.0.0.1            # default; anything else is warned about
    psk: a-long-random-string     # only for an encrypt: true tunnel
```

`aperio-client --bind-tunnels` with no value binds every entry. With no `bind-tunnels:` section at all it binds everything the token may reach, which is safe precisely because the server decides what is on that list.

**Local ports.** A `port:` wins. Otherwise the declared target's port is reused, as it always was, unless that port is privileged (below 1024) or the target has none to parse, in which case a port derived from the tunnel name is used: stable across restarts, so whatever connects to it never needs reconfiguring. Every chosen port is logged at startup.

- An entry that cannot be resolved is reported at startup, next to the names that *can* be bound, and the other entries still come up. Three of four tunnels during an incident beats none.
- If two entries would claim the same local port, the second is skipped and the error names both.
- A key that is not a tunnel name is read as a peer's client id, which binds every tunnel that peer declares. That is the older spelling and still works, including its `override:` map from declared target to local port.

## The rules

- **Binding is a capability.** Three ways in: the master token; the very same token the declaring client used; or a token in the same organization carrying `allow_bind`. That last one is the point of the capability, and it defaults to off: reaching a database for ten minutes should not mean handing over the credential that publishes services as that client.
- **Organizations fence it.** `allow_bind` never crosses an organization. Only the master token binds across all of them.
- **The declaring client dials only what it declared.** A `TcpOpen`/`UdpOpen` for an address outside its own `tunnels:` list is refused, the tunnel analogue of the HTTP SSRF guard. A compromised server cannot turn the client into a generic port scanner.
- **Failures say which gate closed.** Not connected, not permitted, and no path available are three different answers, because they send you to three different places.
- Streams are audited on the server (`tcp_stream_opened` / `udp_stream_opened`, with client and target).

## End-to-end encryption

By default the server decodes and re-encodes tunnel frames, so a compromised server could read relayed bytes. A TCP tunnel declared with `encrypt: true` closes that hole: the two **clients** run an ephemeral X25519 key exchange as the first frame of every stream and seal everything after it with ChaCha20-Poly1305, the server relays only ciphertext.

```yaml
# Declaring side (aperio.yaml)
tunnels:
  - target: 127.0.0.1:5432
    encrypt: true
    psk: a-long-random-string     # optional, never sent anywhere

# Binder side (aperio.yaml)
bind-tunnels:
  pg-main:
    psk: a-long-random-string     # must match the declaring side
```

A passive server learns nothing either way. An **active** server could man-in-the-middle the plain key exchange, which is what the optional `psk` closes: it is mixed into the key derivation on both ends (never transmitted, note it is stripped from the tunnel announcement), so a MITM without it derives mismatched keys and the very first sealed frame fails to open; the stream dies instead of leaking data. Tampered, reordered, or replayed frames also fail closed. `encrypt` is TCP-only; the binder discovers the flag via tunnel discovery, so only the PSK needs out-of-band coordination.

## UDP tunnels

A `protocol: udp` (or `tcp/udp`) declaration binds a local **UDP** socket on the consumer side. Each distinct local peer (source address) gets its own relay stream through the server, so responses find their way back to the right peer, enough for DNS lookups, statsd counters, or a WireGuard handshake in a pinch. The relay is deliberately **best-effort**, matching UDP semantics: when any hop is congested, datagrams are dropped rather than queued; a relay with no traffic in either direction expires after 60 seconds by default, tune it per tunnel with `idle_timeout: <seconds>` on the declaration (e.g. `300` for long-lived WireGuard sessions, `10` for DNS; the binder picks the value up automatically via tunnel discovery, and the next datagram after an expiry opens a fresh relay); and datagrams above 64 KiB are not relayed. Don't expect wire-rate throughput, it's a break-glass path, same as TCP.

## Public expose

A tunnel normally needs a binder peer. An `expose:` entry in the server's `aperio-server.yaml` cuts the binder out: the server itself opens a raw public TCP port and relays every accepted connection into a declared tunnel, useful for exposing SSH or a game server without running `--bind-tunnels` anywhere.

```yaml
# aperio-server.yaml
expose:
  - protocol: tcp
    port: 2222
    tunnel: ssh                   # the tunnel's name
    token: bastion-host           # the token whose client may claim it

# client aperio.yaml
tunnels:
  - name: ssh
    target: 127.0.0.1:22
```

The claim is settled by **identity**: the port accepts the named tunnel only from a client authenticated with the named token. Revoking that token closes the port's source, the audit trail names an owner, and another client in the same organization cannot take the name first and receive the traffic. Omitting `token:` accepts only tunnels declared with the master token, so a named token is what lets an organization own an exposed port.

The older spelling, a shared secret repeated in both files, still works:

```yaml
expose:
  - port: 2222
    key: a-long-random-shared-secret   # minimum 8 characters

tunnels:
  - target: 127.0.0.1:22
    expose: a-long-random-shared-secret
```

It names no owner and cannot be revoked, which is why `tunnel:` + `token:` is preferred. The key travels only inside the tunnel handshake and is never revealed through tunnel discovery.

Deliberate limits: the connection goes to the **first** healthy client that matches (no load balancing), TCP only (a public UDP port is an amplification surface and a separate decision), and `encrypt: true` tunnels are excluded, since a raw public socket cannot run the client-side handshake. Remember the exposed port is **public**: anyone who can reach it talks straight to your backend, so keep the real authentication (SSH keys, database passwords) on the backend itself.

## Limitations

- Tunnel lists are discovered once when the binder starts; re-run the binder after changing a declaring client's `tunnels:` list.
- Tunnel names and client ids are self-reported by clients. Tokens and organizations gate everything, but treat both as identifiers, not secrets.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`emergency_tunnels`](examples/emergency_tunnels/): break-glass TCP tunnels
- [`encrypted_tunnels`](examples/encrypted_tunnels/): end-to-end encrypted tunnels
- [`mqtt`](examples/mqtt/): a broker every client can reach, and nothing else
- [`public_expose`](examples/public_expose/): raw public TCP port
