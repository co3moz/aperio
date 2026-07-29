# Tunnel Protocol & Advanced Features

Everything between server and client flows over one persistent WebSocket connection per client. This article covers what that tunnel can carry beyond plain request/response pairs.

## WebSocket & Socket.io pass-through

WebSocket upgrade requests from visitors are detected automatically and proxied end-to-end: the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints work with zero configuration, and the same hostname/path routing rules apply.

## Chunked body streaming

Bodies over 256 KB are streamed through the tunnel in chunks, responses since protocol v1, and request bodies (uploads) with protocol v2, so memory usage stays bounded on both sides regardless of size. The client truncates backend responses larger than `APERIO_MAX_RESPONSE_BODY` (yaml `max_response_body`) (default 50 MB).

Protocol v2 peers additionally exchange body chunks as **raw binary WebSocket frames** instead of base64-in-JSON, removing the ~33% base64 overhead. Both features negotiate automatically via the heartbeat protocol version: older peers transparently fall back to buffered bodies and base64 frames.

One trade-off: streamed uploads cannot fail over or be replayed from the request inspector, because the body is consumed as it is forwarded.

## Per-stream flow control

The current `PROTOCOL_VERSION` is **3**, which adds flow control to everything that streams: response bodies, proxied WebSockets and raw TCP relays.

The problem it solves is a visitor who reads slower than the backend produces. A tunnel connection has a single read loop shared by every request and stream on it, so the server must never block on one consumer — but it also cannot buffer without bound, and dropping the stream would cut a perfectly healthy download. So the server pushes back on the *producer* instead:

- Each stream's server-side backlog is measured in bytes. Past `APERIO_STREAM_PAUSE_BYTES` (default 2 MB) the server sends `StreamPause { id }`, and the client stops reading that one stream's source — the backend response body, the backend WebSocket, or the TCP socket. Ordinary TCP backpressure then reaches the backend, which is exactly where it belongs.
- Once the backlog drains below `APERIO_STREAM_RESUME_BYTES` (default 512 KB) the server sends `StreamResume { id }` and the client carries on. The two marks are deliberately far apart so the pair cannot flap on every chunk.
- Nothing else on the tunnel is affected: other visitors' responses, other streams and the heartbeat keep flowing while one stream is paused.

`id` is a request id for a streamed response and a stream id for a WebSocket or TCP relay. UDP relays are excluded on purpose: they keep their best-effort, drop-when-congested contract.

Two safety nets bound the mechanism. A client that cannot be paused — a pre-v3 client, or one ignoring the pause — is cut off at `APERIO_STREAM_BACKLOG_LIMIT` (default 16 MB) of buffered bytes, and a consumer that accepts nothing at all for the whole `APERIO_GATEWAY_RESPONSE_TIMEOUT` still ends its own stream as it always did. On the client side, a producer that has been paused for more than 30 seconds resumes on its own, so a `StreamResume` lost with a torn-down stream cannot wedge it.

The watermarks are server settings ([Configuration](configuration.md)); an inconsistent trio is repaired rather than obeyed, so the mechanism cannot be configured into dropping every stream.

## Tunnel compression

With `APERIO_TUNNEL_COMPRESSION=1` (yaml `tunnel_compression`) the server offers per-message zlib compression for JSON frames. Clients that support it acknowledge, and both directions switch to compressed frames; older clients keep working uncompressed. The client bounds decompression output as a memory-protection measure.

## Emergency tunnels

A raw TCP or UDP service (database, SSH, ...) declared in a client's `tunnels:` list can ride the same tunnel, bound locally by another client running `--bind-tunnels` with that tunnel's name. See [Tunnels](emergency-tunnels.md).

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

- [`grpc`](examples/grpc/): a gRPC backend over `h2c://`, alongside HTTP
- [`headers`](examples/headers/): header rules on both sides, and per service
