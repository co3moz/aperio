# In-Flight Failover

A request that has already been dispatched to a client, and whose client's connection drops before it responds, is re-dispatched to another healthy client of the same route when the method is idempotent; with no other client it answers **502**. That is the `safe` resilience profile, the default; `resilience.profile: off` answers 502 at once, and `aggressive` also waits for a client to come back. `resilience.failover.mode` picks the behaviour directly, over the profile's. Failover only ever triggers while **no response bytes have reached the visitor yet**, so a re-dispatch is completely transparent.

A request that has *not* been dispatched yet is a different story: one for a route whose client just dropped, or one that lands in the milliseconds between a client connecting and its first heartbeat declaring the route, is held for a bounded wait before any refusal. The wait ends the moment a candidate appears or the declaration lands, and never outlives the server's `gateway.timeout`.

> **Configuration:** Server settings live in the server's database and are changed from the dashboard (**Settings**), with `PUT /aperio/api/settings`, or in a first-start document (`APERIO_INIT_FILE`); they are written as paths such as `cache.enabled`. Client settings go in `aperio.yaml`. See [Configuration](configuration.md#server-settings).
## Modes

- **`fail`** *(the `off` profile's)*, answer 502 immediately.
- **`retry`** *(the `safe` profile's, the default)*, re-dispatch to another currently available candidate for the same route; 502 when none exists.
- **`wait`**, wait for the **same client** to reconnect and re-dispatch to it. The client is recognized by its self-reported instance ID, which survives reconnects; when the instance is unknown, any candidate counts.
- **`retry-wait`** *(the `aggressive` profile's)*, re-dispatch to another candidate right away; if none exists, wait for one to appear. The most available option.

## Limits

Two settings bound the behavior:

| Setting | Meaning | Default |
| --- | --- | --- |
| `resilience.failover.max_jumps` | Max re-dispatch attempts per request. | `2`, `3` when aggressive |
| `resilience.failover.window` | Total time the waiting modes may spend, across all jumps, starting at the first failure. | `15s`, `30s` when aggressive |

## Idempotency

Only idempotent methods (GET, HEAD, OPTIONS, PUT, DELETE, TRACE) fail over by default: a POST may have already reached the backend before the client died, and re-dispatching could execute the operation twice. Set `resilience.failover.all_methods: true` only if your backends tolerate duplicate deliveries; no profile sets it, since it is a decision about the backend.

Two more caveats:

- Streamed uploads cannot fail over, the body is consumed as it is forwarded. With a protocol-v2 client this covers buffered uploads past 256 KB; a chunked upload of any size and a `server_side:` service stream too.
- Every jump is logged with the old and new client IDs, so re-dispatches are always traceable.

## Choosing a mode

For a single client that occasionally restarts (deploys, laptop sleep), `wait` bridges the gap without visitors noticing. For redundant clients behind the same hostname, `retry` or `retry-wait` moves traffic instantly. `retry-wait` is the best default when you want maximum availability and can accept a request occasionally taking up to `resilience.failover.window` during an outage.

## Retrying error responses (not just dropped connections)

Failover above reacts to a client **disconnecting** mid-request. A client that stays connected but never answers is not a failover case: the request waits out the response timeout and gets a `504`, with no re-dispatch, because nothing says the client is gone rather than slow. A separate,
policy, on in the `aggressive` profile, reacts to a client **answering with a server error**: when
`resilience.failover.retry_on_5xx` is `true`, a fully-buffered response whose status is a retryable
server error is transparently re-dispatched to another client instead of being
returned to the visitor. No response bytes have reached the visitor yet, so
this is safe for retryable methods.

This is deliberately independent of `resilience.failover.mode` (which governs
connection-loss behavior): it triggers on an actual error response, always
re-dispatches to a freshly picked client when the pool has another member (with
a single-candidate pool that is the same client again, so the retry relies on
the backend having recovered), and honors the same guards,
`resilience.failover.max_jumps` and method idempotency
(`resilience.failover.all_methods`).

| Setting | Meaning | Default |
| --- | --- | --- |
| `resilience.failover.retry_on_5xx` | Retry buffered server-error responses on another client. | on only when aggressive |
| `resilience.failover.retry_statuses` | The status codes that trigger the retry. Empty = every 5xx (500-599). | every 5xx; `[502, 503, 504]` when aggressive |

Streamed responses are never retried (bytes may already be in flight), and the
retry shares the failover jump budget, so a persistently failing pool cannot
loop forever.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`failover`](examples/failover/): in-flight failover
