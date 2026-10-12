# Tunnel Protocol & Advanced Features

Everything between server and client flows over one persistent WebSocket connection per client. This article covers what that tunnel can carry beyond plain request/response pairs.

## WebSocket & Socket.io pass-through

WebSocket upgrade requests from visitors are detected automatically and proxied end-to-end: the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints work with zero configuration, and the same hostname/path routing rules apply.

## Protocol versions

The current `PROTOCOL_VERSION` is **10**. Nothing is negotiated by it: a client
and a server pair only on the same minor release (see
[Upgrade Guide](upgrade-guide.md#client--server-compatibility)), so both sides
always speak the whole current protocol. The table is the history of how the
wire reached its present shape.

| Version | What it added |
| --- | --- |
| v2 | Streamed request bodies (`RequestStart` / `RequestChunk` / `RequestEnd`) and raw binary chunk frames instead of base64 inside JSON. |
| v3 | Per-stream flow control, `StreamPause` / `StreamResume`, for everything that streams. |
| v4 | Messages between clients: `Subscribe`, `Unsubscribe`, `Publish`, `PublishAck`, `SubscribeRefused`, `PublishRefused`. |
| v5 | A buffered response travels as one binary frame, envelope and body together. |
| v6 | The same for a buffered request body, server to client. |
| v7 | TCP, UDP and binary WebSocket relay payloads travel as raw binary frames. |
| v8 | A Ping may describe the connection's work as a `services` list: several services on one connection. |
| v9 | A service entry may carry `server_side_target`, and a Ping may carry the client's `name`. |
| v10 | `TcpEof` half-closes a public TCP stream after queued input; `TcpClose` remains immediate. |
| no bump | `HostnameAssigned`, `Notice`, `Draining` and `ServerShutdown` (informational), `CompressionStart` / `CompressionAck` (an offer a peer may never answer), `OtlpExport` (the OTel bridge), and `AuthAsk` / `AuthVerdict` (a `forward` gate asked over the tunnel with `via: client`). Each reaches only a peer that declared the feature behind it or can ignore it, which is what makes them safe without a bump. [The Embedded Profile](embedded-profile.md) classifies every message for a device that implements the protocol by hand. |

## Chunked body streaming

**A buffered body travels as bytes, in the same binary frame as its envelope.**
A buffered response goes client to server that way, and a buffered request body
(an upload under the streaming threshold) goes server to client the same way.
Both frames have a compressed sibling that the sending side's writer produces
when the connection negotiated tunnel compression and deflating actually made
the payload smaller; without it a binary frame would bypass compression
entirely, since compression applies to text frames.

**Relay payloads travel as raw binary frames too.** A TCP chunk
(`FRAME_TCP_DATA`), a UDP datagram (`FRAME_UDP_DATAGRAM`) and a *binary*
WebSocket frame (`FRAME_WS_DATA_BIN`) carry their bytes verbatim in a
`[tag][id_len][stream id][payload]` frame. Text WebSocket frames keep the JSON
`WsData` shape, since they were never encoded and there is nothing to save. A
stream id too long to fit the frame's one-byte length prefix falls back to the
JSON shape rather than being dropped.

These have **no compressed sibling**, and that is a deliberate trade. A relay
payload is an opaque byte stream, often already TLS or an end-to-end-sealed
tunnel, so deflating it usually costs more than it saves. On a server with
`advanced.compression` on, tunnelling a *compressible* protocol over TCP or a raw
expose port therefore sends more bytes than a deflated text frame would, while
spending less CPU per byte; if wire size matters more for that tunnel, compress
inside the tunnelled protocol itself.

**Large bodies stream in chunks.** Response bodies over 32 KB and request
bodies (uploads) over 256 KB are streamed through the tunnel as binary chunk
frames, so memory stays bounded on both sides whatever the size. A backend
response that grows past `APERIO_LIMITS_RESPONSE_BODY` (yaml `limits.response_body`,
default 50 MB) before anything was sent is refused with a `502`; one already
streaming is aborted (`ResponseAbort`), so the visitor sees a broken response
rather than a quietly truncated success.

## One connection, a list of services

A Ping describes the connection's work as a `services` list, always: a
connection carrying one service sends a list of one, and there is no second
spelling in top-level fields. Several entries are one connection for several
services of the same client process, which a client with several services
produces unless its config says `multiplex: false` (see
[One connection for several services](configuration.md#one-connection-for-several-services)).
Each declared service is routed by its own binds, gated by its own `auth:` and
`allowed_ips:`, ejected on its own backend failures without touching its
neighbours, and shown and controlled separately in the dashboard. Identity
across heartbeats is by the *name* the client gives a service, which is why a
multiplexed one must have one.

An empty list is refused: a client saying it serves nothing is a disconnect
written the long way. A service entry may also carry `server_side_target`,
which asks the server to reach that address directly instead of dispatching
over the tunnel (see [Serving From the Server](server-side-services.md)).

One trade-off: streamed uploads cannot fail over or be replayed from the request inspector, because the body is consumed as it is forwarded.

## A gate asked over the tunnel

A `forward` visitor-auth method with `via: client` is decided by an endpoint on the *client's* network. The server, holding a request it cannot yet admit, sends an `AuthAsk` frame to the connection that would serve the request, carrying the composed question: the request line as `X-Forwarded-*` headers plus the allow-listed request headers, the URL to ask, and a timeout. The client calls the endpoint and answers with an `AuthVerdict`, a status and the allow-listed response headers, never a body. See [Visitor authentication](configuration.md#the-forward-method).

## Messages between clients (v4)

Messages between clients arrived in **v4**, which added frames carrying them between the clients of one organization: `Subscribe` and `Unsubscribe` name topic filters, `Publish` carries one message in either direction, `PublishAck` acknowledges a `qos: 1` delivery so the server stops resending it, and `SubscribeRefused` / `PublishRefused` say which filter or message was not accepted and why.

They ride the connection that already exists, so nothing new is dialled and the message is authenticated by the token the client connected with. The server keys subscriptions on the client *process* rather than the connection, so a client running several services receives one copy rather than one per service. See [Messages Between Clients](messaging.md) for the whole shape.

## Per-stream flow control

Everything that streams is flow-controlled: response bodies, proxied WebSockets and raw TCP relays.

The problem it solves is a visitor who reads slower than the backend produces. A tunnel connection has a single read loop shared by every request and stream on it, so the server must never block on one consumer, but it also cannot buffer without bound, and dropping the stream would cut a perfectly healthy download. So the server pushes back on the *producer* instead:

- Each stream's server-side backlog is measured in bytes. Past `advanced.stream.pause_at` (default 2 MB) the server sends `StreamPause { id }`, and the client stops reading that one stream's source, the backend response body, the backend WebSocket, or the TCP socket. Ordinary TCP backpressure then reaches the backend, which is exactly where it belongs.
- Once the backlog drains below `advanced.stream.resume_at` (default 512 KB) the server sends `StreamResume { id }` and the client carries on. The two marks are deliberately far apart so the pair cannot flap on every chunk.
- Nothing else on the tunnel is affected: other visitors' responses, other streams and the heartbeat keep flowing while one stream is paused.

`id` is a request id for a streamed response and a stream id for a WebSocket or TCP relay. UDP relays are excluded on purpose: they keep their best-effort, drop-when-congested contract.

Two safety nets bound the mechanism. A client ignoring the pause is cut off at `advanced.stream.backlog_limit` (default 16 MB) of buffered bytes, and a consumer that accepts nothing at all for the whole `gateway.response_timeout` still ends its own stream as it always did. On the client side, a producer that has been paused for more than 30 seconds resumes on its own, so a `StreamResume` lost with a torn-down stream cannot wedge it.

The watermarks are server settings ([Configuration](configuration.md)); an inconsistent trio is repaired rather than obeyed, so the mechanism cannot be configured into dropping every stream.

The client side has the mirror of the same rule. Its read loop is shared by every request, stream and heartbeat on the connection, so it never blocks on one consumer indefinitely: a frame for a proxied WebSocket, a TCP relay or a buffered upload is handed over without waiting when there is room, and when there is not, it waits **two seconds** before giving up on that stream. A backend that is merely slower than a burst keeps its stream; one that has genuinely stopped reading loses its own stream, with a log line naming it, rather than the tunnel losing its heartbeat and taking every other stream down with it. UDP relays are the exception in both directions, a datagram relay that waits for a congested consumer is not a datagram relay.

## Tunnel compression

With `advanced.compression: true` the server offers per-message zlib compression for JSON frames. The client acknowledges, and both directions switch to compressed frames. The client bounds decompression output as a memory-protection measure.

## Emergency tunnels

A raw TCP or UDP service (database, SSH, ...) declared in a client's `tunnels:` list can ride the same tunnel, bound locally by another client running `aperio-client bind` with that tunnel's name. See [Tunnels](emergency-tunnels.md).

## Server-side response cache

With `cache.enabled` on the server, services that opt in on the client side (`cache.enabled` per `services:` entry or under `defaults:`, or `APERIO_CACHE_ENABLED=1`) get a shared in-memory GET cache at the server's edge: a cache hit is answered immediately, without touching the tunnel or your backend at all.

The cache is deliberately conservative and strictly `Cache-Control`-driven, your backend stays in full control via standard headers:

- Only responses that explicitly allow shared caching are stored: a positive `max-age` (or `s-maxage`, which wins for shared caches), and none of `no-store`, `no-cache`, or `private`. Responses carrying `Vary` or `Set-Cookie` are never cached.
- Only buffered `200 OK` responses to plain GETs are stored, for exactly the advertised lifetime; streamed (chunked) responses are never cached.
- Requests with credentials attached (`Authorization` or `Cookie`) or a `Cache-Control: no-cache`/`no-store` request header always bypass the cache.
- Cache hits carry an `x-aperio-cache: hit` response header, so they are easy to spot in the browser or the request inspector.

Total memory is bounded by `cache.size` (yaml `cache_max_bytes`) (default 64 MB): inserting past the budget evicts the entries closest to expiry, and a single body larger than a quarter of the budget is never cached. Both flags can also be toggled live from the dashboard's server settings.

## Custom error pages

A site's `pages.unavailable` serves your own HTML on gateway-timeout responses, e.g. a branded "tunnel is offline, check back soon" page; `pages.maintenance` does the same for the maintenance-mode response. The `*` site's pages are every hostname's, and a hostname's own site can carry its own. See [Sites](configuration.md#sites).

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`grpc`](examples/grpc/): a gRPC backend over `h2c://`, alongside HTTP
- [`headers`](examples/headers/): header rules on both sides, and per service


### Public TCP write EOF (v10)

Managed public listeners send `TcpEof {stream_id}` on visitor write EOF. The client
flushes queued visitor data, shuts down the backend write half and continues
forwarding its response. The server enforces `drain_timeout_secs`; disabling,
deleting or disconnecting still sends `TcpClose` and ends both directions.
See [public expose deployment and diagnostics](public-exposes.md).
