# Tunnel Protocol & Advanced Features

Everything between server and client flows over one persistent WebSocket connection per client. This article covers what that tunnel can carry beyond plain request/response pairs.

## WebSocket & Socket.io pass-through

WebSocket upgrade requests from visitors are detected automatically and proxied end-to-end: the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints work with zero configuration, and the same hostname/path routing rules apply.

## Chunked body streaming

The current `PROTOCOL_VERSION` is **8**. Every version below is negotiated on
connect, so a client and a server that disagree still work: each feature falls
back to what the older side understands, and the mismatch is logged on both
sides and shown on the dashboard.

Since protocol v5 a **buffered response travels as one binary frame**: the envelope and the body in a single message, with the body as bytes. Before v5 the body was base64-encoded into the JSON, which is a third more bytes on the wire, an encode pass on the client and a decode pass on the server, and a string the size of the response held on both sides. The frame is only sent to a server that announced v5; an older one still gets base64 in JSON, and a v5 server still understands it.

Protocol **v6** does the same in the other direction: a **buffered request body** (an upload under the streaming threshold) travels as bytes in the dispatch frame instead of base64 inside the `Request` JSON. Same layout, same negotiation, sent only to a client that announced v6. Both frames have a compressed sibling that the sending side's writer produces when the connection negotiated tunnel compression and only when deflating actually made the payload smaller; without it a binary frame would bypass compression entirely, since compression applies to text frames.

Protocol **v7** closes the last base64 leg: the **relay payloads** travel as raw binary frames too. A TCP chunk (`FRAME_TCP_DATA`), a UDP datagram (`FRAME_UDP_DATAGRAM`) and a *binary* WebSocket frame (`FRAME_WS_DATA_BIN`) carry their bytes verbatim in a `[tag][id_len][stream id][payload]` frame, where before they were base64-encoded inside a `TcpData` / `UdpDatagram` / `WsData` JSON message: a third more bytes on the wire, plus an encode, a JSON parse and a decode on every 16 KB chunk, in both directions. Text WebSocket frames keep the JSON shape, since they were never encoded and there is nothing to save.

Protocol **v9** lets a service ask to be served by the server itself, with `server_side_target` on its entry, and lets a client say what it is called with `name`.

Protocol **v8** starts describing a connection's work as a *list*. A Ping may carry `services: [...]`, where each entry says what the top-level per-service fields have always said on their own: its binds, its target's announced limits, its gate, its cache and resilience settings. When the list is present it is authoritative and the singular fields are ignored, so the two spellings can never half-agree; when it is absent, which is every client before v8 and every ordinary one-service client after it, nothing changes at all.

This is one connection for several services of the same client process, and **both halves have shipped**: the server serves a list of several since 0.10.0, and a client produces one when its config says `multiplex: true` (see [One connection for several services](configuration.md#one-connection-for-several-services)). Each declared service is routed by its own binds, gated by its own `auth:` and `allowed_ips:`, ejected on its own backend failures without touching its neighbours, and shown and controlled separately in the dashboard. Identity across heartbeats is by the *name* the client gives a service, which is why a multiplexed one must have one; an unnamed service adopts a service that has none yet, so adding a `name:` to a service that was running without one keeps its counters rather than starting a second entry beside it.

An empty list is still refused. A client saying it serves nothing is a disconnect written the long way, and treating it as "no list" would silently keep serving what it just retired.

**Multiplexing is negotiated on the handshake, not assumed.** The server announces the protocol version it speaks in an `x-aperio-protocol` response header on the WebSocket upgrade, which is the only moment early enough to matter: the `Pong` carries the same number, but by the time one arrives the first Ping has already gone out, and this is the first capability that changes what that Ping is allowed to say. A client whose config asks for multiplexing against a server below v8 holds those services back and logs which side has to move, instead of connecting and having the server read the singular fields, bring up the first service and silently drop the rest. A server too old to send the header cannot have the capability, so an absent header reads as "no", never as "assume yes"; a client carrying one service never consults it.

**v9 adds a service that the server serves itself, and negotiates it the same way.** A service entry may carry `server_side_target`, which asks the server to reach that address directly instead of dispatching over the tunnel (see [Serving From the Server](server-side-services.md)). The field is additive, but *honouring* it is not: a server that does not understand it ignores the field and relays the request, and a client sets this precisely when it cannot reach the target itself, so the fallback is not a slower service but connection errors from a backend nobody can see. The client therefore checks the announced protocol before asking and holds the service back with a message naming which side has to move, exactly as it does for multiplexing. A Ping may also carry `name`, which needs no negotiation at all: a server that ignores it shows the client's id, which is what it always showed.

Unlike the body frames, these have **no compressed sibling**, and that is a deliberate trade rather than an oversight. A relay payload is an opaque byte stream, often already TLS or an end-to-end-sealed tunnel, so deflating it usually costs more than it saves; the win here is the per-byte codec, not the wire size.

The part worth knowing before you upgrade: until v7 these payloads rode inside a *text* frame, which the writer deflated whole when the connection had negotiated `tunnel_compression`. A binary frame skips that path. So on a server with compression **on**, tunnelling a **compressible** protocol (a plain-text wire protocol, an uncompressed database protocol) over TCP or a raw expose port now sends more bytes than it did before, while spending less CPU per byte. Nothing breaks and no setting changes; if wire size matters more than CPU for that particular tunnel, compress inside the tunnelled protocol itself. For the payloads the relay path exists for, already-encrypted or already-compressed streams, v7 is a straight win in both.

The negotiation is per stream and per direction: each side asks what the peer announced when the stream opens, and a peer below v7 keeps receiving exactly what it received before. A stream id too long to fit the frame's one-byte length prefix also falls back to the JSON shape rather than being dropped.

Response bodies over 32 KB are streamed through the tunnel in chunks (256 KB against a server too old for binary frames, where streaming buys nothing but bounded memory), and request bodies (uploads) over 256 KB with protocol v2, so memory usage stays bounded on both sides regardless of size. The response threshold is where two costs cross: streaming pays a head, a frame per chunk and a tail per response, while a buffered body is base64-encoded, which is a third more bytes on the wire and a pass over every one of them. The client truncates backend responses larger than `APERIO_MAX_RESPONSE_BODY` (yaml `max_response_body`) (default 50 MB).

Protocol v2 peers additionally exchange body chunks as **raw binary WebSocket frames** instead of base64-in-JSON, removing the ~33% base64 overhead. Both features negotiate automatically via the heartbeat protocol version: older peers transparently fall back to buffered bodies and base64 frames.

One trade-off: streamed uploads cannot fail over or be replayed from the request inspector, because the body is consumed as it is forwarded.

## Messages between clients (v4)

Messages between clients arrived in **v4**, which added frames carrying them between the clients of one organization: `Subscribe` and `Unsubscribe` name topic filters, `Publish` carries one message in either direction, `PublishAck` acknowledges a `qos: 1` delivery so the server stops resending it, and `SubscribeRefused` / `PublishRefused` say which filter or message was not accepted and why.

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

The client side has the mirror of the same rule. Its read loop is shared by every request, stream and heartbeat on the connection, so it never blocks on one consumer indefinitely: a frame for a proxied WebSocket, a TCP relay or a buffered upload is handed over without waiting when there is room, and when there is not, it waits **two seconds** before giving up on that stream. A backend that is merely slower than a burst keeps its stream; one that has genuinely stopped reading loses its own stream, with a log line naming it, rather than the tunnel losing its heartbeat and taking every other stream down with it. UDP relays are the exception in both directions, a datagram relay that waits for a congested consumer is not a datagram relay.

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
