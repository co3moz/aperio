# Configuration Reference

The complete reference for configuring both sides of Aperio, and the naming standard that ties the three configuration surfaces together.

## The standard: one name, three surfaces

Every client setting is reachable through three surfaces, and the names map **mechanically** between them:

| Surface | Form | Example |
| --- | --- | --- |
| CLI argument | `--kebab-case` | `--server-token` |
| yaml key | `snake_case` (nested for the server section) | `server.token` |
| Environment variable | `APERIO_SNAKE_CASE` | `APERIO_SERVER_TOKEN` |

The rule: take the CLI flag, drop the dashes, uppercase it, prefix `APERIO_` — that is the environment variable. Lowercase it with underscores — that is the yaml key. New settings must follow this scheme on all three surfaces (a setting may deliberately skip a surface — e.g. tuning knobs without a CLI flag — but never rename across surfaces).

**Legacy aliases.** The pre-rename spellings remain accepted so existing setups keep working: `APERIO_CLIENT_*` for most client variables, `APERIO_HOSTNAME_BIND` / `APERIO_PATH_BIND`, the flat yaml `server:`/`token:` form, and CLI aliases `--server`, `--token`, `--host`, `--concurrency`. New documentation and examples always use the canonical names.

The server is configured through environment variables only (no yaml, no CLI flags beyond `--version`); most settings can also be edited live from the dashboard, where they become persisted overrides on top of the env defaults (`APERIO_DATA_DIR/settings.json`). Security- and startup-critical flags (proxy trust, cookies, OIDC, metrics, access log) stay env-only; the dashboard settings page lists them read-only with their current values.

**Dashboard users and roles.** Beyond the master token and the optional `APERIO_DASHBOARD_AUTH` password (both of which always sign in as a built-in admin named `aperio`), admins can create named dashboard users from the *Users* page, each with a role: **viewer** (read-only — statistics, traffic, audit), **operator** (day-to-day operations — clients, tokens, webhooks, maintenance, share links), or **admin** (everything, including server settings and user management). Passwords are stored as Argon2id hashes in `APERIO_DATA_DIR/aperio.db`. The role floor of every dashboard route is enforced server-side (a viewer gets `403` on any mutation, and on the admin-only settings/users routes); the UI additionally hides controls a role cannot use. OIDC logins act as admins.

## Client

### Precedence

The client layers four configuration sources, from lowest to highest:

**`~/.aperio.yaml`  <  environment variables  <  `./aperio.yaml`  <  CLI arguments**

`~/.aperio.yaml` holds user-level defaults shared across projects (typically `server.url` and `server.token`), the local `aperio.yaml` describes the service in the current directory, and CLI arguments override everything. With no CLI arguments the client is fully environment-driven — Docker setups work unchanged. `aperio-client check` reports which layer supplied each value.

When a config file is present, the client **hot-reloads** it: edits to `aperio.yaml` (or the `--config` path) are detected within ~5 s, the current connection is dropped gracefully, and the service restarts with the freshly resolved configuration — **every** setting applies. A file that no longer parses (or resolves to an invalid configuration) is ignored with a warning.

### CLI

```
aperio-client                          Run from config files / environment (Docker mode)
aperio-client 3000                     Expose http://localhost:3000
aperio-client example.com              Expose http://example.com
aperio-client --bind-tunnels <id>      Bind the declared tunnels of a peer client locally
aperio-client check                    Diagnose configuration and connectivity
aperio-client --version
aperio-client --help
```

The positional target is optional — a bare port number expands to `http://localhost:<port>`, a bare hostname to `http://<hostname>`, and full URLs pass through. When omitted, the target comes from a config file or the environment.

`aperio-client check` resolves the configuration with the usual precedence — reporting **which layer** (CLI argument, `./aperio.yaml`, environment, `~/.aperio.yaml`) supplied each value — and verifies every hop: server health endpoint (including a client/server version and protocol comparison), token validity (a real tunnel handshake), every local target (all `services:` entries in multi-service mode), and their health endpoints when configured. Exit code 0 = all green — handy in support requests and provisioning scripts.

