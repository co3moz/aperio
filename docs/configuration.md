# Configuration Reference

The complete reference for configuring both sides of Aperio, and the naming standard that ties the three configuration surfaces together.

## The standard: one name, three surfaces

Every client setting is reachable through three surfaces, and the names map **mechanically** between them:

| Surface | Form | Example |
| --- | --- | --- |
| CLI argument | `--kebab-case` | `--server-token` |
| yaml key | `snake_case` (nested for the server section) | `server.token` |
| Environment variable | `APERIO_SNAKE_CASE` | `APERIO_SERVER_TOKEN` |

The rule: take the CLI flag, drop the dashes, uppercase it, prefix `APERIO_`, that is the environment variable. Lowercase it with underscores, that is the yaml key. New settings must follow this scheme on all three surfaces (a setting may deliberately skip a surface, e.g. tuning knobs without a CLI flag, but never rename across surfaces).

### Grouped keys

Settings that share a prefix are written as a block, so the yaml reads as a structure rather than a flat list of run-on names:

```yaml
# ./aperio.yaml (client)
health:
  endpoint: /healthz   # was target_health
  interval: 10         # was health_interval
  timeout: 5           # was health_timeout
  threshold: 3         # was health_threshold
  wait_for_backend: true
```

```yaml
# ./aperio-server.yaml
cache:
  enabled: true        # was cache
  max_bytes: 268435456 # was cache_max_bytes
alert:
  error_rate: 0.25     # was alert_error_rate
  window: 60           # was alert_window
```

The environment variable is unchanged, since the environment has no nesting: `health.interval` is still `APERIO_HEALTH_INTERVAL`, `cache.max_bytes` is still `APERIO_CACHE_MAX_BYTES`. The rule is mechanical in both directions, `<group>.<child>` ⇄ `APERIO_<GROUP>_<CHILD>`, and for a group whose own key is also a setting the child carrying it is `enabled` (`cache.enabled` → `APERIO_CACHE`) or `mode` for `failover`.

**The flat spelling still works.** Every `health_interval:` / `cache_max_bytes:` key is read exactly as before, so no existing file needs changing; the block wins per field when a file uses both. The client logs a one-line deprecation notice per flat key it finds, naming the block to move it to. Client blocks: `health` (also per `services:` entry) and the long-standing `server`. Server blocks: `alert`, `audit`, `backup`, `cache`, `dashboard`, `edge`, `failover`, `gateway`, `ip_limit`, `login_lockout`, `metrics`, `oidc`, `otel`, `outbound`, `retention`, `scaling`, `server`, `stream`. Blocks are folded exactly like the flat keys on a hot-reload too, so a file written with blocks reloads the same way one written flat does.

Each environment variable has exactly one canonical `APERIO_*` name, there are no scoping aliases (`APERIO_CLIENT_*`) or alternate spellings. A few surfaces still offer shorthand: the CLI flags `--server`, `--token`, `--host`, `--concurrency` are visible aliases of `--server-url`, `--server-token`, `--hostname`, `--max-concurrent`, and the flat yaml `server:` / `token:` keys are accepted as shorthand for `server.url` / `server.token`. New documentation and examples always use the canonical names.

The server is configured through environment variables or an optional `aperio-server.yaml` file (no CLI flags beyond `--version`); most settings can also be edited live from the dashboard, where they become persisted overrides (`APERIO_DATA_DIR/settings.json`). Precedence, lowest to highest: **environment variables < dashboard overrides < `aperio-server.yaml`**. A dashboard override applies to a key the file leaves alone; a key the file writes belongs to the file, and the dashboard refuses to override it. That is the point of the file: it is what an operator versions, reviews and deploys, so a value written there must not be quietly outvoted by a change made from a browser months earlier. An override contradicting the file is dropped at startup, named in the log and recorded in the audit trail, rather than kept where it would come back the day the key leaves the file. Security- and startup-critical flags (proxy trust, cookies, OIDC, metrics, access log, the outbound callback policy) never become dashboard overrides, though every one of them is still settable from `aperio-server.yaml`; the settings page lists them read-only with their current values.

**Dashboard users and roles.** Beyond the master token (which signs in as a built-in admin named `aperio`), admins can create named dashboard users from the *Users* page, each with a role: **viewer** (read-only, statistics, traffic, audit), **operator** (day-to-day operations, clients, tokens, webhooks, maintenance, share links), or **admin** (everything, including server settings and user management). Passwords are stored as Argon2id hashes in `APERIO_DATA_DIR/aperio.db`. The role floor of every dashboard route is enforced server-side (a viewer gets `403` on any mutation, and on the admin-only settings/users routes); the UI additionally hides controls a role cannot use. OIDC logins act as admins. Dashboard sessions last 24 hours and are persisted in `APERIO_DATA_DIR/aperio.db` (hashed cookie tokens only), so a server restart does not sign anyone out. Users (and tokens) can also be grouped into isolated **organizations**, in which case a named admin's reach is bounded to their own organization and the server-global settings/export routes are reserved for the built-in `aperio` super-admin, see [Organizations](organizations.md).

**Two-factor authentication (TOTP).** Any named dashboard user can enroll an authenticator app (Google Authenticator, Authy, 1Password, …) from the sidebar's *Two-factor auth* dialog: scan the QR code, confirm a 6-digit code, and store the eight single-use recovery codes shown once. From then on the login form asks for the code after the password (a recovery code works too, and is consumed). Wrong codes count towards the brute-force lockout. An admin can reset a locked-out user's TOTP from the *Users* page (`DELETE /aperio/api/users/:id/totp`); enrollment endpoints live under `/aperio/api/me/totp/*`. The built-in `aperio` admin (master token / dashboard password / OIDC) has no user row and cannot enroll, create named users for anyone who needs 2FA.

