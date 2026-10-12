# Architecture

How Aperio is built, what crosses the tunnel, and what each side trusts the other with.

```
visitor ──HTTP(S)──▶ aperio-server ◀──WebSocket, dialled out by the client── aperio-client ──▶ backend
```

The server is the only component anyone reaches from outside. The client dials out to it and keeps the connection open; requests travel back down that connection, so nothing on the client's network accepts inbound connections.

## The tunnel protocol

One WebSocket per connection carries everything, as tagged JSON messages (`TunnelMessage`) with binary frames for bulk data. A client and a server pair only on the same minor release, checked at the upgrade, so both always speak the whole current protocol (`PROTOCOL_VERSION` 10) and nothing is negotiated. [The Embedded Profile](embedded-profile.md) classifies every message for a device that implements the protocol by hand.

- **`Ping` / `Pong`**, every few seconds. A `Ping` carries the connection's whole configuration as a `services` list (one entry per service it carries, never empty): binds, limits, gate, flags, health. The server applies it idempotently, so a reconnect re-announces everything.
- **Requests and responses.** A buffered body travels as bytes in the same binary frame as its envelope. Response bodies over 32 KB and uploads over 256 KB stream as `[tag][id_len][id][payload]` chunk frames, so memory stays bounded whatever the size. A response that grows past the service's `limits.response_body` is refused (`502`), or aborted if it was already streaming.
- **WebSockets** (Socket.io, GraphQL subscriptions) are relayed end to end with no configuration. TCP, UDP and binary WebSocket payloads travel as raw binary frames; a visitor's half-close on a public TCP stream is `TcpEof`.
- **Flow control.** When one stream's backlog on the server passes `advanced.stream.pause_at`, the server sends `StreamPause` and the client stops reading that stream's source, so TCP backpressure reaches the backend; `StreamResume` follows below `resume_at`. Other streams and the heartbeat are unaffected. A client ignoring a pause is cut off at `backlog_limit`; a paused client resumes by itself after 30 s if the resume is lost. The client's own read loop never waits more than 2 s on one slow consumer. UDP stays best effort.
- **Compression.** With `advanced.compression`, JSON frames are zlib-compressed once the client acknowledges; inflation is bounded. Relay payloads are never compressed.
- **Messages between clients**: `Subscribe`, `Unsubscribe`, `Publish`, `PublishAck`, `SubscribeRefused`, `PublishRefused`.
- **A gate asked over the tunnel**: `AuthAsk` carries a `forward` check to the client that would serve the request, `AuthVerdict` brings back a status and the allowed headers.
- **Informational**: `HostnameAssigned`, `Notice` (a setting the server did not apply), `Draining`, `ServerShutdown`, `OtlpExport`.

The frame decoder and the JSON and zlib paths are fuzzed ([`tools/fuzz/`](../tools/fuzz)).

## A request, in order

`GET /aperio/api/explain` walks the same chain without sending anything.

1. **The visitor's address**, from `X-Forwarded-For` only behind a trusted proxy.
2. **Maintenance**: a flag covering the hostname answers `503`.
3. **A site that answers by itself** (`redirect`, `respond`), before the rate limit, so it costs the visitor nothing.
4. **The visitor's rate limit.**
5. **The gate**, before routing, so an unauthenticated visitor cannot influence which client is chosen.
6. **Waiting for a client**, bounded by `gateway.timeout`, where a cold start begins.
7. **Admission**: the server's concurrency slot.
8. **The firewall and the route's rate limit**, after admission because a rule may read the body.
9. **Routing**: eligibility, hostname, path, strategy, the visitor IP filter. With no client: serve-stale, the site's `fallback`, or `504`.
10. **Quotas**: the token's and the organization's.
11. **The cache**, after routing, because the opt-in is the serving client's.
12. **The client's own concurrency limit.**
13. **Dispatch**, and the wait for the answer.
14. **Failover**, when the client vanished or answered a retryable `5xx`.
15. **The response**: headers rewritten, cached, captured, counted, streamed back.

## Inside the server