| Option | Meaning |
| --- | --- |
| `--server-url URL` (alias `--server`) | Aperio server URL |
| `--server-token TOKEN` (alias `--token`) | Tunnel token (master or dynamic) |
| `--target TARGET` | Alternative to the positional target (usable with subcommands) |
| `--hostname HOSTNAME` (alias `--host`) | Hostname bind (e.g. `app.example.com`) |
| `--path PREFIX` | Path bind (e.g. `/api`) |
| `--max-concurrent N` (alias `--concurrency`) | Local max concurrent requests |
| `--priority N` | Load-balancing priority tier: 0 = primary (default), higher = standby |
| `--pass-hostname` | Forward the original `Host` header to the backend |
| `--public` | Declare the service public (skip the visitor auth gate; needs token permission) |
| `--visitor-auth USER:PASSWORD` | Gate this service behind a client-set visitor login, overriding the server's own visitor password for it (needs the same token permission as `--public`) |
| `--allowed-ips IPS` | Comma-separated visitor IPs/CIDRs allowed to reach this service (everyone when unset); the server rejects other visitors with 403 |
| `--client-id UUID` | Persistent client instance id (default: a random UUID per run) |
| `--bind-tunnels [CLIENT_ID]` | Bind a peer client's declared tunnels locally (see [Emergency Tunnels](emergency-tunnels.md)) |
| `--config FILE` | Config file path (default: `./aperio.yaml`) |

### Settings

