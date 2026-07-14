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

Organized by theme. Every item carries a stable `#N` id — reference them as "planned_features #N". IDs are never renumbered or reused; a shipped item keeps its id and flips to `[x]` in place; new ideas take the next free number.

### Security & access control

- [ ] #1 WAF-lite — per-service request filtering rules (path/method/header, body size)
- [x] #3 Visitor-IP rate limiting — shipped: a per-IP token bucket is enforced on every proxied request (`check_rate_limit`), alongside per-token limits
- [ ] #5 mTLS / client certificate identity for tunnel connections
- [ ] #6 Webhook delivery reliability — retries with exponential backoff, plus a native per-webhook delivery log (each attempt with status/response code, visible in the dashboard, with redelivery) so failed deliveries are observable, not silent
- [x] #7 Extend the login lockout to per-service visitor password logins — shipped: the escalating per-IP lockout sits at the top of the shared `/aperio/auth` handler, which serves visitor-password logins too
- [ ] #8 Active session management — list open dashboard sessions with IP/User-Agent and offer remote "sign out everywhere"
- [ ] #10 Secret redaction in the request inspector — auto-mask Authorization/Cookie/token headers and body patterns
- [ ] #11 API token rotation with a grace period — issue a new token while the old stays valid for N hours
- [ ] #12 CAPTCHA challenge gate (hCaptcha/Cloudflare Turnstile) for public services under attack
- [ ] #14 Encrypted-at-rest SQLite (SQLCipher) option for the data dir
- [ ] #15 Per-service security header presets — HSTS/X-Frame-Options/CSP injection
- [ ] #16 Token-to-device key pinning (TOFU) — bind a token to the public key the client generates on its first dial-out, so a token replayed from another machine (e.g. leaked into CI logs) is rejected without full mTLS/PKI
- [ ] #19 Server TLS cert pinning on dial-out — the client pins the expected server SPKI fingerprint so a rogue TLS proxy in front of the public server can't silently MITM tunnel traffic
- [ ] #20 Canary tokens as leak tripwires — a token marked never-used fires an instant alert the moment it is presented, turning a leaked dump/`aperio.yaml` into an unambiguous breach signal
- [ ] #21 Encrypted secrets vault with `${secret:}` refs — a SQLite-backed encrypted vault managed from the dashboard, referenced in config, keeps backend credentials and header-rewrite values out of plaintext config
- [ ] #22 Client-side backend credential injection — attach basic/bearer credentials from the vault to backend requests while stripping visitor-supplied Authorization headers, keeping origin credentials off the public edge
- [ ] #23 Break-glass panic lockdown — one command/control instantly revokes every token and session and freezes all new connections (distinct from the per-client kill switch and maintenance mode)
- [ ] #24 Usernameless passkey sign-in — opt-in per passkey at registration ("allow signing in without a username?"): when enabled, pressing the passkey button with an empty username runs a discoverable-credential ceremony (the stored user handle identifies the account) and the authenticator's account picker takes over; without the flag the existing username-first flow is unchanged

### Observability & analytics

