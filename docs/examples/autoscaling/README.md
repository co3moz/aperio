# Autoscaling

> **Concept:** [Autoscaling](../../autoscaling.md).

The server sends the desired capacity to an endpoint you control. That endpoint starts instances; clients stop themselves when idle:

- **Scale out** is the server's half. The client declares `scaling:` with the endpoint, the bounds and the pacing; when the pool for its hostname runs hot, or a visitor hits a scaled-to-zero hostname, the server POSTs the desired count there. With `min: 0` and `cold_start`, the visitor's request is held while the first instance boots and dispatched the moment it connects, instead of answering 504.
- **Scale in** is the client's half: `idle_timeout` makes an instance that has served nothing for the window retire itself, gracefully. The server only ever asks for more, so the two halves cannot fight.

The server side is one switch plus the trust decisions: honoring client declarations is opt-in (`scaling.enabled`), and the endpoint must be HTTPS on a public address unless `allow_http` / `allow_private` say otherwise.

Set the same Bearer secret on your scaling endpoint, then export it before starting the client:

```bash
export SCALE_SECRET="$(openssl rand -hex 32)"     # the endpoint accepts this Bearer
```

> **Before running:** Replace `https://api.provider.example/apps/web/scale` with an endpoint that accepts Aperio's [scaling request](../../autoscaling.md#what-your-endpoint-receives) and starts the requested client instances. Set `SCALE_SECRET` in every client process.

A maintenance flag wins over a cold start: a hostname flagged for maintenance serves its 503 page without waking the service behind it.