| Env variable (legacy alias) | CLI | yaml key | Description | Default |
| --- | --- | --- | --- | --- |
| `APERIO_SERVER_TOKEN` | `--server-token` | `server.token` | **Required.** Tunnel token. | — |
| `APERIO_SERVER_URL` | `--server-url` | `server.url` | **Required.** Server URL (`http/https/ws/wss`). | — |
| `APERIO_TARGET` (`APERIO_CLIENT_TARGET`) | positional / `--target` | `target` | **Required.** Local backend to forward to. | — |
| `APERIO_HOSTNAME` (`APERIO_HOSTNAME_BIND`) | `--hostname` | `hostname` | Hostname this client serves. | — |
| `APERIO_PATH` (`APERIO_PATH_BIND`) | `--path` | `path` | Path prefix this client serves. | — |
| `APERIO_TRIM_BIND` (`APERIO_CLIENT_TRIM_BIND`) | — | `trim_bind` | Strip the path bind prefix before forwarding. | `1` when a path bind is set |
| `APERIO_PASS_HOSTNAME` (`APERIO_CLIENT_PASS_HOSTNAME`) | `--pass-hostname` | `pass_hostname` | Forward the original `Host` header instead of the target's. | `0` |
| `APERIO_PUBLIC` (`APERIO_CLIENT_PUBLIC`) | `--public` | `public` | Declare the service public: the server skips its visitor password / OIDC gate for routes served exclusively by this client. Honored only when the token permits publishing public services (master always does). | `0` |
| `APERIO_VISITOR_AUTH` | `--visitor-auth` | `auth` | `user:password` — gate this service behind a client-set visitor login, superseding the server's own `APERIO_SERVER_AUTH` for it (only the client's credentials work; master and dashboard passwords always do). A successful login is scoped to that hostname. Same token permission as `public`; ignored if the server sets `APERIO_IGNORE_CLIENT_AUTH`. Per `services:` entry via `auth:`. | — |
| `APERIO_ALLOWED_IPS` | `--allowed-ips` | `allowed_ips` | Visitor IPs/CIDRs allowed to reach this service (comma-separated on the CLI/env, a list in yaml; e.g. `203.0.113.7,10.0.0.0/8`). The server rejects every other visitor with `403` before dispatching, so blocked traffic never reaches the client. Purely restrictive — no token permission needed. When several clients serve one route, a visitor must pass **every** declared list. Per `services:` entry via `allowed_ips:`. | everyone |
| `APERIO_PRIORITY` (`APERIO_CLIENT_PRIORITY`) | `--priority` | `priority` | Load-balancing priority tier announced to the server (0 = primary, higher = standby; effective with `APERIO_LB_STRATEGY=primary-standby`). | `0` |
| `APERIO_BANDWIDTH` (`APERIO_CLIENT_BANDWIDTH`) | — | `bandwidth` | Link capacity of this client's network, e.g. `8mbit`, `500kbit`, `2MB`, or plain bytes/second. The server paces outgoing tunnel frames (token bucket, 1 s burst) so this client is never pushed faster than its network can drain. | unlimited |
| `APERIO_MAX_CONCURRENT` (`APERIO_CLIENT_MAX_CONCURRENT`) | `--max-concurrent` | `max_concurrent` | Max concurrent requests; announced to the server, which queues the excess instead of flooding the backend. Also enforced locally. | unlimited |
| `APERIO_CONNECTIONS` (`APERIO_CLIENT_CONNECTIONS`) | — | `connections` | Parallel tunnel connections per service (1–16); the server load-balances across them like separate clients. | `1` |
| `APERIO_CACHE` (`APERIO_CLIENT_CACHE`) | — | `cache` | Opt this service into the server-side GET response cache (needs `APERIO_CACHE=1` on the **server**; strictly `Cache-Control`-driven). Per `services:` entry via `cache:`. | `0` |
| `APERIO_CLIENT_ID` | `--client-id` | `client_id` | Persistent client instance id (a UUID). Keeps the id stable across restarts — useful for failover `wait` mode and `--bind-tunnels`. | random UUID per run |
| `APERIO_TARGET_HEALTH` (`APERIO_CLIENT_TARGET_HEALTH`) | — | `target_health` | Health endpoint of the local target (path like `/health`, or a full URL). When set, the client probes it independently and reports the result to the server: a failing backend takes the client **out of routing without dropping the tunnel**; it rejoins automatically when the probe recovers. The dashboard shows a `BACKEND DOWN` badge meanwhile. | — |
| `APERIO_HEALTH_INTERVAL` (`APERIO_CLIENT_HEALTH_INTERVAL`) | — | `health_interval` | Seconds between backend health probes. | `10` |
| `APERIO_HEALTH_TIMEOUT` (`APERIO_CLIENT_HEALTH_TIMEOUT`) | — | `health_timeout` | Per-probe timeout (seconds). | `5` |
| `APERIO_HEALTH_THRESHOLD` (`APERIO_CLIENT_HEALTH_THRESHOLD`) | — | `health_threshold` | Consecutive probe failures before the backend is reported unhealthy. | `2` |
| `APERIO_TIMEOUT` (`APERIO_CLIENT_TIMEOUT`) | — | `timeout` | Per-request backend timeout (seconds). | `30` |
| `APERIO_MAX_REDIRECTS` (`APERIO_CLIENT_MAX_REDIRECTS`) | — | `max_redirects` | Backend redirects followed transparently: same-host scheme upgrades (`http://x` → `https://x`) and hops within the same root domain (`example.com` → `test.example.com`), never downgrading https to http. Redirects beyond this many jumps — or to unrelated hosts — pass through to the visitor unchanged. `0` disables following entirely. | `5` |
| `APERIO_MAX_RESPONSE_BODY` (`APERIO_CLIENT_MAX_RESPONSE_BODY`) | — | `max_response_body` | Max backend response size in bytes; bodies over 256 KB are streamed through the tunnel in chunks, larger than this limit are truncated. | 50 MB |
| `APERIO_MAX_MESSAGE_SIZE` (`APERIO_CLIENT_MAX_MESSAGE_SIZE`) | — | `max_message_size` | Max size of one tunnel message accepted from the server (memory protection). | 32 MB |
| `LOG_LEVEL` | — | — | Log verbosity. | `info` |
| `APERIO_LOG_FORMAT` | — | — | `json` or `pretty`. By default the client auto-detects: human-readable logs on an interactive terminal, JSON when stdout is not a TTY (Docker, pipes, service managers). | auto |

Yaml-only sections: `services:` (multiple exposed targets, below), `tunnels:` and `bind-tunnels:` (see [Emergency Tunnels](emergency-tunnels.md)).

### aperio.yaml & ~/.aperio.yaml

```yaml
# ~/.aperio.yaml — user-level defaults shared across projects
server:
  url: https://tunnel.example.com
  token: apr_xxxxxxxxxxxxxxxx
```

