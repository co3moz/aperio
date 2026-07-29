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

Add `"qos": 1` for at-least-once. See [Delivery](#delivery) for what that does and does not promise.

## Subscribing

In the client's `aperio.yaml`:

```yaml
subscribe:
  - deploy/#
  - $aperio/client/#
```

Filters are MQTT's, because that is the syntax people already know: `+` matches exactly one level, `#` matches the rest and is only legal last. `deploy/+` matches `deploy/web` but not `deploy/web/eu`; `deploy/#` matches both, and `deploy` itself.

A client may hold at most **64 filters**; a subscription past that is refused by name rather than silently ignored. The limit is there because a filter costs a string and a linear match on every publish, so a loop in someone's code should not turn into unbounded server memory.

**A subscription belongs to the client process, not to its connections.** A client with a `services:` list holds one tunnel connection per service and still receives each message once. There is nothing to deduplicate.

## Reacting: running a command

A subscription can run something itself, without an application attached:

```yaml
subscribe:
  - deploy/web                      # listen; the local face delivers it
  - topic: deploy/api               # listen, and run this
    run: ./deploy.sh
    timeout: 120                    # seconds before the run is killed (default 60)
    max_concurrent: 1               # runs at once (default 1)
```

The message body arrives on the command's **stdin**. `APERIO_MESSAGE_TOPIC` and `APERIO_MESSAGE_ID` are set in its environment. The command runs through the shell, so `run: systemctl reload nginx` works as written.

**This is a remote-execution primitive, and it is shaped accordingly.** A message published by another client of the organization causes a command to run on this machine, so:

- **The payload never reaches the command line.** It is only ever stdin and environment, so a message cannot become part of the command no matter what it contains or how it is quoted.
- **Concurrency is capped, and the excess is dropped rather than queued.** A publisher in a loop cannot fork a thousand processes, and a queue for a command that cannot keep up is the same problem one step later with the memory growing instead.
- **Every run is timed**, so a command that hangs does not hold the subscription's slot forever.
- **It is opt-in per topic**, in a file you wrote, and bounded by the publishing token's `topics` on the server side. Give the tokens that may reach a topic like this the narrowest scope you can.
- **Every run is logged**, started and finished, with the topic and the exit status.

`run:` cannot be set from the environment: `APERIO_SUBSCRIBE` carries filters only. What may execute on a machine belongs in a file an operator wrote, not in a variable a process inherited.

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

`event:` is the topic the message was published on, `data:` is the payload in Base64 (a payload is bytes and an SSE field is a line), and `id:` is the server's message id, the same across every delivery of one publish. Subscribing this way is enough: the filter does not have to be in the config file, the client tells the server about it when you attach, and gives it back when you leave, so a session of `curl -N` on one topic after another does not spend the client's filter budget on subscriptions nobody holds. A filter from `subscribe:` is the client's for as long as it runs, however many local subscribers come and go.

Publishing works the same way, without an admin credential, because the client's own token is what carries it:

```bash
curl -X POST 'http://127.0.0.1:1888/publish?topic=deploy%2Fweb' --data 'v1.9.2'
```

Remember to escape `/` as `%2F` and `#` as `%23` in the query string.

The face is loopback because anything that can open a socket on that host is already running next to the client's credentials. Binding it elsewhere is possible and is warned about at startup.

## Receiving: the MQTT face

For an application that would rather use the MQTT client library it already has:

```yaml
messages_mqtt_listen: 127.0.0.1:1883
```

Then connect as you would to any broker. In Node:

```js
const mqtt = require('mqtt')
const client = mqtt.connect('mqtt://127.0.0.1:1883')
client.on('connect', () => client.subscribe('deploy/#'))
client.on('message', (topic, payload) => console.log(topic, payload.toString()))
client.publish('deploy/web', 'v1.9.2')
```

**MQTT exists only between your application and the client.** Nothing on the wire to the Aperio server speaks it, and the server never learns it was involved. That is the whole reason to carry the protocol: at the application boundary it buys a library in every language and no code to write, and on the wire it would buy a dependency and a second connection for something nobody would see.

It is a translator, not a broker: every publish goes up to the server and every delivery comes down, so a local subscriber sees its own message only if the organization sends it back, which is exactly what a subscriber on another machine sees.

What your library gets, stated rather than discovered:

| Feature | Answer |
| --- | --- |
| QoS 0 | as asked |
| QoS 1 and 2 | granted as 0. The tunnel is ordered and reliable, but nothing is stored for an absent subscriber, so promising more would be a lie. Libraries accept the downgrade; that is what the granted-QoS field is for. |
| Retained messages | never stored, never delivered |
| Clean session | always. A session lives exactly as long as the connection. |
| Last will | accepted in CONNECT and never published |
| Username / password | ignored. The tunnel's token is the credential and the listener is loopback. |

Both faces can run at once and share one subscription set, so a shell script on SSE and an application on MQTT see the same messages.

## Server events

Everything the server already reports through webhooks is also published on the reserved `$aperio/` namespace, so a client can react to infrastructure without standing up an HTTP receiver for it. The topic is the event name with its underscores as levels, and the payload is the event's JSON, the same document a webhook would receive.

| Group | Topics |
| --- | --- |
| Clients | `$aperio/client/connected`, `$aperio/client/disconnected`, `$aperio/client/draining` |
| Tokens | `$aperio/token/created`, `$aperio/token/revoked`, `$aperio/token/rotated`, `$aperio/token/expiring`, `$aperio/token/new/ip`, `$aperio/token/pin/mismatch`, `$aperio/canary/tripped` |
| Tunnels and shares | `$aperio/tunnel/created`, `$aperio/tunnel/deleted`, `$aperio/share/created` |
| Operations | `$aperio/maintenance/on`, `$aperio/maintenance/off`, `$aperio/settings/updated`, `$aperio/import/applied`, `$aperio/user/created` |
| Capacity and alerting | `$aperio/alert/triggered`, `$aperio/alert/resolved`, `$aperio/scaling/requested`, `$aperio/org/usage`, `$aperio/disk/usage/warning` |
| Housekeeping | `$aperio/db/backup`, `$aperio/disk/pruned` |

The list is the webhook event list, mechanically transformed; [Observability](observability.md#webhooks) is where the events themselves and their payloads are documented, and anything added there appears here without a second decision. Note what the transformation does to a multi-word name: `token_new_ip` becomes `$aperio/token/new/ip`, so `$aperio/token/#` catches every token event and `$aperio/token/new/+` is not a thing to reach for.

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

The list is a fence, not a wish: a subscription is permitted when a granted filter *covers* it. `deploy/#` covers `deploy/web` and `deploy/+`, and does not cover `#`, otherwise subscribing to everything would be the way around a scope that named one subtree. `*` is accepted and stored as `#`, since that is how the hostname and path lists spell "everything".

The master token is unrestricted, like everywhere else. An ephemeral tunnel token carries no topics: it is a guest of the organization for one hostname and has no business signalling the rest of it.

Note the convention differs from `hostnames` and `paths`, where an empty list means unrestricted. That is deliberate: those fence a capability every token already had, while this one is new, and a new capability that switches itself on for every token predating it is how a permission model stops meaning anything.

A refused subscription is reported by name in the client's log rather than dropped, so a token missing a topic looks like a missing permission and not like a message that never arrived.

## Delivery

`qos: 0`, the default, sends the message to whoever is connected and forgets it. That is the right choice for most signals: a client that was not there did not miss anything it can still act on.

`qos: 1` is at-least-once. The server keeps the message until each subscriber acknowledges it and resends every 3 seconds meanwhile, giving up after 30. The subscriber remembers the ids it has seen for a minute and a half, so a redelivery caused by a lost acknowledgement is dropped rather than handed to your application twice.

The window is the whole of the promise, and it is deliberately short: it covers a connection that died between the write and the acknowledgement, not a subscriber that is away. A client that is offline when you publish does not receive the message later, at any QoS. `qos: 2` is treated as 1, because there is no store-and-forward here to build exactly-once on and granting a level the machinery does not have is worse than saying what it does.

A subscriber that stops acknowledging altogether holds at most 256 messages before the oldest are dropped, so one stuck client cannot grow the server's memory.

## From the dashboard

The settings dialog's **Messages** pane lists the client processes currently subscribed and the filters they asked for, and publishes a message with a topic, a body and the QoS switch. It is the quickest way to answer "why did nothing happen": the subscriber list and the reached-client count together tell a wrong filter from a wrong topic from a token that does not carry it.

## Watching it

The [Prometheus endpoint](observability.md#prometheus-metrics) counts what the messaging path does: `aperio_messages_published_total`, `aperio_messages_delivered_total`, `aperio_messages_dropped_total`, `aperio_messages_resent_total`, `aperio_messages_abandoned_total`, and the gauges `aperio_message_subscribers`, `aperio_message_subscriptions`, `aperio_messages_awaiting_ack`.

`dropped` is the one worth an alert. It counts a delivery that could not be written because the subscriber's connection was not keeping up, which means that client silently missed a message; nothing else in the system will tell you. `abandoned` is its QoS 1 counterpart: a message that was resent until the window ran out and was then given up on.

## What this is not

- **Nothing is stored for a client that is away**, at any QoS. A client that reconnects does not receive what it missed. That is deliberate: this serves reacting to something happening now, and replaying an hour-old event at a machine that just came back is a bug rather than a service.
- **A message never crosses an organization**, and the master organization is not a superset of its children.
- **A slow subscriber misses messages rather than slowing everyone down.** The server drops its copy when its connection is not keeping up; the local face says so with a `: missed N message(s)` comment in the stream.

## Runnable examples

- [`messaging`](examples/messaging/): a reacting side and a publishing side, with both local faces and a `run:` command
- [`mqtt`](examples/mqtt/): the other shape, an MQTT broker of your own carried by a tunnel, for when you want a broker's semantics (retained messages, QoS, offline sessions).