A [tokio](https://tokio.rs) runtime under [axum](https://github.com/tokio-rs/axum). Each tunnel connection has a reader task and a writer task (which applies bandwidth pacing); a proxied request waits on a oneshot for its answer to arrive on the reader. Background tasks prune retention, take backups, evaluate alerts and sample uptime.

**State.**
- `aperio.db` (SQLite, WAL) holds the settings, tokens, admin keys, users, sessions, webhooks and deliveries, organizations, the inbox and the statistics.
- `audit.jsonl` is the hash-chained audit log.
- Everything else lives only in memory and starts clean after a restart: connected clients, pending requests, the cache, rate-limit buckets, inspector captures, latency windows, maintenance flags, ejection state.
- The settings document is compiled into an immutable snapshot. A change builds a new snapshot first, so a value that cannot work is refused before anything moves, and is then swapped in atomically.

**Inbound TLS.** With `tls.enabled` the server terminates HTTPS itself (rustls), choosing a certificate by the name a visitor asks for: one it obtained from the ACME authority, then one of `tls.certificates`, then a wildcard among them. A name nothing covers gets no handshake, rather than another name's certificate. The ACME proof is answered on the same port, only to a handshake asking for `acme-tls/1`, and only while an order is pending. Requests that arrived over it build `https://` callback URLs and default to `Secure` cookies.

**Outbound TLS.** The client's tunnel dial trusts only the Mozilla roots compiled into the binary, so a CA added on the host cannot intercept the tunnel. Every other outbound HTTPS call (webhooks, OIDC, JWKS, autoscaling, OTLP, ACME, the client's calls to its backends) uses the host's certificate store, so an internal CA works once it is installed where the host keeps them; a container needs a CA bundle for that. Two exceptions use the compiled-in roots: `h2://` backends and the server's WebSocket dial for `server_side:`.

## Trust boundaries

**The client does not trust the server, and the server does not trust the visitor.** The visitor is untrusted. The server routes and enforces policy, but the client treats it as a possibly hostile relay; it receives what a client chooses to declare (a visitor password, an autoscaling secret) and never the backend's own credentials. The client and its backend are trusted, and the client listens on nothing unless asked (the message faces, a `bind`).

**Visitor to server.** Per-address rate limits, a global concurrency cap and a body limit; `X-Forwarded-For` believed only from configured proxies; the gate in front of every route, closed by default; escalating login lockout.

**Anyone to the admin surface.** Sessions with roles and an optional second factor. A session made by a visitor credential opens sites, never Aperio; only the master token, a named user, a passkey, OIDC or an admin key open the dashboard and its API. `security.admin_allowed_ips` fences both while the login and visitor sign-in stay reachable. Metrics always need their token.

**Server to client.** The server never connects to a client. Tokens are scoped to hostnames, paths, addresses, lifetimes and rates; `token_new_ip` and canary tokens surface a leaked one; pinning ties a token to one device. The client dials only the targets and tunnels it declared, caps frame sizes and bounds decompression, so a hostile server cannot use it to reach anything else.

**Client to backend.** The client forwards to exactly the address it was given.

**Server to callback destinations.** Webhook and autoscaling URLs are chosen by lower-trust callers, and the delivery log reports how a destination answered, which would make a blind network probe. `outbound.allowlist` (or `outbound.block_private`) fences webhooks, autoscaling, JWKS, `forward` endpoints, OIDC and the ACME directory alike; no redirect is followed on any of them; autoscaling URLs are also limited to `https` and public addresses unless allowed. Both fences are off by default, so a multi-tenant server should set one.

### Server to a client's target

`server_side:` is a request and response channel a tenant steers, so it is fenced harder: the operator names the destinations (`security.server_side_targets`, unset permits nothing), the token must carry `allow_server_side`, the list is judged on the target as written rather than what a name resolves to, nothing a visitor sends can change the host, and a target outside the list is refused rather than relayed.

**Not defended:** a compromised client host or backend, a leaked master token (keep it out of clients), bugs in the backend, and network-layer floods, which belong to the CDN or proxy in front.

**Tamper evidence.** Each audit line commits to the one before it, so a deleted or altered event breaks the chain: `aperio-server audit verify`. Ship the log off the box.

## Examples

[`grpc`](examples/grpc/), [`encrypted_tunnels`](examples/encrypted_tunnels/), [`behind_proxy`](examples/behind_proxy/).
