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
- [ ] Token-to-device key pinning (TOFU) — bind a token to the public key the client generates on its first dial-out, so a token replayed from another machine (e.g. leaked into CI logs) is rejected without full mTLS/PKI
- [ ] One-time client enrollment codes — short-lived single-use bootstrap codes exchanged on the first connect for a real scoped token, keeping long-lived tokens out of `aperio.yaml`, shell history, and Action logs
- [ ] Client-side egress allowlist — each token declares the exact local hosts/ports it may forward to (enforced inside the client), so a stolen token can't be repointed to pivot across the private network
- [ ] Server TLS cert pinning on dial-out — the client pins the expected server SPKI fingerprint so a rogue TLS proxy in front of the public server can't silently MITM tunnel traffic
- [ ] Canary tokens as leak tripwires — a token marked never-used fires an instant alert the moment it is presented, turning a leaked dump/`aperio.yaml` into an unambiguous breach signal
- [ ] Encrypted secrets vault with `${secret:}` refs — a SQLite-backed encrypted vault managed from the dashboard, referenced in config, keeps backend credentials and header-rewrite values out of plaintext config
- [ ] Client-side backend credential injection — attach basic/bearer credentials from the vault to backend requests while stripping visitor-supplied Authorization headers, keeping origin credentials off the public edge
- [ ] Break-glass panic lockdown — one command/control instantly revokes every token and session and freezes all new connections (distinct from the per-client kill switch and maintenance mode)

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
- [ ] Three-hop latency decomposition — split each request's latency into edge-terminate, tunnel-transit, and client-to-backend segments so operators see where slowness lives
- [ ] Synthetic end-to-end canary probes — the server periodically sends a marked synthetic request down a tunnel to the backend, exercising the tunnel itself (unlike the passive backend health probing already shipped)
- [ ] Client host telemetry over the tunnel — the client samples its own CPU/memory/FDs/backend-RTT up the existing WebSocket, shown per client so a struggling remote host is diagnosable
- [ ] Tunnel backpressure & queue-depth metrics — expose per-tunnel send-buffer depth and in-flight count with a saturation alert, catching congestion before it becomes visitor timeouts
- [ ] Audit-sourced chart annotations — overlay deploy/hot-reload/maintenance/kill markers from the audit log onto traffic/latency charts to correlate metric shifts with their cause
- [ ] Alertmanager-lite silences and grouping — mute windows plus grouping/dedup on top of the shipped threshold alerting, to silence noisy services and collapse alert storms
- [ ] Live service topology graph — a dashboard node-graph of hostname/path → tunnel → client → backend with live per-edge request rates

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
- [ ] QUIC/HTTP-3 tunnel with connection migration — dialing out over HTTP/3 lets the tunnel survive wifi↔cellular IP changes and carries each request on its own stream, ending head-of-line blocking of the single TCP WebSocket
- [ ] TLS/SNI passthrough routing — route raw TLS by SNI to the right client without terminating at the edge, so backends needing end-to-end TLS or client-cert auth work unmodified
- [ ] PROXY protocol v2 injection to backends — the client prepends a PROXY v2 header so the local backend sees the real visitor IP/port/TLS metadata instead of the loopback tunnel hop
- [ ] Adaptive keepalive auto-tuning — measure how long each tunnel survives idle before a NAT/firewall drops it and store a per-client tuned heartbeat interval, cutting wasted pings without silent disconnects
- [ ] Happy Eyeballs dual-stack dialing — race IPv4/IPv6 attempts (RFC 8305) on dial-out and keep whichever wins, speeding and hardening establishment on mixed-stack networks
- [ ] Pooled HTTP/2 backend connection reuse — keep a multiplexed keep-alive connection to the local backend and fan requests over it, eliminating per-request connect/TLS churn (with a reuse-ratio metric)
- [ ] Adaptive concurrency limiting (AIMD/gradient) — auto-tune each backend's in-flight cap from observed latency, backing off when it slows, instead of the static per-service `max_concurrent` that ships today
- [ ] Idempotency-key dedup at the edge — honor an `Idempotency-Key` header so a POST re-dispatched by failover (or a retry policy) executes on the backend only once, caching the first response per key for a TTL
- [ ] Passive outlier ejection — eject a backend from rotation after a burst of real 5xx/timeouts in live traffic and periodically re-admit it, complementing the active `/health`-endpoint probing that ships today
- [ ] Sticky failover affinity remap — when a backend a visitor was pinned to goes down, deterministically remap that affinity key to a replacement and remember the remap
- [ ] Request hedging to duplicate backends — for idempotent GETs across multiple client backends, fire a second request after a delay and return the first response, cutting tail latency on flaky tunnels
- [ ] Per-request deadline budget — a per-service total-time budget aborts the relay and frees the tunnel slot when exceeded (bounded 504), passing the remaining budget to the backend as a deadline header

### Edge cache & content

