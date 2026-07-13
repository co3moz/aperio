# Planned Features

Ideas and decisions for future Aperio work. Always written in English.
Syntax: `[ ]` = planned, `[x]` = shipped. Move rejected ideas to the bottom with a short reason.

## Accepted

- [x] Server-side YAML config file (`aperio-server.yaml`, named differently from the client's `aperio.yaml` to avoid confusion)
- [x] Header rewrite rules — per-service request/response header `add`/`remove`, on both server and client side (client side shipped earlier; server side reuses the same syntax via `aperio-server.yaml`)
- [x] Client-less routes — bind a hostname/path to a redirect or a fixed static response without a connected client (`routes:` in `aperio-server.yaml`)
- [x] Experimental public TCP expose — server declares `expose: [{protocol, port, key}]`, a client tunnel binds to it with `expose: <key>`; single-binder semantics (like client-id binding), no load balancing while experimental
- [x] Token expiry early warnings — dashboard "expiring soon" badge + `token_expiring` webhook/audit event before a dynamic token's TTL runs out (APERIO_TOKEN_EXPIRY_WARNING)
- [x] Auto noindex for preview services — configurable `X-Robots-Tag`/`robots.txt` injection for random-subdomain (preview) services (APERIO_PREVIEW_NOINDEX + dashboard toggle)
- [x] Alerting rules — APERIO_ALERT_ERROR_RATE (sliding window, min-request floor) and APERIO_ALERT_CLIENT_DOWN thresholds emit `alert_triggered`/`alert_resolved` webhook+audit events, once per episode with hysteresis
- [x] Dump export/import — GET /aperio/api/export / POST /aperio/api/import (admin only) + dashboard Export & Import card; logical JSON dump of tokens, webhooks, users and settings overrides
- [x] Static file serving mode — `aperio-client --serve ./dist` (or yaml `serve:` / `APERIO_SERVE`) serves a local directory directly, no backend needed
- [x] cURL / HAR export — copy-as-cURL existed; added a single-entry HAR 1.2 download to the request inspector dialog

## Future ideas (not scheduled)

Organized by theme. Earlier ungrouped ideas have been folded into these categories.

### Security & access control

- [ ] WAF-lite — per-service request filtering rules (path/method/header, body size)
- [ ] GeoIP country-based access rules on top of the CIDR allowlist
- [ ] Visitor-IP rate limiting (current limits are per token)
- [ ] Time-window access rules (e.g. business hours only)
- [ ] mTLS / client certificate identity for tunnel connections
- [ ] Webhook delivery reliability — retries with exponential backoff + a dead-letter queue (`emit_event` is fire-and-forget today)
- [ ] Extend the login lockout to per-service visitor password logins (`LockoutTracker` guards only the dashboard login today)
- [ ] Active session management — list open dashboard sessions with IP/User-Agent and offer remote "sign out everywhere"
- [ ] Audit log export to SIEM — syslog/CEF format, or stream to a file/S3
- [ ] Secret redaction in the request inspector — auto-mask Authorization/Cookie/token headers and body patterns
- [ ] API token rotation with a grace period — issue a new token while the old stays valid for N hours
- [ ] CAPTCHA challenge gate (hCaptcha/Cloudflare Turnstile) for public services under attack
- [ ] IP reputation / denylist feeds — block known-bad IPs (the inverse of the CIDR allowlist)
- [ ] Encrypted-at-rest SQLite (SQLCipher) option for the data dir
- [ ] Per-service security header presets — HSTS/X-Frame-Options/CSP injection

### Observability & analytics

- [ ] Ready-made Grafana dashboard templates for the Prometheus metrics
- [ ] SMTP email notifications alongside webhooks
- [ ] Per-service latency histograms — p50/p95/p99 time series (on the date-filter/uptime layer)
- [ ] Live log tail — a `tail -f`-style streaming view of the access log in the dashboard
- [ ] Bandwidth accounting — bytes in/out per token/hostname (billing-style report)
- [ ] Top-N slowest endpoints report
- [ ] Traffic anomaly detection — alert on sudden spikes/drops (on top of error-rate/client-down alerting)
- [ ] Structured log shipping to Loki/Elasticsearch
- [ ] OpenTelemetry traces for the server's own API (only proxied traffic is traced today)
- [ ] Per-route status-code / error-rate trend sparklines
- [ ] Trace-ID correlation — show each request's trace ID in the inspector with a deep link to Jaeger/Tempo
- [ ] Prometheus latency histogram buckets (most metrics are counters/gauges today)

### Proxy, routing & traffic

- [ ] Built-in ACME / Let's Encrypt TLS termination (HTTP-01, DNS-01 for wildcards)
- [ ] Weighted / canary load balancing (percentage traffic split)
- [ ] Traffic mirroring (shadowing) to a second client
- [ ] Edge compression — add gzip/brotli when the backend doesn't (only zlib tunnel compression exists today)
- [ ] Circuit breaker — open the circuit after N backend failures, fast-fail
- [ ] Retry policy — retry idempotent requests on 5xx to another client in the pool
- [ ] Sticky sessions by cookie/header value (only IP affinity exists today)
- [ ] Regex URL rewrite rules — beyond `trim_bind` path rewriting
- [ ] Per-service custom error pages — beyond the global 504/503
- [ ] stale-while-revalidate + a cache purge API (on top of the response cache)
- [ ] WebSocket message inspection/logging — the ws counterpart of the HTTP inspector
- [ ] Per-service request body size limit with an early 413
- [ ] Slow-start (warmup) — ramp traffic to a newly connected client in the LB pool

### Client-side

- [ ] Client terminal UI — ngrok-style live request table in the terminal (`aperio-client --ui`)
- [ ] Service install command (`aperio-client service install` for systemd/launchd/Windows)
- [ ] Multiple local backends per service with client-side failover — the client picks a healthy one
- [ ] Unix socket target — `target: unix:///var/run/app.sock`
- [ ] Outbound HTTP/SOCKS proxy support — dial the server through a corporate proxy
- [ ] Client self-update command — `aperio-client update`
- [ ] Client-side Prometheus metrics endpoint
- [ ] Client-side request/response logging to a local file
- [ ] Environment profiles — `profiles: { dev, prod }` selection in one file
- [ ] Client-side response cache

### Developer experience & integrations

- [ ] SDKs / Terraform provider on top of the OpenAPI spec
- [ ] Embedded Swagger UI / API explorer — `openapi.json` exists but there's no visual UI
- [ ] `aperio-client open` — open the public URL in a browser and print a QR code in the terminal
- [ ] Tunnel presets — `aperio-client --preset vite/next` ready-made framework templates
- [ ] Official Helm chart — a docker-compose example exists, no k8s chart
- [ ] Import from ngrok config — migration helper
- [ ] Slack slash-command bot — create/list tunnels from Slack
- [ ] VS Code extension / status-bar integration

### Operations & deployment

- [ ] HA / multi-server mode — shared state (tokens, routes, stats) across servers, client failover between servers
- [ ] Scheduled maintenance windows
- [ ] Kubernetes operator / ingress mode
- [ ] Scheduled automatic DB backups — periodic snapshot + retention (on top of dump export/import)
- [ ] Per-token quotas — max requests/day, max bandwidth, 429 on exceed
- [ ] Multi-tenancy / organizations — group tokens and users under orgs
- [ ] Server config lint / dry-run — `aperio-server --check-config`
- [ ] Blue-green client deployment — the new client takes over while the old drains (on top of graceful drain)

## Rejected

- Public status page — out of scope; dedicated uptime tools (e.g. Uptime Kuma) do this better, and they can consume `GET /aperio/api/uptime`.
