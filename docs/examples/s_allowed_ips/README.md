# Allowed IPs

`allowed_ips` restricts a service to specific visitor IPs or CIDR ranges: the server rejects every other visitor with `403` **before dispatching**, so blocked traffic never reaches the client. Purely restrictive, no token permission needed. When several clients serve one route, a visitor must pass **every** declared list.

Accurate visitor IPs are the whole point here, so if the server sits behind a reverse proxy or CDN, configure proxy trust too (see [s_behind_proxy](../s_behind_proxy/)), otherwise every visitor appears as the proxy's IP.

Multi-service variant: [m_allowed_ips](../m_allowed_ips/).
