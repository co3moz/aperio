# Allowed IPs (multi-service)

`allowed_ips` is enforced **per service**: the admin panel only accepts office and VPN visitors (the server rejects everyone else with `403` before dispatch), while the public app next to it stays open to the world — one client, two exposure levels.

Accurate visitor IPs matter here; behind a reverse proxy or CDN, configure proxy trust (see [s_behind_proxy](../s_behind_proxy/)).
