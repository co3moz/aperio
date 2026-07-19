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
- [x] #6 Webhook delivery reliability — shipped: retries with backoff (5xx/429/transport, `APERIO_WEBHOOK_RETRY_SCHEDULE`) + a persistent delivery log with per-attempt status and one-click redelivery on the Webhooks page
- [x] #7 Extend the login lockout to per-service visitor password logins — shipped: the escalating per-IP lockout sits at the top of the shared `/aperio/auth` handler, which serves visitor-password logins too
- [x] #8 Active session management — shipped: sessions record IP/User-Agent/time; admin `GET/DELETE /aperio/api/sessions[/{id}]` + Users-page table with per-session revoke and sign-out-everywhere
- [x] #10 Secret redaction in the request inspector — shipped: credential headers and JSON/form secret fields masked server-side before the detail/cURL/HAR views (`APERIO_INSPECTOR_REDACT=0` opts out); raw capture kept so replay still works
- [x] #11 API token rotation with a grace period — shipped: `POST /aperio/api/tokens/{id}/rotate` (`grace_seconds`) mints a new secret returned once; the previous secret stays accepted until the grace deadline (0 = immediate cutover); audited + webhook `token_rotated`
- [ ] #12 CAPTCHA challenge gate (hCaptcha/Cloudflare Turnstile) for public services under attack
- [ ] #14 Encrypted-at-rest SQLite (SQLCipher) option for the data dir
- [x] #15 Per-service security header presets — shipped: `security_headers:` (yaml top-level/per-service; `true` = HSTS/X-Frame-Options/nosniff/Referrer-Policy, or a granular mapping incl. `csp`) folds into the response header rules; explicit `headers:` rules win over the preset
- [ ] #16 Token-to-device key pinning (TOFU) — bind a token to the public key the client generates on its first dial-out, so a token replayed from another machine (e.g. leaked into CI logs) is rejected without full mTLS/PKI
- [ ] #19 Server TLS cert pinning on dial-out — the client pins the expected server SPKI fingerprint so a rogue TLS proxy in front of the public server can't silently MITM tunnel traffic
- [ ] #20 Canary tokens as leak tripwires — a token marked never-used fires an instant alert the moment it is presented, turning a leaked dump/`aperio.yaml` into an unambiguous breach signal
- [ ] #21 Encrypted secrets vault with `${secret:}` refs — a SQLite-backed encrypted vault managed from the dashboard, referenced in config, keeps backend credentials and header-rewrite values out of plaintext config
- [ ] #22 Client-side backend credential injection — attach basic/bearer credentials from the vault to backend requests while stripping visitor-supplied Authorization headers, keeping origin credentials off the public edge
- [ ] #23 Break-glass panic lockdown — one command/control instantly revokes every token and session and freezes all new connections (distinct from the per-client kill switch and maintenance mode)
- [x] #24 Usernameless passkey sign-in — shipped: per-passkey opt-in at registration (discoverable credential); empty-username passkey button runs a discoverable ceremony via webauthn-rs `conditional-ui`, server-enforced opt-in, username-first flow otherwise unchanged

### Observability & analytics