```yaml
# ./aperio.yaml — per-project service description
target: http://localhost:3000

# optional
hostname: app.example.com
path: /api
trim_bind: true
pass_hostname: false
max_concurrent: 8
priority: 0                # 0 = primary, higher = standby tier
target_health: /health     # probe the backend; report unhealthy without dropping the tunnel
health_interval: 10
```

The legacy flat form (`server: https://...` plus top-level `token:`) is still accepted. The local file is hot-reloaded: edits are applied within ~5 s via a graceful reconnect.

### Multiple services

One client process can expose several targets: replace the single `target` with a `services:` list, and the client opens one tunnel connection per entry — each with its own binds, health probe, and knobs:

```yaml
server:
  url: https://tunnel.example.com
  token: apr_xxxxxxxxxxxxxxxx
services:
  - name: web
    target: http://localhost:3000
    hostname: app.example.com
    target_health: /health
  - name: api
    target: http://localhost:4000
    hostname: api.example.com
    max_concurrent: 8
  - name: docs
    target: http://localhost:5000
    path: /docs
```

Per-entry fields: `name`, `target` (required), `hostname`, `path`, `trim_bind`, `pass_hostname`, `max_concurrent`, `connections`, `priority`, `bandwidth`, `timeout`, `max_response_body`, `max_redirects`, `target_health`, `health_interval`, `health_timeout`, `health_threshold`, `public`, `auth`, `headers`. Unset tuning knobs fall back to the top-level values; binds are strictly per entry.

`connections: N` (1–16, default 1, also valid at the top level or as `APERIO_CONNECTIONS`) opens N parallel tunnel connections for a service. The server pools them like separate clients — its load-balancing strategy spreads requests across them — so a single service is no longer serialized behind one WebSocket under heavy parallel traffic. Each connection gets its own instance id (`<id>`, `<id>-c2`, `<id>-c3`, …), so the dashboard's shared-id warning is not triggered and failover/`--bind-tunnels` lookups stay unambiguous; `max_concurrent` applies per connection. The `name` shows up in client logs and as a badge in the dashboard's clients table. The `services:` list is read from the local config file only; a positional CLI target overrides it entirely (single-service mode). Config hot-reload re-resolves the whole list, so adding or removing services doesn't need a restart.

### Header rules

A `headers:` section (top-level, or per `services:` entry — the entry replaces the top-level section entirely when set) edits proxied HTTP traffic on the client: `request` rules apply to what the local backend receives, `response` rules to what the visitor receives. `add` sets a header, replacing any existing value of the same name; `remove` strips headers case-insensitively:

```yaml
headers:
  request:
    add:
      X-Forwarded-Env: staging
    remove: [X-Internal-Debug]
  response:
    add:
      X-Served-By: aperio
    remove: [Server, X-Powered-By]
```

Hop-by-hop and tunnel-critical headers (`Connection`, `Upgrade`, `Sec-WebSocket-*`, …) stay managed by Aperio regardless of these rules, and WebSocket upgrade traffic is not affected. Config file only (no CLI/env form); hot-reload applies edits within ~5 s.

### Editor autocompletion (JSON Schema)

