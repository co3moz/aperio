# Messages Between Clients

> **Concept:** the clients of one organization signalling each other over the tunnel they already hold.

Clients can be reached from outside, and can reach a private service through a peer. This is the third thing: telling each other that something happened. One publish, and every client of the organization that asked for that topic hears about it, wherever it is.

Nothing new is dialled and nothing new is opened. The message goes over the WebSocket connection the client already maintains, so it works from behind NAT, needs no port, and is authenticated by the token the client connected with.

## Publishing

From anywhere with an admin session or key:

```bash
aperio-client api POST /publish -d '{"topic":"deploy/web","payload":"v1.9.2"}'
```

Or over HTTP directly:

```bash
curl -u aperio:$APERIO_SERVER_TOKEN -X POST https://tunnel.example.com/aperio/api/publish \
  -H 'content-type: application/json' \
  -d '{"topic":"deploy/web","payload":"v1.9.2"}'
```

The answer says how far it went: `{"topic":"deploy/web","clients":3,"connections":3}`. A publish to a topic nobody subscribes to is not an error, it reaches nobody.

Use `payload_base64` instead of `payload` for anything that is not text. The ceiling is 256 KB; a message is a signal, and moving data is what tunnels are for.

## Subscribing

In the client's `aperio.yaml`:

```yaml
subscribe:
  - deploy/#
  - $aperio/client/#
```

Filters are MQTT's, because that is the syntax people already know: `+` matches exactly one level, `#` matches the rest and is only legal last. `deploy/+` matches `deploy/web` but not `deploy/web/eu`; `deploy/#` matches both, and `deploy` itself.

**A subscription belongs to the client process, not to its connections.** A client with a `services:` list holds one tunnel connection per service and still receives each message once. There is nothing to deduplicate.

## Receiving: the local face

A client subscribing is only half of it; something has to hand the message to your application. Set a local address and the client speaks plain HTTP on it:

```yaml
messages_listen: 127.0.0.1:1888
```

Subscribe with server-sent events, from a shell or from any language's standard library:

```bash
curl -N 'http://127.0.0.1:1888/subscribe?topic=deploy%2F%23'
```

```
id: 4f1c…
event: deploy/web
data: djEuOS4y
```

`event:` is the topic the message was published on, `data:` is the payload in Base64 (a payload is bytes and an SSE field is a line), and `id:` is the server's message id, the same across every delivery of one publish. Subscribing this way is enough: the filter does not have to be in the config file, the client tells the server about it when you attach.

Publishing works the same way, without an admin credential, because the client's own token is what carries it:

```bash
curl -X POST 'http://127.0.0.1:1888/publish?topic=deploy%2Fweb' --data 'v1.9.2'
```

Remember to escape `/` as `%2F` and `#` as `%23` in the query string.

The face is loopback because anything that can open a socket on that host is already running next to the client's credentials. Binding it elsewhere is possible and is warned about at startup.

## Server events

Everything the server already reports through webhooks is also published on the reserved `$aperio/` namespace, with the event name's underscores as topic levels:

| Event | Topic |
| --- | --- |
| `client_connected` | `$aperio/client/connected` |
| `client_draining` | `$aperio/client/draining` |
| `token_created` | `$aperio/token/created` |
| `tunnel_bound` | `$aperio/tunnel/bound` |

The payload is the event's JSON, the same document a webhook would receive. So a client can react to infrastructure without standing up an HTTP receiver for it:

```yaml
subscribe:
  - $aperio/client/#
```

Two rules keep the namespace meaningful. A client cannot publish into it, so a `$aperio/` message always came from the server. And a bare `#` does **not** match it, so subscribing to everything while debugging does not enroll you in infrastructure events you never asked to parse; ask for them by name.

## Who may use which topics

Messaging is a token capability, off unless the token carries it. A dynamic token has a **topics** list of filters, and one rule covers both directions: a token that may subscribe to `deploy/#` may publish on it, and a token with an empty list can do neither.

```bash
aperio-client api POST /tokens -d '{"name":"deploy-runner","topics":["deploy/#"]}'
```

The list is a fence, not a wish: a subscription is permitted when a granted filter *covers* it. `deploy/#` covers `deploy/web` and `deploy/+`, and does not cover `#` — otherwise subscribing to everything would be the way around a scope that named one subtree. `*` is accepted and stored as `#`, since that is how the hostname and path lists spell "everything".

The master token is unrestricted, like everywhere else. An ephemeral tunnel token carries no topics: it is a guest of the organization for one hostname and has no business signalling the rest of it.

Note the convention differs from `hostnames` and `paths`, where an empty list means unrestricted. That is deliberate: those fence a capability every token already had, while this one is new, and a new capability that switches itself on for every token predating it is how a permission model stops meaning anything.

A refused subscription is reported by name in the client's log rather than dropped, so a token missing a topic looks like a missing permission and not like a message that never arrived.

## What this is not

- **There is no delivery guarantee.** A message reaches the clients connected when it is published. Nothing is stored for one that is away, and a client that reconnects does not receive what it missed. That is deliberate: this serves reacting to something happening now, and replaying an hour-old event to a machine that just came back is a bug rather than a service.
- **A message never crosses an organization**, and the master organization is not a superset of its children.
- **A slow subscriber misses messages rather than slowing everyone down.** The server drops its copy when its connection is not keeping up; the local face says so with a `: missed N message(s)` comment in the stream.

## Runnable examples

- [`mqtt`](examples/mqtt/): the other shape, an MQTT broker of your own carried by a tunnel, for when you want a broker's semantics (retained messages, QoS, offline sessions).
