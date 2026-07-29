# Tuning

> **Concept:** [Performance Tuning](../../performance-tuning.md).


The knobs that shape how much traffic flows and how fast, on both sides:

- **Client**, `max_concurrent` announces a per-connection cap the server queues against instead of flooding the backend; `connections` opens parallel tunnel connections the server load-balances across (so one service isn't serialized behind a single WebSocket); `bandwidth` caps what the server pushes at the client, as a budget divided across every service and connection; `timeout`, `max_response_body`, and `max_redirects` bound individual requests.
- **Server**, global ceilings (`max_concurrent_requests`, `max_tunnels`, `max_body_size`), per-IP rate limiting (`ip_limit_max` burst + `ip_limit_refill` per second), gateway timeouts, and optional `tunnel_compression` for text-heavy traffic on slow links.

The values below are illustrative, measure before copying.

They apply per `services:` entry, with the top-level values as the shared default: the busy API gets parallel tunnel connections and a higher concurrency cap, the report generator gets a long timeout and a big response budget, and the media service is bandwidth-paced so downloads never saturate the uplink. Anything unset falls back to the top-level values, write shared tuning once. The exception is `bandwidth`: a top-level value is a budget the entries share out, not a default each of them repeats.

Server-side ceilings are global.
