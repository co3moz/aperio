# Behind a Reverse Proxy / CDN

> **Concept:** [Production Hardening](../../production-hardening.md).


When aperio-server sits behind nginx, Traefik, a cloud load balancer, or a CDN, it must resolve **real visitor IPs** from forwarding headers, rate limiting, login lockout, IP allowlists, and audit logs all depend on it, without letting arbitrary visitors spoof those headers.

`proxy.trusted` is the recommended, CDN-agnostic model: list your proxies' and CDN egress ranges, and the client IP is resolved by walking `X-Forwarded-For` (plus the direct peer) from the nearest hop backwards past trusted addresses. Headers from an untrusted direct peer are ignored entirely. Behind Cloudflare→proxy chains where the proxy resets XFF, set `proxy.real_ip_header: CF-Connecting-IP` instead, but **only** behind Cloudflare, since any visitor can send that header directly.

The proxy terminates TLS and forwards to a local port, so the server listens on loopback only: `HOST=127.0.0.1 PORT=8080 APERIO_SERVER_TOKEN=apr_behind_proxy_change_me APERIO_INIT_FILE=server-init.yaml aperio-server`.

The client side needs nothing special. See [Tokens & Authentication](../../tokens-and-auth.md) for hardening advice.
