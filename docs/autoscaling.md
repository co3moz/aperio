# Autoscaling

Aperio is the only component that sees demand and supply at the same moment: requests arriving for a hostname, and the clients connected to serve them with the concurrency limits they announced. That makes it a natural place to decide *when* more capacity is needed. It is a poor place to decide *how* to create it, so it does not try: Aperio calls a URL you control and tells it the desired capacity. Starting machines, scaling a deployment, or waking a container is your provider's job.

Two things fall out of that one signal:

- **Cold start (0 to 1).** A request arrives for a hostname no client is serving. Instead of answering `504`, the server calls your endpoint and holds the request until the service comes up.
- **Scale out (N to N+1).** The connected pool is saturated for long enough to be real, and the server asks for one more instance.

Scaling **in** is deliberately not the server's job: an idle client retires itself with `idle_timeout`. The server never kills anything, which removes an entire class of "why did my instance disappear" incidents.

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent `aperio-server.yaml` key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_SCALING` → `scaling`). YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

## Enabling it

Autoscaling is off unless the operator turns it on. A client's declaration is ignored entirely without it:

```yaml
# aperio-server.yaml
scaling: true          # env: APERIO_SCALING=1
```

Then declare it on the client, in `aperio.yaml`:

```yaml
# ./aperio.yaml (client)
server:
  url: https://tunnel.example.com
  token: apr_...
services:
  - target: http://localhost:3000
    hostname: app.example.com

scaling:
  url: https://api.provider.example/apps/web/scale
  secret: ${SCALE_SECRET}   # sent as Authorization: Bearer
  min: 0                    # 0 = scale to zero, so a request can cold start it
  max: 8                    # ceiling the server will never ask to exceed
  cold_start: 45s           # how long a visitor may be held while it starts
  target_utilization: 0.8   # scale out above this
  window: 15s               # ... after it has stayed there this long
  cooldown: 60s             # minimum gap between two calls for one bind

