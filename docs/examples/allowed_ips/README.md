# Allowed IPs

> **Concept:** [Tokens & Authentication](../../tokens-and-auth.md).


`allowed_ips` restricts a service to specific visitor IPs or CIDR ranges. It is enforced **per candidate**: a request dispatches to any client serving the route whose own list admits the visitor, and a route is open when at least one candidate is unrestricted, so joining an unrestricted client to a pool opens it for everyone. A visitor no candidate admits gets the same stealth answer an unclaimed route gives (`504`), or the winning client's `denied:` redirect when one is declared. Blocked traffic never reaches the client. Purely restrictive, no token permission needed.

Accurate visitor IPs are the whole point here, so if the server sits behind a reverse proxy or CDN, configure proxy trust too (see [behind_proxy](../behind_proxy/)), otherwise every visitor appears as the proxy's IP.

It is enforced **per service**: the admin panel only accepts office and VPN visitors, while the public app next to it stays open to the world, one client, two exposure levels.
