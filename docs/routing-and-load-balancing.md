# Routing & Load Balancing

Several clients can be connected to one Aperio server at the same time. When a public request arrives, the server picks a client in four steps: eligibility, hostname, path, and strategy.

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent yaml key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_LB_STRATEGY` → `lb_strategy`, `APERIO_REQUIRE_HOSTNAME_BIND` → `require_hostname_bind`). YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

## Eligibility

Clients are skipped when they are unhealthy (no heartbeat within `APERIO_CLIENT_DOWN_THRESHOLD`, default 15 s), when their own backend health probe is failing, when they are draining for shutdown, or when they were disabled from the dashboard. In-flight requests always finish.

## Hostname binds

A client can claim one or more hostnames, via `--host`, hostnames granted by its token, or an automatic random subdomain. Clients whose binds contain the request's `Host` header (case-insensitive, port ignored) win. If none match, clients *without* any hostname bind act as a fallback pool.

For strict multi-tenant setups, set `APERIO_REQUIRE_HOSTNAME_BIND=1`: clients without a hostname bind then never receive traffic, and unmatched requests fail with 504.

```
a.example.com  ──▶  client A (--host a.example.com)
b.example.com  ──▶  client B (--host b.example.com)
c.example.com  ──▶  client C (no hostname bind, fallback)
```

## Path binds

Within the hostname pool, the longest matching path bind wins. Binds match on segment boundaries: `/api` matches `/api` and `/api/v1`, never `/apixyz`. By default the bind prefix is stripped before forwarding (`/api/v1/users` arrives at the backend as `/v1/users`); set `APERIO_TRIM_BIND=0` to keep the full path.

## Strategies

`APERIO_LB_STRATEGY` decides how a client is picked from the final pool:

- **`round-robin`** (default), clients with identical binds share traffic evenly.
- **`primary-standby`**, only the clients with the lowest announced priority (`--priority`, 0 = primary) receive traffic. Standby tiers take over automatically when every more-primary client is unhealthy, draining, disabled, or gone, and hand back when a primary returns. The dashboard marks standby clients with a `standby N` badge.
- **`sticky`**, first-time visitors are rotated round-robin, then an `aperio_affinity` cookie (HttpOnly, 24 h) pins each visitor to the client that served them, including their WebSockets. Use this when backends hold per-visitor state (PHP sessions, in-memory carts). Affinity keys on the client's instance ID, so it survives reconnects of the same process; the cookie is stripped before requests reach backends.

## Canary releases and weighted routing

`canary:` on a `routes:` policy entry splits a route's traffic between two versions of a service:

```yaml
routes:
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

With `APERIO_RANDOM_SUBDOMAIN="*.example.com"` on the server (fronted by a wildcard DNS/proxy route), every connecting client is automatically assigned a hostname like `a1b2c3d4e5.example.com`. Assignments are per-connection and additive, declared and token-granted binds keep working alongside.

The value is a pattern: the `*` in the leftmost label is replaced with a random label. `example.com` is shorthand for `*.example.com`, and `*-test.example.com` yields `<random>-test.example.com`, same subdomain level, so one wildcard TLS certificate covers the generated hostnames.

## Dashboard overrule

The dashboard can temporarily override any client's binds ("Overrule"), handy for redirecting a hostname live. Overrides live only in server memory: a reconnect or restart reverts to the client's own configuration.

Related: [In-Flight Failover](failover.md) covers what happens when the chosen client dies mid-request.

## Passive outlier ejection

Active health probing (`APERIO_TARGET_HEALTH`) pulls a service from
rotation when its own `/health` check fails. Passive outlier ejection is the
complement: it reacts to how a service behaves under **real traffic**. When
`APERIO_OUTLIER_EJECTION=1`, a service that returns too many server errors,
times out, or drops connections in a short window is temporarily removed from
the routing pool, even while its `/health` probe still reports green.

Ejection is per **service**, not per connection, and the difference is visible
once a client carries more than one with
[`multiplex: true`](configuration.md#one-connection-for-several-services): a
failing backend takes its own service out of rotation and leaves the services
sharing its connection serving. For a client running one service per
connection, which is the ordinary shape, the two are the same thing.

| Variable | Meaning | Default |
| --- | --- | --- |
| `outlier_ejection` (env `APERIO_OUTLIER_EJECTION`) | Enable passive ejection. | off |
| `outlier_max_failures` (env `APERIO_OUTLIER_MAX_FAILURES`) | Failures within the window that trigger an ejection. | `5` |
| `outlier_window` (env `APERIO_OUTLIER_WINDOW`) | Seconds the failures are counted over. | `30` |
| `outlier_eject_secs` (env `APERIO_OUTLIER_EJECT_SECS`) | How long an ejected service stays out before automatic re-admission. | `30` |

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
