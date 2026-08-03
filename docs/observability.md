# Observability

Aperio exposes what it is doing through five channels: metrics for dashboards and alerting, distributed traces for end-to-end request timing, an access log for per-request analysis, an audit trail for security events, and webhooks for pushing events into your own systems.

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent yaml key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_OTEL` → `otel`, `APERIO_ACCESS_LOG` → `access_log`). YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

## Prometheus metrics

Enable with `APERIO_METRICS=1`. The endpoint always requires a token: set `APERIO_METRICS_TOKEN`, or let the server generate one on first start (persisted in `APERIO_DATA_DIR/metrics_token`, printed to the log once).

```yaml
# prometheus.yml
scrape_configs:
  - job_name: aperio
    metrics_path: /aperio/metrics
    params:
      token: ["<your-metrics-token>"]
    static_configs:
      - targets: ["tunnel.example.com"]
```

Exposed metrics include `aperio_requests_total`, `aperio_requests_success_total`, `aperio_requests_failed_total`, `aperio_bytes_transferred_total`, `aperio_connected_clients`, `aperio_pending_requests`, `aperio_ws_streams_active`, `aperio_uptime_seconds`, and per-client `aperio_client_requests_total{client_id=...}`.

A client can attach static labels of its own to its `aperio_client_requests_total` series with `metrics_labels: {env: prod, region: eu-west}` in `aperio.yaml` (or `APERIO_METRICS_LABELS=env=prod,region=eu-west`), so one Prometheus can serve several environments without relabelling rules written against client ids. The server validates and caps what arrives, at most 8 labels, Prometheus-legal names only, values at most 64 characters, and its own label names refused, because these come from clients and label cardinality is how a metrics backend dies. An invalid label is dropped and the client's other labels are kept. Refusals are broken out by cause in `aperio_rate_limited_total{limit=...}`, one series per ceiling (`ip`, `server-concurrency`, `route`, `client-concurrency`, `token-rate`, `token-quota`, `org-quota`), which is how you tell during a load test which limit is firing without reading response headers; see [which limit produced a 429](performance-tuning.md#which-limit-produced-a-429).

Messaging between clients has its own family: `aperio_messages_published_total`, `aperio_messages_delivered_total` (one publish to three subscribers counts three), `aperio_messages_dropped_total`, `aperio_messages_resent_total` and `aperio_messages_abandoned_total` (both QoS 1), plus the gauges `aperio_message_subscribers`, `aperio_message_subscriptions` and `aperio_messages_awaiting_ack`. Subscribers and subscriptions are counted per client *process* and deduplicated, so a client running three services with two filters is one subscriber with two subscriptions rather than three and six. **`aperio_messages_dropped_total` is the one to alert on**: it counts deliveries that could not be written because a subscriber was not keeping up, which means that client silently missed a message. See [Messages Between Clients](messaging.md).

Request latency is exposed as the `aperio_request_duration_seconds` histogram (buckets from 5 ms to 30 s), so p95/p99 can be plotted in Grafana with the usual `histogram_quantile(0.99, rate(aperio_request_duration_seconds_bucket[5m]))` query.

For quota and billing dashboards, per-tenant counters are exposed with `token` and `hostname` labels: `aperio_token_requests_total`, `aperio_token_requests_failed_total`, `aperio_token_bytes_received_total`, `aperio_token_bytes_sent_total` (the label value is the token name, `master` for the master token), and the same four as `aperio_hostname_*_total{hostname=...}` attributed to the request hostname. These are backed by the persistent stats store, so they survive restarts; at most 200 distinct labels are tracked per family, with overflow folded into `__other`.

## Distributed tracing (OpenTelemetry)

Set `APERIO_OTEL=1` to export one span per proxied request over OTLP to an OpenTelemetry collector. Each `proxy.request` span carries the request method, path, host, the selected `aperio.client.id`, and the final response status.

```yaml
# aperio-server.yaml
otel:
  enabled: true                       # env: APERIO_OTEL=1
  endpoint: http://otel-collector:4318 # env: APERIO_OTEL_ENDPOINT, base URL
  protocol: http                      # env: APERIO_OTEL_PROTOCOL, http | grpc
  service_name: aperio-server         # env: APERIO_OTEL_SERVICE_NAME, optional
  sample_rate: 0.01                   # env: APERIO_OTEL_SAMPLE_RATE, 1.0 = every request
