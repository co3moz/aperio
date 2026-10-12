# Traffic

How a request reaches a backend, and what happens when something on the way fails: routing, the client's connections, failover, the response cache and the limits.

## Routing

The server narrows the clients connected to it in stages, then picks one.

**Eligibility.** A client is skipped while it is silent past `gateway.client_down_threshold` (15 s), while its backend's health probe fails, while it drains for shutdown, while it is disabled from the dashboard, or while it is ejected (below). Requests in flight always finish.

**Hostname.** A client serves the hostnames it declares (`hostname:`), the ones its token grants, and a [random subdomain](#random-subdomains) when the server hands them out. The `Host` header is matched case-insensitively, port ignored. A client declaring `hostname: "*"` serves every hostname nobody claims; a token limited to particular hostnames cannot claim `*`. A service naming no hostname is not a catch-all, and is told in its log when nothing routes to it. A request nothing claims answers `504`.

**Path.** Within the hostname, the longest `path:` wins, on segment boundaries: `/api` matches `/api/v1`, never `/apix`. The prefix is stripped before forwarding unless `trim_bind: false`.

**Strategy.** `routing.lb_strategy` picks from what is left:

- **`round-robin`** (default) spreads requests evenly.
- **`primary-standby`** sends traffic only to the lowest `priority` (0 is primary). A standby tier takes over when every client above it is gone, and hands back when one returns.
- **`sticky`** pins a visitor to the client that first served them with an `aperio_affinity` cookie (24 h), WebSockets included, for backends holding per-visitor state.

### Canary releases

A site's `canary` sends a share of visitors to another version of a service:

```yaml
sites:
  - hostname: app.example.com
    canary:
      service: web_v2      # the service name the new version's client announces
      weight: 20           # percent of visitors
      header: x-canary     # and anyone who sends this header
      value: "on"
```

The split is per visitor (a hash of their address), not per request, so one page load never mixes versions. `weight: 0` with a `header` is opt-in only. A canary never empties the pool: if one side is down, the other serves. WebSockets are not split.

### Random subdomains

With `routing.random_subdomain: "*.example.com"` behind a wildcard DNS record and certificate, every client gets a hostname like `a1b2c3d4e5.example.com` beside its own binds. The `*` may sit inside the label (`*-test.example.com`). Parallel connections of one process share one name. `routing.preview_noindex` keeps search engines off them.

## Connections

A client carries its services over WebSocket connections it opens to the server. How many depends on how many services it has.

**Several services share one connection** unless a service writes something sharing would change. That saves a socket, a TLS session, a reader, a writer and a heartbeat per service on both ends. The cost is a shared link: a large response occupies the writer the others send through, and a dropped connection takes them all out of routing together. A service keeps a connection of its own when it writes:

- `multiplex: false`, which is worth writing on a service with large responses;
- `connections:` above one;
- a `bandwidth:` of its own, since the server paces the socket rather than the service;
- `startup_delay:` or `depends_on:`, since a shared connection opens when its last member is ready;
- `tcp_target:`, since a raw stream does not say which service it is for.

`multiplex: true` shares any of them anyway. Each service on a shared connection keeps its own binds, gate, health, ejection and limits. At most 256 services share one connection.

**A service on a connection of its own grows a pool under load.** Unwritten, `connections` is `{min: 1, max: 4}`: one connection while quiet, one more each time the peak of requests in flight reaches eight per open connection (at most every 10 s), and one fewer after two minutes of calm.

```yaml
services:
  api:
    target: http://localhost:3000
    connections: {min: 2, max: 8}    # or a fixed number: connections: 4
```

The server's `limits.connections_per_service`, lowered by a token's `max_connections`, caps the pool. A connection the pool grew that the server refuses as full (its `limits.tunnels`, or an organization's quota) stands down, and the pool waits two minutes before trying again, so a pool only takes room the server has free. A service whose bandwidth is paced keeps one connection unless it says otherwise.

More connections are not automatically faster: each costs tasks and buffers on both ends, and on a host where client and server share CPU, throughput peaked at four connections in our measurement.

**Bandwidth.** `bandwidth:` on a service caps how fast the server pushes to it (`8mbit`, `500kbit`, `2MB`). The top-level `bandwidth` is a budget for the whole client: services that name a rate keep it and the rest split what is left; requests over the budget are scaled to fit, with a warning. A service's share is divided across its pool's ceiling, so a single transfer over one of four connections sees a quarter of it. Services sharing a connection announce their shares' sum, because the socket is what is paced.

## When something fails

### A client dies mid-request

A dispatched request whose client drops before any byte reaches the visitor is re-dispatched, by `resilience.failover.mode`:

| Mode | What happens | Profile |
| --- | --- | --- |
| `fail` | `502` at once. | `off` |
| `retry` | Another client of the route, or `502` if there is none. | `safe` (default) |
| `wait` | The same client, once it reconnects. | |
| `retry-wait` | Another client, or wait for one to appear. | `aggressive` |

`resilience.failover.max_jumps` bounds the attempts and `resilience.failover.window` the waiting. Only idempotent methods fail over unless `resilience.failover.all_methods: true`, since a `POST` may already have reached the backend. Streamed uploads and responses never fail over.

`resilience.failover.retry_on_5xx` (on in `aggressive`) also re-dispatches a buffered response whose status is a server error, sharing the same jump budget.

A request for a route whose client is between connections is held up to `gateway.timeout` for one to appear, rather than refused at once.

### A client keeps failing

**Outlier ejection** takes a service out of routing when it fails too often under real traffic, even while its health probe reports green: `resilience.outlier.max_failures` failures within `resilience.outlier.window` eject it for `resilience.outlier.eject_for`. It is per service, so a failing backend on a shared connection does not take its siblings out. If every candidate of a route is ejected, the route keeps serving from them rather than answering nothing.

`resilience.profile` (`off`, `safe`, `aggressive`) sets all of the above at once; each field written wins over the profile.

### A backend fails behind the client

The server's failover cannot help when a client's own backend refuses connections. `retry:` and `circuit_breaker:` cover that hop, per service or under `defaults:`:

```yaml
services:
  api:
    target: http://localhost:3000
    retry: {attempts: 3, delay: 100ms}       # total attempts; POST only with all_methods: true
    circuit_breaker: {failures: 5, open_for: 30s}
```

A retry covers only failures before a response head, for requests that can be replayed. The breaker counts any response, `500` included, as the backend answering. A service's `resilience:` profile (`off`, `safe`, `aggressive`) sets both; written fields win. A reused keep-alive socket that the backend had just closed is always re-dialled once, whatever `retry:` says.

**Health probes.** `health.endpoint` (`/health`, or a URL) makes the client probe its backend every `health.interval` (10 s). After `health.threshold` failures the service leaves routing without dropping its tunnel, and rejoins when the probe passes. `health.wait_for_backend` holds a new service out of routing until its backend first answers.

## The client keeps itself up

- **Reconnect.** A lost connection is retried with backoff from 1 s to 60 s, randomized, reset after 30 s of being up. A server that is full (`503`) is retried about once a second; one that announced its shutdown, after a few hundred milliseconds.
- **Several servers.** `server.urls` lists servers to fall back to, tried in order when `server.url` is unreachable. A server can also announce alternates with `routing.alternate_servers`.
- **Graceful shutdown.** On `SIGINT` or `SIGTERM` the client tells the server it is draining, which stops routing to it at once, lets requests in flight finish for up to 30 s, and exits. A reload drains the same way, up to `reload_drain`.
- **Idle retirement.** `idle_timeout` makes a client exit after serving nothing for that long, the scale-in half of [autoscaling](integrations.md#autoscaling). Any traffic, a long-lived TCP session included, resets the clock.
- **Redirects.** The client follows a backend's redirect to `https://` on the same host, or within the same root domain, up to `limits.redirects` (5). A downgrade or a foreign domain reaches the visitor as a redirect.

## Response cache

The server can answer repeated `GET`s from memory without crossing the tunnel. A response is cached only when all three agree:

1. the server turned the cache on: `cache.enabled`, with a budget in `cache.size`;
2. the service opted in: `cache: {enabled: true}` on the service or under `defaults:`;
3. the response allows shared caching: `max-age` or `s-maxage`, none of `no-store`, `no-cache`, `private`, no `Vary` and no `Set-Cookie`.

Only `GET`s without credentials or cookies are answered from the cache. A hit carries `x-aperio-cache: hit` and an `Age` header. On top of that:

- `If-None-Match` is answered `304` at the edge, with a synthesized `ETag` when the backend sent none.
- Concurrent requests for one missing entry make one fetch.
- `stale-while-revalidate=N` serves the expired entry while one refresh runs in the background.
- `Range` requests are sliced from the stored body.
- `cache.negative_ttl` holds `404` and `410` answers briefly for hot missing URLs (off by default).

`cache.offline: true` on a service keeps cached entries answering for up to `cache.max_stale` past their lifetime while no client of it is connected, marked `x-aperio-stale: true`, even under the server's `deny` default.

`POST /aperio/api/cache/purge` drops entries by `hostname`, `path_prefix` or `surrogate_key`. A service that opts in while the server's cache is off is told so in its log and shows a `cache off` badge.

## Limits and backpressure

**Which limit answered `429`.** Every refusal names itself in `x-aperio-limit`, and `aperio_rate_limited_total{limit=...}` counts them:

| `x-aperio-limit` | What refused | Where it is set |
| --- | --- | --- |
| `ip` | the per-visitor bucket | `limits.ip_rate` |
| `server-concurrency` | requests in flight on the server | `limits.concurrent_requests` |
| `route` | a site's rate limit | `rate_limit` on the matching site |
| `client-concurrency` | the client's own limit, nothing freed before the gateway timeout | `limits.concurrent_requests` in the client's file |
| `token-rate`, `token-quota` | the token's rate limit or daily bytes | the token |
| `org-quota` | the organization's monthly bytes | the organization's quota |
| `streams-per-ip` | streamed responses one address holds open | `limits.streams_per_ip` |

A load test comes from one address and meets `limits.ip_rate` first; raise it for the run.

**The client's own limit.** `limits.concurrent_requests` on a service is announced to the server, which queues the excess rather than flooding the backend. `limits.adaptive_concurrency: true` moves the announced number with the backend: every 10 s it halves when requests wait over 250 ms for a slot, and climbs back one at a time under 50 ms, never above what was written.

**Slow visitors.** When a visitor reads more slowly than the backend produces, the server asks the client to pause that one stream at `advanced.stream.pause_at` (2 MB of backlog) and resume at `advanced.stream.resume_at` (512 KB), so TCP backpressure reaches the backend. Memory is roughly `pause_at` per slow stream.

**What every request pays for.** On a saturated server, three conveniences are the first to switch off: the request inspector (`inspector.enabled` on the server, or `inspector: false` on one busy service), the per-request access event (`logging.access_events`), and OpenTelemetry at `otel.sample_rate: 1.0` (`0.01` keeps traces useful).

**Compression.** `advanced.compression` compresses tunnel frames: a win on a slow or metered link, a loss on a fast LAN. Bodies over 32 KB travel as binary frames either way.

## Examples

[`load_balancing`](examples/load_balancing/), [`sticky_sessions`](examples/sticky_sessions/), [`random_subdomain`](examples/random_subdomain/), [`routes`](examples/routes/), [`traffic_rules`](examples/traffic_rules/), [`failover`](examples/failover/), [`health_check`](examples/health_check/), [`cache`](examples/cache/), [`resilience`](examples/resilience/), [`tuning`](examples/tuning/).
