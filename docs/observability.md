# Observability

Aperio exposes what it is doing through five channels: metrics for dashboards and alerting, distributed traces for end-to-end request timing, an access log for per-request analysis, an audit trail for security events, and webhooks for pushing events into your own systems.

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

Request latency is exposed as the `aperio_request_duration_seconds` histogram (buckets from 5 ms to 30 s), so p95/p99 can be plotted in Grafana with the usual `histogram_quantile(0.99, rate(aperio_request_duration_seconds_bucket[5m]))` query.

For quota and billing dashboards, per-tenant counters are exposed with `token` and `hostname` labels: `aperio_token_requests_total`, `aperio_token_requests_failed_total`, `aperio_token_bytes_received_total`, `aperio_token_bytes_sent_total` (the label value is the token name, `master` for the master token), and the same four as `aperio_hostname_*_total{hostname=...}` attributed to the request hostname. These are backed by the persistent stats store, so they survive restarts; at most 200 distinct labels are tracked per family, with overflow folded into `__other`.

## Distributed tracing (OpenTelemetry)

Set `APERIO_OTEL=1` to export one span per proxied request over OTLP (HTTP/protobuf) to an OpenTelemetry collector. Each `proxy.request` span carries the request method, path, host, the selected `aperio.client.id`, and the final response status.

```bash
APERIO_OTEL=1
APERIO_OTEL_ENDPOINT=http://otel-collector:4318   # base URL; /v1/traces is appended automatically
APERIO_OTEL_SERVICE_NAME=aperio-server            # optional, defaults to "aperio-server"
```

The standard `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_SERVICE_NAME` variables are honored as fallbacks. Spans are batch-exported and flushed on graceful shutdown.

**Context propagation.** If an incoming request already carries a W3C `traceparent` header (e.g. from an upstream gateway or Cloudflare), Aperio adopts it as the span's parent. It then injects its own trace context into the headers forwarded through the tunnel, so a backend that reads `traceparent` continues the same trace — the visitor → Aperio → backend path shows up as one distributed trace. When `APERIO_OTEL` is off there is no overhead and inbound trace headers pass through untouched.

> **Note:** enabling the OTLP exporter compiles `aws-lc-sys`/rustls into the build, which needs a C toolchain (and CMake) at build time. Prebuilt release binaries already include it.

## Alerting

Two threshold rules turn the webhook pipeline into a simple pager — point a Slack/Discord/Teams webhook at the `alert_triggered` event:

```bash
APERIO_ALERT_ERROR_RATE=5        # alert when ≥5% of proxied requests fail (5xx)…
APERIO_ALERT_WINDOW=300          # …measured over a 300 s sliding window (default)
APERIO_ALERT_MIN_REQUESTS=20     # quiet windows below 20 requests never alert (default)
APERIO_ALERT_CLIENT_DOWN=120     # alert when a known service stays down for 2 minutes
```

Both rules are off unless their threshold is set. One `alert_triggered` event (kinds `error_rate` / `client_down`) fires per episode and one `alert_resolved` when the condition clears — the error rate resolves at 80% of the threshold, so a value hovering at the limit cannot flap. Alerts are also audit-logged. For richer alerting (latency percentiles, arbitrary PromQL), scrape the Prometheus endpoint with Alertmanager instead.

## Access log

