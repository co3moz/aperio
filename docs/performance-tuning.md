# Performance Tuning

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent yaml key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_MAX_CONCURRENT_REQUESTS` → `max_concurrent_requests`, `APERIO_CACHE_MAX_BYTES` → `cache_max_bytes`). YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

The knobs that shape Aperio's throughput and latency, and the trade-offs behind
each. Defaults are chosen for a small-to-medium deployment; tune from there
with real numbers (the [self-health card](observability.md#server-self-health),
the slowest-endpoints report, and the [k6 soak test](../tests/soak.js)).

## Client-side parallelism

- **`connections` (per service, 1-16).** The number of parallel tunnel
  connections a client opens for one service. One connection serializes at the
  WebSocket; several spread requests across sockets and CPU cores on the
  backend. Raise it for a high-RPS backend that can absorb the concurrency;
  leave it at 1 for a single-threaded dev server. Each connection is a full
  client in the routing pool.
- **`max_concurrent` (per service).** The client's own in-flight cap. The server
  queues requests beyond it (bounded by the gateway timeout) instead of flooding
  the backend, the backpressure valve that protects a fragile origin.

## Server-side limits

- **`APERIO_MAX_CONCURRENT_REQUESTS`.** Global in-flight proxied-request ceiling;
  excess visitors get `429`. Size it to what your backends collectively handle.
- **`APERIO_IP_LIMIT_MAX` / `_REFILL`.** Per-visitor token bucket. The burst
  (`max`) absorbs page loads; the refill (`req/s`) sets the sustained rate.
- **`APERIO_MAX_BODY_SIZE`.** Upload ceiling. Bodies over ~256 KiB stream
  (protocol v2 and later) instead of buffering, so a large limit does not cost
  memory per request, but it does bound how big a single upload can be.

## Slow visitors and stream backpressure

A visitor who downloads more slowly than the backend produces used to be the
awkward case: the server cannot block its shared read loop on one consumer, and
buffering without limit is not an option either. Since tunnel protocol v3 it
pushes back on the producer instead, and three watermarks control that.

- **`APERIO_STREAM_PAUSE_BYTES`** (2 MB). Per-stream backlog at which the
  producing client is told to stop reading that stream's source. This is the
  knob that costs memory: budget roughly `pause_bytes` × concurrently-slow
  streams. Raise it for high bandwidth-delay-product links or large media, where
  a bigger in-flight window keeps throughput up; lower it to bound memory on a
  server with many slow consumers.
- **`APERIO_STREAM_RESUME_BYTES`** (512 KB). Where a paused producer restarts.
  Keep it well under the pause mark, the gap is what stops the pair from
  flapping chunk by chunk. Too small and a fast producer stalls waiting for the
  queue to drain almost empty.
- **`APERIO_STREAM_BACKLOG_LIMIT`** (16 MB). Only bites for a producer that
  cannot be paused, i.e. a pre-v3 client. Upgrading clients matters more here
  than tuning the number.

Note that this is the *server's* side of a slow link. The client-side
`bandwidth` cap below is the complement: it paces what the server sends toward a
client that knows its own link capacity.

## The response cache

`APERIO_CACHE=1` plus a service's `cache: true` lets the server answer repeated
cacheable GETs from memory, skipping the tunnel round-trip entirely, the single
biggest latency win for read-heavy, cacheable content.

- **`APERIO_CACHE_MAX_BYTES`** bounds the cache; past it, entries closest to
  expiry are evicted. Bigger cache = higher hit ratio = fewer tunnel round-trips,
  at the cost of server memory. Watch the hit ratio on the cache stats card.
- **stale-while-revalidate** (`Cache-Control: stale-while-revalidate=N`) serves a
  slightly-stale entry instantly while one elected leader refreshes it in the
  background, visitors never wait on a refresh, and a stampede never hits the
  backend.
- **Negative caching** (`APERIO_CACHE_NEGATIVE_TTL`) shields a backend from
  repeated 404/410 probes; keep the TTL short so a resource that appears is not
  masked for long.
- Tracking query params are stripped from the cache key automatically, so
  ad-tagged URL variants share one entry.

Only enable the cache for services whose responses are genuinely shared and
`Cache-Control`-correct, the cache is strictly header-driven and never guesses.

## Compression

`APERIO_TUNNEL_COMPRESSION` zlib-compresses tunnel JSON frames once the client
acknowledges. It trades CPU for bandwidth: a clear win on a bandwidth-constrained
or metered client link, a slight loss on a fast LAN where the CPU cost outweighs
the saving. Body data over the streamed threshold uses raw binary frames
regardless. A client on a slow link can also announce a `bandwidth` cap so the
server paces frames to it instead of overrunning the buffer, and per-stream flow
control handles the other direction, a slow *visitor* behind a fast client.

## Failover vs. latency

`APERIO_FAILOVER=retry-wait` maximizes availability but a request during an
outage can take up to `APERIO_FAILOVER_WINDOW` seconds while a client
reconnects. For latency-sensitive traffic prefer `retry` (instant re-dispatch to
a redundant client) or accept the `fail` default. `APERIO_RETRY_ON_5XX` adds a
retry on error responses; both share the `APERIO_FAILOVER_MAX_JUMPS` budget, so a
persistently failing pool cannot loop.

## Data growth

Long-lived servers should bound their footprint so GC pauses and disk pressure
never surprise you: set the `APERIO_RETENTION_*` TTLs and the `APERIO_DB_MAX_BYTES`
cap (which auto-prunes and vacuums past the limit), and rely on the disk-usage
warning webhook. See [Observability](observability.md).

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`tuning`](examples/tuning/): capacity knobs, shared and per service
