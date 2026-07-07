# Tunnel Protocol & Advanced Features

Everything between server and client flows over one persistent WebSocket connection per client. This article covers what that tunnel can carry beyond plain request/response pairs.

## WebSocket & Socket.io pass-through

WebSocket upgrade requests from visitors are detected automatically and proxied end-to-end: the public WS connection is relayed through the tunnel to your backend in real time. Socket.io (WebSocket transport), GraphQL subscriptions, and raw `ws://` endpoints work with zero configuration, and the same hostname/path routing rules apply.

## Chunked body streaming

Bodies over 256 KB are streamed through the tunnel in chunks with backpressure **in both directions** — responses since protocol v1, and request bodies (uploads) with protocol v2 — so memory usage stays bounded on both sides regardless of size. The client truncates backend responses larger than `APERIO_MAX_RESPONSE_BODY` (default 50 MB).

Protocol v2 peers additionally exchange body chunks as **raw binary WebSocket frames** instead of base64-in-JSON, removing the ~33% base64 overhead. Both features negotiate automatically via the heartbeat protocol version: older peers transparently fall back to buffered bodies and base64 frames.

One trade-off: streamed uploads cannot fail over or be replayed from the request inspector, because the body is consumed as it is forwarded.

## Tunnel compression

With `APERIO_TUNNEL_COMPRESSION=1` the server offers per-message zlib compression for JSON frames. Clients that support it acknowledge, and both directions switch to compressed frames; older clients keep working uncompressed. The client bounds decompression output as a memory-protection measure.

## Experimental TCP tunneling

A raw TCP service (database, SSH, ...) can ride the same tunnel:

```bash
# Private network side: allow TCP streams to exactly one target
APERIO_TCP_TARGET=localhost:5432 aperio-client 3000 --server-url ... --server-token ...

# Consumer side (your laptop): bridge a local port through the server
aperio-client tcp 15432 --server-url https://tunnel.example.com --server-token apr_xxxxxxxx
psql -h 127.0.0.1 -p 15432
```

Consumers authenticate against `GET /aperio/tcp` with any valid tunnel token (dynamic-token IP allowlists apply), and binary WebSocket frames carry the raw bytes. The exposing client only ever connects to its configured `tcp_target`, regardless of what the server asks — the TCP analogue of the HTTP SSRF guard. No extra public ports are opened.

## Custom error pages

`APERIO_504_PAGE=/app/error_504.html` serves your own HTML (loaded once at startup) on gateway-timeout responses — e.g. a branded "tunnel is offline, check back soon" page. `APERIO_503_PAGE` does the same for the maintenance-mode response.
