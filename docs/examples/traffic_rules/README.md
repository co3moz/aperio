# Traffic Rules

> **Concept:** [Routing and Load Balancing](../../routing-and-load-balancing.md), [Production Hardening](../../production-hardening.md).

Server-side rules applied to a request before it enters the tunnel, all of them in `aperio-server.yaml` and none of them visible to the client:

- **`rate_limits:`**, an aggregate requests-per-second ceiling per hostname and path prefix. The per-visitor IP limit bounds one caller; this bounds the route, which is what protects a login endpoint from a thousand polite visitors. A refused request answers `429` and names the limit that fired in its `x-aperio-limit` header, so `curl -i` answers what used to need the server's log.
- **`waf:`**, deny and size rules evaluated first: scanner paths, scanner user-agents, and a per-route upload cap answered `413`.
- **`fallbacks:`**, where visitors of an unserved hostname are redirected instead of receiving a gateway error, the status page usually.
- **`error_pages:`**, per-hostname 503/504 pages over the global ones. A maintenance flag's reason and expiry are substituted into `{reason}` and `{until}` where the page writes them.

For redirects and fixed responses served without a client at all, see the [routes](../routes/) example; for the per-visitor IP limit and admin-surface fencing, [production hardening](../../production-hardening.md).
