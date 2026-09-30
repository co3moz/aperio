# In-Flight Failover

By default, a request that has already been dispatched to a client answers **502** if that client's connection drops before it responds. `APERIO_FAILOVER` (yaml `failover`) changes this. Failover only ever triggers while **no response bytes have reached the visitor yet**, so a re-dispatch is completely transparent.

A request that has *not* been dispatched yet is a different story: one for a route whose client just dropped, or one that lands in the milliseconds between a client connecting and its first heartbeat declaring the route, is held for a bounded wait before any refusal. The wait ends the moment a candidate appears or the declaration lands, and never outlives the server's `gateway.timeout`.

> **Configuration:** Server settings go in `aperio-server.yaml`; use grouped keys such as `failover.max_jumps` where available. File values override environment variables. Client settings go in `aperio.yaml`. See [Configuration](configuration.md#grouped-keys) for the full mapping.

## Modes

- **`fail`** *(default)*, answer 502 immediately.
- **`retry`**, re-dispatch to another currently available candidate for the same route; 502 when none exists.
- **`wait`**, wait for the **same client** to reconnect and re-dispatch to it. The client is recognized by its self-reported instance ID, which survives reconnects; when the instance is unknown, any candidate counts.
- **`retry-wait`**, re-dispatch to another candidate right away; if none exists, wait for one to appear. The most available option.

## Limits

Two settings bound the behavior:

| Variable | Meaning | Default |
| --- | --- | --- |
| `failover.max_jumps` (env `APERIO_FAILOVER_MAX_JUMPS`) | Max re-dispatch attempts per request. | `2` |
| `failover.window` (env `APERIO_FAILOVER_WINDOW`) | Total seconds the waiting modes may spend, across all jumps, starting at the first failure. | `15` |

## Idempotency

Only idempotent methods (GET, HEAD, OPTIONS, PUT, DELETE, TRACE) fail over by default: a POST may have already reached the backend before the client died, and re-dispatching could execute the operation twice. Set `APERIO_FAILOVER_ALL_METHODS=1` only if your backends tolerate duplicate deliveries.

Two more caveats:

- Streamed uploads cannot fail over, the body is consumed as it is forwarded. With a protocol-v2 client this covers buffered uploads past 256 KB; a chunked upload of any size and a `server_side:` service stream too.
- Every jump is logged with the old and new client IDs, so re-dispatches are always traceable.

## Choosing a mode

For a single client that occasionally restarts (deploys, laptop sleep), `wait` bridges the gap without visitors noticing. For redundant clients behind the same hostname, `retry` or `retry-wait` moves traffic instantly. `retry-wait` is the best default when you want maximum availability and can accept a request occasionally taking up to `APERIO_FAILOVER_WINDOW` seconds during an outage.

## Retrying error responses (not just dropped connections)

Failover above reacts to a client **disconnecting** mid-request. A client that stays connected but never answers is not a failover case: the request waits out the response timeout and gets a `504`, with no re-dispatch, because nothing says the client is gone rather than slow. A separate,
opt-in policy reacts to a client **answering with a server error**: when
`APERIO_RETRY_ON_5XX=1`, a fully-buffered response whose status is a retryable
server error is transparently re-dispatched to another client instead of being
returned to the visitor. No response bytes have reached the visitor yet, so
this is safe for retryable methods.

This is deliberately independent of `APERIO_FAILOVER` (which governs
connection-loss behavior): it triggers on an actual error response, always
re-dispatches to a freshly picked client when the pool has another member (with
a single-candidate pool that is the same client again, so the retry relies on
the backend having recovered), and honors the same guards,
`APERIO_FAILOVER_MAX_JUMPS` and method idempotency
(`APERIO_FAILOVER_ALL_METHODS`).

| Variable | Meaning | Default |
| --- | --- | --- |
| `retry_on_5xx` (env `APERIO_RETRY_ON_5XX`) | Retry buffered server-error responses on another client. | off |
| `retry_statuses` (env `APERIO_RETRY_STATUSES`) | Comma-separated status codes that trigger the retry. Empty = every 5xx (500-599). | every 5xx |

Streamed responses are never retried (bytes may already be in flight), and the
retry shares the failover jump budget, so a persistently failing pool cannot
loop forever.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`failover`](examples/failover/): in-flight failover
