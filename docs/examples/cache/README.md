# Response Cache

> **Concept:** [Response Caching](../../caching.md).


The server keeps an in-memory GET response cache for services that opt in with `cache: true` on the client. It is strictly `Cache-Control`-driven: only responses explicitly allowing shared caching (`max-age`/`s-maxage`, no `no-store`/`no-cache`/`private`, no `Vary`/`Set-Cookie`) are stored, for the advertised lifetime; only credential-less plain GETs are answered from it.

Hits carry `x-aperio-cache: hit` and an `Age` header. Entries without a backend validator get a synthesized `ETag`, and a matching `If-None-Match` is answered `304` at the edge without a tunnel round-trip. See also the [resilience](../resilience/) example for serving stale entries while no client is connected.

It is a per-entry opt-in, which is what the pair below shows: the marketing site's GET responses are cached at the server edge, while the API next to it stays strictly proxied, one client, two policies. What actually gets cached is still decided by each backend's `Cache-Control` headers.
