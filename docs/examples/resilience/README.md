# Resilience

> **Concept:** [Response Caching](../../caching.md).


With `resilience: true` (on top of `cache: true`), the server keeps answering visitors from the cache **while no healthy client is connected**, instead of failing with 504. Fresh-or-expired entries answer visitors, marked `x-aperio-stale: true` once past their lifetime, always with an `Age` header, up to the server's `cache_max_stale` window. The moment a client reconnects, normal proxying takes over.

This turns a redeploy or a flaky uplink into a non-event for cacheable pages. See [Client Resilience](../../client-resilience.md).

It is a per-entry opt-in on top of `cache:`, which is what the pair below shows: while the client is away (redeploy, dead uplink), the server keeps answering the marketing site from its cache, even past the entries' lifetime, marked `x-aperio-stale: true`, while the dynamic API correctly fails instead of returning stale data.
