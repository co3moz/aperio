# Observability

Aperio exposes what it is doing through four channels: metrics for dashboards and alerting, an access log for per-request analysis, an audit trail for security events, and webhooks for pushing events into your own systems.

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

## Access log

Every proxied request is emitted as a structured `aperio_access` tracing event on stdout — JSON with `request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, and `error` as top-level fields. Set `APERIO_ACCESS_LOG=/path/to/access.jsonl` to additionally append the same data as raw JSON lines, unaffected by `LOG_LEVEL` — ready to be tailed into Loki or ClickHouse. Query strings are stripped from logs.

## Audit log

Administrative and security events — logins (password and OIDC), token create/update/revoke, ephemeral tunnel provisioning, share link creation, maintenance toggles, client connect/disconnect/drain, kill-switch toggles, overrules, replays, and TCP streams — are appended to `APERIO_DATA_DIR/audit.jsonl` with timestamp, actor IP, and details. The dashboard shows the most recent 200.

## Webhooks

Define webhooks from the dashboard (name, URL, subscribed events — `*` for all). Events are delivered as fire-and-forget JSON POSTs with a 10 s timeout:

```json
{ "event": "client_connected", "timestamp": "2026-07-06T15:16:37+03:00", "data": { "client_id": "…", "ip": "…", "token": "tenant-a" } }
```

Available events: `client_connected`, `client_disconnected`, `client_draining`, `token_created`, `token_revoked`, `tunnel_created`, `share_created`, `maintenance_on`, `maintenance_off`.

## Persistent statistics

Lifetime counters (total requests, success/failure, bytes in each direction, summed duration) and daily/weekly/monthly/yearly buckets survive restarts in `APERIO_DATA_DIR/stats.json` — flushed every 30 s and on shutdown, pruned to 60 days / 26 weeks / 24 months / 10 years.

Traffic is additionally attributed **per token** and **per request hostname**; the dashboard's *Traffic Breakdown* shows the top consumers of each. Up to 200 distinct labels are tracked per dimension, with overflow folded into an `(other)` bucket so unbounded hostname cardinality cannot grow the stats file.
