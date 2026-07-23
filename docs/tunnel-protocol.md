# Tunnel Protocol & Advanced Features

Everything between server and client flows over one persistent WebSocket connection per client. This article covers what that tunnel can carry beyond plain request/response pairs.

## WebSocket & Socket.io pass-through

WebSocket upgrade requests from visitors are detected automatically and proxied end-to-end: the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints work with zero configuration, and the same hostname/path routing rules apply.

## Chunked body streaming

Bodies over 256 KB are streamed through the tunnel in chunks with backpressure **in both directions**, responses since protocol v1, and request bodies (uploads) with protocol v2, so memory usage stays bounded on both sides regardless of size. The client truncates backend responses larger than `APERIO_MAX_RESPONSE_BODY` (yaml `max_response_body`) (default 50 MB).

Protocol v2 peers additionally exchange body chunks as **raw binary WebSocket frames** instead of base64-in-JSON, removing the ~33% base64 overhead. Both features negotiate automatically via the heartbeat protocol version: older peers transparently fall back to buffered bodies and base64 frames.

One trade-off: streamed uploads cannot fail over or be replayed from the request inspector, because the body is consumed as it is forwarded.

## Tunnel compression

With `APERIO_TUNNEL_COMPRESSION=1` (yaml `tunnel_compression`) the server offers per-message zlib compression for JSON frames. Clients that support it acknowledge, and both directions switch to compressed frames; older clients keep working uncompressed. The client bounds decompression output as a memory-protection measure.

## Emergency tunnels

A raw TCP service (database, SSH, ...) declared in a client's `tunnels:` list can ride the same tunnel, bound locally by a peer client running `--bind-tunnels` with the same token and the declaring client's id. See [Emergency Tunnels](emergency-tunnels.md).

## Server-side response cache

With `APERIO_CACHE=1` on the server, services that opt in on the client side (`cache: true` per `services:` entry, or `APERIO_CACHE=1`) get a shared in-memory GET cache at the server's edge: a cache hit is answered immediately, without touching the tunnel or your backend at all.

The cache is deliberately conservative and strictly `Cache-Control`-driven, your backend stays in full control via standard headers:

- Only responses that explicitly allow shared caching are stored: a positive `max-age` (or `s-maxage`, which wins for shared caches), and none of `no-store`, `no-cache`, or `private`. Responses carrying `Vary` or `Set-Cookie` are never cached.
- Only buffered `200 OK` responses to plain GETs are stored, for exactly the advertised lifetime; streamed (chunked) responses are never cached.
- Requests with credentials attached (`Authorization` or `Cookie`) or a `Cache-Control: no-cache`/`no-store` request header always bypass the cache.
- Cache hits carry an `x-aperio-cache: hit` response header, so they are easy to spot in the browser or the request inspector.

Total memory is bounded by `APERIO_CACHE_MAX_BYTES` (yaml `cache_max_bytes`) (default 64 MB): inserting past the budget evicts the entries closest to expiry, and a single body larger than a quarter of the budget is never cached. Both flags can also be toggled live from the dashboard's server settings.

## Custom error pages

`APERIO_504_PAGE=/app/error_504.html` (yaml `504_page`) serves your own HTML (loaded once at startup) on gateway-timeout responses, e.g. a branded "tunnel is offline, check back soon" page. `APERIO_503_PAGE` (yaml `503_page`) does the same for the maintenance-mode response.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`s_grpc`](examples/s_grpc/): gRPC over h2c
- [`m_grpc`](examples/m_grpc/): gRPC + HTTP in one client
- [`s_headers`](examples/s_headers/): header add/remove rules
