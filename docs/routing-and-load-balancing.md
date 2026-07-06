# Routing & Load Balancing

Several clients can be connected to one Aperio server at the same time. When a public request arrives, the server picks a client in four steps: eligibility, hostname, path, and strategy.

## Eligibility

Clients are skipped when they are unhealthy (no heartbeat within `APERIO_CLIENT_DOWN_THRESHOLD`, default 15 s), when their own backend health probe is failing, when they are draining for shutdown, or when they were disabled from the dashboard. In-flight requests always finish.

## Hostname binds

A client can claim one or more hostnames — via `--host`, hostnames granted by its token, or an automatic random subdomain. Clients whose binds contain the request's `Host` header (case-insensitive, port ignored) win. If none match, clients *without* any hostname bind act as a fallback pool.

For strict multi-tenant setups, set `APERIO_REQUIRE_HOSTNAME_BIND=1`: clients without a hostname bind then never receive traffic, and unmatched requests fail with 504.

```
a.example.com  ──▶  client A (--host a.example.com)
b.example.com  ──▶  client B (--host b.example.com)
c.example.com  ──▶  client C (no hostname bind — fallback)
```

## Path binds

Within the hostname pool, the longest matching path bind wins. Binds match on segment boundaries: `/api` matches `/api` and `/api/v1`, never `/apixyz`. By default the bind prefix is stripped before forwarding (`/api/v1/users` arrives at the backend as `/v1/users`); set `APERIO_CLIENT_TRIM_BIND=0` to keep the full path.

## Strategies

`APERIO_LB_STRATEGY` decides how a client is picked from the final pool:

- **`round-robin`** (default) — clients with identical binds share traffic evenly.
- **`primary-standby`** — only the clients with the lowest announced priority (`--priority`, 0 = primary) receive traffic. Standby tiers take over automatically when every more-primary client is unhealthy, draining, disabled, or gone, and hand back when a primary returns. The dashboard marks standby clients with a `standby N` badge.
- **`sticky`** — first-time visitors are rotated round-robin, then an `aperio_affinity` cookie (HttpOnly, 24 h) pins each visitor to the client that served them — including their WebSockets. Use this when backends hold per-visitor state (PHP sessions, in-memory carts). Affinity keys on the client's instance ID, so it survives reconnects of the same process; the cookie is stripped before requests reach backends.

## Random subdomains

With `APERIO_RANDOM_SUBDOMAIN="*.example.com"` on the server (fronted by a wildcard DNS/proxy route), every connecting client is automatically assigned a hostname like `a1b2c3d4e5.example.com`. Assignments are per-connection and additive — declared and token-granted binds keep working alongside.

## Dashboard overrule

The dashboard can temporarily override any client's binds ("Overrule") — handy for redirecting a hostname live. Overrides live only in server memory: a reconnect or restart reverts to the client's own configuration.

Related: [In-Flight Failover](failover.md) covers what happens when the chosen client dies mid-request.
