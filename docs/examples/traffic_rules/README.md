# Traffic Rules

> **Concept:** [Routing and Load Balancing](../../routing-and-load-balancing.md), [Production Hardening](../../production-hardening.md).

Server-side rules applied to a request before it enters the tunnel. All of them are settings of a site in the server's `sites` list (here in `server-init.yaml`, read on the first start), and none of them is visible to the client:

- **`rate_limit`**, an aggregate requests-per-second ceiling for a site. The per-visitor IP limit bounds one caller; this bounds the route, which is what protects a login endpoint from a thousand polite visitors. A refused request answers `429` and names the limit that fired in its `x-aperio-limit` header, so `curl -i` answers what used to need the server's log.
- **`waf`**, deny and size rules evaluated first: scanner paths, scanner user-agents, and a per-route upload cap answered `413`. The rules of every matching site apply, so the `*` site's blocks hold on every hostname.
- **`fallback`**, where visitors of an unserved hostname are redirected instead of receiving a gateway error, the status page usually.
- **`pages`**, a hostname's own 503/504 pages over the `*` site's. A maintenance flag's reason and expiry are substituted into `{reason}` and `{until}` where the page writes them.

A request takes each setting from the most specific site that writes it: an exact hostname before a pattern before `*`, then the longest `path`. That is why `/api/login` gets 5 requests a second while the rest of the API host shares 200.

For redirects and fixed responses served without a client at all, see the [routes](../routes/) example; for the per-visitor IP limit and admin-surface fencing, [production hardening](../../production-hardening.md). The full list of what a site can carry is in [Sites](../../configuration.md#sites).
