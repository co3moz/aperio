# Response Cache

The server keeps an in-memory GET response cache for services that opt in with `cache: true` on the client. It is strictly `Cache-Control`-driven: only responses explicitly allowing shared caching (`max-age`/`s-maxage`, no `no-store`/`no-cache`/`private`, no `Vary`/`Set-Cookie`) are stored, for the advertised lifetime; only credential-less plain GETs are answered from it.

Hits carry `x-aperio-cache: hit` and an `Age` header. Entries without a backend validator get a synthesized `ETag`, and a matching `If-None-Match` is answered `304` at the edge without a tunnel round-trip. See also the [s_resilience](../s_resilience/) example for serving stale entries while no client is connected.
