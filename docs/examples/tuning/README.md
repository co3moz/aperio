# Tuning

> **Concept:** [Performance Tuning](../../performance-tuning.md).


The knobs that shape how much traffic flows and how fast, on both sides:

- **Client**, `limits.concurrent_requests` announces a per-connection cap the server queues against instead of flooding the backend; `connections` opens parallel tunnel connections the server load-balances across (so one service isn't serialized behind a single WebSocket); `bandwidth` caps what the server pushes at the client, as a budget divided across every service and connection; `timeout`, `limits.response_body`, `limits.request_body` and `limits.redirects` bound individual requests, with `response_timeout` asking the server to wait as long as the client will; `inspector: false` keeps a high-volume service out of the request inspector without touching its traffic or statistics. What every service shares is written once under `defaults:`.
- **Server**, global ceilings (`limits.concurrent_requests`, `limits.tunnels`, `limits.request_body`), per-IP rate limiting (`limits.ip_rate`: a `burst` and a `per_second` refill), gateway timeouts (`gateway`), and optional `tunnel.compression` for text-heavy traffic on slow links.

The values below are illustrative, measure before copying.

They apply per `services:` entry, with the top-level values as the shared default: the busy API gets parallel tunnel connections and a higher concurrency cap, the report generator gets a long timeout and a big response budget, and the media service is bandwidth-paced so downloads never saturate the uplink. Anything unset falls back to the top-level values, write shared tuning once. The exception is `bandwidth`: a top-level value is a budget the entries share out, not a default each of them repeats.

Server-side ceilings are global, except `max_connections_per_service`: it bounds what one client may open for a single service, is announced to the client on connect so it sizes itself instead of opening sockets the server would close, and a token's own `max_connections` may lower it further for its holder.