- [ ] #25 Ready-made Grafana dashboard templates for the Prometheus metrics
- [ ] #26 SMTP email notifications alongside webhooks
- [ ] #27 Per-service latency histograms — p50/p95/p99 time series (on the date-filter/uptime layer)
- [x] #28 Live log tail — shipped: a *Live Tail* dashboard page renders each proxied request as a terminal-style line off the existing SSE stream (auto-scroll with pin/unpin, pause, clear, free-text filter incl. the new `host` log field; a line click opens the inspector)
- [ ] #29 Bandwidth accounting — bytes in/out per token/hostname (billing-style report)
- [x] #30 Top-N slowest endpoints report — shipped: rolling in-memory latency window per `host|path` (200 samples, 300 keys with `__other` overflow), `GET /aperio/api/slow-endpoints` (top 20 by recent p95, org-scoped) + a Breakdown-page table
- [ ] #31 Traffic anomaly detection — alert on sudden spikes/drops (on top of error-rate/client-down alerting)
- [ ] #32 Structured log shipping to Loki/Elasticsearch
- [ ] #33 OpenTelemetry traces for the server's own API (only proxied traffic is traced today)
- [ ] #34 Per-route status-code / error-rate trend sparklines
- [ ] #35 Trace-ID correlation — show each request's trace ID in the inspector with a deep link to Jaeger/Tempo
- [x] #36 Prometheus latency histogram buckets — shipped: `/aperio/metrics` exports a request-duration histogram (cumulative buckets + sum + count)
- [x] #37 Request timeline (high-resolution latency decomposition) — shipped: additive `ClientTimings` on buffered responses; server anchors client stages at t0=arrival by splitting tunnel transit evenly (clocks never mixed, estimate flagged); rendered as a waterfall in the inspector
- [ ] #38 Synthetic end-to-end canary probes — the server periodically sends a marked synthetic request down a tunnel to the backend, exercising the tunnel itself (unlike the passive backend health probing already shipped)
- [ ] #39 Client host telemetry over the tunnel — the client samples its own CPU/memory/FDs/backend-RTT up the existing WebSocket, shown per client so a struggling remote host is diagnosable
- [x] #40 Per-stage latency statistics & anomaly detection — shipped: rolling per-route window of per-stage mean/stddev/last via `GET /aperio/api/stage-stats` + a Breakdown-page table; latest sample past mean+3σ flagged as an anomaly
- [ ] #41 Audit-sourced chart annotations — overlay deploy/hot-reload/maintenance/kill markers from the audit log onto traffic/latency charts to correlate metric shifts with their cause
- [ ] #42 Alertmanager-lite silences and grouping — mute windows plus grouping/dedup on top of the shipped threshold alerting, to silence noisy services and collapse alert storms
- [x] #43 Live service topology graph — shipped: a Topology dashboard page drawing routes -> clients -> backends as a health-colored SVG graph with live per-edge request rates

### Proxy, routing & traffic

- [ ] #44 Built-in ACME / Let's Encrypt TLS termination (HTTP-01, DNS-01 for wildcards)
- [ ] #45 Weighted / canary load balancing (percentage traffic split)
- [ ] #46 Traffic mirroring (shadowing) to a second client
- [ ] #47 Edge compression — add gzip/brotli when the backend doesn't (only zlib tunnel compression exists today)
- [ ] #48 Circuit breaker — open the circuit after N backend failures, fast-fail
- [ ] #49 Retry policy — retry idempotent requests on 5xx to another client in the pool
- [ ] #50 Sticky sessions keyed on an app-chosen cookie/header (the built-in sticky strategy pins via Aperio's own affinity cookie)
- [ ] #51 Regex URL rewrite rules — beyond `trim_bind` path rewriting
- [x] #52 Per-service custom error pages — shipped: a structured `error_pages:` section in `aperio-server.yaml` (hostname → `504_page`/`503_page` HTML files) overrides the global pages per hostname; hot-reloaded with the other structured sections
- [x] #53 stale-while-revalidate + a cache purge API — shipped: RFC 5861 `stale-while-revalidate=N` honored (stale served instantly, one elected leader refreshes in the background through the already-selected client, 15s failed-refresh retry); `POST /aperio/api/cache/purge` (admin, audited `cache_purged`) drops entries by hostname/path-prefix or clears all
- [ ] #54 WebSocket message inspection/logging — the ws counterpart of the HTTP inspector
- [x] #55 Per-service request body size limit with an early 413 — shipped: `max_request_body` (yaml top-level/per-service, `APERIO_MAX_REQUEST_BODY`) is announced via Ping; the server rejects bigger uploads with 413 before dispatch, tightening (never widening) the global `APERIO_MAX_BODY_SIZE`
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

- [x] #119 Multiple hostnames per service — shipped: `hostname:` accepts a list (or comma-separated CLI/env); all declared hostnames are token-validated and route to the same service (additive `hostname_binds` Ping field).
### Edge cache & content

