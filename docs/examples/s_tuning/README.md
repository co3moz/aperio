# Tuning (capacity & throughput)

The knobs that shape how much traffic flows and how fast, on both sides:

- **Client** — `max_concurrent` announces a per-connection cap the server queues against instead of flooding the backend; `connections` opens parallel tunnel connections the server load-balances across (so one service isn't serialized behind a single WebSocket); `bandwidth` has the server pace responses to what the client's uplink can drain; `timeout`, `max_response_body`, and `max_redirects` bound individual requests.
- **Server** — global ceilings (`max_concurrent_requests`, `max_tunnels`, `max_body_size`), per-IP rate limiting (`ip_limit_max` burst + `ip_limit_refill` per second), gateway timeouts, and optional `tunnel_compression` for text-heavy traffic on slow links.

The values below are illustrative for a modest VPS fronting one busy service — measure before copying.

Multi-service variant: [m_tuning](../m_tuning/).