```

Both OTLP transports are built in. `protocol: http` sends protobuf over HTTP and appends the `/v1/traces` signal path to the endpoint; `protocol: grpc` sends gRPC to the bare base URL. Left unset, the endpoint's port decides, 4317 is the conventional gRPC port, and anything else is treated as HTTP, so the common collector layouts work without saying anything. Pin it explicitly when the collector listens on a non-standard port: a collector answering the other protocol accepts the connection and drops every span, which is exactly the failure that looks like "tracing is enabled and nothing shows up". The startup probe tests the transport that was actually chosen and warns when the port contradicts it.

The standard `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, and `OTEL_SERVICE_NAME` variables are honored as fallbacks. Spans are batch-exported and flushed on graceful shutdown.

**Sampling.** `sample_rate` is the fraction of requests traced, and it defaults to `1.0`: every request builds a span tree, the proxy span plus a child per phase, and hands it to the exporter. That is the right default for a staging server and the wrong one for a busy production server, where a hundredth of the traffic answers the same questions about latency and error shape. The decision is made once, at the root of the request, and every span of that request follows it, so a sampled trace is always whole rather than a `proxy.request` with three of its eight phases. A request that is not sampled skips the phase-span assembly altogether. If you are benchmarking with tracing on, this is the setting that is in your numbers: at `1.0` you are measuring the exporter as much as the tunnel.

**Context propagation.** If an incoming request already carries a W3C `traceparent` header (e.g. from an upstream gateway or Cloudflare), Aperio adopts it as the span's parent. It then injects its own trace context into the headers forwarded through the tunnel, so a backend that reads `traceparent` continues the same trace, the visitor → Aperio → backend path shows up as one distributed trace. When `APERIO_OTEL` is off there is no overhead and inbound trace headers pass through untouched.

> **Note:** the exporter is compiled into every build (both transports), so enabling it is a configuration change, not a rebuild. It reuses the server's own rustls/ring TLS stack, no second crypto backend and no C toolchain are involved.

## Alerting

Two threshold rules turn the webhook pipeline into a simple pager, point a Slack/Discord/Teams webhook at the `alert_triggered` event:

```yaml
# aperio-server.yaml
alert_error_rate: 5        # env: APERIO_ALERT_ERROR_RATE, alert when ≥5% of proxied requests fail (5xx)…
alert_window: 300          # env: APERIO_ALERT_WINDOW, …measured over a 300 s sliding window (default)
alert_min_requests: 20     # env: APERIO_ALERT_MIN_REQUESTS, quiet windows below 20 requests never alert
alert_client_down: 120     # env: APERIO_ALERT_CLIENT_DOWN, alert when a service stays down for 2 minutes
```

Both rules are off unless their threshold is set. One `alert_triggered` event (kinds `error_rate` / `client_down`) fires per episode and one `alert_resolved` when the condition clears, the error rate resolves at 80% of the threshold, so a value hovering at the limit cannot flap. Alerts are also audit-logged. For richer alerting (latency percentiles, arbitrary PromQL), scrape the Prometheus endpoint with Alertmanager instead.

### Your own rules (`alert_rules:`)

The two rules above are built in. Anything else worth being told about, starting with the disk filling up and the server's own memory climbing, is written as a rule over a quantity the server already measures:

```yaml
# aperio-server.yaml
alert_rules:
  - name: disk-filling          # becomes the alert's `kind`
    metric: store_bytes
    above: 536870912            # 512 MB
    for: 300                    # seconds the condition must hold
  - name: no-clients
    metric: connected_clients
    below: 1
    for: 120
```

| Metric | What it reads |
| --- | --- |
| `connected_clients` | Tunnel clients currently connected |
| `pending_requests` | Proxied requests in flight |
| `store_bytes` | The SQLite store and its `-wal`/`-shm` sidecars on disk, the same figure the self-health panel shows |
| `rss_bytes` | Resident memory of the server process. **Linux only**; elsewhere the rule is reported at startup as one that will never fire, rather than firing on a fabricated zero |

Each rule sets `above` or `below`, never both, and the bound itself is not a breach. `for` applies in **both** directions: the condition must hold that long to fire and hold clear that long to resolve, so a value sitting on its threshold cannot alert and resolve on alternating ticks. Rules fire the same `alert_triggered` / `alert_resolved` events as the built-in ones, with `kind` set to the rule's name, so an existing webhook receiver needs no changes. A malformed rule refuses startup naming the rule, because an alert rule the operator believes is armed is worse than no rule.

