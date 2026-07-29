# Client Resilience

The client is built to run unattended: it survives server restarts, sick backends, config changes, and deployments without dropping visitor traffic on the floor.

> **Config surfaces.** Client settings below are named by their `APERIO_*` environment variable; each also has an equivalent `aperio.yaml` key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_TARGET_HEALTH` → `target_health`, `APERIO_MAX_CONCURRENT` → `max_concurrent`), settable per `services:` entry or at the top level. YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

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

A client that is *not* connected when the signal arrives, sitting in the reconnect backoff above because the server is unreachable, exits right away: there is nothing to announce and no connection to drain. It waits only for whatever a sibling service of the same process still has in flight, and it never opens a new connection on the way out.

## Flow control

Two knobs keep a client from being overwhelmed:

- `APERIO_MAX_CONCURRENT`, announced to the server, which queues the excess instead of flooding the backend; also enforced locally.
- `APERIO_BANDWIDTH`, declare the link capacity (`8mbit`, `500kbit`, `2MB`, or plain bytes/second) and the server paces outgoing tunnel frames with a token bucket (1 s burst) so the client is never pushed faster than its network can drain.
- **Per-stream pause/resume** (tunnel protocol v3), which needs no configuration on the client at all. When a visitor reads more slowly than the backend produces, the server asks this client to pause that one stream, and the client stops reading its source, the backend response body, the backend WebSocket, or the TCP socket, so ordinary TCP backpressure reaches the backend. It resumes when the server says so, and after 30 s on its own if that message is ever lost. See [Tunnel Protocol](tunnel-protocol.md).

## Idle retirement

With `idle_timeout` set, a client that has stopped being used retires itself and exits after the usual graceful drain, which is the scale-in half of [autoscaling](autoscaling.md): the server never kills a client, it only ever asks for more capacity.

What counts as being used is deliberately broad. Any inbound work starts the clock, buffered requests, streamed uploads, WebSocket upgrades and raw TCP or UDP sessions alike, and every relayed frame of a long-lived session keeps re-stamping it in both directions, so a database tunnel or a chat backend that outlives the window is not cut mid-traffic. Retirement also waits while any request is still in flight, which covers a backend that takes minutes to answer or a response that streams for longer than the window. The clock only starts after the first piece of work, so a client that was just cold-started cannot retire before it has had the chance to be used.

## Backend redirects

Backends often answer `http://` targets with a redirect to `https://`, or bounce between hosts of the same domain. The client follows such redirects transparently, same-host scheme upgrades and hops within the same root domain (`example.com` → `test.example.com`), up to `APERIO_MAX_REDIRECTS` jumps (default 5, `0` = pass all redirects through). Https-to-http downgrades and redirects to unrelated domains are never followed; they reach the visitor as normal redirect responses.

## Self-diagnosis

`aperio-client check` resolves the configuration with the usual precedence, reporting which layer (CLI argument, `./aperio.yaml`, environment, `~/.aperio.yaml`) supplied each value, and verifies every hop: the server health endpoint (including a version and protocol comparison), token validity via a real tunnel handshake, every local target (all `services:` entries in multi-service mode), and their health endpoints when configured. Exit code 0 = all green, handy in support requests and provisioning scripts.

## Cross-server failover

`APERIO_SERVER_URLS` (comma-separated) lists additional Aperio servers the client may connect to. The primary `APERIO_SERVER_URL` is always tried first; after a failed or dropped connection the reconnect loop rotates to the next server, so a client survives a whole server going down as long as another accepts it. This is the client half of a highly-available deployment, point several clients at a server fleet behind a shared token (and, when the servers share persistent state, at a shared token store). With a single server the setting is a no-op.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`health_check`](examples/health_check/): independent backend probes per service
