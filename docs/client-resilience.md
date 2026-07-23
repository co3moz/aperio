# Client Resilience

The client is built to run unattended: it survives server restarts, sick backends, config changes, and deployments without dropping visitor traffic on the floor.

> **Config surfaces.** Client settings below are named by their `APERIO_*` environment variable; each also has an equivalent `aperio.yaml` key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_TARGET_HEALTH` → `target_health`, `APERIO_MAX_CONCURRENT` → `max_concurrent`), settable per `services:` entry or at the top level. YAML is the primary surface. See [Configuration](configuration.md) for the full mapping.

## Reconnect with backoff and jitter

On connection loss the client reconnects with exponential backoff, starting at 1 s, doubling up to 60 s, randomized, so a restarted server is not stampeded by its whole client fleet at once. The backoff resets after a connection stays up for 30 s.

## Backend health probing

Set `APERIO_TARGET_HEALTH` (a path like `/health`, or a full URL) and the client probes your backend independently, reporting the result to the server:

- A failing backend takes the client **out of routing without dropping the tunnel**, no reconnect churn, no lost binds.
- It rejoins automatically when the probe recovers.
- The dashboard shows a `BACKEND DOWN` badge meanwhile.

Probe cadence is tunable: `APERIO_HEALTH_INTERVAL` (default 10 s), `APERIO_HEALTH_TIMEOUT` (5 s), and `APERIO_HEALTH_THRESHOLD` (2 consecutive failures before the backend is reported unhealthy).

## Config hot-reload

When a config file is present (`./aperio.yaml` or `--config`), edits are detected within ~5 s: the current connection is dropped gracefully and the service restarts with the freshly resolved configuration, every setting applies, including timeouts, concurrency, bandwidth, health probing, and redirect limits. The usual layering applies on reload (CLI > `./aperio.yaml` > env > `~/.aperio.yaml`); a file that no longer parses (or resolves to an invalid configuration) is ignored with a warning rather than killing the client.

## Graceful shutdown

On `SIGINT`/`SIGTERM` the client tells the server it is **draining**: the server immediately stops routing new requests to it, in-flight requests finish (up to 30 s), then the process exits. This plays well with `docker stop` and rolling deployments, combined with [failover](failover.md) or a standby client, restarts are invisible to visitors.

## Flow control

Two knobs keep a client from being overwhelmed:

- `APERIO_MAX_CONCURRENT`, announced to the server, which queues the excess instead of flooding the backend; also enforced locally.
- `APERIO_BANDWIDTH`, declare the link capacity (`8mbit`, `500kbit`, `2MB`, or plain bytes/second) and the server paces outgoing tunnel frames with a token bucket (1 s burst) so the client is never pushed faster than its network can drain.

## Backend redirects

Backends often answer `http://` targets with a redirect to `https://`, or bounce between hosts of the same domain. The client follows such redirects transparently, same-host scheme upgrades and hops within the same root domain (`example.com` → `test.example.com`), up to `APERIO_MAX_REDIRECTS` jumps (default 5, `0` = pass all redirects through). Https-to-http downgrades and redirects to unrelated domains are never followed; they reach the visitor as normal redirect responses.

## Self-diagnosis

`aperio-client check` resolves the configuration with the usual precedence, reporting which layer (CLI argument, `./aperio.yaml`, environment, `~/.aperio.yaml`) supplied each value, and verifies every hop: the server health endpoint (including a version and protocol comparison), token validity via a real tunnel handshake, every local target (all `services:` entries in multi-service mode), and their health endpoints when configured. Exit code 0 = all green, handy in support requests and provisioning scripts.

## Cross-server failover

`APERIO_SERVER_URLS` (comma-separated) lists additional Aperio servers the client may connect to. The primary `APERIO_SERVER_URL` is always tried first; after a failed or dropped connection the reconnect loop rotates to the next server, so a client survives a whole server going down as long as another accepts it. This is the client half of a highly-available deployment, point several clients at a server fleet behind a shared token (and, when the servers share persistent state, at a shared token store). With a single server the setting is a no-op.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`s_health_check`](examples/s_health_check/): backend health probe
- [`m_health_check`](examples/m_health_check/): per-service health probes