## Access log

Every proxied request is emitted as a structured `aperio_access` tracing event on stdout, JSON with `request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, and `error` as top-level fields. Set `APERIO_ACCESS_LOG=/path/to/access.jsonl` to additionally append the same data as raw JSON lines, unaffected by `LOG_LEVEL`, ready to be tailed into Loki or ClickHouse. Query strings are stripped from logs.


TCP and UDP relays produce a line of their own, one **per connection** rather than per packet, with `event: relay_closed`: `transport` (`tcp`/`udp`), `kind` (`expose` for a public port, `tunnel` for a peer client dialling one), `peer`, `client_id`, `tunnel`, `token`, `port`, `duration_ms`, and bytes each way. A per-packet line would produce one entry per datagram of a video stream, which is not a log anybody reads. It goes to the same two places as a request line and takes the same `access_log_sample_rate`, so a query for "everything that touched this token" answers across transports and an operator who turned the volume down gets it turned down here too. This is the record the topology view's dependency edges do not keep: the graph says who depends on a tunnel, this says when and how much. A public `expose:` port authorizes nobody, so its lines carry no `token`, which is the honest answer rather than an invented one.
**Querying the live window.** The server also keeps the most recent 100 requests in memory for the dashboard's traffic view. `GET /aperio/api/logs` returns them, and takes `status` (an exact code like `404`, or a class like `4xx`/`5xx`; a failed request with no status counts as `5xx`), `method`, `path` (a case-insensitive substring of the URI) and `limit` (newest first). The predicates combine with AND, an empty parameter is not a filter, and the caller's organization fence is applied before any of them.

This window is deliberately small: it answers "what is happening right now" for scripts and for the dashboard, not "what happened last Tuesday". The durable record is the access log file above; searching *that* is not offered here, because its lines carry no organization field and could not be scoped safely to a tenant.

## What a client reports about itself

Beyond "is it pinging" and "does its backend answer", each client announces a
few figures about itself on every heartbeat, visible in the dashboard's
clients table (hover the last-ping cell) and in `/aperio/api/stats`:

| Figure | Meaning |
| --- | --- |
| `rtt_ms` | Round trip of this tunnel connection, as the client measures it from its own ping to the matching pong. Smoothed, so one slow exchange does not become the reading. |
| `jitter_ms` | Smoothed variation between consecutive round trips: whether the link is *steady*, which is a different question from whether it is fast. |
| `reconnects` | Times this connection has been re-established since the client process started. Two clients both answering pings look identical without it; a flapping link does not. |
| `cpu_percent` | CPU used by the client process since the previous heartbeat, as a percentage of one core. |
| `rss_bytes` | Resident memory of the client process. |

The two process figures are read from `/proc` and are therefore **Linux only**;
elsewhere they are absent rather than approximated, because a wrong number is
worse than no number. Inside a container they describe the process, not the
cgroup, so they say what the client is using and not how close the container is
to being killed. A client that reports nothing (an older build, or a platform
that cannot read a figure) shows nothing: the server stores the absence rather
than keeping the last value, which would age silently while looking live.

Round-trip time is the figure that separates "the backend is slow" from "the
tunnel is slow", which is otherwise indistinguishable from the server's side.

## Audit log

Administrative and security events, logins (password and OIDC), token create/update/revoke, ephemeral tunnel provisioning, share link creation, maintenance toggles, client connect/disconnect/drain, kill-switch toggles, overrules, replays, and tunnel streams, are appended to `APERIO_DATA_DIR/audit.jsonl` with timestamp, actor IP, and details. Each event also records the acting user and the organization it belongs to. The dashboard shows the most recent 200, filtered to the caller's organization (see [Organizations](organizations.md)). The file is size-rotated (`APERIO_AUDIT_MAX_SIZE`, default 10 MB; `APERIO_AUDIT_MAX_FILES` generations kept, default 3) so long-lived installations cannot fill the disk.

**Searching it.** The recent-200 view answers "what just happened"; a log that is hash-chained and retained by policy also has to answer "what did this user do last Tuesday", which is past the end of that ring. `GET /aperio/api/audit` takes filters, and any filter switches it from the ring to a search of the durable log itself, the active file plus every rotated generation:

| Parameter | Meaning |
| --- | --- |
| `event` | Exact event kind, e.g. `login_success` |
| `actor` | Exact acting user |
| `q` | Case-insensitive substring of the details, the event kind or the actor |
| `from`, `to` | Inclusive bounds, either unix seconds or `YYYY-MM-DD` (UTC; `to` covers the whole day) |
| `limit` | Maximum events returned, default 200, capped at 5000 |

Results come back newest first. `GET /aperio/api/export/audit.csv` takes exactly the same parameters and returns the matching rows as CSV (default limit 5000, capped at 50000), for the auditor who wants them in a spreadsheet. The dashboard's *Audit Log* section exposes both: filter fields, and an export button that carries the current filter.

The organization fence is applied around the search, never inside it: filters can only narrow what a caller may already see, and a filtered request from a child-org user cannot reach another organization's events.

## Webhooks

Define webhooks from the dashboard (name, URL, subscribed events, `*` for all). Where an [outbound policy](threat-model.md) is configured, a URL it does not permit is rejected at creation with the reason rather than failing quietly later. A webhook belongs to the organization that created it and fires only for that organization's events (see [Organizations](organizations.md)). Events are delivered as JSON POSTs with a 10 s timeout:

```json
{ "event": "client_connected", "timestamp": "2026-07-06T15:16:37+03:00", "data": { "client_id": "…", "ip": "…", "token": "tenant-a" } }
```

Available events, grouped by what they are about:

- **Clients**: `client_connected`, `client_disconnected`, `client_draining`.
- **Tokens**: `token_created`, `token_revoked`, `token_rotated`, `token_expiring`, `token_new_ip`, `token_pin_mismatch`, `canary_tripped`.
- **Tunnels and shares**: `tunnel_created`, `tunnel_deleted`, `share_created`.
- **Operations**: `maintenance_on`, `maintenance_off`, `settings_updated`, `import_applied`, `user_created`.
- **Capacity and alerting**: `alert_triggered`, `alert_resolved`, `scaling_requested`, `org_usage`, `disk_usage_warning`.
- **Housekeeping**: `db_backup`, `disk_pruned`.

### Testing a webhook before it matters

A webhook that was configured wrong is otherwise discovered the next time something actually happens, which is exactly the wrong moment. The *Test* button on each webhook (`POST /aperio/api/webhooks/{id}/test`) sends one synthetic `webhook_test` event through the **real** delivery path, the outbound policy check, the signature, the same client and timeout, and reports what the receiver answered: status and duration, or the reason it failed. It lands in the delivery log like any other delivery.

Two deliberate differences from a real event: it is sent **once**, with no retries, because the caller is waiting for the answer and a success on the fourth attempt reported as a success would hide the failure being tested for; and it carries its own event name, so a receiver that switches on `event` can ignore it rather than acting on a deploy that never happened.

### Delivery reliability & the delivery log

A delivery that fails with a transport error, a 5xx, or a 429 is **retried with backoff**, by default 4 retries over ~1.5 minutes (`1s, 5s, 25s, 60s` between attempts; override with `APERIO_WEBHOOK_RETRY_SCHEDULE`, comma-separated seconds, empty = no retries). Other 4xx responses are treated as permanent and not retried. A delivery refused by the outbound policy is not retried either, and the destination is never contacted at all: the refusal is recorded in the log with its reason, so a policy introduced after a webhook was created is visible rather than silent. Redeliveries are re-checked the same way.

Every final outcome (success or failure, with the HTTP status or error, the attempt count, and the exact payload sent) lands in the **delivery log**: the *Recent deliveries* table on the dashboard's Webhooks page, or `GET /aperio/api/webhooks/deliveries` (`?webhook_id=` to filter). The last 500 outcomes are kept in `aperio.db`. Any logged delivery can be **redelivered**, the same payload is re-sent to the webhook's current URL with a fresh signature and the normal retry policy (`POST /aperio/api/webhooks/deliveries/{id}/redeliver`, or the *Redeliver* button), and the outcome is logged as a new row.

### Chat-service formats

Besides the raw JSON above (`generic`, the default), a webhook can be created with a **format** of `slack`, `discord`, or `teams`: point it straight at that service's *incoming webhook* URL and Aperio delivers a ready-made **coloured card** instead, titled with the event and carrying its fields, a Slack `attachment`, a Discord `embed`, or a Teams `MessageCard`. The card colour encodes the event's nature (green for good/recovered events, red for failures like `client_disconnected`/`alert_triggered`, amber for warnings, neutral otherwise). No relay or transformation service needed.

### Signed deliveries

Give a webhook a **signing secret** (16-128 chars, set at creation; never shown again) and every delivery carries:

- `X-Aperio-Timestamp`: Unix seconds at send time.
- `X-Aperio-Signature`: `sha256=<hex HMAC-SHA256 over "<timestamp>.<raw body>">` with the shared secret.

Verify by recomputing the MAC over the exact received body bytes and comparing in constant time; reject stale timestamps (e.g. > 5 minutes old) to block replays:

```python
# in your webhook receiver, not in Aperio
import hmac, hashlib
expected = hmac.new(secret, f"{ts}.".encode() + raw_body, hashlib.sha256).hexdigest()
ok = hmac.compare_digest(f"sha256={expected}", signature_header) and abs(time.time() - int(ts)) < 300
```

## Persistent statistics

Lifetime counters (total requests, success/failure, bytes in each direction, summed duration) and daily/weekly/monthly/yearly buckets survive restarts in `APERIO_DATA_DIR/aperio.db` (SQLite), flushed every 30 s and on shutdown, pruned to 60 days / 26 weeks / 24 months / 10 years.

Traffic is additionally attributed **per token** and **per request hostname**; the dashboard's *Traffic Breakdown* shows the top consumers of each. Up to 200 distinct labels are tracked per dimension, with overflow folded into an `(other)` bucket so unbounded hostname cardinality cannot grow the stats file.

## Retention policies

Independent TTLs, all in days, unset = keep forever, bound how long each data type is held, enforced by a background pruner (at startup, then hourly; each pruning cycle writes a `retention_pruned` audit event with per-surface counts):

| Variable | Prunes |
| --- | --- |
| `retention_captures` (env `APERIO_RETENTION_CAPTURES`) | Inspector captures and webhook inbox entries |
| `retention_access_log` (env `APERIO_RETENTION_ACCESS_LOG`) | Structured access-log file lines (rewritten in place) |
| `retention_audit` (env `APERIO_RETENTION_AUDIT`) | Audit events, expired rotated generations are deleted whole; the active file loses only its leading expired prefix, so the hash chain stays verifiable |
| `retention_stats` (env `APERIO_RETENTION_STATS`) | Day-granularity statistics buckets (coarser buckets keep their built-in caps) |

The same hourly cycle also runs the **disk-usage guard** when `APERIO_DB_MAX_BYTES` caps the SQLite store: nearing the cap (90%) emits a `disk_usage_warning` webhook/audit event once per episode, and exceeding it auto-prunes the lowest-priority persisted data (oldest webhook inbox entries, delivery-log rows, and day-stat buckets), vacuums the database so the file shrinks on disk, and records a `disk_pruned` event with before/after sizes.

## Right-to-erasure selective purge

`POST /aperio/api/purge` (master super-admin only) deletes traffic records matching a selector without wiping the whole store, the GDPR-style "erase what you hold about X" operation:

```bash
# from anywhere, against the server's admin API
curl -X POST -b "$SESSION" -H 'Content-Type: application/json' \
  --data '{"hostname": "app.example.com"}' https://tunnel.example.com/aperio/api/purge