- [ ] #25 Ready-made Grafana dashboard templates for the Prometheus metrics
- [ ] #26 SMTP email notifications alongside webhooks
- [ ] #27 Per-service latency histograms — p50/p95/p99 time series (on the date-filter/uptime layer)
- [ ] #28 Live log tail — a `tail -f`-style streaming view of the access log in the dashboard
- [ ] #29 Bandwidth accounting — bytes in/out per token/hostname (billing-style report)
- [ ] #30 Top-N slowest endpoints report
- [ ] #31 Traffic anomaly detection — alert on sudden spikes/drops (on top of error-rate/client-down alerting)
- [ ] #32 Structured log shipping to Loki/Elasticsearch
- [ ] #33 OpenTelemetry traces for the server's own API (only proxied traffic is traced today)
- [ ] #34 Per-route status-code / error-rate trend sparklines
- [ ] #35 Trace-ID correlation — show each request's trace ID in the inspector with a deep link to Jaeger/Tempo
- [x] #36 Prometheus latency histogram buckets — shipped: `/aperio/metrics` exports a request-duration histogram (cumulative buckets + sum + count)
- [ ] #37 Request timeline (high-resolution latency decomposition) — per-request timestamps anchored at t0 = server first received the request, each later stage recorded as a high-resolution offset from t0 (e.g. +45.325ms): dispatched to the tunnel (left the queue), received by the client, backend request sent, backend first/last byte, client began sending the response, server received the response, response fully sent to the visitor / visitor connection closed; client-side stages are measured as monotonic durations on the client and anchored server-side (clock skew never mixes clocks); shown as a waterfall in the inspector
- [ ] #38 Synthetic end-to-end canary probes — the server periodically sends a marked synthetic request down a tunnel to the backend, exercising the tunnel itself (unlike the passive backend health probing already shipped)
- [ ] #39 Client host telemetry over the tunnel — the client samples its own CPU/memory/FDs/backend-RTT up the existing WebSocket, shown per client so a struggling remote host is diagnosable
- [ ] #40 Per-stage latency statistics & anomaly detection — build on the #37 timeline: keep mean/stddev per stage (queue wait, tunnel transit, backend time, ...) per service, surface them in the dashboard, and flag anomalies when a stage leaves its normal band (e.g. mean +/- k*sigma), so 'requests usually queue +5-10ms, now +25-30ms' is detected and attributable to a stage
- [ ] #41 Audit-sourced chart annotations — overlay deploy/hot-reload/maintenance/kill markers from the audit log onto traffic/latency charts to correlate metric shifts with their cause
- [ ] #42 Alertmanager-lite silences and grouping — mute windows plus grouping/dedup on top of the shipped threshold alerting, to silence noisy services and collapse alert storms
- [ ] #43 Live service topology graph — a dashboard node-graph of hostname/path -> tunnel -> client -> backend with live per-edge request rates; an alternative visual view of the existing clients/routes list

### Proxy, routing & traffic

- [ ] #44 Built-in ACME / Let's Encrypt TLS termination (HTTP-01, DNS-01 for wildcards)
- [ ] #45 Weighted / canary load balancing (percentage traffic split)
- [ ] #46 Traffic mirroring (shadowing) to a second client
- [ ] #47 Edge compression — add gzip/brotli when the backend doesn't (only zlib tunnel compression exists today)
- [ ] #48 Circuit breaker — open the circuit after N backend failures, fast-fail
- [ ] #49 Retry policy — retry idempotent requests on 5xx to another client in the pool
- [ ] #50 Sticky sessions keyed on an app-chosen cookie/header (the built-in sticky strategy pins via Aperio's own affinity cookie)
- [ ] #51 Regex URL rewrite rules — beyond `trim_bind` path rewriting
- [ ] #52 Per-service custom error pages — beyond the global 504/503
- [ ] #53 stale-while-revalidate + a cache purge API (on top of the response cache)
- [ ] #54 WebSocket message inspection/logging — the ws counterpart of the HTTP inspector
- [ ] #55 Per-service request body size limit with an early 413
- [ ] #56 Slow-start (warmup) — ramp traffic to a newly connected client in the LB pool
- [ ] #57 QUIC/HTTP-3 tunnel with connection migration — dialing out over HTTP/3 lets the tunnel survive wifi↔cellular IP changes and carries each request on its own stream, ending head-of-line blocking of the single TCP WebSocket
- [ ] #58 TLS/SNI passthrough routing — route raw TLS by SNI to the right client without terminating at the edge, so backends needing end-to-end TLS or client-cert auth work unmodified
- [ ] #59 PROXY protocol v2 injection to backends — the client prepends a PROXY v2 header so the local backend sees the real visitor IP/port/TLS metadata instead of the loopback tunnel hop
- [ ] #60 Adaptive keepalive auto-tuning — measure how long each tunnel survives idle before a NAT/firewall drops it and store a per-client tuned heartbeat interval, cutting wasted pings without silent disconnects
- [ ] #61 Happy Eyeballs dual-stack dialing — race IPv4/IPv6 attempts (RFC 8305) on dial-out and keep whichever wins, speeding and hardening establishment on mixed-stack networks
- [ ] #62 Pooled HTTP/2 backend connection reuse — keep a multiplexed keep-alive connection to the local backend and fan requests over it, eliminating per-request connect/TLS churn (with a reuse-ratio metric)
- [ ] #63 Adaptive concurrency limiting (AIMD/gradient) — auto-tune each backend's in-flight cap from observed latency, backing off when it slows, instead of the static per-service `max_concurrent` that ships today
- [ ] #64 Idempotency-key dedup at the edge — honor an `Idempotency-Key` header so a POST re-dispatched by failover (or a retry policy) executes on the backend only once, caching the first response per key for a TTL
- [ ] #65 Passive outlier ejection — eject a backend from rotation after a burst of real 5xx/timeouts in live traffic and periodically re-admit it, complementing the active `/health`-endpoint probing that ships today
- [ ] #66 Sticky failover affinity remap — when a backend a visitor was pinned to goes down, deterministically remap that affinity key to a replacement and remember the remap
- [ ] #67 Request hedging to duplicate backends — for idempotent GETs across multiple client backends, fire a second request after a delay and return the first response, cutting tail latency on flaky tunnels
- [ ] #68 Per-request deadline budget — a per-service total-time budget aborts the relay and frees the tunnel slot when exceeded (bounded 504), passing the remaining budget to the backend as a deadline header

