# Behind a Reverse Proxy / CDN

> **Concept:** [Production Hardening](../../production-hardening.md).


When aperio-server sits behind nginx, Traefik, a cloud load balancer, or a CDN, it must resolve **real visitor IPs** from forwarding headers, rate limiting, login lockout, IP allowlists, and audit logs all depend on it, without letting arbitrary visitors spoof those headers.

`trusted_proxies` is the recommended, CDN-agnostic model: list your proxies' and CDN egress ranges, and the client IP is resolved by walking `X-Forwarded-For` (plus the direct peer) from the nearest hop backwards past trusted addresses. Headers from an untrusted direct peer are ignored entirely. Behind Cloudflare→proxy chains where the proxy resets XFF, set `real_ip_header: CF-Connecting-IP` instead (or `trust_cf_header: true` as shorthand), but **only** behind Cloudflare, since any visitor can send that header directly.

The client side needs nothing special. See [Tokens & Authentication](../../tokens-and-auth.md) for hardening advice.