- [x] #69 Serve-stale-on-origin-failure — shipped: per-service `resilience` flag (needs the server cache); while no healthy client is connected, cached responses are served past TTL (marked `x-aperio-stale`/`Age`) up to `APERIO_CACHE_MAX_STALE`, with immediate takeover on reconnect
- [ ] #70 Edge image transformation proxy — resize/crop/re-encode to WebP/AVIF via `?w=&format=` params at the server, caching derived variants so the origin serves each original once
- [x] #71 ETag synthesis and 304 handling — shipped: cached bodies without a validator get a body-hash ETag; a matching `If-None-Match` is answered 304 at the edge (fresh or serve-stale), no tunnel round-trip
- [ ] #72 Edge HTML link rewriting — rewrite hardcoded `http://localhost`/internal hostnames inside HTML/CSS bodies to the public tunnel hostname as they stream through
- [x] #73 Single-flight coalescing on cache miss — shipped: the first cacheable miss per `host|uri` key becomes the leader; concurrent identical misses wait on its watch channel and re-answer from the freshly stored entry (uncacheable outcomes fall back to normal dispatch after one wait)
- [x] #74 Range requests served from cache — shipped: cache hits answer single `bytes=` ranges as 206 sliced from the stored full body (`Accept-Ranges`/`Content-Range`, 416 when unsatisfiable, `If-Range` honored; multi-range degrades to the full 200 per RFC 9110); backend 206s are never cached

### Client-side

- [ ] #75 Client terminal UI — ngrok-style live request table in the terminal (`aperio-client --ui`)
- [ ] #76 Service install command (`aperio-client service install` for systemd/launchd/Windows)
- [ ] #77 Multiple local backends per service with client-side failover — the client picks a healthy one
- [x] #78 Unix socket target — shipped: `target: unix:///var/run/app.sock` dials the backend over a Unix domain socket (hyper HTTP/1.1 per request; Unix-only, validated at startup; WebSocket upgrades answer 502)
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
- [x] #89 Wait-for-backend startup gate — shipped: `wait_for_backend: true` (`APERIO_WAIT_FOR_BACKEND`, per-service too) starts the service out of routing and a 1s connect-probe loop marks it routable on the backend's first accepted connection; superseded by `target_health` when set
- [x] #122 Per-service static file serving — shipped: `serve:` is accepted per `services:` entry (mutually exclusive with `target`/`tcp_target`), one loopback file server per distinct directory shared across entries and hot-reloads; `aperio-client check` validates serve directories
- [x] #123 Per-candidate allowed_ips + denied-request handling — shipped: `allowed_ips` is now per-candidate eligibility (each candidate filtered by its own list before the LB strategy; the request dispatches to any passing candidate — union semantics, fail-open by design: route-wide lockdown belongs to the token-level IP allowlist, documented). A fully rejected visitor gets the `denied:` redirect (yaml top-level/per-service, `APERIO_DENIED`, declared via Ping and validated on both sides) of the most-primary declaring rejecting candidate, or a stealth answer identical to an unclaimed route (504 — replaces the old 403; docs + e2e updated). Blocked traffic still never enters the tunnel; failover re-picks apply the same filter.

### Developer experience & integrations


- [x] #118 Ctrl/Cmd+S saves the Settings page — shipped: a keydown handler on the settings form persists pending overrides (matching editor muscle memory) instead of the browser's save-page dialog.
- [ ] #90 SDKs / Terraform provider on top of the OpenAPI spec
- [x] #91 Embedded Swagger UI / API explorer — shipped: a self-contained *API Explorer* dashboard page renders `/aperio/api/openapi.json` grouped by tag with expandable operations and an inline try-it form (session-authenticated, no external Swagger assets)
- [ ] #92 `aperio-client open` — open the public URL in a browser and print a QR code in the terminal
- [ ] #93 Tunnel presets — `aperio-client --preset vite/next` ready-made framework templates
- [ ] #94 Official Helm chart — a docker-compose example exists, no k8s chart
- [ ] #95 Import from ngrok config — migration helper
- [ ] #96 Slack slash-command bot — create/list tunnels from Slack
- [ ] #97 VS Code extension / status-bar integration
- [ ] #98 Server-side request breakpoints — pause a matching request the server already holds and let a developer edit method/headers/body (or synthesize a response) in the dashboard: Charles/Fiddler interception with zero client changes
- [ ] #99 Per-route fault injection — chaos rules on a hostname/path/token add delay, return synthetic 5xx, or drop the relay, to test frontend/client resilience against a misbehaving backend
- [x] #100 Inbound webhook capture inbox — shipped: `webhook_inbox: true` (per-service) persists every inbound POST into a restart-surviving SQLite inbox; a *Webhook Inbox* dashboard page browses (redacted) payloads, deletes, and re-fires any event to the currently routed client (`/aperio/api/inbox[...]`, audited `webhook_refired`)
- [ ] #101 Mock/stub response library — extend client-less fixed routes into a matcher-based mock library (method/path/header/query) so a frontend can develop against canned responses before the backend exists
- [ ] #102 Auto-inferred OpenAPI from traffic — incrementally infer path/param/schema shapes from live tunnel traffic and emit a draft OpenAPI document per hostname
- [ ] #103 Golden-diff response drift detection — snapshot a response as a baseline and flag when future responses to the same route diverge in status/headers/body shape
- [ ] #104 Portable traffic session bundle — record a window of a tunnel's traffic into a replayable `.aperio-session` bundle to re-inject or hand to a teammate