### Edge cache & content

- [ ] #69 Serve-stale-on-origin-failure — opt-in per service (client `resilience` flag) and only when the server cache is enabled: while the route has no healthy client, cached responses are served even past TTL, marked (e.g. `X-Aperio-Stale` + `Age`), bounded by a configurable max-stale window; the moment any healthy client reconnects, normal proxying resumes and fresh responses replace the stale entries, so a newly connected client immediately takes over
- [ ] #70 Edge image transformation proxy — resize/crop/re-encode to WebP/AVIF via `?w=&format=` params at the server, caching derived variants so the origin serves each original once
- [ ] #71 ETag synthesis and 304 handling — when the server cache is enabled: for cached bodies lacking validators, synthesize an ETag (body hash) and answer If-None-Match with 304 at the edge, saving tunnel bandwidth
- [ ] #72 Edge HTML link rewriting — rewrite hardcoded `http://localhost`/internal hostnames inside HTML/CSS bodies to the public tunnel hostname as they stream through
- [ ] #73 Single-flight coalescing on cache miss — collapse many simultaneous identical cacheable misses into one upstream fetch, protecting local backends from thundering-herd load on cache expiry
- [ ] #74 Range requests served from cache — satisfy HTTP Range requests (video scrubbing, resumable downloads) from cached full objects at the edge so partial-content requests never re-traverse the tunnel

### Client-side

- [ ] #75 Client terminal UI — ngrok-style live request table in the terminal (`aperio-client --ui`)
- [ ] #76 Service install command (`aperio-client service install` for systemd/launchd/Windows)
- [ ] #77 Multiple local backends per service with client-side failover — the client picks a healthy one
- [ ] #78 Unix socket target — `target: unix:///var/run/app.sock`
- [ ] #79 Outbound HTTP/SOCKS proxy support — dial the server through a corporate proxy
- [ ] #80 Client self-update command — `aperio-client update`
- [ ] #81 Client-side Prometheus metrics endpoint
- [ ] #82 Client-side request/response logging to a local file
- [ ] #83 Environment profiles — `profiles: { dev, prod }` selection in one file
- [ ] #84 Client-side response cache
- [x] #85 `aperio-client doctor` preflight — shipped as `aperio-client check`: config resolution with per-layer provenance, server health + WebSocket/TLS reachability, auth probes, and backend target probes
- [ ] #86 Zero-config one-shot tunnel — `aperio-client http <port>` auto-registers an ephemeral tunnel via the server API and prints the public URL with no `aperio.yaml`
- [ ] #87 Local backend port auto-discovery — a quick-start mode that scans common dev ports (3000/5173/8080/8000) and offers to tunnel whatever is listening
- [ ] #88 Config includes / file composition — let `aperio.yaml` pull in other files via `include: routes/*.yaml`, complementing hot-reload for large multi-service configs
- [ ] #89 Wait-for-backend startup gate — hold the tunnel as not-ready until the local backend passes a health check, avoiding the connection-refused window during a slow dev-server boot

### Developer experience & integrations

