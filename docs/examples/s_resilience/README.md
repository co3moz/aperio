# Resilience (serve-stale)

> **Concept:** [Response Caching](../../caching.md).


With `resilience: true` (on top of `cache: true`), the server keeps answering visitors from the cache **while no healthy client is connected**, instead of failing with 504. Fresh-or-expired entries answer visitors, marked `x-aperio-stale: true` once past their lifetime, always with an `Age` header, up to the server's `cache_max_stale` window. The moment a client reconnects, normal proxying takes over.

This turns a redeploy or a flaky uplink into a non-event for cacheable pages. See [Client Resilience](../../client-resilience.md).

Multi-service variant: [m_resilience](../m_resilience/).
