# Planned Features

Ideas and decisions for future Aperio work. Always written in English.
Syntax: `[ ]` = planned, `[x]` = shipped. Move rejected ideas to the bottom with a short reason.

## Accepted

- [x] Server-side YAML config file (`aperio-server.yaml`, named differently from the client's `aperio.yaml` to avoid confusion)
- [x] Header rewrite rules — per-service request/response header `add`/`remove`, on both server and client side (client side shipped earlier; server side reuses the same syntax via `aperio-server.yaml`)
- [x] Client-less routes — bind a hostname/path to a redirect or a fixed static response without a connected client (`routes:` in `aperio-server.yaml`)
- [x] Experimental public TCP expose — server declares `expose: [{protocol, port, key}]`, a client tunnel binds to it with `expose: <key>`; single-binder semantics (like client-id binding), no load balancing while experimental
- [ ] Token expiry early warnings — dashboard indication + webhook event before a dynamic token's TTL runs out
- [x] Auto noindex for preview services — configurable `X-Robots-Tag`/`robots.txt` injection for random-subdomain (preview) services (APERIO_PREVIEW_NOINDEX + dashboard toggle)
- [ ] Alerting rules — threshold-based webhook alerts (error rate, latency, client-down duration), kept simple
- [ ] Dump export/import — full export/import of `aperio.db` + settings overrides, as a failsafe across version upgrades
- [ ] Static file serving mode — `aperio-client` serves a local directory directly, no backend needed
- [ ] cURL / HAR export — export actions in the dashboard request inspector menu

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
