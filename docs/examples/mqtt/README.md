# MQTT Between Clients

> **Concept:** [Emergency & Ephemeral Tunnels](../../emergency-tunnels.md).


An MQTT broker reachable by every machine in an organization, and by nothing else. The broker stays on one host, bound to loopback; the tunnel is what carries it, so it is never on the public internet and needs no certificate, no firewall rule and no public address of its own.

- `aperio.yaml` runs beside the broker and declares it as a tunnel named `mqtt`.
- `aperio-binder.yaml` runs beside each publisher or subscriber and binds that tunnel to local `1883`. The application connects to `127.0.0.1:1883` and does not know a tunnel exists.
- The broker is the one that decides topics, retained messages and QoS. Aperio moves the bytes and decides *who may reach it*: only a token of the same organization, carrying `allow_bind`.

Any broker works, since nothing here is MQTT-specific. `mosquitto -p 1883` on the declaring host is enough to try it.

## What this costs

Each message crosses the tunnel once from the publisher to the broker, and once more from the broker to **each** subscriber. For a handful of subscribers that is invisible; for a fan-out of dozens it is the thing to measure first. Putting the broker on the machine with the most subscribers, rather than the one that happens to publish, is usually the whole optimization.

## TLS

The broker's own TLS is unnecessary between the application and the tunnel (the hop is loopback) and is not what protects the wire: the tunnel itself is TLS to the Aperio server. If the *contents* must be opaque to the Aperio server as well, declare the tunnel with `encrypt: true` and a pre-shared key on both sides, see [encrypted_tunnels](../encrypted_tunnels/).