- [ ] #90 SDKs / Terraform provider on top of the OpenAPI spec
- [ ] #91 Embedded Swagger UI / API explorer — `openapi.json` exists but there's no visual UI
- [ ] #92 `aperio-client open` — open the public URL in a browser and print a QR code in the terminal
- [ ] #93 Tunnel presets — `aperio-client --preset vite/next` ready-made framework templates
- [ ] #94 Official Helm chart — a docker-compose example exists, no k8s chart
- [ ] #95 Import from ngrok config — migration helper
- [ ] #96 Slack slash-command bot — create/list tunnels from Slack
- [ ] #97 VS Code extension / status-bar integration
- [ ] #98 Server-side request breakpoints — pause a matching request the server already holds and let a developer edit method/headers/body (or synthesize a response) in the dashboard: Charles/Fiddler interception with zero client changes
- [ ] #99 Per-route fault injection — chaos rules on a hostname/path/token add delay, return synthetic 5xx, or drop the relay, to test frontend/client resilience against a misbehaving backend
- [ ] #100 Inbound webhook capture inbox — persist inbound third-party webhooks (Stripe, GitHub) hitting a tunnel, render them in a dashboard inbox, and re-fire any event to the local client
- [ ] #101 Mock/stub response library — extend client-less fixed routes into a matcher-based mock library (method/path/header/query) so a frontend can develop against canned responses before the backend exists
- [ ] #102 Auto-inferred OpenAPI from traffic — incrementally infer path/param/schema shapes from live tunnel traffic and emit a draft OpenAPI document per hostname
- [ ] #103 Golden-diff response drift detection — snapshot a response as a baseline and flag when future responses to the same route diverge in status/headers/body shape
- [ ] #104 Portable traffic session bundle — record a window of a tunnel's traffic into a replayable `.aperio-session` bundle to re-inject or hand to a teammate

### Operations, data & compliance

- [ ] #105 HA / multi-server mode — shared state (tokens, routes, stats) across servers, client failover between servers
- [ ] #106 Scheduled maintenance windows
- [ ] #107 Kubernetes operator / ingress mode
- [ ] #108 Scheduled automatic DB backups — periodic snapshot + retention (on top of dump export/import)
- [ ] #109 Per-token quotas — max requests/day, max bandwidth, 429 on exceed
- [ ] #110 Multi-tenancy / organizations — group tokens and users under orgs
- [ ] #111 Server config lint / dry-run — `aperio-server --check-config`
- [ ] #112 Blue-green client deployment — the new client takes over while the old drains (on top of graceful drain)
- [ ] #113 Hash-chained tamper-evident audit log — chain each audit row's hash into the next with a verify command, making any edit/deletion of the audit history detectable after the fact
- [ ] #114 Per-data-type retention policies — independent TTLs for request captures, access logs, audit entries, and stats rows, enforced by a background pruner
- [ ] #115 Disk-usage guard with auto-prune at cap — a configurable max `aperio.db` size that auto-prunes the oldest low-priority captures and emits a webhook alert as the cap nears
- [ ] #116 Right-to-erasure selective purge — delete all persisted requests/logs/stats/inspector history matching a given visitor IP, hostname, or token, without wiping the whole store
- [ ] #117 Client-side store-and-forward capture buffer — the client queues request metadata to a small local buffer when the WebSocket drops and replays it on reconnect, so no traffic records are lost during outages

## Rejected

- #2 GeoIP country-based access rules — rejected: not a current need; the CIDR allowlist covers our access-scoping use cases.
- #4 Time-window access rules — rejected: business-hours logic belongs to the application behind the tunnel, not to Aperio.
- #9 Audit log export to SIEM — rejected: out of scope for now.
- #13 IP reputation / denylist feeds — rejected: curating threat intelligence is not a proxy's responsibility (comparable reverse proxies don't take it on either); operators can feed external blocklists into their firewall.
- #17 One-time client enrollment codes — rejected: dynamic bootstrap complicates the setup story; today a single static config brings everything up, and that simplicity is worth keeping.
- #18 Client-side egress allowlist — rejected: it only defends against an attacker who already controls the client host, and such an attacker can edit the client config anyway — the enforcement point is inside the compromised trust boundary.
- Public status page — out of scope; dedicated uptime tools (e.g. Uptime Kuma) do this better, and they can consume `GET /aperio/api/uptime`.
