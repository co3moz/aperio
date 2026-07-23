# Response Caching

Aperio can answer repeated `GET` requests from an in-memory cache on the
**server**, instead of forwarding every one down the tunnel to your client and
backend. On a hot, cacheable URL this removes the tunnel round-trip entirely,
lower latency for visitors and far less load on the backend.

Caching is **off by default** and strictly opt-in on both sides.

## The two-key model

A response is only cached when three independent conditions all agree:

1. **The server operator enabled the cache**, `cache: true` in
   `aperio-server.yaml` (env `APERIO_CACHE=1`; the dashboard's live settings
   persist it as a `cache_enabled` override). This provisions the shared
   in-memory cache and its memory budget.
2. **The service owner opted the service in**, `cache: true` in the client
   config (per `services:` entry, or top-level, or `APERIO_CACHE=1` on the
   client). Only the service owner knows whether *their* responses are safe to
   cache, so this consent lives with the client and is announced over the
   tunnel.
3. **The response says it is cacheable**, the cache is strictly
   `Cache-Control`-driven (see below).

This separation is deliberate: the operator provides the capability, the service
owner declares eligibility, and the response itself has the final say. None of
the three can cache on its own.

> **If a service sets `cache: true` but the server cache is off**, the opt-in
> silently does nothing. Aperio surfaces this so it is not a mystery: the server
> logs a one-time warning per client (`… requested response caching … but the
> server cache is disabled (APERIO_CACHE off); the opt-in is ignored`), and the
> dashboard's **Clients** table shows a `cache off` badge on that connection.
> Fix it by enabling the server cache, or drop the flag from the service.

## What gets cached

Only responses that explicitly allow *shared* caching, for exactly the lifetime
they advertise:

- A cacheable `Cache-Control` (`max-age` / `s-maxage`) and **none** of
  `no-store`, `no-cache`, `private`.
- No `Vary` and no `Set-Cookie` (responses that depend on the request or carry
  per-user state are never stored).
- Only **credential-less plain `GET`s** are answered from the cache (a request
  carrying `Authorization`/cookies bypasses it).

Nothing is cached implicitly, if your backend never sends `Cache-Control`,
nothing is stored, no matter the flags.

## What you get on a hit

- Hits carry `x-aperio-cache: hit` and an `Age` header.
- **Edge `304`**: entries without a backend validator get a synthesized `ETag`;
  a matching `If-None-Match` is answered `304 Not Modified` at the edge with no
  tunnel round-trip.
- **Single-flight**: concurrent identical cacheable `GET`s collapse into one
  upstream fetch, followers wait for the leader and answer from the freshly
  stored entry, so expiry on a hot URL cannot stampede the backend.
- **`stale-while-revalidate` (RFC 5861)**: a response advertising
  `stale-while-revalidate=N` keeps serving for `N` seconds past expiry (marked
  `x-aperio-stale`) while one background revalidation refreshes it, visitors
  never wait on the refresh.
- **`Range` requests**: single-range requests (video scrubbing, resumable
  downloads) are sliced from the stored full body at the edge, `206 Partial
  Content` with `Accept-Ranges`/`Content-Range`, `416` when out of range,
  honoring `If-Range`, without re-traversing the tunnel.
- **Purge**: `POST /aperio/api/cache/purge` (admin) drops entries by `hostname`
  and/or `path_prefix` (empty body = the whole cache) for immediate
  invalidation after a deploy.

## Serve-stale resilience

`resilience: true` on a service (needs `cache: true` and the server cache) lets
cached responses keep answering visitors **while no healthy client is
connected**, instead of failing with `504`. Fresh-or-expired entries answer up
to the `cache_max_stale` (env `APERIO_CACHE_MAX_STALE`) window past their lifetime, marked
`x-aperio-stale: true` once past it and always with an `Age` header. The moment
a client reconnects, normal proxying takes over. See
[Client Resilience](client-resilience.md).

## Knobs

Every setting is shown by its yaml key (env var in parentheses). Server keys go
in `aperio-server.yaml`, client keys in `aperio.yaml` (per `services:` entry).

| yaml key | Where | Effect | Default |
|---|---|---|---|
| `cache` (env `APERIO_CACHE`) | server | Enable the shared response cache. | `0` |
| `cache` (env `APERIO_CACHE`) | client, per service | Opt this service in. | `0` |
| `cache_max_bytes` (env `APERIO_CACHE_MAX_BYTES`) | server | Total in-memory budget; inserting past it evicts the entries closest to expiry, and a body larger than a quarter of the budget is never cached. | `67108864` (64 MB) |
| `resilience` (env `APERIO_RESILIENCE`) | client, per service | Serve stale while no client is connected. | `0` |
| `cache_max_stale` (env `APERIO_CACHE_MAX_STALE`) | server | Serve-stale window in seconds; `0` disables it. | `3600` |

The full option reference lives in [Configuration](configuration.md); the
end-to-end request path is in
[Tunnel Protocol & Advanced Features](tunnel-protocol.md), and the throughput
trade-offs in [Performance Tuning](performance-tuning.md).