- [ ] Serve-stale-on-origin-failure — when a client disconnects or backend health fails, answer visitors from cached responses even past TTL, turning the response cache into a resilience layer instead of a 502 source
- [ ] Edge image transformation proxy — resize/crop/re-encode to WebP/AVIF via `?w=&format=` params at the server, caching derived variants so the origin serves each original once
- [ ] ETag synthesis and 304 handling — for backends emitting no validators, hash bodies to generate ETags and answer conditional requests with 304 at the edge, saving scarce tunnel bandwidth
- [ ] Edge HTML link rewriting — rewrite hardcoded `http://localhost`/internal hostnames inside HTML/CSS bodies to the public tunnel hostname as they stream through
- [ ] Single-flight coalescing on cache miss — collapse many simultaneous identical cacheable misses into one upstream fetch, protecting local backends from thundering-herd load on cache expiry
- [ ] Range requests served from cache — satisfy HTTP Range requests (video scrubbing, resumable downloads) from cached full objects at the edge so partial-content requests never re-traverse the tunnel

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
- [ ] `aperio-client doctor` preflight — check outbound WebSocket reachability, TLS handshake, DNS, clock skew, and token scope before tunneling, turning silent reconnect loops into a locally diagnosed failure
- [ ] Zero-config one-shot tunnel — `aperio-client http <port>` auto-registers an ephemeral tunnel via the server API and prints the public URL with no `aperio.yaml`
- [ ] Local backend port auto-discovery — a quick-start mode that scans common dev ports (3000/5173/8080/8000) and offers to tunnel whatever is listening
- [ ] Config includes / file composition — let `aperio.yaml` pull in other files via `include: routes/*.yaml`, complementing hot-reload for large multi-service configs
- [ ] Wait-for-backend startup gate — hold the tunnel as not-ready until the local backend passes a health check, avoiding the connection-refused window during a slow dev-server boot

### Developer experience & integrations

- [ ] SDKs / Terraform provider on top of the OpenAPI spec
- [ ] Embedded Swagger UI / API explorer — `openapi.json` exists but there's no visual UI
- [ ] `aperio-client open` — open the public URL in a browser and print a QR code in the terminal
- [ ] Tunnel presets — `aperio-client --preset vite/next` ready-made framework templates
- [ ] Official Helm chart — a docker-compose example exists, no k8s chart
- [ ] Import from ngrok config — migration helper
- [ ] Slack slash-command bot — create/list tunnels from Slack
- [ ] VS Code extension / status-bar integration
- [ ] Server-side request breakpoints — pause a matching request the server already holds and let a developer edit method/headers/body (or synthesize a response) in the dashboard: Charles/Fiddler interception with zero client changes
- [ ] Per-route fault injection — chaos rules on a hostname/path/token add delay, return synthetic 5xx, or drop the relay, to test frontend/client resilience against a misbehaving backend
- [ ] Inbound webhook capture inbox — persist inbound third-party webhooks (Stripe, GitHub) hitting a tunnel, render them in a dashboard inbox, and re-fire any event to the local client
- [ ] Mock/stub response library — extend client-less fixed routes into a matcher-based mock library (method/path/header/query) so a frontend can develop against canned responses before the backend exists
- [ ] Auto-inferred OpenAPI from traffic — incrementally infer path/param/schema shapes from live tunnel traffic and emit a draft OpenAPI document per hostname
- [ ] Golden-diff response drift detection — snapshot a response as a baseline and flag when future responses to the same route diverge in status/headers/body shape
- [ ] Portable traffic session bundle — record a window of a tunnel's traffic into a replayable `.aperio-session` bundle to re-inject or hand to a teammate

### Operations, data & compliance

- [ ] HA / multi-server mode — shared state (tokens, routes, stats) across servers, client failover between servers
- [ ] Scheduled maintenance windows
- [ ] Kubernetes operator / ingress mode
- [ ] Scheduled automatic DB backups — periodic snapshot + retention (on top of dump export/import)
- [ ] Per-token quotas — max requests/day, max bandwidth, 429 on exceed
- [ ] Multi-tenancy / organizations — group tokens and users under orgs
- [ ] Server config lint / dry-run — `aperio-server --check-config`
- [ ] Blue-green client deployment — the new client takes over while the old drains (on top of graceful drain)
- [ ] Hash-chained tamper-evident audit log — chain each audit row's hash into the next with a verify command, making any edit/deletion of the audit history detectable after the fact
- [ ] Per-data-type retention policies — independent TTLs for request captures, access logs, audit entries, and stats rows, enforced by a background pruner
- [ ] Disk-usage guard with auto-prune at cap — a configurable max `aperio.db` size that auto-prunes the oldest low-priority captures and emits a webhook alert as the cap nears
- [ ] Right-to-erasure selective purge — delete all persisted requests/logs/stats/inspector history matching a given visitor IP, hostname, or token, without wiping the whole store
- [ ] Client-side store-and-forward capture buffer — the client queues request metadata to a small local buffer when the WebSocket drops and replays it on reconnect, so no traffic records are lost during outages

## Rejected

- Public status page — out of scope; dedicated uptime tools (e.g. Uptime Kuma) do this better, and they can consume `GET /aperio/api/uptime`.
