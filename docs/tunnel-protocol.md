# Tunnel Protocol & Advanced Features

Everything between server and client flows over one persistent WebSocket connection per client. This article covers what that tunnel can carry beyond plain request/response pairs.

## WebSocket & Socket.io pass-through

WebSocket upgrade requests from visitors are detected automatically and proxied end-to-end: the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints work with zero configuration, and the same hostname/path routing rules apply.

## Chunked body streaming

Since protocol v5 a **buffered response travels as one binary frame**: the envelope and the body in a single message, with the body as bytes. Before v5 the body was base64-encoded into the JSON, which is a third more bytes on the wire, an encode pass on the client and a decode pass on the server, and a string the size of the response held on both sides. The frame is only sent to a server that announced v5; an older one still gets base64 in JSON, and a v5 server still understands it.

Protocol **v6** does the same in the other direction: a **buffered request body** (an upload under the streaming threshold) travels as bytes in the dispatch frame instead of base64 inside the `Request` JSON. Same layout, same negotiation, sent only to a client that announced v6. Both frames have a compressed sibling that the sending side's writer produces when the connection negotiated tunnel compression and only when deflating actually made the payload smaller; without it a binary frame would bypass compression entirely, since compression applies to text frames.

Response bodies over 32 KB are streamed through the tunnel in chunks (256 KB against a server too old for binary frames, where streaming buys nothing but bounded memory), and request bodies (uploads) over 256 KB with protocol v2, so memory usage stays bounded on both sides regardless of size. The response threshold is where two costs cross: streaming pays a head, a frame per chunk and a tail per response, while a buffered body is base64-encoded, which is a third more bytes on the wire and a pass over every one of them. The client truncates backend responses larger than `APERIO_MAX_RESPONSE_BODY` (yaml `max_response_body`) (default 50 MB).

Protocol v2 peers additionally exchange body chunks as **raw binary WebSocket frames** instead of base64-in-JSON, removing the ~33% base64 overhead. Both features negotiate automatically via the heartbeat protocol version: older peers transparently fall back to buffered bodies and base64 frames.

One trade-off: streamed uploads cannot fail over or be replayed from the request inspector, because the body is consumed as it is forwarded.

## Messages between clients (v4)

The current `PROTOCOL_VERSION` is **4**, which adds frames carrying messages between the clients of one organization: `Subscribe` and `Unsubscribe` name topic filters, `Publish` carries one message in either direction, `PublishAck` acknowledges a `qos: 1` delivery so the server stops resending it, and `SubscribeRefused` / `PublishRefused` say which filter or message was not accepted and why.

They ride the connection that already exists, so nothing new is dialled and the message is authenticated by the token the client connected with. The server keys subscriptions on the client *process* rather than the connection, so a client running several services receives one copy rather than one per service. See [Messages Between Clients](messaging.md) for the whole shape.

An older client is unaffected: it never sends `Subscribe`, so it never receives a `Publish`, and the two server-to-client frames are ignored the way every unknown message is.

## Per-stream flow control

`PROTOCOL_VERSION` **3** added flow control to everything that streams: response bodies, proxied WebSockets and raw TCP relays.

The problem it solves is a visitor who reads slower than the backend produces. A tunnel connection has a single read loop shared by every request and stream on it, so the server must never block on one consumer, but it also cannot buffer without bound, and dropping the stream would cut a perfectly healthy download. So the server pushes back on the *producer* instead:

- Each stream's server-side backlog is measured in bytes. Past `APERIO_STREAM_PAUSE_BYTES` (default 2 MB) the server sends `StreamPause { id }`, and the client stops reading that one stream's source, the backend response body, the backend WebSocket, or the TCP socket. Ordinary TCP backpressure then reaches the backend, which is exactly where it belongs.
- Once the backlog drains below `APERIO_STREAM_RESUME_BYTES` (default 512 KB) the server sends `StreamResume { id }` and the client carries on. The two marks are deliberately far apart so the pair cannot flap on every chunk.
- Nothing else on the tunnel is affected: other visitors' responses, other streams and the heartbeat keep flowing while one stream is paused.

`id` is a request id for a streamed response and a stream id for a WebSocket or TCP relay. UDP relays are excluded on purpose: they keep their best-effort, drop-when-congested contract.

Two safety nets bound the mechanism. A client that cannot be paused, a pre-v3 client, or one ignoring the pause, is cut off at `APERIO_STREAM_BACKLOG_LIMIT` (default 16 MB) of buffered bytes, and a consumer that accepts nothing at all for the whole `APERIO_GATEWAY_RESPONSE_TIMEOUT` still ends its own stream as it always did. On the client side, a producer that has been paused for more than 30 seconds resumes on its own, so a `StreamResume` lost with a torn-down stream cannot wedge it.

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
