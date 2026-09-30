# The embedded profile

A written minimum of the tunnel protocol, for a device that cannot run
`aperio-client`.

An ESP32 cannot usually run the full client: its TLS stack, HTTP client,
configuration parser, admin CLI, and optional features exceed a small device's
memory and flash budget. This page lists the protocol messages a minimal
device client must handle, may ignore, and will not receive.

The classification lives in `aperio-server/src/protocol_profile.rs`. Its exhaustive
match requires every new message type to be classified, and a check keeps this
page aligned with that list.

## What a device must handle

| Message | Direction | What it is |
| --- | --- | --- |
| `Ping` | client to server | the client's whole declaration, sent on connect and as a heartbeat |
| `Pong` | server to client | the answer to a Ping |
| `Request` | server to client | a buffered request: the common case, and the only one a device with a small ceiling needs to accept |
| `Response` | client to server | the buffered answer to it |
| `RequestStart` | server to client | the head of a streamed request; a device may answer with an error status rather than assembling one, but it must not be confused by it |
| `RequestChunk` | server to client | a body piece of that request |
| `RequestEnd` | server to client | the end of it |
| `StreamPause` | server to client | flow control, and the one thing a device cannot ignore if it streams: ignoring a pause is how a 300 KB device meets a backlog it cannot hold |
| `StreamResume` | server to client | the other half of it |

That is the profile. A device that speaks these serves HTTP through the
tunnel.

Two notes on the ones that are easy to underestimate:

- **`StreamPause` and `StreamResume` are not optional if you stream.**
  Ignoring a pause is how a device with 300 KB of RAM meets a backlog it
  cannot hold. A device that only ever answers with a buffered `Response`
  never has to send a pause, but it must still stop sending when it receives
  one.
- **The body encoding matters more here than anywhere else.** JSON with
  base64 payloads costs 1.33x in a buffer that has to exist all at once,
  which is the wrong shape for this much RAM. The v2 binary frames are worth
  more to a device than to a laptop.

## What a device may ignore

Parse and discard, or do not parse at all.

| Message | Direction | What it is |
| --- | --- | --- |
| `ResponseStart` | client to server | only produced by a client that chooses to stream a response; a device that always buffers never sends one |
| `ResponseChunk` | client to server | a piece of such a response |
| `ResponseEnd` | client to server | the end of one |
| `ResponseAbort` | both ways | a streamed response given up on; a device that does not stream neither sends nor receives it |
| `HostnameAssigned` | server to client | the random subdomain the server picked, for logging |
| `Draining` | server to client | the server is going away; reconnecting on close covers it |
| `ServerShutdown` | server to client | the same, at the end; the socket closing says it too |
| `CompressionStart` | server to client | an offer, and a device that never answers it is never compressed to |
| `CompressionAck` | client to server | the acceptance a device simply never sends |

Compression is the interesting one: it is an *offer*. A device that never
sends `CompressionAck` is never compressed to, so declining costs nothing but
the decision.

## What a device is not sent

Each of these exists for a capability a client declares. A device that serves
one HTTP target declares none of them and therefore receives none.

| Message | Direction | What it is |
| --- | --- | --- |
| `UpgradeRequest` | server to client | WebSocket relay |
| `UpgradeResponse` | client to server | WebSocket relay |
| `WsData` | both ways | WebSocket relay |
| `WsClose` | both ways | WebSocket relay |
| `AuthAsk` | server to client | a declared `forward` gate with `via: client` |
| `AuthVerdict` | client to server | a declared `forward` gate with `via: client` |
| `TcpOpen` | server to client | a declared TCP tunnel |
| `TcpData` | both ways | a declared TCP tunnel |
| `TcpClose` | both ways | a declared TCP tunnel |
| `UdpOpen` | server to client | a declared UDP tunnel |
| `UdpDatagram` | both ways | a declared UDP tunnel |
| `UdpClose` | both ways | a declared UDP tunnel |
| `OtlpExport` | client to server | the OTel bridge |
| `Subscribe` | client to server | messaging |
| `Unsubscribe` | client to server | messaging |
| `SubscribeRefused` | server to client | messaging |
| `Publish` | both ways | messaging |
| `PublishAck` | server to client | messaging |
| `PublishRefused` | server to client | messaging |

## What this does not promise yet

The server does not currently gate itself on a declared profile. A device
avoids the messages above by not declaring the features that produce them,
which is different from the server refusing to send them, and the difference
matters if you are deciding what to leave unimplemented.

Turning this into a **negotiated capability**, where the device announces the
profile in its handshake and the server undertakes to stay inside it for that
connection (no compression offered, a declared chunk ceiling,
`max_concurrent: 1`, no relay message types) is **not on the backlog**: the
idea was withdrawn in `planned_features.md` as `#116` because it was
enforcement with nothing on the other side of it, no device client exists yet
to announce anything. If one appears, the gate is small additive work and can
be opened under a new id, together with the reference C client and the
conformance answer it would need: a device client that silently mishandles one
message type is an outage nobody can debug from the device end.

## Why the WebSocket stays

An HTTP long-poll transport sounds smaller and is not. Both need one TCP
socket and one TLS session, and WebSocket framing is a few kilobytes of code
on top, available as a component in every ESP-IDF build. Polling then adds
what a device can least afford: a request arrives on one connection and its
response has to go back on another or on the next poll, so a device with four
usable sockets spends two per in-flight request; every request costs a poll
round trip; and the TLS handshake, the single most expensive thing an ESP32
does here, repeats unless the connection is held open, at which point it is a
persistent connection with worse framing.

The saving is real only for a device that cannot hold a connection at all,
which is a power problem rather than a transport one, and the answer to that
is for the device to sit behind something that can.
