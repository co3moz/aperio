# Load Balancing, sticky sessions

> **Concept:** [Routing & Load Balancing](../../routing-and-load-balancing.md).


With `lb_strategy: sticky`, first-time visitors are rotated round-robin, then an `aperio_affinity` cookie (HttpOnly, 24 h) pins each visitor to the client that served them, including their WebSockets. Use this when backends hold per-visitor state (PHP sessions, in-memory carts).

Affinity keys on the client's **instance id**, so it survives reconnects of the same process, that is why the client below pins a `client_id`. The cookie is stripped before requests reach backends. Run the same `aperio.yaml` on each backend machine, each with its own `client_id`.