Every proxied request is emitted as a structured `aperio_access` tracing event on stdout — JSON with `request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, and `error` as top-level fields. Set `APERIO_ACCESS_LOG=/path/to/access.jsonl` to additionally append the same data as raw JSON lines, unaffected by `LOG_LEVEL` — ready to be tailed into Loki or ClickHouse. Query strings are stripped from logs.

## Audit log

Administrative and security events — logins (password and OIDC), token create/update/revoke, ephemeral tunnel provisioning, share link creation, maintenance toggles, client connect/disconnect/drain, kill-switch toggles, overrules, replays, and tunnel streams — are appended to `APERIO_DATA_DIR/audit.jsonl` with timestamp, actor IP, and details. Each event also records the acting user and the organization it belongs to. The dashboard shows the most recent 200, filtered to the caller's organization (see [Organizations](organizations.md)). The file is size-rotated (`APERIO_AUDIT_MAX_SIZE`, default 10 MB; `APERIO_AUDIT_MAX_FILES` generations kept, default 3) so long-lived installations cannot fill the disk.

## Webhooks

Define webhooks from the dashboard (name, URL, subscribed events — `*` for all). A webhook belongs to the organization that created it and fires only for that organization's events (see [Organizations](organizations.md)). Events are delivered as JSON POSTs with a 10 s timeout:

```json
{ "event": "client_connected", "timestamp": "2026-07-06T15:16:37+03:00", "data": { "client_id": "…", "ip": "…", "token": "tenant-a" } }
```

Available events: `client_connected`, `client_disconnected`, `client_draining`, `token_created`, `token_revoked`, `token_expiring`, `tunnel_created`, `tunnel_deleted`, `share_created`, `maintenance_on`, `maintenance_off`, `settings_updated`, `import_applied`, `alert_triggered`, `alert_resolved`.

### Delivery reliability & the delivery log

A delivery that fails with a transport error, a 5xx, or a 429 is **retried with backoff** — by default 4 retries over ~1.5 minutes (`1s, 5s, 25s, 60s` between attempts; override with `APERIO_WEBHOOK_RETRY_SCHEDULE`, comma-separated seconds, empty = no retries). Other 4xx responses are treated as permanent and not retried.

Every final outcome (success or failure, with the HTTP status or error, the attempt count, and the exact payload sent) lands in the **delivery log**: the *Recent deliveries* table on the dashboard's Webhooks page, or `GET /aperio/api/webhooks/deliveries` (`?webhook_id=` to filter). The last 500 outcomes are kept in `aperio.db`. Any logged delivery can be **redelivered** — the same payload is re-sent to the webhook's current URL with a fresh signature and the normal retry policy (`POST /aperio/api/webhooks/deliveries/{id}/redeliver`, or the *Redeliver* button), and the outcome is logged as a new row.

### Chat-service formats

Besides the raw JSON above (`generic`, the default), a webhook can be created with a **format** of `slack`, `discord`, or `teams`: point it straight at that service's *incoming webhook* URL and Aperio delivers a ready-made message instead — a Slack mrkdwn `text`, a Discord markdown `content`, or a Teams MessageCard with the event's fields as facts. No relay or transformation service needed.

### Signed deliveries

Give a webhook a **signing secret** (16–128 chars, set at creation; never shown again) and every delivery carries:

- `X-Aperio-Timestamp`: Unix seconds at send time.
- `X-Aperio-Signature`: `sha256=<hex HMAC-SHA256 over "<timestamp>.<raw body>">` with the shared secret.

Verify by recomputing the MAC over the exact received body bytes and comparing in constant time; reject stale timestamps (e.g. > 5 minutes old) to block replays:

```python
import hmac, hashlib
expected = hmac.new(secret, f"{ts}.".encode() + raw_body, hashlib.sha256).hexdigest()
ok = hmac.compare_digest(f"sha256={expected}", signature_header) and abs(time.time() - int(ts)) < 300
```

## Persistent statistics

Lifetime counters (total requests, success/failure, bytes in each direction, summed duration) and daily/weekly/monthly/yearly buckets survive restarts in `APERIO_DATA_DIR/aperio.db` (SQLite) — flushed every 30 s and on shutdown, pruned to 60 days / 26 weeks / 24 months / 10 years.

Traffic is additionally attributed **per token** and **per request hostname**; the dashboard's *Traffic Breakdown* shows the top consumers of each. Up to 200 distinct labels are tracked per dimension, with overflow folded into an `(other)` bucket so unbounded hostname cardinality cannot grow the stats file.

## Right-to-erasure selective purge

`POST /aperio/api/purge` (master super-admin only) deletes traffic records matching a selector without wiping the whole store — the GDPR-style "erase what you hold about X" operation:

```bash
curl -X POST -b "$SESSION" -H 'Content-Type: application/json' \
  --data '{"hostname": "app.example.com"}' https://tunnel.example.com/aperio/api/purge
# → { "status": "ok", "removed": { "traffic_log": 12, "inspector_captures": 3, "stats_rows": 2, … } }
```

Selectors (at least one required): `hostname` (a request hostname), `token` (a token label), `ip` (a visitor IP). A purge touches the in-memory traffic log, the request inspector captures, the per-hostname/per-token statistics aggregates, per-route latency stage windows, the response cache, and the structured `APERIO_ACCESS_LOG` file (rewritten in place). Lifetime totals and period buckets are aggregates without personal attribution and stay intact. Visitor IPs are deliberately never persisted in logs or stats (queries are sanitized, no IP field is written), so the `ip` selector only matches inspector captures via their forwarded-IP request headers. Every purge writes a `data_purged` audit event with the per-surface removal counts.