# → { "status": "ok", "removed": { "traffic_log": 12, "inspector_captures": 3, "stats_rows": 2, … } }
```

Selectors (at least one required): `hostname` (a request hostname), `token` (a token label), `ip` (a visitor IP). A purge touches the in-memory traffic log, the request inspector captures, the per-hostname/per-token statistics aggregates, per-route latency stage windows, the response cache, and the structured `APERIO_ACCESS_LOG` file (rewritten in place). Lifetime totals and period buckets are aggregates without personal attribution and stay intact. Visitor IPs are deliberately never persisted in logs or stats (queries are sanitized, no IP field is written), so the `ip` selector only matches inspector captures via their forwarded-IP request headers. Every purge writes a `data_purged` audit event with the per-surface removal counts.

## Server self-health

`GET /aperio/api/self-health` (master-admin) returns a snapshot of the server process itself, uptime, connected clients, resident-set memory (Linux), the on-disk SQLite store size (db + WAL/SHM), and response-cache occupancy/hit-rate, surfaced as a card on the dashboard Breakdown page.

## CSV export

`GET /aperio/api/export/traffic.csv?unit=day|week|month|year&count=N` streams the per-period traffic history (requests, success/failed, bytes in/out, average latency) as CSV for the caller's organization, ready for a spreadsheet or a billing pipeline. A one-click *Export traffic CSV* button sits on the self-health card.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`observability`](examples/observability/): metrics, traces, alerts
