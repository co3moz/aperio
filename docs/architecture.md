# Architecture Deep-Dive

How Aperio is put together: the tunnel protocol, the request lifecycle, the
concurrency model, and where state lives. For the security view see the
[Threat Model](threat-model.md).

## The shape of a deployment

```
  visitor ──HTTP──▶ aperio-server ──WebSocket (outbound from client)──▶ aperio-client ──▶ backend
```

The server is the only publicly-reachable component. The client dials **out** to
the server over a single WebSocket and keeps it open; proxied requests travel
back down that socket. Nothing on the client's network accepts inbound
connections.

## The tunnel protocol

The wire protocol is a tagged JSON message enum (`TunnelMessage`) with a small
set of binary frames layered on top for bulk body data. `PROTOCOL_VERSION`
(currently 7) is bumped on breaking changes so version skew surfaces in logs
and on the dashboard rather than failing obscurely.

Key messages:

- **`Ping` / `Pong`**, the heartbeat, sent every few seconds. The `Ping`
  carries the client's *entire* live configuration for the connection: its
  hostname/path binds, concurrency limit, priority, bandwidth cap, backend
  health, and every opt-in flag (cache, resilience, public, visitor auth,
  webhook inbox, per-service response timeout, device key, …). The server
  applies changes idempotently on each heartbeat, so a client re-announces
  its state on every reconnect.
- **`Request` / response**, a buffered request/response pair. Small bodies
  ride inside the JSON (base64).
- **Streamed bodies (protocol v2)**, `RequestStart`/`Chunk`/`End` and raw
  binary chunk frames (`[tag][id_len][id][payload]`) carry large bodies without
  the base64+JSON overhead. The tag byte never collides with a zlib-compressed
  JSON frame (which starts `0x78`), so the reader can tell them apart.
- **Per-stream flow control (protocol v3)**, `StreamPause` / `StreamResume`
  (server to client, keyed by request id or stream id). When one stream's
  server-side backlog crosses a byte watermark, the producing client stops
  reading that stream's source, so backpressure reaches the backend instead of
  the server buffering or dropping a slow visitor's download. Nothing else on
  the connection is affected; UDP relays keep their best-effort contract. See
  [Tunnel Protocol](tunnel-protocol.md).
- **Compression**, when both sides agree, JSON frames are zlib-compressed;
  inflation is output-bounded to prevent a decompression bomb.

The frame decoder and the JSON/zlib paths are the primary corruption surface
and are exercised by the [`tools/fuzz/`](../tools/fuzz) targets.

## Request lifecycle (server side)

A proxied request flows through, in this order. `GET /aperio/api/explain`
walks the same chain on a request nobody sends, so it is the executable
version of this list:

1. **Client IP resolution** (`extract_client_ip`), honoring `trust_proxy` /
   `trusted_proxies` when configured, otherwise the socket peer.
2. **Maintenance mode**, a flag covering the hostname answers `503` before
   anything else is looked at.
3. **Static routes**, a client-less `routes:` entry with a `redirect` or a
   `respond` answers directly. Because this is above the rate limit, an
   operator-authored answer costs a visitor nothing from their bucket. A
   `routes:` entry with neither is policy, not an answer, and does not end the
   request here.
4. **Per-IP rate limit**, the visitor's token bucket.
5. **Visitor gate**, the password, OIDC or client-declared login, before
   routing so an unauthenticated visitor never influences client selection.
6. **Wait for a client**, bounded by the gateway timeout, and where a cold
   start is triggered. Before the global slot is taken, so visitors waiting on
   a sleeping service cannot starve healthy ones.
7. **Admission**, the global concurrency slot.
8. **WAF and the per-route rate limit**, after admission because a WAF rule may
   match on the body, which is not read until a slot is held.
9. **Routing** (`select_client_pool` → `apply_lb_strategy` → `pick_proxy_client`),
   eligibility (healthy, not draining, not ejected) → hostname → path →
   load-balancing strategy → per-visitor IP filter. No client → serve-stale, a
   fallback URL redirect, or a `504`.
10. **Cache**, after routing rather than before it, because the opt-in is the
    serving client's (`cache: true`) and the server has to know who would serve
    the request to know whether it may be cached. A fresh hit, or a
    stale-while-revalidate hit, short-circuits the tunnel; concurrent misses
    coalesce behind a single-flight leader.
11. **Per-token / per-org limits and per-client admission**, token rate/quota,
    org monthly bytes, and the chosen client's own concurrency limiter.
12. **Dispatch**, the request is sent down the chosen client's socket and the
    server awaits the response with the per-service (or global) response timeout.
13. **Failover / retry**, a vanished client re-dispatches per `failover_mode`; a
    buffered 5xx re-dispatches when `retry_on_5xx` is on; both are bounded by the
    jump budget.
14. **Response**, headers rewritten, cached when eligible, captured for the
    inspector, accounted to stats/quota, and streamed or buffered back.

## Concurrency model

The server is a [tokio](https://tokio.rs) multi-threaded runtime built on
[axum](https://github.com/tokio-rs/axum). Each tunnel connection owns a reader
task (its message loop) and a writer task (draining an mpsc queue, applying
bandwidth shaping). Proxied requests are ordinary async handler invocations that
park on a `oneshot` until the matching response frame arrives on the client's
reader task. Background tasks handle retention pruning, scheduled backups, alert
evaluation, and uptime sampling. A global `AtomicUsize` bounds in-flight proxied
requests so the limit can change at runtime without rebuilding a semaphore.

## Where state lives

- **`aperio.db` (SQLite, WAL mode)**, the durable store: dynamic tokens, admin
  keys, dashboard users, sessions, webhooks + deliveries, organizations, the
  inbox, and restart-surviving traffic statistics. Each store keeps its own
  connection; WAL + a busy timeout make concurrent access safe.
- **`audit.jsonl`**, an append-only, hash-chained audit log (tamper-evident;
  `--verify-audit`), size-rotated across generations.
- **In-memory only**, the live client table, pending requests, the response
  cache, rate-limit buckets, captured requests (bounded ring), per-route latency
  windows, maintenance flags, and the token-seen-IP / outlier-ejection tracking.
  These are deliberately not persisted; a restart starts them clean.

Configuration is layered (environment defaults < dashboard overrides <
`aperio-server.yaml` file) into an immutable `ServerConfig` snapshot behind an
`RwLock`; a hot-reload swaps the snapshot atomically and audits the key diff.
The file is last on purpose: it is what an operator writes, versions and
reviews, so a stored override it contradicts is dropped at startup rather than
silently outranking it.