- [x] #120 JSON Schema for `aperio-server.yaml` — shipped: a documented `ServerFileConfig` schema generated to `schemas/aperio-server.schema.json` (build.rs + `aperio-config --server`) and attached to releases, for editor completion/validation.
### Operations, data & compliance

- [ ] #105 HA / multi-server mode — shared state (tokens, routes, stats) across servers, client failover between servers
- [ ] #106 Scheduled maintenance windows
- [ ] #107 Kubernetes operator / ingress mode
- [ ] #108 Scheduled automatic DB backups — periodic snapshot + retention (on top of dump export/import)
- [ ] #109 Per-token quotas — max requests/day, max bandwidth, 429 on exceed
- [x] #110 Multi-tenancy / organizations — group tokens and users under orgs (shipped: master + child orgs; per-org isolation of clients, tokens, and users; the aperio super-admin switches orgs from the sidebar; audit records the acting user)
- [x] #111 Server config lint / dry-run — shipped: `aperio-server --check-config` validates the layered file+env configuration (scalar parses, enum values, CIDRs, page files, structured sections, OIDC coherence) and exits 0/1 without starting the server
- [ ] #112 Blue-green client deployment — the new client takes over while the old drains (on top of graceful drain)
- [ ] #113 Hash-chained tamper-evident audit log — chain each audit row's hash into the next with a verify command, making any edit/deletion of the audit history detectable after the fact
- [x] #114 Per-data-type retention policies — shipped: `APERIO_RETENTION_{CAPTURES,ACCESS_LOG,AUDIT,STATS}` (days) enforced by an hourly background pruner (audit prunes chain-safely: whole expired rotations + only the active file's leading prefix); each cycle audited as `retention_pruned`
- [x] #115 Disk-usage guard with auto-prune at cap — shipped: `APERIO_DB_MAX_BYTES` caps aperio.db(+WAL/SHM); 90% emits `disk_usage_warning` (hysteresis at 80%), past the cap the hourly guard prunes oldest inbox/delivery/day-stat rows, vacuums, and emits `disk_pruned`
- [x] #116 Right-to-erasure selective purge — shipped: `POST /aperio/api/purge` (admin) erases the traffic log, inspector captures, per-hostname/per-token stats rows, stage windows, cache entries, and access-log lines matching a hostname/token/visitor-IP selector; audited as `data_purged`
- [ ] #117 Client-side store-and-forward capture buffer — the client queues request metadata to a small local buffer when the WebSocket drops and replays it on reconnect, so no traffic records are lost during outages

- [x] #121 `aperio-server.yaml` hot-reload — shipped: the server watches its config file and re-applies live-editable settings + `headers:`/`routes:` without a restart (layered env -> file -> dashboard, no runtime `set_var`); structural keys still need a restart. `APERIO_CONFIG_HOT_RELOAD=0` disables it.
## Rejected

- #2 GeoIP country-based access rules — rejected: not a current need; the CIDR allowlist covers our access-scoping use cases.
- #4 Time-window access rules — rejected: business-hours logic belongs to the application behind the tunnel, not to Aperio.
- #9 Audit log export to SIEM — rejected: out of scope for now.
- #13 IP reputation / denylist feeds — rejected: curating threat intelligence is not a proxy's responsibility (comparable reverse proxies don't take it on either); operators can feed external blocklists into their firewall.
- #17 One-time client enrollment codes — rejected: dynamic bootstrap complicates the setup story; today a single static config brings everything up, and that simplicity is worth keeping.
- #18 Client-side egress allowlist — rejected: it only defends against an attacker who already controls the client host, and such an attacker can edit the client config anyway — the enforcement point is inside the compromised trust boundary.
- Public status page — out of scope; dedicated uptime tools (e.g. Uptime Kuma) do this better, and they can consume `GET /aperio/api/uptime`.