Building the client emits a JSON Schema for `aperio.yaml` to `schemas/aperio-client.schema.json` (git-ignored — it's a build artifact, regenerated from the parser types so it never drifts). Point your editor's YAML extension at it for completion, hover docs, and validation:

```jsonc
// .vscode/settings.json (VS Code / Antigravity, with the YAML extension)
{
  "yaml.schemas": {
    "./schemas/aperio-client.schema.json": ["aperio.yaml", "**/aperio.yaml", "~/.aperio.yaml"]
  }
}
```

Run `cargo build -p aperio-client` once to generate it (or `cargo run -p aperio-config > schemas/aperio-client.schema.json`). Tagged releases attach the schema as a release asset twice: a versioned `aperio-client.<tag>.json` for pinning, and a stable-named `aperio-client.schema.json` so schema managers can point at a URL that always serves the latest release:

```
https://github.com/co3moz/aperio/releases/latest/download/aperio-client.schema.json
```

## Server

### Core

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_TOKEN` | **Required.** Master token: authenticates tunnel clients and doubles as the dashboard admin password (`aperio:<token>`). | — |
| `HOST` | Bind address. | `0.0.0.0` |
| `PORT` | Listen port. | `8080` |
| `APERIO_DATA_DIR` | Directory for persisted state (tokens, stats, audit log, webhooks). **Mount a volume here in Docker.** | `./data` |
| `LOG_LEVEL` | `error`, `warn`, `info`, `debug`, `trace`. | `info` |

### Routing & load balancing

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_REQUIRE_HOSTNAME_BIND` | `1` = clients without a hostname bind never receive traffic (strict multi-tenant mode). | `0` |
| `APERIO_RANDOM_SUBDOMAIN` | Pattern with a `*` placeholder in the leftmost label — every connecting client gets the pattern with `*` replaced by a random label, in addition to its other binds. `example.com` ≡ `*.example.com`; `*-test.example.com` yields `<random>-test.example.com` (stays on the same subdomain level, so one wildcard TLS cert covers it). | — |
| `APERIO_CLIENT_DOWN_THRESHOLD` | Seconds without a heartbeat before a client is dropped from the routing pool (it rejoins on the next ping). | `15` |
| `APERIO_LB_STRATEGY` | Load-balancing strategy: `round-robin`, `primary-standby` (client priority tiers), or `sticky` (visitor affinity via cookie). See [Routing & Load Balancing](routing-and-load-balancing.md). | `round-robin` |
| `APERIO_FAILOVER` | What to do when a client dies mid-request: `fail`, `retry`, `wait`, or `retry-wait`. See [In-Flight Failover](failover.md). | `fail` |
| `APERIO_FAILOVER_MAX_JUMPS` | Max re-dispatch attempts per request. | `2` |
| `APERIO_FAILOVER_WINDOW` | Total seconds the `wait`/`retry-wait` modes may spend waiting for a candidate, across all jumps. | `15` |
| `APERIO_FAILOVER_ALL_METHODS` | `1` = also fail over non-idempotent methods (POST/PATCH). Off by default because a re-dispatched request may reach a backend twice. | `0` |

### Limits & protection

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_MAX_BODY_SIZE` | Max request body size in bytes. | `10485760` (10 MB) |
| `APERIO_MAX_CONCURRENT_REQUESTS` | Max in-flight proxied requests across all tunnels. | `100` |
| `APERIO_MAX_TUNNELS` | Max simultaneously connected tunnel clients. | `10` |
| `APERIO_IP_LIMIT_MAX` | Per-IP token bucket burst capacity. | `100` |
| `APERIO_IP_LIMIT_REFILL` | Per-IP refill rate (requests/second). | `5` |
| `APERIO_LOGIN_LOCKOUT_THRESHOLD` | Consecutive failed logins from one IP before it is locked out. | `5` |
| `APERIO_LOGIN_LOCKOUT_SECS` | Base lockout window in seconds; doubles with each repeat lockout (capped at 1 h). A successful login resets the state. | `60` |
| `APERIO_SERVER_GATEWAY_TIMEOUT` | Seconds to wait for a client to (re)connect before failing a request. | `10` |
| `APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT` | Seconds to wait for a client to answer a dispatched request. | `30` |
| `APERIO_TRUST_PROXY` | `1` = trust `X-Forwarded-For` / `X-Real-IP` for client IPs. Enable **only** behind a trusted reverse proxy. | `0` |
| `APERIO_TRUSTED_PROXIES` | Comma-separated IPs/CIDRs of your reverse proxies and CDN egress ranges (e.g. `10.0.0.0/8,173.245.48.0/20`). When set, the client IP is resolved by walking `X-Forwarded-For` (plus the direct peer) from the nearest hop backwards past trusted addresses — the CDN-agnostic model that works for Cloudflare, Fastly, Akamai, LB chains, etc. Headers from an untrusted direct peer are ignored entirely. Implies `APERIO_TRUST_PROXY=1`. Prefer this over the header-based options. | — |
| `APERIO_REAL_IP_HEADER` | Header consulted **before** `X-Forwarded-For` for the real client IP (with `APERIO_TRUST_PROXY=1`). Needed behind CDN→proxy chains where the proxy resets XFF to the CDN edge — e.g. set `CF-Connecting-IP` behind Cloudflare, or configure the proxy's `forwardedHeaders.trustedIPs` instead. | — |
| `APERIO_TRUST_CF_HEADER` | `1` = shorthand for `APERIO_REAL_IP_HEADER=CF-Connecting-IP` (an explicit `APERIO_REAL_IP_HEADER` wins). Enable **only** behind Cloudflare: any visitor can send this header, so on other deployments trusting it lets clients spoof their IP for rate limiting, audit logs, and token IP allowlists. | `0` |
| `APERIO_SECURE_COOKIES` | `1` = set the `Secure` flag on session cookies. Defaults to the `APERIO_TRUST_PROXY` value. | — |
| `APERIO_TUNNEL_COMPRESSION` | `1` = offer per-message zlib compression to clients (enabled per connection once acknowledged; old clients keep plain frames). | `0` |
| `APERIO_CACHE` | `1` = enable the server-side GET response cache for services that opt in with the client `cache: true` setting. Strictly `Cache-Control`-driven: only responses explicitly allowing shared caching (`max-age`/`s-maxage`, no `no-store`/`no-cache`/`private`, no `Vary`/`Set-Cookie`) are stored, for the advertised lifetime; only credential-less plain GETs are answered from it. Hits carry `x-aperio-cache: hit`. | `0` |
| `APERIO_CACHE_MAX_BYTES` | Total in-memory budget of the response cache; inserting past it evicts the entries closest to expiry, and a single body larger than a quarter of the budget is never cached. | `67108864` (64 MB) |
| `APERIO_504_PAGE` | Path to an HTML file served on 504 gateway-timeout responses instead of the plain-text default. | — |
| `APERIO_503_PAGE` | Path to an HTML file served while a hostname is in maintenance mode instead of the plain-text default. | — |
| `APERIO_AUDIT_MAX_SIZE` | Rotate `audit.jsonl` once it exceeds this many bytes (`0` = never rotate). | `10485760` (10 MB) |
| `APERIO_AUDIT_MAX_FILES` | Rotated audit generations to keep (`audit.jsonl.1` … `.N`; `0` = truncate instead of keeping history). | `3` |
| `APERIO_ACCESS_LOG` | File path for the structured access log: one JSON line per proxied request (`request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, `error`) — directly ingestible by Loki/ClickHouse. The same data is always emitted to stdout as structured `aperio_access` tracing events. | — |
| `APERIO_OTEL` | `1` = export one OTLP span per proxied request to an OpenTelemetry collector (adopts inbound W3C `traceparent`, propagates its own context to the backend). | `0` |
| `APERIO_OTEL_ENDPOINT` | OTLP/HTTP collector base URL (`/v1/traces` is appended if absent). Falls back to the standard `OTEL_EXPORTER_OTLP_ENDPOINT`. | `http://localhost:4318` |
| `APERIO_OTEL_SERVICE_NAME` | `service.name` reported on exported spans. Falls back to `OTEL_SERVICE_NAME`. | `aperio-server` |

