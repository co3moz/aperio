# Response Cache (multi-service)

> **Concept:** [Response Caching](../../caching.md).


`cache:` is a per-entry opt-in: the marketing site's GET responses are cached at the server edge, while the API next to it stays strictly proxied, one client, two policies. What actually gets cached is still decided by each backend's `Cache-Control` headers (see [s_cache](../s_cache/) for the rules).
