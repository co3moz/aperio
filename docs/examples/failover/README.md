# In-Flight Failover

`failover` decides what happens when a client dies **mid-request**:

- `fail` (default) — the request fails immediately.
- `retry` — re-dispatch to another healthy client right away.
- `wait` — hold the request until a candidate (re)connects, up to `failover_window`.
- `retry-wait` — retry immediately, and wait for a candidate if none is available.

By default only idempotent methods fail over; `failover_all_methods: true` extends it to POST/PATCH — off by default because a re-dispatched request may reach a backend twice. Pinning a `client_id` on the client makes `wait` mode recognize the same client when it reconnects. See [In-Flight Failover](../../failover.md) for the full story.
