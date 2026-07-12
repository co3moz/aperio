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

- [ ] Client terminal UI — ngrok-style live request table in the terminal (`aperio-client --ui`)
- [ ] Built-in ACME / Let's Encrypt TLS termination (HTTP-01, DNS-01 for wildcards)
- [ ] HA / multi-server mode — shared state (tokens, routes, stats) across servers, client failover between servers
- [ ] WAF-lite — per-service request filtering rules (path/method/header, body size)
- [ ] GeoIP country-based access rules on top of the CIDR allowlist
- [ ] Ready-made Grafana dashboard templates for the Prometheus metrics
- [ ] Weighted / canary load balancing (percentage traffic split)
- [ ] Traffic mirroring (shadowing) to a second client
- [ ] Visitor-IP rate limiting (current limits are per token)
- [ ] Time-window access rules (e.g. business hours only)
- [ ] mTLS / client certificate identity for tunnel connections
- [ ] SMTP email notifications alongside webhooks
- [ ] Scheduled maintenance windows
- [ ] Service install command (`aperio-client service install` for systemd/launchd/Windows)
- [ ] Kubernetes operator / ingress mode
- [ ] SDKs / Terraform provider on top of the OpenAPI spec

## Rejected

- Public status page — out of scope; dedicated uptime tools (e.g. Uptime Kuma) do this better, and they can consume `GET /aperio/api/uptime`.
