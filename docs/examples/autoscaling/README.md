# Autoscaling

> **Concept:** [Autoscaling](../../autoscaling.md).

The server signals desired capacity to an endpoint you control; it never starts or stops anything itself. The pair splits the loop in two:

- **Scale out** is the server's half. The client declares `scaling:` with the endpoint, the bounds and the pacing; when the pool for its hostname runs hot, or a visitor hits a scaled-to-zero hostname, the server POSTs the desired count there. With `min: 0` and `cold_start`, the visitor's request is held while the first instance boots and dispatched the moment it connects, instead of answering 504.
- **Scale in** is the client's half: `idle_timeout` makes an instance that has served nothing for the window retire itself, gracefully. The server only ever asks for more, so the two halves cannot fight.

The server side is one switch plus the trust decisions: honoring client declarations is opt-in (`scaling.enabled`), and the endpoint must be HTTPS on a public address unless `allow_http` / `allow_private` say otherwise.

A maintenance flag wins over a cold start: a hostname flagged for maintenance serves its 503 page without waking the service behind it.
