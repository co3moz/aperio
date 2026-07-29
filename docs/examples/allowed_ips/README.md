# Allowed IPs

> **Concept:** [Tokens & Authentication](../../tokens-and-auth.md).


`allowed_ips` restricts a service to specific visitor IPs or CIDR ranges: the server rejects every other visitor with `403` **before dispatching**, so blocked traffic never reaches the client. Purely restrictive, no token permission needed. When several clients serve one route, a visitor must pass **every** declared list.

Accurate visitor IPs are the whole point here, so if the server sits behind a reverse proxy or CDN, configure proxy trust too (see [behind_proxy](../behind_proxy/)), otherwise every visitor appears as the proxy's IP.

It is enforced **per service**: the admin panel only accepts office and VPN visitors (the server rejects everyone else with `403` before dispatch), while the public app next to it stays open to the world, one client, two exposure levels.
