# Resilience

> **Concept:** [Response Caching](../../caching.md).


With `resilience: true` and `cache: true`, the server can answer cacheable requests while no healthy client is connected. Expired entries carry `x-aperio-stale: true` and remain usable only within `cache.max_stale`. Once a client reconnects, cache misses can reach the backend again; valid cache hits still come from the cache.

This turns a redeploy or a flaky uplink into a non-event for cacheable pages. See [Client Resilience](../../client-resilience.md).

It is a per-entry opt-in on top of `cache:`, which is what the pair below shows: while the client is away (redeploy, dead uplink), the server keeps answering the marketing site from its cache, even past the entries' lifetime, marked `x-aperio-stale: true`, while the dynamic API correctly fails instead of returning stale data.