### Authentication & dashboard

> 📖 Concepts and hardening advice: [Tokens & Authentication](tokens-and-auth.md)

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_AUTH` | `user:password` — a visitor login form in front of all proxied traffic. | — |
| `APERIO_IGNORE_CLIENT_AUTH` | `1` = ignore any client-declared per-service visitor password (see the client `auth` setting) and keep sole control of the visitor gate with `APERIO_SERVER_AUTH` / OIDC. | `0` |
| `APERIO_DASHBOARD` | `0` = disable the admin dashboard entirely. | `1` |
| `APERIO_UI_LANGUAGE` | Default dashboard/login UI language (`en`, `de`, `es`, `fr`, `tr`, `ru`, `zh`, `ja`) used when the visitor's browser language is unsupported; also dashboard-editable. | `en` |
| `APERIO_DASHBOARD_AUTH` | Separate dashboard-only password (username `aperio`), so the master token doesn't have to be shared with dashboard users. | — |
| `APERIO_METRICS` | `1` = enable the Prometheus endpoint at `/aperio/metrics`. | `0` |
| `APERIO_METRICS_TOKEN` | Token required to scrape metrics (`?token=` or `Authorization: Bearer`). Unset = a random one is generated on first start and persisted in `APERIO_DATA_DIR/metrics_token`. | generated |

### OIDC / SSO

Put an identity-provider login (Google, Keycloak, Authentik, ...) in front of everything the tunnel serves. Unauthenticated visitors are redirected to the provider; after login, the verified email (fetched from the issuer's `userinfo` endpoint over TLS) is checked against the allowlist — exact addresses, `*@domain`, or `*`. Sessions last 24h.

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_OIDC_ISSUER` | Issuer URL. Setting it enables SSO enforcement. | — |
| `APERIO_OIDC_CLIENT_ID` / `APERIO_OIDC_CLIENT_SECRET` | OAuth client registered at the issuer. Redirect URI: `https://<your-host>/aperio/oidc/callback`. | — |
| `APERIO_OIDC_ALLOWED_EMAILS` | Comma-separated allowlist (required with issuer). | — |
| `APERIO_OIDC_SCOPES` | Requested scopes. | `openid email profile` |
| `APERIO_OIDC_REDIRECT_URL` | Fixed callback URL; otherwise derived from the request `Host` (and `X-Forwarded-Proto` when `APERIO_TRUST_PROXY=1`). Recommended to set explicitly. | derived |

