# Observability

> **Concept:** [Observability](../../observability.md).


Everything the server can emit about traffic, in one config:

- **Prometheus metrics:** `metrics.enabled` serves `/aperio/metrics`, protected by `metrics.token` (generated and persisted when unset).
- **Access log:** `access_log` writes JSON lines for proxied requests. Structured `aperio_access` events also go to stdout unless disabled; successful requests may be sampled.
- **OpenTelemetry:** `otel.enabled` exports request spans to the configured collector.
- **Alerting:** `alert.error_rate` and `alert.client_down` emit audit and webhook events when their thresholds are met.

See [Observability](../../observability.md) for dashboards, webhooks, and the audit log.

> **Before running:** Start an OTLP collector at the configured endpoint (`localhost:4318` in this example). Configure a webhook receiver if you want alerts delivered outside Aperio. See [Distributed tracing](../../observability.md#distributed-tracing-opentelemetry).
