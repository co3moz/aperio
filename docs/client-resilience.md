# Client Resilience

The client is built to run unattended: it survives server restarts, sick backends, config changes, and deployments without dropping visitor traffic on the floor.

## Reconnect with backoff and jitter

On connection loss the client reconnects with exponential backoff — starting at 1 s, doubling up to 60 s, randomized — so a restarted server is not stampeded by its whole client fleet at once. The backoff resets after a connection stays up for 30 s.

## Backend health probing

Set `APERIO_CLIENT_TARGET_HEALTH` (a path like `/health`, or a full URL) and the client probes your backend independently, reporting the result to the server:

- A failing backend takes the client **out of routing without dropping the tunnel** — no reconnect churn, no lost binds.
- It rejoins automatically when the probe recovers.
- The dashboard shows a `BACKEND DOWN` badge meanwhile.

Probe cadence is tunable: `APERIO_CLIENT_HEALTH_INTERVAL` (default 10 s), `APERIO_CLIENT_HEALTH_TIMEOUT` (5 s), and `APERIO_CLIENT_HEALTH_THRESHOLD` (2 consecutive failures before the backend is reported unhealthy).

## Config hot-reload

When a config file is present (`./aperio.yaml` or `--config`), edits are detected within ~5 s: the current connection is dropped gracefully and the client reconnects with the freshly resolved `token`, `server`, `target`, `hostname`, `path`, and `priority`. CLI arguments and environment variables keep their precedence over the file; a file that no longer parses is ignored with a warning rather than killing the client.

## Graceful shutdown

On `SIGINT`/`SIGTERM` the client tells the server it is **draining**: the server immediately stops routing new requests to it, in-flight requests finish (up to 30 s), then the process exits. This plays well with `docker stop` and rolling deployments — combined with [failover](failover.md) or a standby client, restarts are invisible to visitors.

## Flow control

Two knobs keep a client from being overwhelmed:

- `APERIO_CLIENT_MAX_CONCURRENT` — announced to the server, which queues the excess instead of flooding the backend; also enforced locally.
- `APERIO_CLIENT_BANDWIDTH` — declare the link capacity (`8mbit`, `500kbit`, `2MB`, or plain bytes/second) and the server paces outgoing tunnel frames with a token bucket (1 s burst) so the client is never pushed faster than its network can drain.

## Self-diagnosis

`aperio-client check` resolves the configuration with the usual precedence and verifies every hop: the server health endpoint (including a version and protocol comparison), token validity via a real tunnel handshake, the local target, and its health endpoint when configured. Exit code 0 = all green — handy in support requests and provisioning scripts.
