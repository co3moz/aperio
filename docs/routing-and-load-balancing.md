# Routing & Load Balancing

Several clients can be connected to one Aperio server at the same time. When a public request arrives, the server narrows the pool in stages (eligibility, hostname, path), then applies the strategy, the per-candidate visitor IP filter, outlier-ejection state and any canary split.

> **Configuration:** Server settings live in the server's database and are changed from the dashboard (**Settings**), with `PUT /aperio/api/settings`, or in a first-start document (`APERIO_INIT_FILE`); they are written as paths such as `cache.enabled`. Client settings go in `aperio.yaml`. See [Configuration](configuration.md#server-settings).
## Eligibility

Clients are skipped when they are unhealthy (no heartbeat within `gateway.client_down_threshold`, default 15 s), when their own backend health probe is failing, when they are draining for shutdown, when they were disabled from the dashboard, or when the server refused their `server_side:` declaration. In-flight requests always finish.

## Hostname binds

A client can claim one or more hostnames, via `hostname:` (`--hostname`), hostnames granted by its token, or an automatic random subdomain. Clients whose binds contain the request's `Host` header (case-insensitive, port ignored) win. If none match, the clients that wrote **`hostname: "*"`** take the request: they serve every hostname no other service claims, and a request with no `Host` at all. A token limited to particular hostnames cannot claim `*`.

A service naming no hostname is **not** a catch-all. It used to be, which made a forgotten `hostname:` a site answering for whatever reached the server; now such a service is reached only by a hostname its token grants or a random subdomain, and when it has neither the client is told, in its own log, that nothing routes to it and which line to write. A request nothing claims answers `504`.

```
a.example.com  ──▶  client A (hostname: a.example.com)
b.example.com  ──▶  client B (hostname: b.example.com)
c.example.com  ──▶  client C (hostname: "*", every hostname nobody claims)
```

## Path binds

Within the hostname pool, the longest matching path bind wins. Binds match on segment boundaries: `/api` matches `/api` and `/api/v1`, never `/apixyz`. By default the bind prefix is stripped before forwarding (`/api/v1/users` arrives at the backend as `/v1/users`); set `APERIO_TRIM_BIND=0` to keep the full path.

## Strategies

`routing.lb_strategy` decides how a client is picked from the final pool:

- **`round-robin`** (default), clients with identical binds share traffic evenly.
- **`primary-standby`**, only the clients with the lowest announced priority (`--priority`, 0 = primary) receive traffic. Standby tiers take over automatically when every more-primary client is unhealthy, draining, disabled, or gone, and hand back when a primary returns. The dashboard marks standby clients with a `standby N` badge.
- **`sticky`**, first-time visitors are rotated round-robin, then an `aperio_affinity` cookie (HttpOnly, 24 h) pins each visitor to the client that served them, including their WebSockets. Use this when backends hold per-visitor state (PHP sessions, in-memory carts). Affinity keys on the client's instance ID, so it survives reconnects of the same process; the cookie is stripped before requests reach backends.

## Canary releases and weighted routing

`canary:` on a site splits its traffic between two versions of a service:

```yaml
# settings
sites:
  - hostname: app.example.com
    canary:
      service: web-v2      # the services: entry running the new version
      weight: 20           # percent of visitors sent there
      header: x-canary     # and anybody who asks, whatever the weight says
      value: on            # optional; any non-empty value matches when omitted
```

Weighted routing and a header-based canary are one mechanism seen from two angles, which is why they are one block. `weight: 0` with a `header` is the opt-in-only shape: nobody is moved, and a developer reaches the new version on demand.

**The split is per visitor, not per request.** It is decided by hashing the visitor's address, so the same visitor stays where they landed. A per-request coin flip would send one page load's twenty assets to both versions, which is a mixture rather than a canary and breaks the thing being tested first. The cost is that the split is only as even as the addresses are spread: at low traffic, or behind one large NAT, twenty percent may not look like twenty percent. The hash is stable across processes and restarts, so two servers behind a load balancer agree about who is in the canary.

Membership is by the client's **service name**, the `name` of a `services:` entry. A client that announces no service name is on the stable side: it predates the split, and treating it as the new version would send traffic to the one candidate nobody nominated.

Two things it deliberately does not do. **It never empties the pool**: if the canary side is down, or the stable side has been fully replaced, the request is served from whatever is left, because an experiment must not take the route with it. And **a failover keeps the visitor on their side** where it can, since a re-dispatch that moved them to the other version would make the split mean nothing exactly when something is going wrong.

Proxied WebSockets are not split. A socket is one long-lived connection rather than a stream of requests, so a split could only ever apply to the upgrade, and a rule that quietly meant something different there would be a second rule beside the one written for HTTP.

## Random subdomains

With `routing.random_subdomain="*.example.com"` on the server (fronted by a wildcard DNS/proxy route), every connecting client is automatically assigned a hostname like `a1b2c3d4e5.example.com`. Assignments are additive, declared and token-granted binds keep working alongside, and they are per-connection except when the client sends an instance group and the token granted binds: then the label is derived from both, so every parallel connection of one process shares the same random hostname rather than getting one each.

The value is a pattern: the `*` in the leftmost label is replaced with a random label. `example.com` is shorthand for `*.example.com`, and `*-test.example.com` yields `<random>-test.example.com`, same subdomain level, so one wildcard TLS certificate covers the generated hostnames.

## Dashboard overrule

The dashboard can temporarily override any client's binds ("Overrule"), handy for redirecting a hostname live. Overrides live only in server memory: a reconnect or restart reverts to the client's own configuration.

Related: [In-Flight Failover](failover.md) covers what happens when the chosen client dies mid-request.

## Passive outlier ejection

Active health probing (`health.endpoint`, `APERIO_HEALTH_ENDPOINT`) pulls a service from
rotation when its own `/health` check fails. Passive outlier ejection is the
complement: it reacts to how a service behaves under **real traffic**. It is on
in the `safe` resilience profile, the default, and `aggressive`, and off in `off`;
`resilience.outlier.enabled` decides it directly. A service that returns too many server errors,
times out, or drops connections in a short window is temporarily removed from
the routing pool, even while its `/health` probe still reports green.

Ejection is per **service**, not per connection, and the difference is visible
once a client carries more than one with
[`multiplex: true`](configuration.md#one-connection-for-several-services): a
failing backend takes its own service out of rotation and leaves the services
sharing its connection serving. For a client running one service per
connection, which is the ordinary shape, the two are the same thing.

| Setting | Meaning | Default |
| --- | --- | --- |
| `resilience.outlier.enabled` | Enable passive ejection. | on, off when the profile is `off` |
| `resilience.outlier.max_failures` | Failures within the window that trigger an ejection. | `5`, `3` when aggressive |
| `resilience.outlier.window` | Time the failures are counted over. | `30s` |
| `resilience.outlier.eject_for` | How long an ejected service stays out before automatic re-admission. | `30s`, `1m` when aggressive |

Ejection is **per-route fail-open**: if every candidate for a route is ejected,
the route keeps serving from the struggling pool rather than returning no route
at all, a bad backend is still better than a guaranteed error.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`load_balancing`](examples/load_balancing/): primary/standby tiers, per service
- [`sticky_sessions`](examples/sticky_sessions/): sticky sessions
- [`random_subdomain`](examples/random_subdomain/): preview subdomains
- [`routes`](examples/routes/): client-less routes
- [`traffic_rules`](examples/traffic_rules/): per-route rate limits, WAF-lite, fallbacks, per-hostname error pages
