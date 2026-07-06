# In-Flight Failover

By default, a request that has already been dispatched to a client answers **502** if that client's connection drops before it responds. `APERIO_FAILOVER` changes this. Failover only ever triggers while **no response bytes have reached the visitor yet**, so a re-dispatch is completely transparent.

## Modes

- **`fail`** *(default)* — answer 502 immediately.
- **`retry`** — re-dispatch to another currently available candidate for the same route; 502 when none exists.
- **`wait`** — wait for the **same client** to reconnect and re-dispatch to it. The client is recognized by its self-reported instance ID, which survives reconnects; when the instance is unknown, any candidate counts.
- **`retry-wait`** — re-dispatch to another candidate right away; if none exists, wait for one to appear. The most available option.

## Limits

Two settings bound the behavior:

| Variable | Meaning | Default |
| --- | --- | --- |
| `APERIO_FAILOVER_MAX_JUMPS` | Max re-dispatch attempts per request. | `2` |
| `APERIO_FAILOVER_WINDOW` | Total seconds the waiting modes may spend, across all jumps, starting at the first failure. | `15` |

## Idempotency

Only idempotent methods (GET, HEAD, OPTIONS, PUT, DELETE, TRACE) fail over by default: a POST may have already reached the backend before the client died, and re-dispatching could execute the operation twice. Set `APERIO_FAILOVER_ALL_METHODS=1` only if your backends tolerate duplicate deliveries.

Two more caveats:

- Streamed uploads (request bodies over 256 KB on tunnel protocol v2) cannot fail over — the body is consumed as it is forwarded.
- Every jump is logged with the old and new client IDs, so re-dispatches are always traceable.

## Choosing a mode

For a single client that occasionally restarts (deploys, laptop sleep), `wait` bridges the gap without visitors noticing. For redundant clients behind the same hostname, `retry` or `retry-wait` moves traffic instantly. `retry-wait` is the best default when you want maximum availability and can accept a request occasionally taking up to `APERIO_FAILOVER_WINDOW` seconds during an outage.