**Passkeys (WebAuthn).** Set `APERIO_WEBAUTHN_ORIGIN` to the public URL the dashboard is reached at (e.g. `https://tunnel.example.com`, the RP ID is its domain, so an IP origin won't work) and named users can register passkeys (YubiKeys, Touch ID / Face ID, password managers) from the sidebar's *Passkeys* dialog, then sign in passwordless from the login page. Passkey sign-ins skip TOTP (the authenticator already verifies the user), failed ceremonies count towards the brute-force lockout, and registrations/deletions are audit-logged. Up to 10 passkeys per user, stored in `aperio.db` (public keys only, private keys never leave the authenticator). A passkey registered with **“allow signing in without a username”** becomes a discoverable credential: pressing the passkey button with an empty username brings up the authenticator's account picker directly. Passkeys without the opt-in keep working username-first only. By default a passkey is scoped to the exact host of `APERIO_WEBAUTHN_ORIGIN`, so it will **not** work when the dashboard is also reached through a sibling host (e.g. `test-aperio.robogon.com` next to `aperio.robogon.com`); set `APERIO_WEBAUTHN_RP_ID` to the shared parent domain (`robogon.com`) to make one passkey cover every subdomain. WebAuthn has no wildcard RP ID; the parent-domain RP ID is the supported way to get the `*.robogon.com` effect.

## Client

### Precedence

The client layers four configuration sources, from lowest to highest:

**`~/.aperio.yaml`  <  environment variables  <  `./aperio.yaml`  <  CLI arguments**

`~/.aperio.yaml` holds user-level defaults shared across projects (typically `server.url` and `server.token`), the local `aperio.yaml` describes the service in the current directory, and CLI arguments override everything. With no CLI arguments the client is fully environment-driven, Docker setups work unchanged. `aperio-client check` reports which layer supplied each value.

When a config file is present, the client **hot-reloads** it: edits to `aperio.yaml` (or the `--config` path) are detected within ~5 s, the current connection is dropped gracefully, and the service restarts with the freshly resolved configuration. A file that no longer parses (or resolves to an invalid configuration) is ignored with a warning. Alongside the services, a reload re-applies the process-wide facilities: the `messages_listen` and `messages_mqtt_listen` faces (a face whose address is unchanged keeps serving its connections; one that moved is rebound, one the file dropped is stopped), the `subscribe:` filters and their `run:` commands, and `idle_timeout`. The one exception is `otel_bridge:`'s own shape, its listeners, queue and transport: that queue is held by whichever tunnel connection is live and cannot be rebuilt underneath them, so a reload that changes one of those logs a warning saying a restart is needed. The server and token the `https` transport posts with are *not* an exception; they are read per export and follow a reload like everything else. Settings fixed for the life of the process are documented as such where they appear (`ip_family`, for instance).

### CLI

```
aperio-client                          Run from config files / environment (Docker mode)
aperio-client 3000                     Expose http://localhost:3000
aperio-client example.com              Expose http://example.com
aperio-client --bind-tunnels <name>    Bind a declared tunnel locally (a client id binds all of that peer's)
aperio-client check                    Diagnose configuration and connectivity
aperio-client api <group> <action>     Call the server's admin API (see below)
aperio-client --version
aperio-client --help
```

The positional target is optional, a bare port number expands to `http://localhost:<port>`, a bare hostname to `http://<hostname>`, and full URLs pass through. When omitted, the target comes from a config file or the environment.

`aperio-client api ...` performs one admin API call and prints the JSON answer, the dashboard's operations (share links, tokens, tunnels, maintenance, users, reports) from a script. It authenticates with an admin key rather than the tunnel token; see [Admin API from the CLI](cli-api.md).

`aperio-client check` resolves the configuration with the usual precedence, reporting **which layer** (CLI argument, `./aperio.yaml`, environment, `~/.aperio.yaml`) supplied each value, and verifies every hop: server health endpoint (including a client/server version and protocol comparison), token validity (a real tunnel handshake), every local target (all `services:` entries in multi-service mode), and their health endpoints when configured. Exit code 0 = all green, handy in support requests and provisioning scripts.

| Option | Meaning |
| --- | --- |
| `--server-url URL` (alias `--server`) | Aperio server URL |
| `--server-token TOKEN` (alias `--token`) | Tunnel token (master or dynamic) |
| `--target TARGET` | Alternative to the positional target (usable with subcommands) |
| `--serve DIR` | Serve a local directory of static files instead of forwarding to a backend (mutually exclusive with a target; directories serve their `index.html`). One command to publish a `dist/` folder: `aperio-client --serve ./dist`. Also available per `services:` entry via `serve:`, so one client can serve several directories on different binds. Files stream from disk and single-range `Range` requests are answered `206`. Responses carry a strong `ETag` from the file's size and modification time, so an unchanged file answers `304` to a matching `If-None-Match` (on `GET` and `HEAD`) instead of being sent again, and a resumed download with `If-Range` continues rather than restarting. Two options refine not-found handling, `serve_spa` / `serve_404` in yaml or `APERIO_SERVE_SPA=1` (navigation falls back to the root `index.html` with status 200, for React/Vue routers) / `APERIO_SERVE_404=/path/to/404.html` (custom page, status 404); both are process-wide. Full article: [Static File Serving](static-serving.md). |
| `--hostname HOSTNAME` (alias `--host`) | Hostname bind(s) (e.g. `app.example.com`, or comma-separated `a.example.com,b.example.com`) |
| `--path PREFIX` | Path bind (e.g. `/api`) |
| `--max-concurrent N` (alias `--concurrency`) | Local max concurrent requests |
| `--priority N` | Load-balancing priority tier: 0 = primary (default), higher = standby |
| `--pass-hostname` | Forward the original `Host` header to the backend |
| `--public` | Declare the service public (skip the visitor auth gate; needs token permission) |
| `--visitor-auth USER:PASSWORD` | Gate this service behind a client-set visitor login, overriding the server's own visitor password for it (needs the same token permission as `--public`) |
| `--allowed-ips IPS` | Comma-separated visitor IPs/CIDRs allowed to reach this service (everyone when unset); enforced per candidate by the server, a fully rejected visitor gets the `denied:` redirect or a stealth answer |
| `--client-id UUID` | Persistent client instance id (default: a random UUID per run) |
| `--bind-tunnels [NAME]` | Bind a declared tunnel locally by name; a client id binds every tunnel that peer declares, and no value binds the `bind-tunnels:` section (see [Tunnels](emergency-tunnels.md)) |
| `--api-key KEY` | Admin API key used by the `api` subcommand (never by the tunnel) |
| `--config FILE` | Config file path (default: `./aperio.yaml`) |

### Settings

Only three settings are required, `APERIO_SERVER_TOKEN`, `APERIO_SERVER_URL`, and `APERIO_TARGET` (the target can be the positional argument). Together with the everyday flags in [CLI](#cli) above, they cover most usage. Everything else in the table is optional per-service tuning (health probing, caching, concurrency, body limits); reach for it only when you need it. `aperio-client check` reports which layer supplied each resolved value.

| Env variable | CLI | yaml key | Description | Default |
| --- | --- | --- | --- | --- |
| `APERIO_SERVER_TOKEN` | `--server-token` | `server.token` | **Required.** Tunnel token. |  |
| `APERIO_SERVER_URL` | `--server-url` | `server.url` | **Required.** Server URL (`http/https/ws/wss`). |  |
| `APERIO_API_KEY` | `--api-key` | `server.api_key` | Programmatic admin key for `aperio-client api ...` calls, unrelated to the tunnel itself. Falls back to the tunnel token, which the server accepts only where the master token is a valid credential. See [Admin API from the CLI](cli-api.md). |  |
| `APERIO_SERVER_URLS` |  | `server.urls` | Additional server URLs to fail over to (comma-separated on env, a list in yaml), tried in order when the primary `server.url` is unreachable, for a redundant control plane. |  |
| `APERIO_IP_FAMILY` | `--ip-family` | `ip_family` | Address family used to dial the server: `auto` (default; IPv4-first, trying each resolved address with a per-address timeout), `ipv4`, or `ipv6`. Use `ipv4` when the server hostname resolves to an unreachable IPv6 address. | `auto` |
| `APERIO_EGRESS_PROXY` | `--egress-proxy` | `egress_proxy` | HTTP proxy to dial the tunnel server through, for a network that allows no direct outbound connection: `host:port`, `http://host:port`, either with an optional `user:password@` in front. The client sends `CONNECT` and runs TLS inside the tunnel the proxy opens, so the proxy sees the server's hostname and nothing else. **The tunnel only**: requests to your own backend are never sent through it, and neither is anything else on the machine. A port is required in practice; without one the http default (`80`) is used, and a value that names a port but no host (`:3128`) is refused at startup rather than read as a hostname. An IPv6 proxy is written bracketed (`[2001:db8::1]:3128`). An `https://` proxy (TLS to the proxy itself) is refused rather than dialed in the clear. A proxy that refuses `CONNECT` fails the dial with the proxy and its status named, and `aperio-client check` reports the route it took. | direct |
| `APERIO_TLS_MIN_VERSION` | `--` | `tls_min_version` | Lowest TLS version offered when dialing the tunnel server over `wss://`: `1.2` or `1.3`. Unset leaves rustls' own set (1.2 and 1.3), which is the right default; pin it when a policy has to name the floor in writing rather than inherit it from a dependency. A value this client cannot offer is refused at startup, never silently ignored. Process-wide: a hot-reload does not change it. | *(rustls default: 1.2 and 1.3)* |
| `APERIO_TLS_CIPHER_SUITES` | `--` | `tls_cipher_suites` | Exact cipher suites offered when dialing the tunnel server, by their IANA names, comma-separated (e.g. `TLS13_AES_256_GCM_SHA384,TLS13_CHACHA20_POLY1305_SHA256`). Unset leaves rustls' preference order alone, which is almost always better; name them only when something external requires it. An unknown name, or a set that cannot meet `APERIO_TLS_MIN_VERSION`, is refused at startup. Process-wide. | *(rustls' own order)* |
| `APERIO_TARGET` | positional / `--target` | `target` | **Required.** Local backend to forward to. `http(s)://` (a bare port or hostname is normalized to it), or `h2c://` / `h2://` for HTTP/2 backends, gRPC: requests are dialed over HTTP/2 (cleartext prior knowledge / TLS), `te: trailers` is forwarded, and response trailers (`grpc-status`) are relayed to the visitor. The visitor leg must also be HTTP/2 for trailers to survive (aperio-server accepts h2c; have the fronting proxy forward gRPC as HTTP/2). `unix:///var/run/app.sock` forwards over a Unix domain socket (HTTP/1.1; Unix platforms only, WebSocket upgrades unsupported; each request dials the socket fresh, matching socket-activated backends). |  |
| `APERIO_SERVE` | `--serve` | `serve` | Serve a local directory of static files with a built-in file server instead of proxying to a backend, the directory takes the place of `target`. See [Static File Serving](static-serving.md) for SPA fallback, custom 404, streaming, `Range` support and the path-safety rules. |  |
| `APERIO_TCP_TARGET` |  | `tcp_target` | Bridge a raw local TCP service (experimental) rather than an HTTP backend, reached through the server's `/aperio/tcp` endpoint. |  |
| `APERIO_HOSTNAME` | `--hostname` | `hostname` | Hostname(s) this client serves. yaml `hostname:` accepts a single value or a list (`[a.example.com, b.example.com]`); the CLI/env value may be comma-separated. Each must be permitted by the client's token. |  |
| `APERIO_PATH` | `--path` | `path` | Path prefix this client serves. |  |
| `APERIO_TRIM_BIND` |  | `trim_bind` | Strip the path bind prefix before forwarding. | `1` when a path bind is set |
| `APERIO_PASS_HOSTNAME` | `--pass-hostname` | `pass_hostname` | Forward the original `Host` header instead of the target's. | `0` |
| `APERIO_PUBLIC` | `--public` | `public` | Declare the service public: the server skips its visitor password / OIDC gate for routes served exclusively by this client. Honored only when the token permits publishing public services (master always does). | `0` |
| `APERIO_VISITOR_AUTH` | `--visitor-auth` | `auth` | Gate this service behind a client-set visitor login, superseding the server's own `APERIO_SERVER_AUTH` for it (only the client's credentials work; master and dashboard passwords always do). A successful login is scoped to that hostname. Same token permission as `public`; ignored if the server sets `APERIO_IGNORE_CLIENT_AUTH`. Per `services:` entry via `auth:`. In yaml this is either the `user:password` scalar or a method block, `auth: {method: basic, users: "admin:s3cret"}` and `auth: {method: none}` (the long spelling of `public: true`); the CLI flag and the environment variable are scalars, so they always mean `basic`. See [Visitor authentication](#visitor-authentication). |  |
| `APERIO_ALLOWED_IPS` | `--allowed-ips` | `allowed_ips` | Visitor IPs/CIDRs allowed to reach this service (comma-separated on the CLI/env, a list in yaml; e.g. `203.0.113.7,10.0.0.0/8`). Enforced by the server **per candidate, before dispatch**, blocked traffic never enters the tunnel. When several clients serve one route, each candidate is filtered by its *own* list and the request goes to any passing candidate (**union semantics**, deliberately fail-open: one unrestricted client joining a route opens it; use the token-level `allowed_ips` for route-wide lockdown). A visitor every candidate rejects gets the `denied:` redirect of the most-primary declaring candidate, or, without one, a **stealth answer identical to an unclaimed route** (504), so the route's existence never leaks to blocked IPs. Purely restrictive, no token permission needed. Per `services:` entry via `allowed_ips:`. | everyone |
| `APERIO_DENIED` |  | `denied` | Absolute http(s) URL a fully rejected visitor (see `allowed_ips`) is redirected to (302). Declared via the tunnel handshake; unset = the stealth answer. Per `services:` entry via `denied:`. | stealth |
| `APERIO_PRIORITY` | `--priority` | `priority` | Load-balancing priority tier announced to the server (0 = primary, higher = standby; effective with `APERIO_LB_STRATEGY=primary-standby`). | `0` |
| `APERIO_BANDWIDTH` |  | `bandwidth` | Link capacity of this client's network, e.g. `8mbit`, `500kbit`, `2MB`, or plain bytes/second. The server paces outgoing tunnel frames (token bucket, 1 s burst) so this client is never pushed faster than its network can drain. It is a **budget for the whole client process**, not a per-service default: it is divided across the `services:` entries and each entry's parallel `connections`, so the total never exceeds it. See [Sharing the bandwidth budget](#sharing-the-bandwidth-budget). | unlimited |
| `APERIO_MAX_CONCURRENT` | `--max-concurrent` | `max_concurrent` | Max concurrent requests; announced to the server, which queues the excess instead of flooding the backend. Also enforced locally. | unlimited |
| `APERIO_CONNECTIONS` |  | `connections` | Parallel tunnel connections per service; the server load-balances across them like separate clients. The ceiling is the server's `max_connections_per_service` (default 16), announced on connect and lowerable per token. More is not automatically faster, each connection costs CPU on both ends; see the note under [Multiple services](#multiple-services). | `1` |
| `APERIO_OTEL_BRIDGE_LISTEN` |  | `otel_bridge.listen` | Address for the client's OTLP/HTTP receiver. Anything on the edge host exports to it with one environment variable and no SDK change. | `127.0.0.1:4318` |
| `APERIO_OTEL_BRIDGE_LISTEN_GRPC` |  | `otel_bridge.listen_grpc` | Address for the OTLP/gRPC receiver, for an SDK pinned to that transport. Unset = no gRPC listener. |  |
| `APERIO_OTEL_BRIDGE_TRANSPORT` |  | `otel_bridge.transport` | How exports reach the server: `tunnel` sends them on the WebSocket the client already holds, which is what preserves the "one outbound connection" property; `https` posts them to the server's endpoint instead, for telemetry bursty enough that it should stay off the tunnel. | `tunnel` |
| `APERIO_OTEL_BRIDGE_QUEUE` |  | `otel_bridge.queue` | Exports held when the far end is not keeping up. Past this the newest is dropped and counted, never waited on: an exporter that cannot hand off its batch blocks the application it is instrumenting. | `256` |
| `APERIO_STARTUP_DELAY` |  | `startup_delay` | Seconds a service waits before opening its tunnel, for a backend that starts alongside the client and is not ready the moment the process is. Only the first connection of a parallel pool waits; the rest are the same service, and making each wait would turn a stagger into a per-connection delay. | `0` |
| `APERIO_PID_FILE` |  | `pid_file` | Path to write the process id to at startup, removed on a clean exit (not after a crash: a stale pid file would have an init system signalling whatever process now holds that number). A pid file that cannot be written is a warning, not a refusal to start. |  |
| `APERIO_ADAPTIVE_CONCURRENCY` |  | `adaptive_concurrency` | Move the announced `max_concurrent` with backend pressure: halve it while requests queue waiting for a local permit, climb back one at a time when they stop. The server already queues rather than dispatching past the announced number, so a client that has become slow simply stops being sent work it cannot do; the server then holds the request, picks another client in the pool, or asks for capacity through autoscaling, all of which beat a refusal. Needs `max_concurrent` set, since that is the number being moved, and never leaves the band `1..=max_concurrent`. See [Client Resilience](client-resilience.md). | `false` |
| `APERIO_CONNECT_TIMEOUT` |  | `connect_timeout` | Seconds to wait for the TCP connection to a backend, separate from `timeout`, which covers the whole request. Per service: a backend across a VPN needs longer than one on loopback, and one number for both means either slow failure detection everywhere or spurious failures for the far one. Unset = only the whole-request budget applies. |  |
| `APERIO_MIN_TLS_VERSION` |  | `min_tls_version` | Lowest TLS version accepted from an `https://` backend, `1.2` or `1.3`. Per service so a fleet with one legacy backend does not have to lower the floor for all of them. A value that is not a TLS version is refused at startup rather than ignored: a typo that quietly leaves the floor where it was is what makes a security setting worse than none. Unset = rustls's own floor. |  |
| `APERIO_METRICS_LABELS` |  | `metrics_labels` | Static Prometheus labels attached to this client's own `aperio_client_requests_total` series, so one Prometheus can serve several environments without relabelling rules. YAML takes a mapping (`{env: prod, region: eu-west}`); the environment spelling is a flat `env=prod,region=eu-west` list, because that is what a container platform can inject. The server validates and caps what arrives, at most 8 labels, names must be `[a-zA-Z_][a-zA-Z0-9_]*`, values at most 64 characters, and the names the server writes itself (`client_id`, `job`, `instance`, `token`, `hostname`, `limit`) are refused. An invalid label is dropped and the rest are kept. |  |
| `APERIO_CONNECTIONS_MIN` |  | `connections.min` | Floor of an elastic pool: `connections: {min: 1, max: 8}` opens one connection and grows towards eight while requests queue up. Writing either half turns the setting into a range; a plain number stays a fixed pool. See [Elastic connections](#elastic-connections). | `1` |
| `APERIO_CONNECTIONS_MAX` |  | `connections.max` | Ceiling of an elastic pool. The server's `max_connections_per_service` still wins over it. | `min` |
| `APERIO_CAPTURE` | `--no-capture` | `capture` | Record this service's transactions for the dashboard's request inspector. On by default; `capture: false` (or `--no-capture`) trades the ability to inspect and replay this service's requests for the per-request cost of recording them. Live traffic, statistics and the access log are unaffected. | `1` (on) |
| `APERIO_CACHE` |  | `cache` | Opt this service into the server-side GET response cache (needs `APERIO_CACHE=1` on the **server**; strictly `Cache-Control`-driven). Per `services:` entry via `cache:`. | `0` |
| `APERIO_RESILIENCE` | `--resilience` | `resilience` | Keep serving this service's cached responses while no healthy client is connected, instead of a 504: fresh-or-expired entries answer visitors (marked `x-aperio-stale: true` once past their lifetime, always with an `Age` header) up to the server's `APERIO_CACHE_MAX_STALE` window. Needs `cache: true` and the server cache. The moment a client reconnects, normal proxying takes over. Per `services:` entry via `resilience:`. | `0` |
| `APERIO_WEBHOOK_INBOX` |  | `webhook_inbox` | Persist every inbound **POST** to this service into the server's webhook inbox (dashboard *Webhook Inbox* page): browse the payloads and re-fire any event to the connected client. Restart-surviving, newest 500 entries kept. Per `services:` entry via `webhook_inbox:`. | `0` |
| `APERIO_SUBSCRIBE` |  | `subscribe` | Comma-separated topic filters this client subscribes to (the yaml form also accepts an object per entry, adding `run:`/`timeout:`/`max_concurrent:` to run a command when a message arrives; `run:` is file-only), for messages from the other clients of its organization. MQTT filter syntax: `+` is one level, `#` is the rest. `$aperio/...` carries the server's own events (`$aperio/client/connected`, `$aperio/token/created`, …) and is never matched by a bare `#`, nor granted by one: the token's `topics` has to name it. Process-wide, not per service: a client running several services receives one copy, not one per service. |  |
| `APERIO_MESSAGES_LISTEN` |  | `messages_listen` | Local address the message face listens on, so an application on this machine can subscribe and publish without speaking the tunnel protocol: `GET /subscribe?topic=<filter>` streams server-sent events, `POST /publish?topic=<topic>` sends the body. Loopback is the sensible value; anything else is warned about, since whoever can reach it can publish as this client. Unset = no local listener. | |
| `APERIO_MESSAGES_MQTT_LISTEN` |  | `messages_mqtt_listen` | Local address an MQTT listener answers on, for an application that would rather use the MQTT client library it already has. MQTT 3.1.1, QoS 0 (higher is granted as 0), no retained messages, no persistent sessions; the protocol never leaves the machine. Unset = no MQTT listener. | |
| `APERIO_VERSION` | `--` | `version` | The Aperio version this configuration was written for, e.g. `0.5.0`. On startup the client compares it against its own build and reports every recorded change to the configuration format that landed in between, naming the affected keys; a change marked security-relevant refuses the start instead. Unset disables the check. |  |
| `APERIO_IDLE_TIMEOUT` | `--` | `idle_timeout` | Retire this client after it has served nothing for this long (e.g. `5m`); unset = never. The scale-in half of `scaling:`, see [Autoscaling](autoscaling.md). The shutdown is graceful, and the clock only starts after the first request. Every kind of traffic counts, not just buffered HTTP requests: each relayed frame of a WebSocket, TCP or UDP session re-stamps the clock in both directions, and retirement is held back while any request is still in flight (a slow backend, or a response streaming for longer than the window), so a live session is never cut mid-traffic. |  |
| `APERIO_SCALING_URL` |  | `scaling.url` | Endpoint the server POSTs to when a service of this client needs capacity. HTTPS and public addresses only, unless the operator opened those (see [Autoscaling](autoscaling.md)). Without it there is nothing to call, so the declaration is off. |  |
| `APERIO_SCALING_SECRET` |  | `scaling.secret` | Sent as `Authorization: Bearer` on that call. Write-only: never echoed back, never logged. |  |
| `APERIO_SCALING_MIN` |  | `scaling.min` | Instances that should always be running. `0` opts into scale-to-zero: a request for an unserved hostname cold-starts instead of answering 504. | `0` |
| `APERIO_SCALING_MAX` |  | `scaling.max` | Ceiling the server will never ask to exceed. `0` means cold starts only, no scale-out. | `0` |
| `APERIO_SCALING_COLD_START` |  | `scaling.cold_start` | How long a visitor request may be held while a cold start completes, e.g. `45s`. `0` answers immediately instead of holding. | `45s` |
| `APERIO_SCALING_TARGET_UTILIZATION` |  | `scaling.target_utilization` | Pool utilization above which the server asks for one more instance, between 0 and 1. | `0.8` |
| `APERIO_SCALING_WINDOW` |  | `scaling.window` | How long utilization must stay above the target before scaling out. Guards against reacting to a single spike. | `15s` |
| `APERIO_SCALING_COOLDOWN` |  | `scaling.cooldown` | Minimum gap between two calls for one bind. A new instance needs time to appear; without this the server would ask again while it is still starting. | `60s` |
| `APERIO_CLIENT_ID` | `--client-id` | `client_id` | Persistent client instance id (a UUID). Keeps the id stable across restarts, useful for failover `wait` mode and `--bind-tunnels`. | random UUID per run |
| `APERIO_DEVICE_KEY` |  | `device_key` | Explicit device key announced for trust-on-first-use token pinning (server `APERIO_TOKEN_PINNING`): pins the token to this device so a leaked token replayed elsewhere is rejected. | none announced |
| `APERIO_DEVICE_KEY_FILE` |  | `device_key_file` | Path holding the device key, its contents are used, or a fresh random key is generated and persisted there (owner-only `0600`) on first run. Ignored when `APERIO_DEVICE_KEY` is set. |  |
| `APERIO_CUSTOM_NAME` |  | `custom_name` | What to call this service on screen (dashboard, client logs). Free text: any language, any punctuation, spaces, changeable at will, nothing addresses it. `name:` is the handle that does, and it is an identifier (`a-z`, `0-9`, `_`). Per `services:` entry via `custom_name:`, and available on a `tunnels:` entry too. |  |
| `APERIO_TARGET_HEALTH` |  | `health.endpoint` | Health endpoint of the local target (path like `/health`, or a full URL). When set, the client probes it independently and reports the result to the server: a failing backend takes the client **out of routing without dropping the tunnel**; it rejoins automatically when the probe recovers. The dashboard shows a `BACKEND DOWN` badge meanwhile. **The service starts *unhealthy* (out of routing) until the first probe succeeds**, the client never claims a backend is up before it has checked it, and the first probe runs immediately at startup, so a healthy backend becomes routable within a probe (not a probe interval). Before that first probe completes the dashboard shows a **CHECKING** badge (rather than *BACKEND DOWN*), so "not probed yet" is distinguishable from "probed and down". Probes never follow redirects. Against an `h2c://`/`h2://` target the value names the **gRPC service** to health-check instead, and the probe calls `grpc.health.v1.Health/Check`: a plain GET cannot reach a server that speaks HTTP/2 with prior knowledge and routes by method name. `/` asks about the server as a whole. An absolute `http(s)://` URL still means an ordinary HTTP probe. |  |
| `APERIO_WAIT_FOR_BACKEND` |  | `health.wait_for_backend` | Startup gate: hold the service **out of routing until the backend first accepts a connection**, avoiding the connection-refused window while a slow dev server boots. Connection-level only (a probe per second); once the backend is up the gate never re-engages. Superseded by `target_health`, which gates startup *and* tracks health continuously. Per `services:` entry via `wait_for_backend:`. | `0` |
| `APERIO_HEALTH_INTERVAL` |  | `health.interval` | Seconds between backend health probes. | `10` |
| `APERIO_HEALTH_TIMEOUT` |  | `health.timeout` | Per-probe timeout (seconds). | `5` |
| `APERIO_HEALTH_THRESHOLD` |  | `health.threshold` | Consecutive probe failures before the backend is reported unhealthy. | `2` |
| `APERIO_TIMEOUT` |  | `timeout` | Per-request backend timeout (seconds). | `30` |
| `APERIO_RESPONSE_TIMEOUT` |  | `response_timeout` | Per-service override (seconds) of the server's gateway response timeout for requests dispatched to this service. Unset = the server's global `APERIO_GATEWAY_RESPONSE_TIMEOUT`. Per `services:` entry via `response_timeout:`. | server global |
| `APERIO_MAX_REDIRECTS` |  | `max_redirects` | Backend redirects followed transparently: same-host scheme upgrades (`http://x` → `https://x`) and hops within the same root domain (`example.com` → `test.example.com`), never downgrading https to http. Redirects beyond this many jumps, or to unrelated hosts, pass through to the visitor unchanged. `0` disables following entirely. | `5` |
| `APERIO_RELOAD_DRAIN` |  | `reload_drain` | Seconds a configuration reload lets in-flight requests finish before the affected tunnel connections are dropped. The client announces `Draining` first, so the server stops dispatching to it and the wait actually terminates. `0` drops at once, which is what happened before this setting existed. | `10` |
| `APERIO_RETRY_ATTEMPTS` |  | `retry.attempts` | Total attempts for a backend request that fails **before any response arrives** (a refused connection, a reset, a timeout on the head). `1` = no retrying, which does not cover a connection the backend had already closed: see the note under this table. Only idempotent methods are retried unless `retry.all_methods` says otherwise, and only requests whose body can be replayed: a streamed upload is consumed by the first attempt, so it is never retried. Once a response head has arrived the request is past this point, so an error later in the body is not retried either. | `1` (off) |
| `APERIO_RETRY_BACKOFF` |  | `retry.backoff` | Milliseconds before the second attempt, doubled before each further one. | `100` |
| `APERIO_RETRY_ALL_METHODS` |  | `retry.all_methods` | `1` = also retry non-idempotent methods (POST, PATCH). Off by default because a retried write may reach the backend twice, the same trade as the server's `failover.all_methods`. | `0` |
| `APERIO_BREAKER_FAILURES` |  | `circuit_breaker.failures` | Consecutive backend failures that open the circuit breaker. While open the backend is **not dialed at all** and the visitor gets `502` immediately, so a dead backend stops being hammered once per request and nobody waits out a connect that will not succeed. `0` disables it. A response head of any status counts as a success: a `500` is a backend that is up, and refusing to dial it would turn an application error into an outage. | `0` (off) |
| `APERIO_BREAKER_OPEN_FOR` |  | `circuit_breaker.open_for` | Seconds the breaker stays open. After that one request is let through to probe the backend; if it fails, a fresh window starts. | `30` |
| `APERIO_MAX_RESPONSE_BODY` |  | `max_response_body` | Max backend response size in bytes; bodies over 32 KB are streamed through the tunnel as binary chunks rather than base64 in one message (256 KB against a server too old for binary frames), larger than this limit are truncated. | 50 MB |
| `APERIO_MAX_REQUEST_BODY` |  | `max_request_body` | Max request body size in bytes visitors may upload to this service. Announced to the server, which rejects bigger uploads with an early **413** before the body ever enters the tunnel. Can only tighten the server's global `APERIO_MAX_BODY_SIZE`, never widen it. Per `services:` entry via `max_request_body:`. | server limit only |
| `APERIO_MAX_MESSAGE_SIZE` |  | `max_message_size` | Max size of one tunnel message accepted from the server (memory protection). | 32 MB |
| `LOG_LEVEL` |  |, | Log verbosity. | `info` |
| `APERIO_LOG_FORMAT` |  |, | `json` or `pretty`. By default the client auto-detects: human-readable logs on an interactive terminal, JSON when stdout is not a TTY (Docker, pipes, service managers). | auto |

Yaml-only sections: `services:` (multiple exposed targets, below), `tunnels:` and `bind-tunnels:` (see [Tunnels](emergency-tunnels.md)).

### aperio.yaml & ~/.aperio.yaml

```yaml
# ~/.aperio.yaml, user-level defaults shared across projects
server:
  url: https://tunnel.example.com
  token: apr_xxxxxxxxxxxxxxxx
```

```yaml
# ./aperio.yaml, per-project service description
services:
  - target: http://localhost:3000
    # optional
    hostname: app.example.com
    path: /api
    trim_bind: true
    pass_hostname: false
    max_concurrent: 8
    target_health: /health   # probe the backend; report unhealthy without dropping the tunnel

# top-level keys are the defaults every entry falls back to
priority: 0                  # 0 = primary, higher = standby tier
health_interval: 10
```

**A config file describes services under `services:`, even when there is one.** Naming a single backend at the top level (`target:`, `serve:`, `hostname:`, `path:`, `tcp_target:`, and the probe path `target_health:` / `health.endpoint`) was deprecated from 0.6.0 and removed in 0.9.0: those keys only ever did anything in a file *without* a `services:` list, so a reader had to know which of two shapes they were looking at before they could read anything else. The rest of the `health:` block stays where it is: `interval`, `timeout`, `threshold` and `wait_for_backend` are genuine defaults every entry inherits, while a probe path belongs to the backend it probes. **A file has not accepted them since 0.9.0**: a client that finds one refuses to start and names the keys, rather than running while the file says something it is not doing. Migrating is mechanical, indent them under one `services:` entry. Single-service mode is unchanged where it belongs, on the command line and in the environment.

Single-service mode itself is not going away, it moves to where a one-liner belongs, the command line and the environment:

```bash
# the positional target is single-service mode
aperio-client http://localhost:3000 \
  --server-url https://tunnel.example.com --server-token apr_xxxxxxxxxxxxxxxx \
  --hostname app.example.com --path /api
```

```bash
# the same thing, for a container or a unit file
APERIO_SERVER_URL=https://tunnel.example.com
APERIO_SERVER_TOKEN=apr_xxxxxxxxxxxxxxxx
APERIO_TARGET=http://localhost:3000
APERIO_HOSTNAME=app.example.com
APERIO_PATH=/api
```

The legacy flat form (`server: https://...` plus top-level `token:`) is still accepted. The local file is hot-reloaded: edits are applied within ~5 s via a graceful reconnect.

### Splitting the config across files (`include:`)

One `aperio.yaml` stops scaling when there are twenty services, or when different teams own different entries. `include:` reads other files first:

```yaml
# aperio.yaml
include:
  - shared/health.yaml     # relative to *this* file, not the working directory
  - services/prod.yaml
timeout: 30                # this file wins over anything an include set
services:
  - name: web              # appended after the included services
    target: http://localhost:3000
```

The rule is one sentence: **an included file's keys are used unless the including file sets them, and sequences of mappings concatenate.** That second half is what makes it useful, `services:`, `subscribe:` and `expose:` are collections a file adds to, while `allowed_ips:` and the rest are values it sets. Includes are merged in the order written, so a later one wins over an earlier one, and the including file wins over all of them.

Paths resolve relative to the file that wrote them, so a fragment means the same thing whichever directory the client is started from. Chains may nest five deep; a cycle, a missing file, or a malformed `include:` is an error naming the file rather than a silently partial configuration. Hot-reload watches every file that contributed, not just the root, and re-reads the set on each change so adding or removing an include is picked up too.

### Multiple services

One client process can expose several targets: add an entry per backend to `services:`, and the client opens one tunnel connection per entry, each with its own binds, health probe, and knobs:

```yaml
# ./aperio.yaml (client)
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

Per-entry fields: `name`, `target` (required, or `serve` in its place: a local directory of static files served as this service, mutually exclusive with `target`; one loopback file server runs per distinct directory), `hostname`, `path`, `trim_bind`, `pass_hostname`, `max_concurrent`, `connections`, `priority`, `bandwidth`, `timeout`, `max_response_body`, `max_request_body`, `max_redirects`, `target_health`, `wait_for_backend`, `health_interval`, `health_timeout`, `health_threshold`, `public`, `auth`, `allowed_ips`, `denied`, `headers`, `security_headers`, `retry`, `circuit_breaker`. Unset tuning knobs fall back to the top-level values; binds are strictly per entry. The one exception is `bandwidth`, where the top-level value is a budget shared by the entries rather than a default copied into each of them.

**Backend resilience (`retry:` and `circuit_breaker:`).** The server can fail a request over to another *client* and eject one that keeps misbehaving; neither helps the client whose own backend is refusing connections. These two blocks cover that hop, per service or at the top level as the default for every entry. They apply to every backend scheme: plain HTTP, `h2c://` / `h2://`, and `unix://`.

```yaml
services:
  - name: api
    target: http://localhost:3000
    retry:
      attempts: 3        # total, including the first; 1 = off (the default)
      backoff: 100       # ms before the second attempt, doubled after that
      all_methods: false # POST/PATCH are not retried unless this is true
    circuit_breaker:
      failures: 5        # consecutive failures that open it; 0 = off (default)
      open_for: 30       # seconds before one request probes the backend again
```

Retrying only covers failures that happen *before* a response head arrives, and only requests that can be replayed: a streamed upload is consumed by its first attempt. The breaker counts any response head as a success regardless of status, because a `500` is a backend that is up and answering.

**One case is handled whatever `retry:` says.** The client keeps backend connections alive and reuses them, and the backend closes idle ones on its own schedule, so there is a window where a request is written onto a socket the backend has just finished with. Nothing is wrong with the backend, and no response head arrives; under load that window is hit constantly. The client re-dials once, silently, for a request the same fences already allow to be replayed (an idempotent method, a replayable body). This is not the retry policy, which is about a backend that failed, and it is why the same backend behind any mainstream proxy does not answer with a scattering of `502`s under load. A second such failure on the same request is a backend genuinely closing on the client, and it reaches the visitor.

`connections: N` (default 1, also valid at the top level or as `APERIO_CONNECTIONS`) opens N parallel tunnel connections for a service. How many N may be is the server's decision, `max_connections_per_service` (default 16), lowerable per token and announced on connect: a client asking for more opens what it is allowed and says so in its log. The server pools them like separate clients, its load-balancing strategy spreads requests across them, so a single service is no longer serialized behind one WebSocket under heavy parallel traffic. Each connection gets its own instance id (`<id>`, `<id>-c2`, `<id>-c3`, …), so the dashboard's shared-id warning is not triggered and failover/`--bind-tunnels` lookups stay unambiguous; `max_concurrent` applies per connection. The `name` shows up in client logs and as a badge in the dashboard's clients table. The `services:` list is read from the local config file only; a positional CLI target overrides it entirely (single-service mode). Config hot-reload re-resolves the whole list, so adding or removing services doesn't need a restart.

More connections is not automatically faster: each one costs a reader and a writer task on both ends, and its own sockets and queues. Raising N pays off when the bottleneck is the network, a long round-trip or a single TCP stream's throughput ceiling between client and server. When client and server share a machine's CPU (loopback, a dev box, co-located containers), small values win; in a loopback bulk-throughput measurement the curve peaked at 4 connections and fell past it, with 10 slower than 1. Start small, and let a measurement on your own deployment justify each increase.

### Template variables

`${NAME}` and `${NAME:-default}` in a client config file are expanded from the environment before the yaml is parsed, so one file can serve several environments instead of being copied per environment, which is how two files drift:

```yaml
server:
  token: ${APERIO_TOKEN}
services:
  - target: http://localhost:3000
    hostname: ${ENV:-dev}.example.com
```

Only this spelling is expanded. A bare `$NAME` is left exactly as written, because `$` appears in generated passwords, regular expressions and the shell snippets inside `run:` commands, and a config loader that rewrote those would corrupt files that work today. A variable that is not set and has no default is an error naming the variable, not an empty string: substituting nothing produces a file that still parses and means something else (`hostname: .example.com`, or an empty token), which then fails somewhere unrelated. Expansion happens per file, so an included fragment reports its own name.

### Typos and literal secrets

A key nothing recognizes has always been ignored silently, which is the most expensive kind of typo: the file says the setting is configured and the behavior says it is not. Unknown keys now produce a warning naming the key they were probably meant to be (`` `hostnme` is not a setting; did you mean `hostname`? ``), at the top level and inside `services:` entries, against the same generated schema editors complete from. It stays a warning rather than an error, so a file carrying keys for a newer client than the one running still starts.

`aperio-client check` also warns when a credential (`token`, `psk`, `client_secret`, `password`, `api_key`, `device_key`) is written into the file literally rather than as `${VAR}`. A warning, not a failure: it is a working configuration, and where the file is a private deploy artifact it may be the deliberate one. But a secret typed into a file ends up in a repository, a backup and a support ticket, and the alternative costs one `${VAR}`.

### Service start order

A `services:` entry can name others it should follow:

```yaml
services:
  - name: db
    tcp_target: 127.0.0.1:5432
  - name: api
    target: http://localhost:3000
    depends_on: [db]
    startup_delay: 2
```

`depends_on` waits for those services to have a live tunnel, then **proceeds anyway** after 60 seconds: a dependency that never arrives must not keep a service that could be serving traffic off the air forever. The wait is bounded and what it gave up on is logged.

"Has a live tunnel" means right now, not "did at some point": a service that loses its connection stops counting as ready, and one with `connections: N` counts as ready while any of its connections is up. It orders startup and nothing more, though. Once a service is past its own gate it stays up whatever its dependency does afterwards, because taking a healthy service off the air over someone else's outage turns one failure into two.

A name that is not a service in this configuration, a service depending on itself, and a cycle are all refused at startup. All three would otherwise be invisible: at runtime every one of them ends with everybody waiting out the grace period and then starting anyway, which looks exactly like working.

### Elastic connections

A plain `connections: N` opens N connections at startup and keeps them, whether or not there is traffic. Written as a range, the pool sizes itself:

```yaml
services:
  - name: api
    target: http://localhost:3000
    connections: { min: 1, max: 8 }
```

One connection is open while the service is quiet, so the URL works the moment the client starts, and the pool grows towards `max` while requests pile up. Growth is driven by requests **in flight**, not by a request rate: a thousand requests a second that each answer in a millisecond need one connection, while ten slow uploads need room to run in parallel, and in flight is the quantity that tells those apart.

The exact rule, so the behavior is predictable rather than a black box. Every **2 seconds** the pool takes the peak number of requests in flight since the last look, and compares it against the connections it has open:

| | Condition | Cooldown |
|---|---|---|
| **Grow by one** | peak ≥ 8 × open connections | 10 s |
| **Shrink by one** | peak ≤ 2 × (open − 1) | 120 s |

Eight per connection is not a capacity limit, a tunnel connection multiplexes: it is the point at which a connection's frames start queueing behind each other instead of going out as they arrive. The two thresholds are far apart on purpose, and that gap is the hysteresis: a service sitting between them would otherwise open and close a connection every few seconds, which costs both ends more than the connection ever saved. The cooldowns are asymmetric for the same reason the thresholds are: the pool opens one connection at a time and waits ten seconds to see whether it helped, and waits two minutes of calm before giving one back, because being one connection short costs latency on live traffic while being one over costs a little memory. `min` connections are never retired.

The client logs each decision with the numbers behind it, and the dashboard's connection config view shows the range beside the size the pool is running right now (`connections: { min: 1, max: 5 }  # 4 open right now`), so the count in the clients table having grown past what the file's `min` says is legible rather than a discrepancy.

Environment: `APERIO_CONNECTIONS_MIN` / `APERIO_CONNECTIONS_MAX`. The server's `max_connections_per_service` still wins over `max`, and the `bandwidth:` share is divided by `max` (never by the current size), so growing the pool cannot exceed the budget the file declares. The dashboard shows the connections a service actually has open, not its ceiling.

A range is opt-in for a reason: as the note above says, more connections is not automatically faster, and on a host where client and server share CPU the curve has a peak. `connections: N` keeps behaving exactly as it always did.

### Sharing the bandwidth budget

The server shapes each tunnel connection with a token bucket of its own, so a client that announced the same rate on every connection could be pushed at N times that rate. The client therefore divides what it announces instead of repeating it: a service's share is split across its `connections`, and the top-level `bandwidth` is a budget the services are settled against.

- **A service's own limit is divided by its connections.** `connections: 10` with `bandwidth: 10mbit` announces 1mbit per connection.
- **Services that named a rate keep it; the rest split what is left equally.** With a 10mbit budget, `x: 3mbit` and an unset `y` gives y 7mbit; add an unset `z` and y and z take 3.5mbit each.
- **With no top-level budget nothing is settled.** A service that named a rate gets it, a service that did not stays unlimited.
- **Requests that starve the others are dropped, with a warning.** If the named rates leave nothing of the budget for the services without one (2mbit of a 2mbit budget, or more), every named rate is ignored and the budget is split equally, since a service configured to run cannot be given zero.
- **Requests that overshoot are scaled, with a warning.** When every service named a rate and together they exceed the budget, the rates keep their relative weight and are scaled to fit: `3mbit` and `7mbit` under a 5mbit budget become 1.5mbit and 3.5mbit.

The budget is split per **service**, not per connection, so a service does not get a bigger share by opening more connections. Each service logs the rate a single connection of it announces at startup. Note the trade-off of per-connection shaping: with `connections: N` a single transfer runs over one connection and therefore sees `1/N` of the service's share, even while its siblings are idle.

### Header rules

A `headers:` section (top-level, or per `services:` entry, the entry replaces the top-level section entirely when set) edits proxied HTTP traffic on the client: `request` rules apply to what the local backend receives, `response` rules to what the visitor receives. `add` sets a header, replacing any existing value of the same name; `remove` strips headers case-insensitively:

```yaml
# ./aperio.yaml (client)
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

### Security header preset (`security_headers:`)

A `security_headers:` key (top-level, or per `services:` entry, the entry replaces the top-level value when set) injects standard security response headers without hand-writing `headers:` rules. `security_headers: true` enables the standard set, `Strict-Transport-Security: max-age=63072000`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, and `false` opts a single service out of a top-level preset. A mapping picks headers individually:

```yaml
# ./aperio.yaml (client)
security_headers:
  hsts: true                  # Strict-Transport-Security (only meaningful behind HTTPS)
  hsts_max_age: 31536000      # optional, default 63072000 (2 years)
  frame_options: SAMEORIGIN   # X-Frame-Options: SAMEORIGIN or DENY (anything
                              # else is sent as DENY, a browser acts on those
                              # two and ignores a typo, which would leave a
                              # header that only looks like protection)
  nosniff: true               # X-Content-Type-Options: nosniff
  referrer_policy: strict-origin-when-cross-origin
  csp: "default-src 'self'"   # Content-Security-Policy (no default, app-specific)
```

Preset headers replace whatever the backend sent for the same name, but explicit `headers:` rules always win: a name you `add` or `remove` yourself is left to your rules. Config file only; hot-reload applies changes within ~5 s.

The preset has environment spellings too, for a client with no file: `APERIO_SECURITY_HEADERS` (the whole preset on or off) and the granular `APERIO_SECURITY_HEADERS_HSTS`, `APERIO_SECURITY_HEADERS_HSTS_MAX_AGE`, `APERIO_SECURITY_HEADERS_FRAME_OPTIONS`, `APERIO_SECURITY_HEADERS_NOSNIFF`, `APERIO_SECURITY_HEADERS_REFERRER_POLICY` and `APERIO_SECURITY_HEADERS_CSP`, each read on its own so one of them can be overridden without restating the rest.

### Visitor authentication

`auth:` is the same grammar on both sides of the tunnel: on the server it is the default gate for every route, on a client's service it overrides that gate for the traffic routed there. Three spellings of one thing.

```yaml
auth: "admin:s3cret"                              # the scalar, unchanged
auth: { method: basic, users: "admin:s3cret" }    # one method
auth:                                             # several: any one admits
  - method: basic
    users: [admin:s3cret, ops:hunter2]
```

The list is **any-of**: a visitor is admitted by the first method that admits them, which is what lets one route say "a browser signs in, a script presents a key". Five methods exist, simplest first:

| `method:` | What it does | Where it may be written |
| --- | --- | --- |
| `none` | Deliberately open. The long spelling of `public: true`, and it needs the same token permission. | both |
| `basic` | A `user:password` login. `users:` takes one credential or a list, all alternatives on the same gate. | both |
| `bearer` | An opaque secret presented as `Authorization: Bearer <secret>`. `secret:` takes one or a list, so a key rotates by adding the new one before withdrawing the old. | both |
| `jwt` | A token the visitor already holds, verified against the keys its issuer publishes (`jwks_url:`) or a shared secret (`hmac_secret:`). No round trip per request. | both |
| `forward` | Ask an endpoint you run about each request: `2xx` admits it, anything else refuses it and the endpoint's own answer is what the visitor gets. | server only |

The set is closed on purpose rather than being a plugin interface, and `forward` is what lets it stay closed: anything deliberately left out (LDAP, SAML, a rule nobody anticipated) is thirty lines behind that URL, in a process that is not Aperio's, with a contract that is two HTTP messages rather than an ABI. A `method:` this build does not know **refuses the start** and names the ones it does, so a gate is never silently absent. `oidc` as a visitor method is still its own piece of work.

#### How a refusal is shaped

A browser navigation (a `GET` carrying `Accept: text/html`) is sent to the login page. Anything else, where the gate has a method a caller can satisfy on the request itself (`bearer` or `jwt`), is answered `401` with `WWW-Authenticate: Bearer`. Redirecting a script to an HTML login form answers a question it did not ask, and it is why a gated route could not be reached with `curl` at all. A `forward` refusal is the endpoint's own answer, whatever it chose.

So one route serves a person and a script each in the form they can act on:

```yaml
auth:
  - method: basic                  # a person, in a browser
    users: "admin:s3cret"
  - method: bearer                 # a script, with no browser
    secret: ${API_SECRET}
```

#### The `bearer` method

An opaque secret, compared verbatim. `secret:` takes one value or a list, so a key is rotated by adding the new one and withdrawing the old afterwards.

`query: true` additionally accepts the secret as `?aperio_token=<secret>`, for callers that cannot set a header: an `<img src>`, a link in an email, a sender with a fixed request shape. **Off by default**, because a query string reaches the `Referer` header, browser history and every proxy in front, and it is per method, so a gate that never asked for it cannot be opened that way.

Aperio keeps its own record clean: the access log has only ever stored the path, the inspector masks the value beside `aperio_share`, the parameter is stripped before the request reaches the backend, and a *page load* carrying one is answered with a redirect to the clean address plus a short-lived cookie, so the secret is not repeated on every asset of that page. That last move is what a share link already does on its first click, and it reuses the same cookie.

The secret itself does not travel: an `Authorization` header that opened Aperio's gate is stripped before the request is forwarded, on the same rule that already strips the `aperio_session` and `aperio_share` cookies while leaving every other cookie alone. A credential addressed to the gate is not addressed to what is behind it. An `Authorization` header that did *not* open the gate is the visitor's own and passes through untouched.

A `bearer` secret is compared verbatim rather than hashed, unlike a `basic` password: it is a high-entropy value the operator generated, so there is no dictionary to defend against and no user half to slow a guess down. That is also why one shorter than 16 characters is refused where it is written.

#### The `forward` method

```yaml
auth:
  - method: forward
    url: http://127.0.0.1:7070/_authcheck
    request_headers: [cookie, authorization]   # the default
    response_headers: [x-auth-user]            # empty by default
    timeout: 5                                 # seconds; a timeout refuses
    cache: 30                                  # seconds; 0 (default) asks every time
```

What nginx spells `auth_request` and Traefik spells ForwardAuth. Five decisions, each deliberate:

**What crosses, to the endpoint.** A `GET` describing the request rather than replaying it: `X-Forwarded-Method`, `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Uri`, `X-Forwarded-For`, plus the request headers you name. The default is `cookie` and `authorization`, the two that carry an identity, rather than everything the visitor sent: handing the endpoint the whole request makes every header it happens to read part of a contract nobody wrote down.

**What crosses back.** Only the response headers you name, and the list is empty by default. It is how the pattern delivers an identity onto the request that reaches your backend, and an open list is how it becomes a header injection.

A visitor's own copy of a name on that list is **dropped**, always, whether or not the endpoint answered with one. You named those headers so your backend could trust what is in them, and a request arriving with `X-Auth-User: admin` already on it would otherwise reach the backend alongside the endpoint's answer, where most frameworks read the first of the two. This is the same rule the `x-aperio-` namespace has, and for the same reason.

**A timeout refuses.** An auth gate that opens when its check is unreachable is not a gate. This means the endpoint's availability becomes the route's, which is the trade you are making, and it is stated rather than discovered.

**A refusal is the endpoint's own answer**, relayed with its status, `Location`, `WWW-Authenticate`, `Content-Type` and `Set-Cookie`, so it can send a browser to a login of its own. Redirects from the endpoint are deliberately not followed: a `302` is an answer for the visitor, not a request for Aperio to make.

**`cache:` remembers a verdict** for an identical *request*, so a busy route does not pay a round trip per page load. Only admissions are remembered, never refusals: somebody who has just been given access must not keep being turned away for the rest of the window. The key is a hash of everything the subrequest carried, the endpoint, the hostname, the method, the path, the visitor's address and the credential headers, so no secret is held in it and a `yes` for one path, or for one address, is never a `yes` for another. Your endpoint is told the method and the path and may answer on them, which is the ordinary way this pattern is used, so a cache that keyed on the credential alone would quietly turn a per-request authorization into a per-session one.

The URL goes through the server's [outbound policy](threat-model.md), like every other destination the server is told to call.

#### The `jwt` method

```yaml
auth:
  - method: jwt
    jwks_url: https://accounts.example.com/.well-known/jwks.json
    issuer: https://accounts.example.com
    audience: aperio                 # one value or a list
    claims: { groups: engineering }  # further claims, each an exact value
    cookie: CF_Authorization         # where the token is, if not `Authorization: Bearer`
```

One `jwt` method subsumes a shelf of provider integrations: Cloudflare Access, an ALB doing OIDC, and anyone running their own auth service all hand out the same thing. That is the argument for it over anything vendor-shaped.

Two ways of knowing who signed a token, and exactly one of them per method: `jwks_url:`, the issuer's public keys, fetched and cached for an hour and re-fetched when a token names a key id that is not in the cache (which is what a key rotation looks like from here, so rotation needs no restart); or `hmac_secret:` for `HS256`, when the issuer is your own service. The key-set URL goes through the server's [outbound policy](threat-model.md) like any other destination the server is told to call, and no redirect from it is followed: the policy vets the URL in the file, and a `Location` would mean the address it vetted is not the address that gets the request.

**`iss` and `aud` are only checked when the file asks, and asking means the claim must be present.** An unset `audience:` is not "accept any audience", and a configured one refuses a token that carries none, which is exactly the token the requirement was written to keep out. `exp` is required whatever the file says: a token with no expiry is one that never stops working.

A token in the `Authorization` header is Aperio's credential and is stripped before the request is forwarded; one in a `cookie:` is the visitor's own and travels, because stripping the cookie header would take the application's session with it.

`jsonwebtoken` is pinned to its 9 line deliberately. It builds on `ring`, which rustls already puts in every Aperio binary, so the method adds no crypto implementation and no cross-compilation surface; 10 and later want a different backend, which would be a third crypto stack here. `aperio-server/Cargo.toml` carries the reason.

#### What a client may declare, and how that is agreed

A client may write `none`, `basic`, `bearer` and `jwt`. `forward` is server-only: its URL would be called by the *server*, from the server's network, so a client writing `localhost:7070` would mean the server's localhost and not its own.

Which of those actually travel is **negotiated on the handshake rather than assumed**. The server announces the methods it accepts from a client on the upgrade response, and a client whose `auth:` needs one that is missing **does not serve that service**: it says which side is too old and retries, instead of connecting under a gate the server was never told about. A server too old to send the announcement sends nothing, which reads as "only what the scalar `visitor_auth` can carry", since that is the only field such a server reads: `method: none`, or a `basic` naming a *single* `user:password`. A gate whose method is one of those two but whose shape is not, `basic` with two users, say, is refused there as well, and the message says the server is too old rather than naming the method: the method is not the problem, the shape is, and there is one credential's worth of room in the field it would have to travel in.

That is worth the machinery because of what the alternative does quietly: a server that ignored a policy it did not understand would read the client as declaring *no* gate, and the route would come up open. Only the one service stops; its siblings keep serving.

**The announcement is about the connection, not about the build.** Declaring a visitor gate needs the same token permission as `public`, so a token without it is answered with an empty list and holds the service back, rather than being told the gate was accepted and then having it dropped a message later. The server's log names the token and the reason; the client says the permission is the usual cause.

#### The scalar spelling, and what still reads it

The scalar keeps working everywhere it worked, and it is still what `APERIO_SERVER_AUTH`, `APERIO_VISITOR_AUTH`, `--visitor-auth` and the dashboard's visitor-password field carry, since each of those is a single value. A policy the scalar cannot express (several users, or `method: none`) shows as empty in that field, so `GET /aperio/api/settings` reports `visitor_auth_methods` beside it and what is in force stays readable.

#### Telling the backend who came in

The gate has always been a wall: it decided whether a request continued and told the backend nothing, so an application behind a tunnel could not greet anyone without building a second login next to Aperio's. `visitor_identity_headers` (`APERIO_VISITOR_IDENTITY_HEADERS`) turns on two headers on the forwarded request:

| Header | Value |
| --- | --- |
| `x-aperio-visitor-how` | `session`, `bearer` or `share`, how they were admitted. |
| `x-aperio-visitor-id` | The email or username behind a session. Absent for `bearer`, which identifies a caller rather than a person. |

The secret itself does not travel: an `Authorization` header that opened Aperio's gate is stripped before the request is forwarded, on the same rule that already strips the `aperio_session` and `aperio_share` cookies while leaving every other cookie alone. A credential addressed to the gate is not addressed to what is behind it. An `Authorization` header that did *not* open the gate is the visitor's own and passes through untouched.

Off by default: it is the same new trust surface as `identity_headers` and should be adopted as deliberately. A route that is open or ungated identifies nobody and sends neither header, since a value meaning "anonymous" is noise a backend has to learn to ignore. The forgery half is already done, inbound `x-aperio-*` headers are stripped from every proxied request whatever this is set to.

#### Two planes, one cookie

A session is for one of two things: administering Aperio, or viewing a site behind it. They share a store and a cookie, and what tells them apart is which credential created the session:

| Credential | Plane |
| --- | --- |
| Master token, a named dashboard user, a passkey, OIDC | **admin**: the dashboard, its API, and (fenced by organization) the visitor gate |
| `server.auth` / `APERIO_SERVER_AUTH` | **visitor**: every proxied hostname, nothing administrative |
| A client's own `auth:` | **visitor**, and only on that hostname |

The visitor password is server-wide, so a visitor who signs in on one hostname is not asked again on the next; what it never opens is Aperio itself.

#### Closed by default

`default_access` (`APERIO_DEFAULT_ACCESS`) says what a route **nobody gated** means:

| Value | Meaning |
| --- | --- |
| `deny` (default since 0.10.0) | A route is served because something said so, an `auth:` policy that admits the visitor or an explicit `method: none` / `public: true`, and not because nothing said otherwise. |
| `allow` | What the server did before 0.10.0: with no `auth:` anywhere, every route serves everyone. |

Under `deny` an undeclared route is refused with the same answer an unclaimed hostname gives, so the existence of something there does not leak to a caller who was never going to be let in. A route that *is* gated is untouched: the posture decides what an unstated route means, nothing else.

This is the setting that makes `auth: DUMMY:DUMMY` unnecessary. It was the only way to say "not public" while `public:` was an exemption from a gate that might not exist.

**The default is `deny` from 0.10.0.** Before that it was `allow`, and a client serving something nothing gated was warned once per connection naming the line to write, so the flip was not the first anyone heard of it. If this server publishes a public site, which is the commonest deployment there is, every client serving one now has to say so with `public: true` or `auth: {method: none}`; until it does, those routes answer as an unclaimed hostname does. Setting `allow` restores the previous behaviour exactly and is a supported posture, not a migration escape hatch. An unreadable value refuses the start rather than being guessed at: `off`, `false` and `no` could each mean either side of this.

### Editor autocompletion (JSON Schema)

Building the client emits JSON Schemas for both config files to `schemas/` (git-ignored build artifacts, regenerated from the parser types so they never drift): `aperio-client.schema.json` for `aperio.yaml` and `aperio-server.schema.json` for `aperio-server.yaml`. Point your editor's YAML extension at them for completion, hover docs, and validation:

```jsonc
// .vscode/settings.json (VS Code / Antigravity, with the YAML extension)
{
  "yaml.schemas": {
    "./schemas/aperio-client.schema.json": ["aperio.yaml", "**/aperio.yaml", "~/.aperio.yaml"],
    "./schemas/aperio-server.schema.json": ["aperio-server.yaml", "**/aperio-server.yaml"]
  }
}
```

Run `cargo build -p aperio-client` once to generate them (or `cargo run -p aperio-config > schemas/aperio-client.schema.json` and `cargo run -p aperio-config -- --server > schemas/aperio-server.schema.json`). The server binary also emits its own schema, `aperio-server --print-schema > aperio-server.schema.json`, so a deployment can regenerate it without the source tree. Tagged releases attach each schema twice: a versioned `aperio-{client,server}.<tag>.json` for pinning, and a stable-named `aperio-{client,server}.schema.json` so schema managers can point at a URL that always serves the latest release:

```
https://github.com/co3moz/aperio/releases/latest/download/aperio-client.schema.json
https://github.com/co3moz/aperio/releases/latest/download/aperio-server.schema.json
```

## Server

### The `aperio-server.yaml` file

`aperio-server.yaml` is the primary way to configure the server, a single, reviewable, schema-checked file (see [Editor autocompletion](#editor-autocompletion-json-schema) and `--print-schema`), with structured sections (`headers:`, `routes:`, `error_pages:`, …) that have no environment-variable equivalent. Environment variables remain fully supported as the fallback surface, convenient for container orchestration and secrets injection, and every scalar setting is expressible either way.

Put the file next to the binary (or at the path in `APERIO_SERVER_CONFIG`; the name deliberately differs from the client's `aperio.yaml` so the two are never confused). Keys follow the naming standard, the environment variable without the `APERIO_` prefix, lowercase: `max_body_size` maps to `APERIO_MAX_BODY_SIZE`, and `host`, `port`, `log_level` map to their bare names. Settings that share a prefix are written as a block (see [Grouped keys](#grouped-keys)), so `cache_max_bytes` is `cache.max_bytes` and still reaches `APERIO_CACHE_MAX_BYTES`. Booleans are written as `true`/`false`, and list-valued settings (e.g. `trusted_proxies`) may use YAML lists:

```yaml
# aperio-server.yaml
server:
  token: change-me-to-a-long-random-string
port: 8080
trust_proxy: true
trusted_proxies:
  - 10.0.0.0/8
  - 173.245.48.0/20
lb_strategy: primary-standby
cache: true
```

The file is read once at startup and takes precedence over environment variables and over dashboard overrides for the keys it writes. It is not hot-reloaded, use the dashboard's live settings for runtime changes.

#### Per-route rate limits (`rate_limits:`)

The `rate_limits:` section caps the aggregate request rate to a specific hostname + path prefix, protecting an expensive endpoint (login, export, search) even from many distinct visitors or tokens, a complement to the per-IP (`ip_limit_*`) and per-token limits. Each rule owns one shared token bucket; rules match first-match in file order (`hostname` unset = any host, `path` unset = any path, matched on a path-segment boundary). A request that would drain an empty bucket gets `429 Too Many Requests`.

An optional `methods:` list scopes a rule to those verbs, so a write path can be limited without throttling reads of the same route. Rules that differ only by method own separate buckets, and a rule without `methods:` covers every verb.

```yaml
# aperio-server.yaml
rate_limits:
  - hostname: app.example.com
    path: /login
    rps: 5          # sustained requests/second to this route
    burst: 10       # token-bucket burst (defaults to rps)
  - path: /export   # any hostname
    rps: 1
  - path: /api/items
    rps: 2
    methods: [POST, PUT, DELETE]   # reads of /api/items stay unlimited
```

Like the other structured sections it is re-applied on config hot-reload, and `--check-config` validates it. A route that carries its own `rate_limit:` block (see [Client-less routes](#client-less-routes-routes)) is limited by that instead, so the hostname and path do not have to be written twice.

#### Per-hostname fallback URLs (`fallbacks:`)

When no client is connected to serve a hostname, the visitor normally gets a `504`. A `fallbacks:` entry turns that into a graceful redirect to an origin/status URL instead, a maintenance page, a static origin, a "back soon" site. A `*` hostname is the catch-all for any otherwise-unclaimed host (an exact hostname match wins over it).

```yaml
# aperio-server.yaml
fallbacks:
  - hostname: app.example.com
    url: https://status.example.com     # 302 by default
  - hostname: "*"
    url: https://www.example.com
    preserve_path: true                 # append the request path + query
    permanent: true                     # 301 instead of 302
```

Rejected visitors (IP allowlist denials) never get a fallback redirect, the stealth `504` answer is preserved so the route's existence never leaks. Re-applied on hot-reload and validated by `--check-config`.

#### WAF-lite (`waf:`)

The `waf:` section is a small request firewall evaluated before a request is dispatched. Each rule ANDs the conditions it lists, a `path` regex, a `methods` list, and/or a `header` name + value regex, and a match answers `403 Forbidden`. A rule with `max_body` is a size limit for its matched route instead of a deny: exceeding it answers `413 Payload Too Large`. It is a coarse first line of defense (path/method/header/body-size filtering), meant to complement, not replace, the rate limits above.

```yaml
# aperio-server.yaml
waf:
  - path: "^/\\.git"            # block probes for an exposed repo
  - path: "^/admin"
    methods: [POST, PUT, DELETE]
  - header:
      name: user-agent
      regex: "(?i)sqlmap|nikto"
  - path: "^/upload"
    max_body: 1048576          # 1 MiB cap on this path
```

An invalid regex or a rule with no conditions is dropped with a logged error (and flagged by `--check-config`) rather than breaking proxying. Re-applied on hot-reload.

#### Config lint (`--check-config`)

`aperio-server --check-config` validates the layered configuration, the file (if any) plus the environment, and exits without starting the server: no port is bound and the data directory is untouched. It flags values the server would otherwise silently replace with defaults (unparsable numbers, unknown `lb_strategy`/`failover` values, a bad `random_subdomain` pattern), malformed `trusted_proxies`/`server_auth`, unreadable `504_page`/`503_page`/`error_pages:` files, invalid structured sections (`headers:`, `routes:`, `expose:`, `error_pages:`), and incomplete OIDC settings. Exit code 0 means the configuration is valid (warnings possible), 1 means at least one error, handy as a pre-deploy CI step or before a restart:

```console
$ aperio-server --check-config
Checking configuration (aperio-server.yaml)
  ok    APERIO_SERVER_TOKEN is set
  FAIL  APERIO_LB_STRATEGY 'bogus' is unknown (expected round-robin, primary-standby or sticky)

Configuration check FAILED: 1 error(s), 0 warning(s)
```

#### Effective config (`--print-config`)

`aperio-server --print-config` prints the resolved configuration and exits without starting the server. It answers "what is actually set, and where did each value come from?": every `APERIO_*` variable in effect, each attributed to its source, the real environment or the `aperio-server.yaml` file (including values materialized from grouped blocks), plus the structured file sections and any persisted dashboard overrides (which win at runtime). Secret-looking values are masked and long ones summarized. Settings not listed use their built-in defaults (see the reference tables below, or `--print-schema` for the machine-readable catalogue):

```console
$ aperio-server --print-config
Effective Aperio server configuration
=====================================
config file : ./aperio-server.yaml
data dir    : ./data

Settings (3 set, the rest use defaults):
  APERIO_MAX_BODY_SIZE   = 8000000  [aperio-server.yaml]
  APERIO_SERVER_TOKEN    = [REDACTED]  [env]
  APERIO_TRUSTED_PROXIES = 10.0.0.0/8  [aperio-server.yaml]

Structured aperio-server.yaml sections: headers

Dashboard overrides (./data/settings.json), these win over env/yaml at runtime:
  cache_enabled = true
```

#### Hot-reload

`aperio-server.yaml` is watched for changes: edits are applied live, without a restart. The re-applied surface is the **live-editable settings** (the same set the dashboard can change, cache, failover, rate limits, lockout, body/concurrency limits, audit rotation, `require_hostname_bind`, `tunnel_compression`, `ui_language`, `preview_noindex`, `server_auth`) plus the structured `headers:`, `routes:` and `error_pages:` sections. **Structural keys are not hot-reloaded** and need a restart: `host`/`port`/`data_dir`, proxy-trust flags, OIDC, the random-subdomain pattern, the `504_page`/`503_page` file paths, and `expose:` ports. Dashboard overrides still win over the file. Set `APERIO_CONFIG_HOT_RELOAD=0` to disable the watcher. Reloads are audit-logged (`config_reloaded`).

#### Server-side header rules (`headers:`)

The file may also carry a structured `headers:` section, the server-wide counterpart of the client's per-service `headers:` config, applied to every proxied HTTP request across all services (WebSocket upgrades pass through untouched). `request` edits what tunnel clients (and thus backends) receive, `response` edits what visitors receive; `add` sets a header (replacing any existing value of the same name), `remove` strips names case-insensitively. Client rules run too, the server applies its rules on its side of the tunnel, the client applies its own on the backend side. Response edits happen before the response cache and the request inspector see the response, so all views agree. Hop-by-hop and tunnel-critical headers stay managed by Aperio regardless.

```yaml
# aperio-server.yaml
headers:
  request:
    add:
      X-Proxied-By: aperio
    remove: [X-Internal-Debug]
  response:
    add:
      Strict-Transport-Security: max-age=63072000
    remove: [Server, X-Powered-By]
```

#### Public TCP expose (`expose:`, experimental)

A structured `expose:` list opens raw public TCP ports that relay into declared client tunnels. An entry names the tunnel and the organization whose client may claim it (`tunnel:` + `org:`, or the one-line `tunnel: <org>@<name>`); omitting the organization means the master one. `token:` is the earlier spelling and still works, but a token name is not unique across organizations, so a rule naming one can match a client of another. The older shared-secret form (`key:`, matched against `expose: <key>` on the declaration) still works too. See [Tunnels](emergency-tunnels.md#public-expose) for the full story and security notes.

```yaml
# aperio-server.yaml
expose:
  - protocol: tcp        # only tcp while experimental
    port: 2222
    key: a-long-random-shared-secret
```

#### Client-less routes (`routes:`)

A structured `routes:` list binds a hostname and/or path prefix directly to a server-produced answer, no tunnel client involved. Each rule matches on an exact `hostname` and/or a `path` prefix (bind semantics; first match wins, in file order) and carries exactly one action: `redirect` (302, or 301 with `permanent: true`; `preserve_path: true` appends the request path and query) or `respond` (a fixed response with optional `status`, `content_type`, `body`). Typical uses: vanity redirects, a "coming soon" page for a hostname whose client is not deployed yet, or a fixed `/robots.txt`. Routes match before client routing and maintenance-mode still wins; they serve operator-authored content, so the visitor gate does not apply.

```yaml
# aperio-server.yaml
routes:
  - hostname: old.example.com
    redirect: https://new.example.com
    permanent: true
    preserve_path: true
  - hostname: soon.example.com
    respond:
      status: 503
      body: "<h1>Coming soon</h1>"
  - path: /robots.txt
    respond:
      content_type: text/plain
      body: "User-agent: *\nDisallow: /\n"
```

**A route entry can also carry policy instead of an answer.** An entry with neither `redirect` nor `respond` does not end the request; it annotates the proxied traffic that matches it, so per-route settings are written next to the route they govern rather than scattered across the file:

| Field | Effect |
| --- | --- |
| `timeout` | Seconds to wait for the serving client's answer on this route. Wins over the client's per-service `response_timeout` and the global `gateway.response_timeout`, since it is the operator's own server-side configuration. |
| `headers` | `request:` / `response:` edits in the same shape as the server-wide `headers:` section, applied **after** it so the narrower rule gets the last word. A `cache-control` added here is what the visitor, the response cache and the inspector all see. |
| `rate_limit` | `rps`, optional `burst`, optional `methods:`, exactly as a `rate_limits:` rule but without repeating the hostname and path. Wins over any `rate_limits:` entry matching the same request. |

```yaml
# aperio-server.yaml
routes:
  - path: /uploads              # policy: no redirect, no respond
    timeout: 600                # large uploads need longer than the global 30s
    rate_limit:
      rps: 2
      methods: [POST]           # only the writes are throttled
  - hostname: cdn.example.com
    path: /static
    headers:
      response:
        add:
          cache-control: "public, max-age=3600"
        remove: [x-powered-by]
```

Answer rules and policy rules are matched independently, each first-match in file order, so a redirect placed after a policy entry still fires. The two kinds cannot be combined on one entry: a static answer never reaches a backend, so a backend timeout on it could not mean anything, and the server refuses to start rather than ignore it. `--check-config` reports the same problem before a deploy, and its shadowing lint compares each kind only against its own.

#### Per-hostname error pages (`error_pages:`)

A structured `error_pages:` list overrides the global `APERIO_504_PAGE` / `APERIO_503_PAGE` per hostname, so each exposed site can carry its own branding on gateway-timeout and maintenance responses. Each entry matches an exact `hostname` (case-insensitive) and points at HTML files via `504_page` and/or `503_page`; hostnames without an entry (and requests without a Host header) keep the global pages. Files are read when the section is loaded, at startup and on config hot-reload; an unreadable file logs an error and falls back to the global page:

```yaml
# aperio-server.yaml
error_pages:
  - hostname: app.example.com
    504_page: ./pages/app-504.html
    503_page: ./pages/app-503.html
```

### Names

Everything Aperio addresses by name, an organization, a service, a tunnel, carries a **handle**: `a-z`, `0-9` and `_`, at most 64 characters. Nothing else, and deliberately so. A handle is written in one file and read in another, typed into a command line and joined with other handles to form an address (`payments@postgres`), so every character outside that set is a way for two people to write down what they think is the same name and be wrong: `Postgres` and `postgres`, `pg-main` and `pg_main`, an `i` that is actually `ı`.

What is left out stays available as *syntax around* a name rather than inside one: `@` already separates an organization from a tunnel, and `-`, `.` and `*` are reserved for whatever an address needs to say next.

Anything a person should read goes in `custom_name:` instead, free text, any language, any punctuation, changeable at any time, because nothing addresses it. Services and tunnels take it in `aperio.yaml`; an organization takes it when it is created and can be renamed from the dashboard afterwards. The handle never changes, since an `expose:` rule and a binder's config on another machine point at it.

```yaml
services:
  - name: web                    # the handle
    custom_name: "Public Web"    # what the dashboard shows
    target: http://localhost:3000

tunnels:
  - name: pg_main
    custom_name: "Primary Postgres"
    target: 127.0.0.1:5432
```

### Common settings

Most deployments only need a handful of settings. These everyday knobs cover the common cases; the topic tables that follow, **Core**, **Routing & load balancing**, **Limits & protection**, **Authentication & dashboard**, **OIDC / SSO**, are the complete reference for advanced tuning. Run `aperio-server --print-config` to see which are set and where each value came from.

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_TOKEN` | **Required.** Master token: authenticates clients and is the dashboard admin password. |  |
| `HOST` / `PORT` | Bind address and listen port. | `0.0.0.0` / `8080` |
| `APERIO_DATA_DIR` | Directory for persisted state. **Mount a volume here in Docker.** | `./data` |
| `LOG_LEVEL` | `error` / `warn` / `info` / `debug` / `trace`. | `info` |
| `APERIO_SERVER_AUTH` | The default visitor gate in front of all proxied traffic: `user:password`. A key to the site and **not** to Aperio: the session it creates reaches every proxied hostname and nothing on the dashboard or its API. In yaml `server.auth` also takes a method block or a list of them. See [Visitor authentication](#visitor-authentication). |  |
| `APERIO_TRUST_PROXY` + `APERIO_TRUSTED_PROXIES` | Trust `X-Forwarded-For` behind your reverse proxy / CDN, and which hops to trust. | `0` |
| `APERIO_RANDOM_SUBDOMAIN` | Auto-assign every client a random subdomain from a `*` pattern. |  |
| `APERIO_LB_STRATEGY` | Load balancing: `round-robin`, `primary-standby`, or `sticky`. | `round-robin` |
| `APERIO_MAX_TUNNELS` | Max simultaneously connected tunnel clients. | `10` |
| `APERIO_MAX_CONNECTIONS_PER_SERVICE` | Parallel tunnel connections one client may open for a single service (its `connections:`). Announced to the client on connect, so it sizes its fan from this rather than guessing; a connection past the ceiling is closed. A token's own `max_connections` can lower it for its holder, never raise it. | `16` |
| `APERIO_INSPECTOR` | `0`/`false` = do not record transactions for the request inspector. On by default. Off gives back a mutex, two header clones and a capture entry per proxied request, at the cost of not being able to inspect or replay anything. A client may opt out for one service with `capture: false`. | `1` (on) |
| `APERIO_ACCESS_EVENTS` | `0`/`false` = do not emit the per-request structured access event for a **successful** request (`target: aperio_access`, level `info`). On by default. Distinct from `LOG_LEVEL`: it silences the one-per-request line and leaves warnings and errors where they are, so a refused or failed request still logs, at `warn`. The access log *file* (`APERIO_ACCESS_LOG`) is separate and unaffected. | `1` (on) |
| `APERIO_MAX_BODY_SIZE` | Max request body size in bytes. | `10485760` (10 MB) |
| `APERIO_CACHE` | Enable the server-side GET response cache (opt-in per service). | `0` |
| `APERIO_METRICS` | Enable the Prometheus endpoint at `/aperio/metrics`. | `0` |
| `APERIO_UI_LANGUAGE` | Default dashboard / login language. | `en` |

### Core

> The tables below are the **complete reference**, every server setting, grouped by topic. For a first deployment, [Common settings](#common-settings) above is usually enough.

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_TOKEN` | **Required.** Master token: authenticates tunnel clients and doubles as the dashboard admin password (`aperio:<token>`). |  |
| `HOST` | Bind address. | `0.0.0.0` |
| `PORT` | Listen port. | `8080` |
| `APERIO_DATA_DIR` | Directory for persisted state (tokens, stats, audit log, webhooks). **Mount a volume here in Docker.** | `./data` |
| `LOG_LEVEL` | `error`, `warn`, `info`, `debug`, `trace`. | `info` |
| `APERIO_REUSEPORT` | `1` = bind the listener with `SO_REUSEPORT`, so a second process can bind the same `host:port` while the first is still running, enables a zero-downtime rolling restart. | `0` |

### Routing & load balancing

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_REQUIRE_HOSTNAME_BIND` | `1` = clients without a hostname bind never receive traffic (strict multi-tenant mode). | `0` |
| `APERIO_VISITOR_IDENTITY_HEADERS` | `1` = tell the backend who the visitor is: `x-aperio-visitor-how` (`session` / `bearer` / `share`) and `x-aperio-visitor-id` (the email or username behind a session, where there is one). Off by default, the same new trust surface as `APERIO_IDENTITY_HEADERS` and adopted the same way. An open or ungated route identifies nobody and sends neither header. Inbound `x-aperio-*` is stripped from every proxied request regardless, so a visitor cannot forge one. | `0` |
| `APERIO_DEFAULT_ACCESS` | What a route nobody gated means: `allow` (today's behaviour) or `deny`, where a route is served because something declared it reachable rather than because nothing declared otherwise. See [Closed by default](#closed-by-default). | `deny` |
| `APERIO_RANDOM_SUBDOMAIN` | Pattern with a `*` placeholder in the leftmost label, every connecting client gets the pattern with `*` replaced by a random label, in addition to its other binds. `example.com` ≡ `*.example.com`; `*-test.example.com` yields `<random>-test.example.com` (stays on the same subdomain level, so one wildcard TLS cert covers it). |  |
| `APERIO_PREVIEW_NOINDEX` | `1` = services reached through their random subdomain answer with `X-Robots-Tag: noindex, nofollow` and a disallow-all `/robots.txt`, so preview environments never end up in search engines. Also live-editable from the dashboard. | `0` |
| `APERIO_CLIENT_DOWN_THRESHOLD` | Seconds without a heartbeat before a client is dropped from the routing pool (it rejoins on the next ping). | `15` |
| `APERIO_LB_STRATEGY` | Load-balancing strategy: `round-robin`, `primary-standby` (client priority tiers), or `sticky` (visitor affinity via cookie). See [Routing & Load Balancing](routing-and-load-balancing.md). | `round-robin` |
| `APERIO_FAILOVER` | What to do when a client dies mid-request: `fail`, `retry`, `wait`, or `retry-wait`. See [In-Flight Failover](failover.md). | `fail` |
| `APERIO_FAILOVER_MAX_JUMPS` | Max re-dispatch attempts per request. | `2` |
| `APERIO_FAILOVER_WINDOW` | Total seconds the `wait`/`retry-wait` modes may spend waiting for a candidate, across all jumps. | `15` |
| `APERIO_FAILOVER_ALL_METHODS` | `1` = also fail over non-idempotent methods (POST/PATCH). Off by default because a re-dispatched request may reach a backend twice. | `0` |
| `APERIO_RETRY_ON_5XX` | `1` = when a fully-buffered response is a retryable server error, re-dispatch the request to a freshly picked client instead of returning it. Shares the failover jump budget and honors method idempotency; streamed responses are never retried. See [In-Flight Failover](failover.md). | `0` |
| `APERIO_RETRY_STATUSES` | Comma-separated status codes that trigger `APERIO_RETRY_ON_5XX` (e.g. `502,503`). Empty = every 5xx (500-599). | every 5xx |
| `APERIO_OUTLIER_EJECTION` | `1` = passively eject a client from routing when it returns too many errors / timeouts / dropped connections under real traffic, even while its `/health` probe still reports green. Per-route fail-open. See [Routing & Load Balancing](routing-and-load-balancing.md#passive-outlier-ejection). | `0` |
| `APERIO_OUTLIER_MAX_FAILURES` | Failures within `APERIO_OUTLIER_WINDOW` that trigger an ejection. | `5` |
| `APERIO_OUTLIER_WINDOW` | Seconds the outlier failures are counted over. | `30` |
| `APERIO_OUTLIER_EJECT_SECS` | Seconds an ejected client stays out before automatic re-admission. | `30` |

### Limits & protection

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_ALERT_ERROR_RATE` | Failed-request percentage that fires an `alert_triggered` webhook/audit event (kind `error_rate`); resolves at 80% of the threshold. `0`/unset = off. See [Observability](observability.md#alerting). | off |
| `APERIO_ALERT_WINDOW` | Sliding window (seconds) the error rate is measured over. | `300` |
| `APERIO_ALERT_MIN_REQUESTS` | Minimum requests inside the window before the error-rate rule may fire. | `20` |
| `APERIO_ALERT_CLIENT_DOWN` | Seconds a known service may stay down before an `alert_triggered` event (kind `client_down`); resolves when it comes back. `0`/unset = off. | off |
| `APERIO_MAX_BODY_SIZE` | Max request body size in bytes. | `10485760` (10 MB) |
| `APERIO_MAX_CONCURRENT_REQUESTS` | Max in-flight proxied requests across all tunnels. | `100` |
| `APERIO_MAX_WS_CONNECTIONS` | Max concurrently-live proxied public WebSockets (they are long-lived, so they get their own ceiling separate from the request limit above); beyond it an upgrade gets `503`. `0` = no cap. | `10000` |
| `APERIO_MAX_TUNNELS` | Max simultaneously connected tunnel clients. | `10` |
| `APERIO_MAX_CONNECTIONS_PER_SERVICE` | Parallel tunnel connections one client may open for a single service (its `connections:`). Announced to the client on connect, so it sizes its fan from this rather than guessing; a connection past the ceiling is closed. A token's own `max_connections` can lower it for its holder, never raise it. | `16` |
| `APERIO_INSPECTOR` | `0`/`false` = do not record transactions for the request inspector. On by default. Off gives back a mutex, two header clones and a capture entry per proxied request, at the cost of not being able to inspect or replay anything. A client may opt out for one service with `capture: false`. | `1` (on) |
| `APERIO_ACCESS_EVENTS` | `0`/`false` = do not emit the per-request structured access event for a **successful** request (`target: aperio_access`, level `info`). On by default. Distinct from `LOG_LEVEL`: it silences the one-per-request line and leaves warnings and errors where they are, so a refused or failed request still logs, at `warn`. The access log *file* (`APERIO_ACCESS_LOG`) is separate and unaffected. | `1` (on) |
| `APERIO_IP_LIMIT_MAX` | Per-IP token bucket burst capacity. | `100` |
| `APERIO_IP_LIMIT_REFILL` | Per-IP refill rate (requests/second). | `5` |
| `APERIO_LOGIN_LOCKOUT_THRESHOLD` | Consecutive failed logins from one IP before it is locked out. | `5` |
| `APERIO_LOGIN_LOCKOUT_SECS` | Base lockout window in seconds; doubles with each repeat lockout (capped at 1 h). A successful login resets the state. | `60` |
| `APERIO_GATEWAY_TIMEOUT` | Seconds to wait for a client to (re)connect before failing a request. | `10` |
| `APERIO_GATEWAY_RESPONSE_TIMEOUT` | Seconds to wait for a client to answer a dispatched request. | `30` |
| `APERIO_TRUST_PROXY` | `1` = trust `X-Forwarded-For` / `X-Real-IP` for client IPs. Enable **only** behind a trusted reverse proxy. | `0` |
| `APERIO_TRUSTED_PROXIES` | Comma-separated IPs/CIDRs of your reverse proxies and CDN egress ranges (e.g. `10.0.0.0/8,173.245.48.0/20`). When set, the client IP is resolved by walking `X-Forwarded-For` (plus the direct peer) from the nearest hop backwards past trusted addresses, the CDN-agnostic model that works for Cloudflare, Fastly, Akamai, LB chains, etc. Headers from an untrusted direct peer are ignored entirely. Implies `APERIO_TRUST_PROXY=1`. Prefer this over the header-based options. |  |
| `APERIO_REAL_IP_HEADER` | Header consulted **before** `X-Forwarded-For` for the real client IP (with `APERIO_TRUST_PROXY=1`). Needed behind CDN→proxy chains where the proxy resets XFF to the CDN edge, e.g. set `CF-Connecting-IP` behind Cloudflare, or configure the proxy's `forwardedHeaders.trustedIPs` instead. |  |
| `APERIO_TRUST_CF_HEADER` | `1` = shorthand for `APERIO_REAL_IP_HEADER=CF-Connecting-IP` (an explicit `APERIO_REAL_IP_HEADER` wins). Enable **only** behind Cloudflare: any visitor can send this header, so on other deployments trusting it lets clients spoof their IP for rate limiting, audit logs, and token IP allowlists. | `0` |
| `APERIO_ADMIN_ALLOWED_IPS` | Comma-separated IPs/CIDRs allowed to reach the authenticated admin surface (`/aperio` dashboard + `/aperio/api/*`); other sources get a network-level block. The login page and its auth endpoints stay reachable from anywhere so password-gated services keep working. Empty = no restriction. An invalid entry refuses startup rather than applying a partial allowlist. |  |
| `APERIO_DENIED_IPS` | Comma-separated IPs/CIDRs refused everything, checked before every other rule: proxied traffic, the dashboard and its API, and the tunnel endpoints alike. The inverse of the allowlists, for blocking an abusive source without turning on an `allowed_ips` that would lock out everyone unnamed. Blocked requests get `403` and never reach a handler, so they cannot spend a rate-limit bucket or occupy a request slot. The client IP is resolved with the same proxy-trust rules as everything else, so a deployment behind a trusted proxy blocks the visitor rather than the proxy. Written as a yaml list (`denied_ips:`), it is **hot-reloadable**: an address can be blocked without a restart. An invalid entry refuses startup rather than applying a partial deny list. Quote IPv6 entries in yaml (`denied_ips: ["::1"]`): a bare `::1` inside a flow sequence is a yaml syntax error, and the whole file is then refused rather than half-applied. |  |
| `APERIO_IDENTITY_HEADERS` | `1` = tell the backend which client, organization and token served the request, as `x-aperio-client-id`, `x-aperio-org` (absent for the master organization) and `x-aperio-token`. Off by default: they are new trust surface, and a backend that starts believing them should do so deliberately. Added per dispatch attempt, so after a failover they name the client that actually served it. **Inbound `x-aperio-*` headers are stripped from every proxied request whatever this is set to**, so a visitor can never forge one, and a backend that trusts them can do so without checking whether the announcement is enabled. | `0` |
| `APERIO_REQUEST_ID` | `0`/`false` = do not manage a request-id header. On by default: the id the server already assigns every proxied request is sent to the backend and echoed on the response, so one identifier joins the visitor's report, the server's access log and the backend's own logs. Written as a block, `request_id.enabled`. | `1` (on) |
| `APERIO_REQUEST_ID_HEADER` | Header the id travels in (`request_id.header`). | `x-request-id` |
| `APERIO_REQUEST_ID_TRUST_INBOUND` | `1` = adopt the visitor's own value when the request already carries the header, instead of replacing it (`request_id.trust_inbound`). Off by default because the header is attacker-supplied: trusting it lets any visitor choose what appears in your logs and your backend's, and repeat somebody else's id. Turn it on only behind a proxy that sets the header itself. An adopted value must be at most 128 characters of `A-Za-z0-9-_.:/+`; anything else is quietly ignored in favour of the server's own id. | `0` |
| `APERIO_SECURE_COOKIES` | `1` = set the `Secure` flag on session cookies, which also lets the session cookie carry the `__Host-` prefix (`__Host-aperio_session`). The prefix matters here because the same server also serves other people's sites: without it, a tenant on a sibling hostname can set a cookie for the parent domain that the dashboard would read as its own. Defaults to the `APERIO_TRUST_PROXY` value. |  |
| `APERIO_TUNNEL_COMPRESSION` | `1` = offer per-message zlib compression to clients (enabled per connection once acknowledged; old clients keep plain frames). | `0` |
| `APERIO_CACHE` | `1` = enable the server-side GET response cache for services that opt in with the client `cache: true` setting. Strictly `Cache-Control`-driven: only responses explicitly allowing shared caching (`max-age`/`s-maxage`, no `no-store`/`no-cache`/`private`, no `Vary`/`Set-Cookie`) are stored, for the advertised lifetime; only credential-less plain GETs are answered from it. Hits carry `x-aperio-cache: hit` and an `Age` header; entries without a backend validator get a synthesized `ETag`, and a matching `If-None-Match` is answered `304` at the edge without a tunnel round-trip. Misses are **single-flighted**: concurrent identical cacheable GETs collapse into one upstream fetch (followers wait for the leader and answer from the freshly stored entry), so cache expiry on a hot URL cannot stampede the backend. Responses advertising `stale-while-revalidate=N` (RFC 5861) keep serving for N seconds past expiry (marked `x-aperio-stale`) while one background revalidation refreshes the entry, visitors never wait on the refresh. `POST /aperio/api/cache/purge` (admin) drops entries by `hostname` and/or `path_prefix` (empty body = whole cache), for immediate invalidation after a deploy. Cached entries also satisfy single-range **`Range` requests** (video scrubbing, resumable downloads) at the edge: `206 Partial Content` sliced from the stored full body with `Accept-Ranges`/`Content-Range`, `416` when out of range, honoring `If-Range`, partial requests never re-traverse the tunnel while the entry lives. | `0` |
| `APERIO_CACHE_MAX_BYTES` | Total in-memory budget of the response cache; inserting past it evicts the entries closest to expiry, and a single body larger than a quarter of the budget is never cached. | `67108864` (64 MB) |
| `APERIO_CACHE_MAX_STALE` | Serve-stale window in seconds for services that set `resilience: true`: how long past its advertised lifetime a cached response may still answer visitors while the route has no healthy client. `0` disables serve-stale. | `3600` |
| `APERIO_CACHE_NEGATIVE_TTL` | Seconds to briefly cache error / negative responses (e.g. `404`) so a hot missing URL cannot hammer the backend with repeated misses. `0` = disabled. Needs `APERIO_CACHE`. | `0` |
| `APERIO_VERSION` | The Aperio version this configuration was written for, e.g. `0.5.0` (yaml `version:`). On startup the server compares it against its own build and reports every recorded change to the configuration format that landed in between, naming the affected keys; a change marked security-relevant refuses the start instead. Unset disables the check. |  |
| `APERIO_OUTBOUND_ALLOWLIST` | Optional policy over where the server may send *outbound* callbacks (webhook deliveries, autoscaling hooks): a comma-separated list of host/CIDR patterns (`hooks.example.com`, `*.corp.example`, `10.1.0.0/16`). When set, a destination either matches an entry or is refused, both when a webhook is created and again at every delivery; a matching entry is trusted even if it is private, since the operator named it. Empty = no restriction. An invalid entry refuses startup rather than applying a partial allowlist. Cannot be combined with a proxy in the environment (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, either case): the server refuses to start, naming both, because a proxy resolves the destination itself and connects on the server's behalf, so the addresses this policy vetted are not the ones reached. |  |
| `APERIO_OUTBOUND_BLOCK_PRIVATE` | With no allowlist configured: `1` = refuse outbound callback destinations that are, or resolve to, internal addresses (loopback, RFC 1918, link-local and the cloud metadata services, CGNAT, unique-local). Off by default because internal receivers are the normal webhook deployment; turn it on when tenants you do not fully trust may create webhooks, since the delivery log would otherwise let them probe the server's private network one port at a time. Like the allowlist, this cannot be enforced when a proxy is named in the environment, and the server refuses to start rather than leave it looking active: the check resolves the destination here, while a proxy resolves it on its own network. | `0` |
| `APERIO_OUTBOUND_PROXY` | HTTP proxy the server's outbound callbacks (webhooks, autoscaling hooks, JWKS, `forward` auth, OIDC, OTLP) go through, on a network with no direct route out: `host:port` or `http://host:port`, with an optional `user:password@`. **The ambient `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` are no longer read by anything**, so this is the only way to send them through a proxy; a server that still has those set is told at startup that they do nothing. Note what a proxy costs the two settings above: the proxy resolves the destination's name on its own network, so `block_private` covers addresses written as addresses only, and CIDR allowlist entries cannot admit a named destination. The startup log names which half is in force. Yaml: `outbound.proxy`. | direct |
| `APERIO_OUTBOUND_NO_PROXY` | Destinations that skip the proxy and are called directly, comma-separated: exact names, or `.suffix` / `*.suffix` for a domain and everything under it. The usual case is a `forward` auth endpoint or an OIDC issuer inside your own network. Loopback is always direct, listed or not. These destinations keep the **whole** outbound policy, since the server chooses their addresses itself. `NO_PROXY` from the environment is not consulted (it was measured not to behave as a bypass switch even set to `*`). Yaml: `outbound.no_proxy`. | empty |
| `APERIO_STREAM_PAUSE_BYTES` | Per-stream flow control (tunnel protocol v3): server-side backlog bytes of one streamed response / WebSocket / TCP relay at which the producing client is asked to pause that stream, so a visitor reading slower than the backend sends throttles the backend through ordinary TCP backpressure instead of piling data up on the server. Values under 64 KB are raised to 64 KB. Each stream snapshots the three watermarks when it starts, so a live edit applies to streams opened after it. | `2097152` (2 MB) |
| `APERIO_STREAM_RESUME_BYTES` | Backlog bytes under which a paused producer is asked to resume. Kept well below the pause mark so the pair does not flap; a value at or above `APERIO_STREAM_PAUSE_BYTES` is repaired to a quarter of it. | `524288` (512 KB) |
| `APERIO_STREAM_BACKLOG_LIMIT` | Hard per-stream backlog cap: a stream whose producer cannot be paused (a pre-v3 client, or one ignoring the pause) is dropped past this many buffered bytes. A value below twice the pause mark is raised to it, so there is always room to pause before the cap bites. | `16777216` (16 MB) |
| `APERIO_504_PAGE` | Path to an HTML file served on 504 gateway-timeout responses instead of the plain-text default. |  |
| `APERIO_503_PAGE` | Path to an HTML file served while a hostname is in maintenance mode instead of the plain-text default. |  |
| `APERIO_AUDIT_MAX_SIZE` | Rotate `audit.jsonl` once it exceeds this many bytes (`0` = never rotate). | `10485760` (10 MB) |
| `APERIO_AUDIT_MAX_FILES` | Rotated audit generations to keep (`audit.jsonl.1` … `.N`; `0` = truncate instead of keeping history). | `3` |
| `APERIO_ACCESS_LOG` | File path for the structured access log: one JSON line per proxied request (`request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, `error`), directly ingestible by Loki/ClickHouse. The same data is always emitted to stdout as structured `aperio_access` tracing events. |  |
| `APERIO_STREAM_MIN_THROUGHPUT` | Bytes per second a streamed response's consumer must take **while data is waiting for it**, or the stream is ended. The pump already ends a stream whose consumer cannot take a single chunk within the gateway timeout, so a reader taking *nothing* was covered; this closes the gap in between, a reader that accepts one chunk just inside the timeout forever, holding a client concurrency slot and megabytes of buffer for as long as it likes. Only the time the consumer kept data waiting counts, so a stream that is quiet because the **backend** has nothing to send (server-sent events, long polling) is never ended for it. Measured over 30-second windows. | `0` (no floor) |
| `APERIO_ALTERNATE_SERVERS` | Comma-separated Aperio server URLs (`ws://`/`wss://`) a client of this one may fall back to, announced on every handshake. A planned migration or a regional failover otherwise means editing every client's config; announce the new server here and clients learn it on their next connection. Advice rather than instruction: a client appends these **after** the servers its own config names, so the operator's list still decides the order, and the rotation wraps, so an alternate is never a one-way door. A client that has never reached this server learns nothing, which is why this is for a migration announced in advance, not a rescue. At most 8. |  |
| `APERIO_MAX_STREAMS_PER_IP` | Streamed responses one visitor address may hold open at once. Saturating a service's concurrency budget otherwise takes one host holding many slow streams; this makes it take a botnet. The slot is taken when a response turns out to *be* a stream and released when the body ends or the visitor walks away, so it is a concurrency limit rather than a rate limit: opening and closing streams quickly never trips it, holding them open does. Refusals appear as `aperio_rate_limited_total{limit="streams-per-ip"}`. **Check `trust_proxy` before setting it**, or every visitor behind your CDN shares one address. | `0` (no limit) |
| `APERIO_OTEL_BRIDGE` | `1` = accept OTLP exports from tunnel clients and forward them to the collector `otel.endpoint` names. Off by default: it is an outbound path a client can drive, so it is a decision rather than a consequence of having `otel` on. See [the OTel bridge](observability.md#the-otel-bridge-telemetry-from-the-edge). | `0` |
| `APERIO_OTEL_HEADERS` | Extra headers on every outgoing OTLP request, as `k=v,k=v`. Where a collector's credential goes, and it stays on the server: the point of the bridge is that an edge host does not hold one. |  |
| `APERIO_SHUTDOWN_DRAIN` | Seconds to let in-flight proxied requests finish before shutdown ends the connections carrying them. Behind a load balancer this is the number that decides whether a deploy is invisible or shows up as a handful of 502s: the balancer needs long enough to take the instance out of rotation, and the requests it already sent need long enough to answer. `auto` sizes it from the drain budgets connected clients announce (the longest of them, since the drain is over when the slowest client has finished), capped at 30s, so a client cannot hold the process past a typical orchestrator grace period. | `0` (do not wait) |
| `APERIO_SHUTDOWN_TIMEOUT` | Seconds after which shutdown stops waiting for anything still holding a connection open (a proxied WebSocket, a TCP relay, a stalled peer) and exits. Was a fixed 10 seconds. | `10` |
| `APERIO_ACCESS_LOG_SAMPLE_RATE` | Fraction of **successful** requests that produce an access line, `0.0` to `1.0`. Thins out both the `aperio_access` event and the `APERIO_ACCESS_LOG` file, for the case where the choice would otherwise be between a log bill and no request log at all. Sampling is deterministic (`0.1` = exactly one line in ten, not one in ten on average). A response of `500` or worse is **always** logged whatever the rate, and so is every refused or failed request. The dashboard's counters, the latency histogram and the rate charts are unaffected: they are fed before sampling and stay exact. | `1.0` (log everything) |
| `APERIO_INSPECTOR_REDACT` | `0`/`false` = disable secret masking in the request inspector. On (default), credential headers (`Authorization`, `Cookie`, `X-Api-Key`, …) and secret-looking body fields (`password`, `token`, `api_key`, …) show as `[REDACTED]` in the inspector, the cURL copy, and the HAR export; the raw capture kept in memory for replay is always intact. | `1` (on) |
| `APERIO_RETENTION_CAPTURES` | Days to keep inspector captures and webhook inbox entries; a background pruner enforces the TTL at startup and hourly. `0`/unset = keep (bounded only by the entry caps). |  |
| `APERIO_RETENTION_ACCESS_LOG` | Days to keep `APERIO_ACCESS_LOG` lines; expired lines are pruned in place. |  |
| `APERIO_RETENTION_AUDIT` | Days to keep audit events: rotated generations whose newest event expired are deleted whole, and the active file loses only its leading expired prefix, the tamper-evidence hash chain stays verifiable. |  |
| `APERIO_RETENTION_STATS` | Days to keep day-granularity statistics buckets (week/month/year buckets keep their built-in caps). |  |
| `APERIO_UPTIME_TICK_SECS` | Seconds between availability-history ticks for the dashboard Uptime panel (`GET /aperio/api/uptime`). Minimum `1`. | `10` |
| `APERIO_DB_MAX_BYTES` | Disk-usage guard: cap on `aperio.db` (plus its WAL/SHM sidecars). At 90% a `disk_usage_warning` webhook/audit event fires (once per episode, resetting below 80%); past the cap the hourly guard auto-prunes the lowest-priority data, oldest webhook inbox entries, oldest webhook deliveries, oldest day-stat buckets, then vacuums so the file actually shrinks, and emits `disk_pruned`. | unbounded |
| `APERIO_BACKUP_INTERVAL` | Seconds between automatic physical snapshots of the SQLite store (a consistent online backup). Unset/`0` = disabled; enabling also requires `APERIO_BACKUP_DIR`. | off |
| `APERIO_BACKUP_DIR` | Destination directory for the automatic backups. Required to enable them. |  |
| `APERIO_BACKUP_KEEP` | How many timestamped backup snapshots to retain; older ones are pruned. | `7` |
| `APERIO_BACKUP_KEY` | Encryption key for the snapshots, as 64 hex characters or base64 of 32 bytes. Unset writes them in the clear, as before; set, each snapshot is written as `aperio-<epoch>.db.enc` with AES-256-GCM. Prefer `APERIO_BACKUP_KEY_FILE`: a key here is a key in a config file, which backups and configuration management copy around. A key that cannot be used **disables backups** rather than falling back to plaintext. | off |
| `APERIO_BACKUP_KEY_FILE` | File holding that key, which is what a secret manager mounts. **Refused when it is inside `APERIO_BACKUP_DIR`**: whoever has the backups would have the key, which is the one arrangement encryption cannot survive. Warns when the file is readable beyond its owner. Restore with `aperio-server --decrypt-backup <snapshot.db.enc> [out.db]`. | off |
| `APERIO_WEBHOOK_RETRY_SCHEDULE` | Comma-separated backoff seconds between webhook redelivery attempts after a transport error / 5xx / 429 (attempt count = schedule length + 1). Empty = no retries. See [Observability](observability.md#delivery-reliability--the-delivery-log). | `1,5,25,60` |
| `APERIO_OTEL` | `1` = export one OTLP span per proxied request to an OpenTelemetry collector (adopts inbound W3C `traceparent`, propagates its own context to the backend). | `0` |
| `APERIO_OTEL_ENDPOINT` | OTLP collector base URL. Falls back to the standard `OTEL_EXPORTER_OTLP_ENDPOINT`. Over HTTP the `/v1/traces` signal path is appended if absent; over gRPC it is stripped, since gRPC takes the bare base URL. The default follows `APERIO_OTEL_PROTOCOL`. | `http://localhost:4318` |
| `APERIO_OTEL_PROTOCOL` | OTLP transport: `http` (protobuf over HTTP, the spec spelling `http/protobuf` is also accepted) or `grpc`. Falls back to the standard `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` / `OTEL_EXPORTER_OTLP_PROTOCOL`. Unset, the endpoint's port decides: 4317 is gRPC, anything else HTTP. A collector answering the other protocol drops every span in silence, so the startup probe checks the transport that was actually chosen and warns when the port contradicts it. | port-derived |
| `APERIO_OTEL_SERVICE_NAME` | `service.name` reported on exported spans. Falls back to `OTEL_SERVICE_NAME`. | `aperio-server` |
| `APERIO_OTEL_SAMPLE_RATE` | Fraction of traces to record, `0.0` to `1.0`. Default `1.0`, every request builds a span tree and exports it, which is the setting that makes tracing visible in a benchmark. `0.01` samples one request in a hundred; the decision is made once per request and every span of that request follows it. An unparseable or out-of-range value traces everything rather than nothing. | `1.0` |

**The per-IP bucket above does not charge every call one token.** A credential attempt (login, OIDC, WebAuthn, a token refresh) costs two, and something that provisions or reads the whole store (an ephemeral tunnel, an export, an import) costs five. The multiples are gentle on purpose: the bucket was sized when every call cost one, so a steeper price does not make that class cost more, it tightens the limit on it against a ceiling nobody re-chose, and an office behind one NAT signing in on a Monday morning is the first thing that breaks. One bucket at different prices rather than a bucket per class: separate buckets would let an attacker spend a full allowance on each, and the thing being protected, this server's capacity, is shared anyway. A refused call is not charged, since it was never served, and the expensive calls are charged **after** authentication and argument checks, so a malformed request is answered `400` for free.

### Authentication & dashboard

> 📖 Concepts and hardening advice: [Tokens & Authentication](tokens-and-auth.md)

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_AUTH` | The default visitor gate in front of all proxied traffic: `user:password`. In yaml `server.auth` also takes a method block or a list of them. See [Visitor authentication](#visitor-authentication). |  |
| `APERIO_WEBAUTHN_ORIGIN` | Public dashboard URL enabling passkey (WebAuthn) sign-in for named users; its domain becomes the RP ID. | passkeys off |
| `APERIO_WEBAUTHN_RP_ID` | Override the passkey RP ID with a parent registrable domain (e.g. `robogon.com`) so one passkey works across **all** its subdomains (`aperio.robogon.com`, `test-aperio.robogon.com`, ...), instead of only the exact host of `APERIO_WEBAUTHN_ORIGIN`. Must be that host or a parent of it, and never a public suffix (`com`). Ceremonies from any subdomain of it are then accepted. Changing it invalidates passkeys enrolled under the previous RP ID (users re-enroll). | origin's host |
| `APERIO_IGNORE_CLIENT_AUTH` | `1` = ignore any client-declared per-service visitor password (see the client `auth` setting) and keep sole control of the visitor gate with `APERIO_SERVER_AUTH` / OIDC. | `0` |
| `APERIO_DASHBOARD` | `0` = disable the admin dashboard entirely. | `1` |
| `APERIO_UI_LANGUAGE` | Default dashboard/login UI language (`en`, `de`, `es`, `fr`, `tr`, `ru`, `zh`, `ja`) used when the visitor's browser language is unsupported; also dashboard-editable. | `en` |
| `APERIO_TOKEN_EXPIRY_WARNING` | Seconds before a dynamic token's expiry at which a `token_expiring` webhook/audit event fires (once per token per expiry window; `0` = off). The dashboard tokens table shows an "expiring soon" badge in the last 24 h regardless. | `86400` (24 h) |
| `APERIO_TOKEN_PINNING` | `1` = trust-on-first-use device pinning: the first client device key seen for a dynamic token is pinned, and a later connection presenting a different (or missing) key for that token is rejected, so a leaked token replayed from another machine cannot serve. See [Tokens & Authentication](tokens-and-auth.md). | `0` |
| `APERIO_METRICS` | `1` = enable the Prometheus endpoint at `/aperio/metrics`. | `0` |
| `APERIO_METRICS_TOKEN` | Token required to scrape metrics (`?token=` or `Authorization: Bearer`). Unset = a random one is generated on first start and persisted in `APERIO_DATA_DIR/metrics_token`. | generated |

### Autoscaling

Let the server ask an endpoint you control for capacity when a bind needs it: a cold start when nothing is serving the hostname, a scale-out when the pool saturates. Full walkthrough: [Autoscaling](autoscaling.md).

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SCALING` | `1` = honor the `scaling:` block clients announce. Off = the declaration is ignored entirely. | `0` |
| `APERIO_SCALING_ALLOW_HTTP` | `1` = permit a plain-http autoscaling endpoint. The URL comes from a client, so https is the floor by default. | `0` |
| `APERIO_SCALING_ALLOW_PRIVATE` | `1` = permit an autoscaling endpoint that resolves to a private, loopback or link-local address. The URL comes from a client, so it is treated as untrusted input: every address the name resolves to is checked, since a hostname pointing at `127.0.0.1` or `169.254.169.254` is the classic SSRF bypass. Turn it on only when the scaler genuinely lives on the same private network. | `0` |
| `APERIO_JWKS_ALLOW_HTTP` | `1` = permit a plain-http `jwks_url` on a `jwt` visitor gate **declared by a client**. A client-declared key-set URL is fetched by the server, from the server's network, before any signature is checked, so it is fenced the way an autoscaling endpoint is: https only by default. A `jwt` gate in the *server's own* configuration is not subject to this, since an operator naming their own issuer is describing their own network. | `0` |
| `APERIO_JWKS_ALLOW_PRIVATE` | `1` = permit a client-declared `jwks_url` that resolves to a private, loopback or link-local address. Off by default for the same reason as the row above: a tunnel-token holder must not be able to aim the server at a metadata service or an internal admin port. Again, the server's own `jwt` configuration is unaffected. | `0` |
| `APERIO_SCALING_RECORD_TTL` | Seconds after which an autoscaling record nothing has re-announced is dropped. | `2592000` (30 days) |

### Edge proxy integration

Publish the hostnames Aperio currently serves to a dynamic reverse proxy in front of it, so Traefik or Caddy can route and issue certificates for names that only exist at runtime. Full walkthrough: [Behind a Dynamic Edge Proxy](edge-proxy.md).

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_EDGE_TOKEN` | Credential the edge proxy presents (`Authorization: Bearer` or `?token=`). **Unset = the `/aperio/api/edge/*` endpoints are not registered at all.** |  |
| `APERIO_EDGE_SERVICE_URL` | URL the edge should forward matched traffic to, i.e. this server as the edge sees it (e.g. `http://aperio:8080`). Required by the Traefik document; the Caddy `ask` endpoint works without it. |  |
| `APERIO_EDGE_ENTRYPOINTS` | Comma-separated Traefik entry points set on the generated routers. Empty = leave it to Traefik's defaults. |  |
| `APERIO_EDGE_CERT_RESOLVER` | Traefik certificate resolver named on the generated routers. Unset = no `tls` block. |  |
| `APERIO_EDGE_INCLUDE_OFFLINE` | `1` = also publish hostnames a token permits but no client is serving, so a certificate can exist before the first connect. Lets a tenant provoke an ACME request for any hostname its token covers. | `0` |

### OIDC / SSO

Put an identity-provider login (Google, Keycloak, Authentik, ...) in front of everything the tunnel serves. Unauthenticated visitors are redirected to the provider; after login, the verified email (fetched from the issuer's `userinfo` endpoint over TLS) is checked against the allowlist, exact addresses, `*@domain`, or `*`. Sessions last 24h.

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_OIDC_ISSUER` | Issuer URL. Setting it enables SSO enforcement. |  |
| `APERIO_OIDC_CLIENT_ID` / `APERIO_OIDC_CLIENT_SECRET` | OAuth client registered at the issuer. Redirect URI: `https://<your-host>/aperio/oidc/callback`. |  |
| `APERIO_OIDC_ALLOWED_EMAILS` | Comma-separated allowlist (required with issuer). |  |
| `APERIO_OIDC_SCOPES` | Requested scopes. | `openid email profile` |
| `APERIO_OIDC_REDIRECT_URL` | Fixed callback URL; otherwise derived from the request `Host` (and `X-Forwarded-Proto` when `APERIO_TRUST_PROXY=1`), and warned about at startup. **Set it.** Deriving means the `Host` of the request that starts a login decides where the provider returns the authorization code: a visitor lured to any hostname that resolves to this server has their code sent there. Redeeming it still needs the client secret, and your provider's registered-callback list is the other gate, but neither of those is Aperio's to enforce. | derived |

Discovery is fetched from `<issuer>/.well-known/openid-configuration` at startup. A misconfigured SSO setup is a **fatal error**, the server refuses to start rather than silently serving an unprotected proxy. Grants and denials are audit-logged.

## HTTP endpoints

| Endpoint | Description | Auth |
| --- | --- | --- |
| `/*` (fallback) | Proxied to tunnel clients. | visitor password / OIDC if configured |
| `GET /aperio/ws` | Tunnel endpoint for clients. | master or dynamic token (Bearer / `x-auth-token`) |
| `GET /aperio/tunnels` | Lists the tunnels the presented token may bind (see [Tunnels](emergency-tunnels.md)). | a tunnel token |
| `GET /aperio/tunnels/:client_id` | Per-client tunnel discovery for `--bind-tunnels`. | master, the client's own token, or one in its organization with `allow_bind` |
| `GET /aperio` | Admin dashboard. | dashboard session |
| `GET /aperio/api/stats`, `/api/logs`, `/api/audit` | Live stats, request log, audit events. | dashboard session |
| `GET/POST /aperio/api/tokens`, `PUT/DELETE /aperio/api/tokens/:id` | Dynamic token management. | dashboard session |
| `GET/POST /aperio/api/webhooks`, `DELETE /aperio/api/webhooks/:id` | Webhook management. | dashboard session |
| `GET /aperio/api/requests/:id`, `POST /aperio/api/requests/:id/replay` | Request inspector & replay. | dashboard session |
| `POST /aperio/api/clients/:id/override`, `POST /aperio/api/clients/:id/enabled` | Temporary bind overrule / enable-disable toggle. | dashboard session |
| `GET /aperio/api/activity` | Request volume per bucket (total and failed) over the requested span, for the activity chart's long views. `range=15m` (default, 5-second slices), `2h` (2-minute) or `1d` (15-minute). | dashboard session |
| `GET /aperio/api/explain` | Dry run: which rule would answer a request to a hostname and path, and what every other stage saw. Spends no rate limit and wakes nothing. | dashboard session (operator+) |
| `GET/POST /aperio/api/maintenance` | List / toggle maintenance mode for a hostname, a `*.example.com` subdomain wildcard, or `*` (master only). `reason` and `ttl_seconds` are optional: the reason reaches the 503 page, the window lifts the flag by itself. | dashboard session |
| `POST /aperio/api/share` | Generate a signed share link (see [Share Links](share-links.md)). | dashboard session |
| `GET/PUT /aperio/api/settings` | Read / edit runtime server settings (persisted overrides on top of env defaults). | master super-admin |
| `POST /aperio/api/tunnels`, `DELETE /aperio/api/tunnels/:id` | Programmatic ephemeral tunnel provisioning. See [Ephemeral Tunnels](ephemeral-tunnels.md). | master token (Bearer) or dashboard session |
| `GET/POST /aperio/auth` | Login page / login API. |  |
| `GET /aperio/oidc/login`, `/aperio/oidc/callback` | OIDC flow. |  |
| `GET /aperio/metrics` | Prometheus metrics. | metrics token |
| `GET /aperio/health` | Liveness probe (status, client count, uptime). | none |
| `GET /aperio/healthz` | Liveness probe for a container runtime: `200` with an empty body, no locks taken. Use this for a Docker `HEALTHCHECK` or a Kubernetes `livenessProbe`; `/aperio/health` builds a JSON document and takes two locks, and a probe that waits on a lock reports a busy process as a dead one. | none |
| `GET /aperio/readyz` | Readiness probe: `200` while the server should receive traffic, `503` from the moment a shutdown signal arrives. Pair it with `APERIO_SHUTDOWN_DRAIN`: readiness turns off so the load balancer stops sending new requests, and the drain gives the ones already in flight time to finish. Never wire this to a `livenessProbe`, restarting on it would kill the drain it exists to protect. | none |
| `GET /aperio/api/openapi.json` | OpenAPI 3.1 document describing this whole API (generated from the handlers; point Swagger UI or a client generator at it). | dashboard session |
| `GET /aperio/api/export` | Logical JSON dump, a failsafe for upgrades and migrations. `?include=` names the sections: `tokens`, `webhooks`, `users`, `organizations`, `scaling`, `settings_overrides` (the default set), plus `statistics`, `uptime`, `activity` (the two-hour and one-day request-volume rings behind the dashboard chart), `inbox`, `admin_keys`. Without `organizations`, only the master organization's rows travel. Sessions and the audit log are never included. | master super-admin |
| `POST /aperio/api/import` | Applies a dump; each present section **replaces** the corresponding store. | master super-admin |
| `GET/POST /aperio/api/users`, `PUT/DELETE /aperio/api/users/:id` | Dashboard user management (create/edit/delete, roles). | dashboard session (**admin**) |
| `GET /aperio/api/scaling`, `DELETE /aperio/api/scaling/:id` | Autoscaling records armed by clients, with live pool utilization, see [Autoscaling](autoscaling.md). | dashboard session |
| `GET /aperio/api/edge/ask?domain=`, `GET /aperio/api/edge/traefik` | Live hostname inventory for a reverse proxy in front of Aperio (Caddy on-demand TLS, Traefik HTTP provider), see [Behind a Dynamic Edge Proxy](edge-proxy.md). | `APERIO_EDGE_TOKEN` |
| `GET/POST /aperio/api/orgs`, `DELETE /aperio/api/orgs/:id`, `PUT /aperio/api/orgs/:id/hostnames`, `POST /aperio/api/orgs/select` | Organization management, hostname allowlist, and switching, see [Organizations](organizations.md). | master super-admin |

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`simple`](examples/simple/): minimal one-target pair
- [`multiple_services`](examples/multiple_services/): several backends from one client
- [`headers`](examples/headers/): header rules on both sides, and per service
- [`static_site`](examples/static_site/): serve local directories, one or several