Discovery is fetched from `<issuer>/.well-known/openid-configuration` at startup. A misconfigured SSO setup is a **fatal error** — the server refuses to start rather than silently serving an unprotected proxy. Grants and denials are audit-logged.

## HTTP endpoints

| Endpoint | Description | Auth |
| --- | --- | --- |
| `/*` (fallback) | Proxied to tunnel clients. | visitor password / OIDC if configured |
| `GET /aperio/ws` | Tunnel endpoint for clients. | master or dynamic token (Bearer / `x-auth-token`) |
| `GET /aperio/tunnels/:client_id` | Declared-tunnels discovery for `--bind-tunnels` (see [Emergency Tunnels](emergency-tunnels.md)). | the same token the client connected with (or master) |
| `GET /aperio` | Admin dashboard. | dashboard session |
| `GET /aperio/api/stats`, `/api/logs`, `/api/audit` | Live stats, request log, audit events. | dashboard session |
| `GET/POST /aperio/api/tokens`, `PUT/DELETE /aperio/api/tokens/:id` | Dynamic token management. | dashboard session |
| `GET/POST /aperio/api/webhooks`, `DELETE /aperio/api/webhooks/:id` | Webhook management. | dashboard session |
| `GET /aperio/api/requests/:id`, `POST /aperio/api/requests/:id/replay` | Request inspector & replay. | dashboard session |
| `POST /aperio/api/clients/:id/override`, `POST /aperio/api/clients/:id/enabled` | Temporary bind overrule / enable-disable toggle. | dashboard session |
| `GET/POST /aperio/api/maintenance` | List / toggle per-hostname maintenance mode. | dashboard session |
| `POST /aperio/api/share` | Generate a signed share link (see [Share Links](share-links.md)). | dashboard session |
| `GET/PUT /aperio/api/settings` | Read / edit runtime server settings (persisted overrides on top of env defaults). | dashboard session |
| `POST /aperio/api/tunnels`, `DELETE /aperio/api/tunnels/:id` | Programmatic ephemeral tunnel provisioning. See [Ephemeral Tunnels](ephemeral-tunnels.md). | master token (Bearer) or dashboard session |
| `GET/POST /aperio/auth` | Login page / login API. | — |
| `GET /aperio/oidc/login`, `/aperio/oidc/callback` | OIDC flow. | — |
| `GET /aperio/metrics` | Prometheus metrics. | metrics token |
| `GET /aperio/health` | Liveness probe (status, client count, uptime). | none |
| `GET /aperio/api/openapi.json` | OpenAPI 3.1 document describing this whole API (generated from the handlers; point Swagger UI or a client generator at it). | dashboard session |
| `GET/POST /aperio/api/users`, `PUT/DELETE /aperio/api/users/:id` | Dashboard user management (create/edit/delete, roles). | dashboard session (**admin**) |
