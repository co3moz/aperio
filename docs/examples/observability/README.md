# Observability

> **Concept:** [Observability](../../observability.md).


Everything the server can emit about traffic, in one config:

- **Prometheus metrics**, `metrics: true` serves `/aperio/metrics`, gated by `metrics_token` (a random one is generated and persisted if unset).
- **Access log**, one JSON line per proxied request (`request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, `error`), directly ingestible by Loki/ClickHouse. The same data is always emitted to stdout as structured `aperio_access` tracing events.
- **OpenTelemetry**, `otel: true` exports one OTLP span per proxied request (adopts inbound W3C `traceparent`, propagates its own context to the backend).
- **Alerting**, `alert_error_rate` fires an `alert_triggered` webhook/audit event when the failed-request percentage inside a sliding window crosses the threshold (resolves at 80 % of it); `alert_client_down` fires when a known service stays down too long.

See [Observability](../../observability.md) for dashboards, webhooks, and the audit log.