idle_timeout: 5m            # the scale-in half: retire when nothing arrives
max_concurrent: 10          # what one instance can handle, and how saturation is measured
```

The declaration is announced on every heartbeat and **persisted server-side against the hostname**. That is the point: with `min: 0` the server must be able to call your endpoint when nothing is running at all, long after the client that declared it exited.

## What your endpoint receives

A `POST` with a JSON body, and your secret as `Authorization: Bearer` when you set one:

```json
{
  "reason": "cold_start",
  "hostname": "app.example.com",
  "path": null,
  "org_id": null,
  "current": 0,
  "desired": 1,
  "min": 0,
  "max": 8
}
```

`reason` is `cold_start` or `scale_out`. Any `2xx` counts as accepted; nothing else about the response is used, the body is never read, and redirects are never followed.

**Make it idempotent.** The same call can legitimately arrive more than once: several Aperio servers in an HA pair each keep their own view, and a server restart forgets that a call was in flight.

## What actually triggers a scale-out

Not the request count. Each client already carries a concurrency limiter sized by its announced `max_concurrent`, and the server queues on it rather than flooding a backend, so permits in use is exactly "work the pool is doing right now". Utilization is:

```
utilization = inflight / sum(max_concurrent)   # over routable, primary-tier clients
```

The server scales out when that stays at or above `target_utilization` for the whole `window`. A single spike does not qualify, which is what keeps a bursty service from oscillating. Four instances of `max_concurrent: 10` are saturated at 32 concurrent requests, not at "request number 41".

Two exclusions matter: standby-tier clients (`priority > 0`) never count as capacity, because they exist to be idle and counting them would mask saturation of the primaries; and clients that are draining, disabled, or failing their backend probe are not capacity either. A service whose clients announce no `max_concurrent` has no measurable utilization and is never scaled out (cold starts still work).

## What a cold start does to the request

The request that finds an empty pool is **held**, not failed, and re-dispatched once an instance is routable. Because it was never dispatched anywhere, holding it is safe for every method including `POST`, unlike a failover re-dispatch, which is why the `failover` rules do not apply here.

The hold releases its global concurrency slot first. Otherwise a hundred visitors waiting 45 seconds for one sleeping service would occupy every request slot on the server and take healthy services down with it.

If the budget expires, the normal chain resumes: a stale cached answer if the service opted into `resilience`, then a `fallbacks:` redirect, then `504`.

Three things deliberately do **not** trigger a cold start:

- a hostname in **maintenance mode**, which is explicit operator intent that the site be down;
- a visitor the owning token's `allowed_ips` would have rejected. With an empty pool there are no candidates left to evaluate that against, so it is checked up front. Without it, a blocked address could trigger a billable cold start and learn the route exists;
- a bind whose record the breaker has disarmed (below).

## Single flight, cooldown, and the breaker

A burst of a hundred requests against a sleeping service makes **one** call. Everyone else waits on the same signal.

After a call the bind cools down for `cooldown`, because a new instance needs time to appear and asking again while it starts just costs money. A failed call backs off exponentially instead, and after five consecutive failures the record is **disarmed**: it stops being called at all, and an alert lands in the audit log. Re-announcing the declaration (or editing it) re-arms it, so restarting a fixed client is enough to recover.

Across all binds, at most eight calls are ever in flight at once, so a server restart with many armed records cannot turn into a burst against your provider's API.

## Watching it

```bash
# from anywhere, with an admin key (see docs/cli-api.md)
aperio-client api scaling list
aperio-client api scaling disarm <id>
```

`list` shows every armed record for your organization with its live pool: `instances`, `capacity`, `inflight`, `utilization`, and whether the breaker has tripped. The secret is never returned, only whether one is set. `disarm` removes a record; a client that is still running and still declares the block re-arms it on its next heartbeat, which is the intended way to undo an accidental delete.

Every call is audited (`scaling_requested` / `scaling_failed`) and emitted as a webhook event, so a scale-out is visible next to the traffic that caused it.

## Security

The declaration comes from a client, which is a lower-trust credential than an operator, and it makes the server perform an outbound request. That is server-side request forgery by construction, so the destination is fenced:

- **https only**, unless the operator sets `APERIO_SCALING_ALLOW_HTTP=1`.
- **public addresses only**, unless the operator sets `APERIO_SCALING_ALLOW_PRIVATE=1`; every address the hostname resolves to is checked, not just the first.
- **Every resolved address is checked**: loopback, private ranges, link-local (including `169.254.169.254`, the cloud metadata address), carrier-grade NAT, and their IPv6 equivalents are refused. A hostname that resolves to one of them is refused too.
- **Redirects are never followed**, since a redirect is a way to reach an address the pre-flight check just refused.
- The secret is write-only: never returned by the API, never logged.
- **The operator's outbound policy applies on top of all of that.** `APERIO_OUTBOUND_ALLOWLIST` / `APERIO_OUTBOUND_BLOCK_PRIVATE` cover webhook deliveries and scaling hooks alike; a destination has to pass both the fence above and the policy. See [Threat Model](threat-model.md).

Records are scoped to the organization of the token that armed them, and a record disappears when the last token that armed it is revoked or expires. `APERIO_SCALING_RECORD_TTL` (default 30 days) additionally drops records nothing has re-announced.

## Settings

| Variable | yaml key | Description | Default |
| --- | --- | --- | --- |
| `APERIO_SCALING` | `scaling` | Honor client `scaling:` declarations. | `0` |
| `APERIO_SCALING_ALLOW_HTTP` | `scaling_allow_http` | Permit a plain-http endpoint. | `0` |
| `APERIO_SCALING_ALLOW_PRIVATE` | `scaling_allow_private` | Permit an endpoint resolving to a private/loopback/link-local address. | `0` |
| `APERIO_OUTBOUND_ALLOWLIST` | `outbound.allowlist` | Host/CIDR patterns the server may call for scaling hooks and webhooks; everything else refused. Empty = no restriction. | |
| `APERIO_OUTBOUND_BLOCK_PRIVATE` | `outbound.block_private` | With no allowlist: refuse destinations that resolve to internal addresses. | `0` |
| `APERIO_SCALING_RECORD_TTL` | `scaling_record_ttl` | Seconds before an unrefreshed record is dropped. | `2592000` (30 d) |

Client side, in `aperio.yaml`: the `scaling:` block above, `idle_timeout` (also `APERIO_IDLE_TIMEOUT`), and `max_concurrent`, which is what makes utilization measurable.

## Related

- [Client Resilience](client-resilience.md), graceful drain, which is what makes `idle_timeout` invisible to visitors.
- [Performance Tuning](performance-tuning.md), the concurrency knobs autoscaling reads.
- [Observability](observability.md), the audit and webhook events.
