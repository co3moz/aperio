# Messages Between Clients

> **Concept:** [Messages Between Clients](../../messaging.md).


One machine finishes a build and says so; every machine of the organization that cares hears about it and acts. No broker to run, no port to open, no inbound anything: the message travels on the tunnel connection each client already holds.

- `aperio.yaml` is the **reacting** side. It subscribes to `deploy/#`, runs `./deploy.sh` for `deploy/web`, and opens both local faces so an application on the same machine can join in.
- `aperio-publisher.yaml` is the **publishing** side. It exposes no service at all — a client whose whole job is to send is a complete configuration — and publishes through its own local face.

Publishing needs no client at all if you have an admin credential:

```bash
aperio-client api POST /publish -d '{"topic":"deploy/web","payload":"v1.9.2"}'
```

The answer says how far it went: `{"topic":"deploy/web","clients":2,...}`. A publish that reaches nobody is not an error, and the count is how you tell that from a publish that worked.

## Trying it

Start the reacting side, then send it something from anywhere:

```bash
# from the publishing machine
curl -X POST 'http://127.0.0.1:1888/publish?topic=deploy%2Fweb' --data 'v1.9.2'

# or watch the traffic from a shell, no client library needed
curl -N 'http://127.0.0.1:1888/subscribe?topic=deploy%2F%23'
```

An application uses whichever face suits it. Both run at once and share one subscription set:

```js
// MQTT, with the library you already have
const client = require('mqtt').connect('mqtt://127.0.0.1:1883')
client.on('connect', () => client.subscribe('deploy/#'))
client.on('message', (topic, payload) => console.log(topic, payload.toString()))
```

## `run:` is a remote-execution primitive

A message published by another client makes a command run on the reacting machine. That is the point, and it is why the shape is what it is: the payload reaches the command on **stdin** and never on the command line, so a message can never become part of the command; runs are capped and timed; and the whole thing is opt-in per topic in a file you wrote.

Keep the topic narrow, and give the publishing side a token scoped to it rather than a token that may reach everything:

```bash
aperio-client api POST /tokens \
  -d '{"name":"ci","hostnames":["*"],"topics":["deploy/#"]}'
```

A token with no `topics` cannot publish or subscribe at all, which is what every token minted before you asked for this carries.

## What it does not promise

A message reaches the clients connected when it is published. `qos: 1` holds it until each subscriber acknowledges and resends meanwhile, but **nothing is stored for a client that is away**, at any QoS. This is for reacting to something happening now; a client that was offline does not get the deploy event from an hour ago, which is a feature and not a gap.
