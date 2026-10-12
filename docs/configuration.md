# Configuration Reference

The complete reference for configuring both sides of Aperio.

The two sides are configured differently, on purpose. A **client** is a file next to the service it exposes, `aperio.yaml`, with an environment variable for every setting and a CLI flag for the common ones. A **server** reads a handful of boot settings from its environment and keeps every other setting in its database, changed from the dashboard or the API without a restart; see [Server](#server).

## The client's standard: one name, three surfaces

Every client setting has one name, its yaml key. The other two surfaces are derived from it:

| Surface | Form | Example |
| --- | --- | --- |
| yaml key | `snake_case`, dotted for a block | `server.token`, `health.interval` |
| Environment variable | `APERIO_` + the key, dots as underscores | `APERIO_SERVER_TOKEN`, `APERIO_HEALTH_INTERVAL` |
| CLI flag | `--` + the key, dots and underscores as dashes | `--server-token` |

The environment variables are not a list somebody keeps: they are generated from the same schema the parser and the editor read, so a key and its variable cannot drift apart. `log_level` is the one bare name, `LOG_LEVEL`. A service's settings are written once under `defaults:` and their variables carry no `DEFAULTS_` prefix, `APERIO_TIMEOUT` is `defaults.timeout`, because that is where one service's settings go when there is no file to put them in. The CLI has flags for the settings a one-liner needs; everything else is the file or the environment.

A value that is not one scalar is still reachable from the environment: a list is comma-separated (`APERIO_ALLOWED_IPS=10.0.0.0/8,203.0.113.7`), a map of strings is `k=v,k=v` (`APERIO_METRICS_LABELS=env=prod,region=eu`), and anything can be written as yaml (`APERIO_ACCESS='{method: basic, users: "admin:secret"}'`).

### Units

A duration is written with its unit, `250ms`, `30s`, `5m`, `2h`, `1d`; a size with its unit, `512KB`, `10MB`, `1GB`. A bare number is still read, as seconds or as bytes, and no key carries its unit in its name.

## Client

### Layers

The client reads four layers and merges them, lowest first:

**`~/.aperio.yaml`  <  `./aperio.yaml`  <  environment variables  <  CLI arguments**

One rule merges them, the same one `include:` uses: **mappings merge key by key, and anything else is replaced.** A later layer wins a key; a block a later layer writes is laid over the earlier one rather than replacing it, so `~/.aperio.yaml` can hold `server.url` and `server.token` for every project while each project's file adds its services. The environment beating the file is what a container expects: the image carries the file, the deployment overrides a value. `aperio-client check` reports which layer supplied each value.

**A key nothing reads is an error**, at startup and on a reload, naming the key it was probably meant to be (`` `services.web.timeot` is not a setting; did you mean `timeout`? ``), or, for a key an earlier release read, what replaced it. The same goes for a variable an earlier release read (`APERIO_PUBLIC` was replaced by `APERIO_ACCESS=public`). Top-level keys starting with `x-` are left alone, for yaml anchors a file reuses (`x-common: &common`).

When a config file is present, the client **hot-reloads** it: edits to `aperio.yaml` (or the `--config` path) are detected within ~5 s, the replacement connections come up before the old ones are drained, and the services restart with the freshly resolved configuration. A file that no longer parses, or resolves to an invalid configuration, is ignored with a warning and the previous configuration keeps serving. Alongside the services, a reload re-applies the process-wide facilities (the message faces, the subscriptions and their commands). A few keys are applied once, when the client starts: how it dials the server (`server.ip_family`, `server.proxy`, `server.min_tls_version`, `server.tls_cipher_suites`), its identity (`client_id`, `client_id_file`, `device_key`, `device_key_file`), `otel_bridge`, `pid_file`, `log_level` and `log_format`. A reload that changes one of them says so by name, rather than reporting a change it did not make; they are marked `x-reload: restart` in the schema.

### CLI

```
aperio-client                          Run from config files / environment (Docker mode)
aperio-client 3000                     Expose http://localhost:3000
aperio-client example.com              Expose http://example.com
aperio-client bind [NAME...]           Bind tunnels a peer client declares, locally
aperio-client init 3000 --hostname app.example.com
                                       Write the aperio.yaml that command line describes
aperio-client check                    Diagnose configuration and connectivity
aperio-client api <group> <action>     Call the server's admin API (see below)
aperio-client completions <shell>      Print a shell completion script
aperio-client --version
aperio-client --help
```

The positional target is optional: a bare port number expands to `http://localhost:<port>`, a bare hostname to `http://<hostname>`, and full URLs pass through. The same reading applies to a `target:` in a file. A target on the command line or in the environment runs as the one service, in place of the file's `services:`.

`aperio-client init` writes down a one-liner: it takes the service the command line (and the environment) describes, and every other setting given there, and writes the `aperio.yaml` that means the same thing, the service named after its hostname's first label unless `--service NAME` says otherwise. The token is not written into the file; the file reads `${APERIO_SERVER_TOKEN}` instead. It refuses to replace an existing file without `--force`, and `--stdout` prints the file rather than writing it. What it writes is read back the way the client reads a file before it is written, so it is a file the client accepts.

```bash
aperio-client init 3000 --hostname app.example.com --access public \
  --server-url https://tunnel.example.com
```

`aperio-client api ...` performs one admin API call and prints the JSON answer, the dashboard's operations (share links, tokens, tunnels, maintenance, users, reports) from a script. It authenticates with an admin key rather than the tunnel token; see [Admin API from the CLI](cli-api.md).

`aperio-client check` resolves the configuration the usual way, reporting **which layer** (the command line, `./aperio.yaml`, the environment, `~/.aperio.yaml`) supplied each value, and verifies every hop: the server's health endpoint (including whether the server is on this client's minor release), token validity (a real tunnel handshake), every local target with its health endpoint, every `serve:` directory, and every declared tunnel. It exits non-zero when anything fails, so it works as a deploy gate.

| Flag | Key it writes |
| --- | --- |
| `--server-url URL` | `server.url` |
| `--server-token TOKEN` | `server.token` |
| `--server-api-key KEY` | `server.api_key`, used by the `api` subcommand, never by the tunnel |
| `--server-ip-family auto\|ipv4\|ipv6` | `server.ip_family` |
| `--server-proxy [user:password@]host:port` | `server.proxy` |
| `--name NAME` | `name` |
| `--client-id UUID` | `client_id` |
| `--serve DIR` | the one service's `serve`: a local directory of static files instead of a backend. `aperio-client --serve ./dist` publishes a folder |
| `--hostname HOSTNAME` | the one service's `hostname`, comma-separated for several |
| `--path PREFIX` | the one service's `path` |
| `--access POLICY` | `defaults.access`: `public`, or a method block as yaml |
| `--allowed-ips IPS` | `defaults.allowed_ips`, comma-separated |
| `--priority N` | `defaults.priority` |
| `--pass-hostname` | `defaults.pass_hostname` |
| `--config FILE` | the config file to read (default: `./aperio.yaml`) |

### Settings

A single HTTP backend needs a server URL, a tunnel token, and a target (or `serve:` for static files). A client that only binds tunnels or handles messages needs no target. `aperio-client check` shows where each resolved setting came from.

#### The client

Top-level keys: what the client as a whole is, and how it reaches its server.

| Env variable | CLI | yaml key | Description | Default |
| --- | --- | --- | --- | --- |
| `APERIO_SERVER_URL` | `--server-url` | `server.url` | **Required.** Server URL (`http/https/ws/wss`). |  |
| `APERIO_SERVER_TOKEN` | `--server-token` | `server.token` | **Required.** Tunnel token, master or a scoped dynamic token. |  |
| `APERIO_SERVER_URLS` |  | `server.urls` | Additional server URLs to fail over to (comma-separated on env, a list in yaml), tried in order when the primary `server.url` is unreachable, for a redundant control plane. |  |
| `APERIO_SERVER_API_KEY` | `--server-api-key` | `server.api_key` | Programmatic admin key for `aperio-client api ...` calls, unrelated to the tunnel itself. Falls back to the tunnel token, which the server accepts only where the master token is a valid credential. See [Admin API from the CLI](cli-api.md). |  |
| `APERIO_SERVER_IP_FAMILY` | `--server-ip-family` | `server.ip_family` | Address family used to dial the server: `auto` (default; IPv4-first, trying each resolved address with a per-address timeout), `ipv4`, or `ipv6`. Use `ipv4` when the server hostname resolves to an unreachable IPv6 address. | `auto` |
| `APERIO_SERVER_PROXY` | `--server-proxy` | `server.proxy` | HTTP proxy to dial the tunnel server through, for a network that allows no direct outbound connection: `host:port`, `http://host:port`, either with an optional `user:password@` in front. The client sends `CONNECT` and runs TLS inside the tunnel the proxy opens, so the proxy sees the server's hostname and nothing else. **The tunnel only**: requests to your own backend are never sent through it, and neither is anything else on the machine. A port is required in practice; without one the http default (`80`) is used, and a value that names a port but no host (`:3128`) is refused at startup rather than read as a hostname. An IPv6 proxy is written bracketed (`[2001:db8::1]:3128`). An `https://` proxy (TLS to the proxy itself) is refused rather than dialed in the clear. A proxy that refuses `CONNECT` fails the dial with the proxy and its status named, and `aperio-client check` reports the route it took. | direct |
| `APERIO_SERVER_MIN_TLS_VERSION` |  | `server.min_tls_version` | Lowest TLS version offered when dialing the tunnel server over `wss://`: `1.2` or `1.3`. Unset leaves rustls' own set (1.2 and 1.3), which is the right default; pin it when a policy has to name the floor in writing rather than inherit it from a dependency. A value this client cannot offer is refused at startup, never silently ignored. Process-wide: a hot-reload does not change it. | *(rustls default: 1.2 and 1.3)* |
| `APERIO_SERVER_TLS_CIPHER_SUITES` |  | `server.tls_cipher_suites` | Exact cipher suites offered when dialing the tunnel server, by their IANA names, comma-separated (e.g. `TLS13_AES_256_GCM_SHA384,TLS13_CHACHA20_POLY1305_SHA256`). Unset leaves rustls' preference order alone, which is almost always better; name them only when something external requires it. An unknown name, or a set that cannot meet `APERIO_TLS_MIN_VERSION`, is refused at startup. Process-wide. | *(rustls' own order)* |
| `APERIO_NAME` | `--name` | `name` | What to call this client on screen and in the server's logs, e.g. `eu_server_1`. Lowercase letters, digits and `_`, like a service name. **A label, not an address:** `client_id` stays the identity that failover recognizes this process by, because that needs something unique and a name is not. Three replicas an operator would happily call `eu_server` are three processes, and sharing a name must not make the server treat them as one. Unset shows the id. | the client id |
| `APERIO_CLIENT_ID` | `--client-id` | `client_id` | This client's instance id, a UUID kept across restarts so failover's `wait` mode recognizes the process when it comes back. Unset, one is generated on the first run and kept in `client_id_file`. | kept in `client_id_file` |
| `APERIO_CLIENT_ID_FILE` |  | `client_id_file` | Where a generated `client_id` is kept. By default a file under `~/.aperio/clients/` named after the absolute path of the config file the client reads, so two projects on one machine never share an id and the file never lands in a repository. A client started with no config file (a one-liner, or a container configured through the environment) has no default and runs with a fresh id each time; point this at a volume to keep one. A place that cannot be written costs persistence, never the start. | under `~/.aperio/clients/` |
| `APERIO_ALLOW_SERVER_FORWARD` |  | `allow_server_forward` | Answer a `forward` check (`via: client`) whose endpoint the **server** wrote, in its own visitor gate or a site's policy, rather than one of this client's services. Such a check makes the client call a URL the server chose, from the client's network, so it is refused unless this is set; the checks the client's own `access:` declares are always answered. A refused check refuses the visitor. See [the forward method](#the-forward-method). | `0` |
| `APERIO_BANDWIDTH` |  | `bandwidth` | Link capacity of this client's network, e.g. `8mbit`, `500kbit`, `2MB`, or plain bytes/second. The server paces outgoing tunnel frames (token bucket, 1 s burst) so this client is never pushed faster than its network can drain. It is a **budget for the whole client process**, not a per-service default: it is divided across the `services:` entries and each entry's parallel `connections`, so the total never exceeds it. See [Sharing the bandwidth budget](#sharing-the-bandwidth-budget). | unlimited |
| `APERIO_DEVICE_KEY` |  | `device_key` | Explicit device key announced for trust-on-first-use token pinning (server `security.token_pinning`): pins the token to this device so a leaked token replayed elsewhere is rejected. | none announced |
| `APERIO_DEVICE_KEY_FILE` |  | `device_key_file` | Path holding the device key, its contents are used, or a fresh random key is generated and persisted there (owner-only `0600`) on first run. Ignored when `APERIO_DEVICE_KEY` is set. |  |
| `APERIO_IDLE_TIMEOUT` |  | `idle_timeout` | Retire this client after it has served nothing for this long (e.g. `5m`); unset = never. The scale-in half of `scaling:`, see [Autoscaling](autoscaling.md). The shutdown is graceful, and the clock only starts after the first request. Every kind of traffic counts, not just buffered HTTP requests: each relayed frame of a WebSocket, TCP or UDP session re-stamps the clock in both directions, and retirement is held back while any request is still in flight (a slow backend, or a response streaming for longer than the window), so a live session is never cut mid-traffic. |  |
| `APERIO_LOG_FORMAT` |  | `log_format` | `json` or `pretty`. By default the client auto-detects: human-readable logs on an interactive terminal, JSON when stdout is not a TTY (Docker, pipes, service managers). | auto |
| `LOG_LEVEL` |  | `log_level` | Log verbosity; `RUST_LOG` overrides it. | `info` |
| `APERIO_MAX_MESSAGE_SIZE` |  | `max_message_size` | Max size of one tunnel message accepted from the server (memory protection). | `32MB` |
| `APERIO_MESSAGES_LISTEN` |  | `messages.listen` | Local address the message face listens on, so an application on this machine can subscribe and publish without speaking the tunnel protocol: `GET /subscribe?topic=<filter>` streams server-sent events, `POST /publish?topic=<topic>` sends the body. Loopback is the sensible value; anything else is warned about, since whoever can reach it can publish as this client. Unset = no local listener. | | unset |
| `APERIO_MESSAGES_MQTT_LISTEN` |  | `messages.mqtt_listen` | Local address an MQTT listener answers on, for an application that would rather use the MQTT client library it already has. MQTT 3.1.1, QoS 0 on subscriptions (a subscription asking for 1 or 2 is granted 0), no retained messages, no persistent sessions. A publish carrying QoS 1 is honored as at-least-once and acknowledged with `PUBACK`; QoS 2 has no exchange of its own and is published at-least-once. The protocol never leaves the machine. Unset = no MQTT listener. | | unset |
| `APERIO_OTEL_BRIDGE_LISTEN` |  | `otel_bridge.listen` | Address for the client's OTLP/HTTP receiver. Anything on the edge host exports to it with one environment variable and no SDK change. | `127.0.0.1:4318` |
| `APERIO_OTEL_BRIDGE_LISTEN_GRPC` |  | `otel_bridge.listen_grpc` | Address for the OTLP/gRPC receiver, for an SDK pinned to that transport. Unset = no gRPC listener. |  |
| `APERIO_OTEL_BRIDGE_QUEUE` |  | `otel_bridge.queue` | Exports held when the far end is not keeping up. Past this the newest is dropped and counted, never waited on: an exporter that cannot hand off its batch blocks the application it is instrumenting. | `256` |
| `APERIO_OTEL_BRIDGE_TRANSPORT` |  | `otel_bridge.transport` | How exports reach the server: `tunnel` sends them on the WebSocket the client already holds, which is what preserves the "one outbound connection" property; `https` posts them to the server's endpoint instead, for telemetry bursty enough that it should stay off the tunnel. | `tunnel` |
| `APERIO_PID_FILE` |  | `pid_file` | Path to write the process id to at startup, removed on a clean exit (not after a crash: a stale pid file would have an init system signalling whatever process now holds that number). A pid file that cannot be written is a warning, not a refusal to start. |  |
| `APERIO_RELOAD_DRAIN` |  | `reload_drain` | How long a configuration reload lets in-flight requests finish before the affected tunnel connections are dropped. The client announces `Draining` first, so the server stops dispatching to it and the wait actually terminates. `0s` drops at once. | `10s` |
| `APERIO_SCALING_COLD_START` |  | `scaling.cold_start` | How long a visitor request may be held while a cold start completes, e.g. `45s`. `0` answers immediately instead of holding. | `45s` |
| `APERIO_SCALING_COOLDOWN` |  | `scaling.cooldown` | Minimum gap between two calls for one bind. A new instance needs time to appear; without this the server would ask again while it is still starting. | `60s` |
| `APERIO_SCALING_MAX` |  | `scaling.max` | Ceiling the server will never ask to exceed. `0` means cold starts only, no scale-out. | `0` |
| `APERIO_SCALING_MIN` |  | `scaling.min` | Instances that should always be running. `0` opts into scale-to-zero: a request for an unserved hostname cold-starts instead of answering 504. | `0` |
| `APERIO_SCALING_SECRET` |  | `scaling.secret` | Sent as `Authorization: Bearer` on that call. Write-only: never echoed back, never logged. |  |
| `APERIO_SCALING_TARGET_UTILIZATION` |  | `scaling.target_utilization` | Pool utilization above which the server asks for one more instance, between 0 and 1. | `0.8` |
| `APERIO_SCALING_URL` |  | `scaling.url` | Endpoint the server POSTs to when a service of this client needs capacity. HTTPS and public addresses only, unless the operator opened those (see [Autoscaling](autoscaling.md)). Without it there is nothing to call, so the declaration is off. |  |
| `APERIO_SCALING_WINDOW` |  | `scaling.window` | How long utilization must stay above the target before scaling out. Guards against reacting to a single spike. | `15s` |
| `APERIO_SERVE_404` |  | `serve_404` | Static-file services: HTML file served with status 404 for misses the SPA fallback does not cover. Applies to every `serve:` directory of this client. |  |
| `APERIO_SERVE_SPA` |  | `serve_spa` | Static-file services: answer a navigation (`Accept: text/html`) that matches no file with the root `index.html` and status 200, so a client-side router owns its routes. A missing asset still 404s. Applies to every `serve:` directory of this client. Default: `false`. |  |
| `APERIO_SUBSCRIBE` |  | `subscribe` | Comma-separated topic filters this client subscribes to (the yaml form also accepts an object per entry, adding a required `topic:` and `env:` alongside `run:`/`timeout:`/`max_concurrent:` to run a command when a message arrives; `run:` is file-only), for messages from the other clients of its organization. MQTT filter syntax: `+` is one level, `#` is the rest. `$aperio/...` carries the server's own events (`$aperio/client/connected`, `$aperio/token/created`, …) and is never matched by a bare `#`, nor granted by one: the token's `topics` has to name it. Process-wide, not per service: a client running several services receives one copy, not one per service. |  |

#### A service: where its traffic goes

Written in a `services:` entry. On the command line and in the environment these describe the one service those two can run without a file; a target there runs on its own, in place of the file's `services:`.

| Env variable | CLI | yaml key | Description | Default |
| --- | --- | --- | --- | --- |
| `APERIO_TARGET` | positional | `target` | **Required.** Local backend to forward to. `http(s)://` (a bare port or hostname is normalized to it), or `h2c://` / `h2://` for HTTP/2 backends, gRPC: requests are dialed over HTTP/2 (cleartext prior knowledge / TLS), `te: trailers` is forwarded, and response trailers (`grpc-status`) are relayed to the visitor. The visitor leg must also be HTTP/2 for trailers to survive (aperio-server accepts h2c; have the fronting proxy forward gRPC as HTTP/2). `unix:///var/run/app.sock` forwards over a Unix domain socket (HTTP/1.1; Unix platforms only, WebSocket upgrades unsupported; each request dials the socket fresh, matching socket-activated backends). |  |
| `APERIO_SERVE` | `--serve` | `serve` | Serve a local directory of static files with a built-in file server instead of proxying to a backend, the directory takes the place of `target`. See [Static File Serving](static-serving.md) for SPA fallback, custom 404, streaming, `Range` support and the path-safety rules. |  |
| `APERIO_TCP_TARGET` |  | `tcp_target` | Bridge a raw local TCP service (experimental) rather than an HTTP backend, reached through the server's `/aperio/tcp` endpoint. |  |
| `APERIO_HOSTNAME` | `--hostname` | `hostname` | Hostname(s) this client serves. yaml `hostname:` accepts a single value or a list (`[a.example.com, b.example.com]`); the CLI/env value may be comma-separated. Each must be permitted by the client's token. `*` serves every hostname no other service claims, and needs a token not limited to particular hostnames; a service naming none is reached only by a hostname its token grants or a random subdomain. |  |
| `APERIO_PATH` | `--path` | `path` | Path prefix this client serves. |  |
| `APERIO_CUSTOM_NAME` |  | `custom_name` | What to call this service on screen (dashboard, client logs). Free text: any language, any punctuation, spaces, changeable at will, nothing addresses it. The key a service is written under is the handle that does, and it is an identifier (`a-z`, `0-9`, `_`). Available on a `tunnels:` entry too. |  |

#### A service: what it does with its traffic

Written in a `services:` entry for that service, or under `defaults:` for every service that does not write its own. The environment variable and the flag set the `defaults:` value: `APERIO_TIMEOUT=60s` is `defaults.timeout`, and a service that writes its own `timeout` keeps it.

| Env variable | CLI | yaml key | Description | Default |
| --- | --- | --- | --- | --- |
| `APERIO_ACCESS` | `--access` | `access` | Who may reach the service: `public`, one method block, or a list of them (any one admits). Unset, the server's own `access` decides, and a site's `access` on the server wins over this either way. | the server's gate |
| `APERIO_ALLOWED_IPS` | `--allowed-ips` | `allowed_ips` | Visitor IPs/CIDRs allowed to reach this service (comma-separated on the CLI/env, a list in yaml; e.g. `203.0.113.7,10.0.0.0/8`). Enforced by the server **per candidate, before dispatch**, blocked traffic never enters the tunnel. When several clients serve one route, each candidate is filtered by its *own* list and the request goes to any passing candidate (**union semantics**, deliberately fail-open: one unrestricted client joining a route opens it; use the token-level `allowed_ips` for route-wide lockdown). A visitor every candidate rejects gets the `denied:` redirect of the most-primary declaring candidate, or, without one, a **stealth answer identical to an unclaimed route** (504), so the route's existence never leaks to blocked IPs. Purely restrictive, no token permission needed. | everyone |
| `APERIO_CACHE_ENABLED` |  | `cache.enabled` | Opt this service into the server-side GET response cache (needs `cache.enabled` on the **server**; strictly `Cache-Control`-driven). | `false` |
| `APERIO_CACHE_OFFLINE` |  | `cache.offline` | Serve fresh or stale cached responses while no healthy client is connected, up to the server's `cache.max_stale` window. Needs `cache.enabled` here and caching on the server. Once a client reconnects, cache hits still apply and misses can reach the backend. | `false` |
| `APERIO_CIRCUIT_BREAKER_FAILURES` |  | `circuit_breaker.failures` | Consecutive backend failures that open the circuit breaker. While open the backend is **not dialed at all** and the visitor gets `502` immediately, so a dead backend stops being hammered once per request and nobody waits out a connect that will not succeed. `0` disables it. A response head of any status counts as a success: a `500` is a backend that is up, and refusing to dial it would turn an application error into an outage. | off, `3` when aggressive |
| `APERIO_CIRCUIT_BREAKER_OPEN_FOR` |  | `circuit_breaker.open_for` | How long the breaker stays open. After that one request is let through to probe the backend; if it fails, a fresh window starts. | `30s` |
| `APERIO_CONNECT_TIMEOUT` |  | `connect_timeout` | How long to wait for the TCP connection to a backend, separate from `timeout`, which covers the whole request. Per service: a backend across a VPN needs longer than one on loopback, and one number for both means either slow failure detection everywhere or spurious failures for the far one. Unset = only the whole-request budget applies. |  |
| `APERIO_CONNECTIONS` |  | `connections` | Parallel tunnel connections per service; the server load-balances across them like separate clients. The ceiling is the server's `limits.connections_per_service` (default 16), announced on connect and lowerable per token. More is not automatically faster, each connection costs CPU on both ends; see the note under [Multiple services](#multiple-services). | `1` |
| `APERIO_CONNECTIONS_MAX` |  | `connections.max` | Ceiling of an elastic pool. The server's `limits.connections_per_service` still wins over it. | `min` |
| `APERIO_CONNECTIONS_MIN` |  | `connections.min` | Floor of an elastic pool: `connections: {min: 1, max: 8}` opens one connection and grows towards eight while requests queue up. Writing either half turns the setting into a range; a plain number stays a fixed pool. See [Elastic connections](#elastic-connections). | `1` |
| `APERIO_DENIED` |  | `denied` | Absolute http(s) URL a fully rejected visitor (see `allowed_ips`) is redirected to (302). Declared via the tunnel handshake; unset = the stealth answer. | stealth |
| `APERIO_HEADERS_REQUEST_ADD` |  | `headers.request.add` | Headers to set, name → value; replaces an existing header of the same name. |  |
| `APERIO_HEADERS_REQUEST_REMOVE` |  | `headers.request.remove` | Header names to strip (case-insensitive). |  |
| `APERIO_HEADERS_RESPONSE_ADD` |  | `headers.response.add` | Headers to set, name → value; replaces an existing header of the same name. |  |
| `APERIO_HEADERS_RESPONSE_REMOVE` |  | `headers.response.remove` | Header names to strip (case-insensitive). |  |
| `APERIO_HEALTH_ENDPOINT` |  | `health.endpoint` | Health endpoint of the local target (path like `/health`, or a full URL). When set, the client probes it independently and reports the result to the server: a failing backend takes the client **out of routing without dropping the tunnel**; it rejoins automatically when the probe recovers. The dashboard shows a `BACKEND DOWN` badge meanwhile. **The service starts *unhealthy* (out of routing) until the first probe succeeds**, the client never claims a backend is up before it has checked it, and the first probe runs immediately at startup, so a healthy backend becomes routable within a probe (not a probe interval). Before that first probe completes the dashboard shows a **CHECKING** badge (rather than *BACKEND DOWN*), so "not probed yet" is distinguishable from "probed and down". Probes never follow redirects. Against an `h2c://`/`h2://` target the value names the **gRPC service** to health-check instead, and the probe calls `grpc.health.v1.Health/Check`: a plain GET cannot reach a server that speaks HTTP/2 with prior knowledge and routes by method name. `/` asks about the server as a whole. An absolute `http(s)://` URL still means an ordinary HTTP probe. |  |
| `APERIO_HEALTH_INTERVAL` |  | `health.interval` | Time between backend health probes. | `10s` |
| `APERIO_HEALTH_THRESHOLD` |  | `health.threshold` | Consecutive probe failures before the backend is reported unhealthy. | `2` |
| `APERIO_HEALTH_TIMEOUT` |  | `health.timeout` | Per-probe timeout. | `5s` |
| `APERIO_HEALTH_WAIT_FOR_BACKEND` |  | `health.wait_for_backend` | Startup gate: hold the service **out of routing until the backend first accepts a connection**, avoiding the connection-refused window while a slow dev server boots. Connection-level only (a probe per second); once the backend is up the gate never re-engages. Superseded by `health.endpoint`, which gates startup *and* tracks health continuously. | `0` |
| `APERIO_INSPECTOR` |  | `inspector` | Record this service's transactions for the dashboard's request inspector. On by default; `inspector: false` (or `APERIO_INSPECTOR=0`) trades the ability to inspect and replay this service's requests for the per-request cost of recording them. Live traffic, statistics and the access log are unaffected. | `true` |
| `APERIO_LIMITS_ADAPTIVE_CONCURRENCY` |  | `limits.adaptive_concurrency` | Move the announced `limits.concurrent_requests` with backend pressure: halve it while requests queue waiting for a local permit, climb back one at a time when they stop. The server already queues rather than dispatching past the announced number, so a client that has become slow simply stops being sent work it cannot do; the server then holds the request, picks another client in the pool, or asks for capacity through autoscaling, all of which beat a refusal. Needs `limits.concurrent_requests` set, since that is the number being moved, and never leaves the band `1..=max_concurrent`. See [Client Resilience](client-resilience.md). | `false` |
| `APERIO_LIMITS_CONCURRENT_REQUESTS` |  | `limits.concurrent_requests` | Max concurrent requests; announced to the server, which queues the excess instead of flooding the backend. Also enforced locally. | unlimited |
| `APERIO_LIMITS_REDIRECTS` |  | `limits.redirects` | Backend redirects followed transparently: same-host scheme upgrades (`http://x` → `https://x`) and hops within the same root domain (`example.com` → `test.example.com`), never downgrading https to http. Redirects beyond this many jumps, or to unrelated hosts, pass through to the visitor unchanged. `0` disables following entirely. | `5` |
| `APERIO_LIMITS_REQUEST_BODY` |  | `limits.request_body` | Largest request body visitors may upload to this service, e.g. `10MB`. Announced to the server, which rejects bigger uploads with an early **413** before the body ever enters the tunnel. Can only tighten the server's global `limits.request_body`, never widen it. | server limit only |
| `APERIO_LIMITS_RESPONSE_BODY` |  | `limits.response_body` | Largest backend response this client relays, e.g. `50MB`; bodies over 32 KB are streamed through the tunnel as binary chunks rather than base64 in one message (256 KB against a server too old for binary frames). A response that grows past this limit before any of it was sent is refused with a `502`; one already streaming is aborted, so the visitor sees a broken response rather than a silently truncated success. | `50MB` |
| `APERIO_METRICS_LABELS` |  | `metrics_labels` | Static Prometheus labels attached to this client's own `aperio_client_requests_total` series, so one Prometheus can serve several environments without relabelling rules. YAML takes a mapping (`{env: prod, region: eu-west}`); the environment spelling is a flat `env=prod,region=eu-west` list, because that is what a container platform can inject. The server validates and caps what arrives, at most 8 labels, names must be `[a-zA-Z_][a-zA-Z0-9_]*` and at most 32 characters, values at most 64 characters, and both the names the server writes itself (`client_id`, `job`, `instance`, `token`, `hostname`, `limit`, `service`) and any `__`-prefixed name are refused. An invalid label is dropped and the rest are kept. |  |
| `APERIO_MIN_TLS_VERSION` |  | `min_tls_version` | Lowest TLS version accepted from an `https://` backend, `1.2` or `1.3`. Per service so a fleet with one legacy backend does not have to lower the floor for all of them. A value that is not a TLS version is refused at startup rather than ignored: a typo that quietly leaves the floor where it was is what makes a security setting worse than none. Unset = rustls's own floor. |  |
| `APERIO_MULTIPLEX` |  | `multiplex` | Carry every service that sets this on one WebSocket instead of opening a connection each: one socket, one TLS session, one reader, one writer and one heartbeat for all of them, rather than that many of each on both ends. Written under `defaults:` it turns on once, and a service still keeps a connection of its own with `multiplex: false`. Needs two services; the one service of the command line never multiplexes. `connections:` is not honored for a multiplexed service. See [One connection for several services](#one-connection-for-several-services). | `false` |
| `APERIO_PASS_HOSTNAME` | `--pass-hostname` | `pass_hostname` | Forward the original `Host` header instead of the target's. | `0` |
| `APERIO_PRIORITY` | `--priority` | `priority` | Load-balancing priority tier announced to the server (0 = primary, higher = standby; effective with `routing.lb_strategy: primary-standby`). | `0` |
| `APERIO_RESILIENCE` |  | `resilience` | How hard this service works to answer when its backend fails: `off`, `safe` (an idempotent request that fails before any response is tried once more) or `aggressive` (three attempts, and a circuit breaker). `retry` and `circuit_breaker`, where written, win over the profile's values. | `safe` |
| `APERIO_RESPONSE_TIMEOUT` |  | `response_timeout` | This service's own `gateway.response_timeout`: how long the server waits for it to answer a dispatched request, for slow report and upload endpoints. Unset = the server's. | server global |
| `APERIO_RETRY_ALL_METHODS` |  | `retry.all_methods` | `1` = also retry non-idempotent methods (POST, PATCH). Off by default because a retried write may reach the backend twice, the same trade as the server's `resilience.failover.all_methods`. | `0` |
| `APERIO_RETRY_ATTEMPTS` |  | `retry.attempts` | Total attempts for a backend request that fails **before any response arrives** (a refused connection, a reset, a timeout on the head). `1` = no retrying, which does not cover a connection the backend had already closed: see the note under this table. Values are clamped to `1..=10`, so a larger number silently becomes 10. Only idempotent methods are retried unless `retry.all_methods` says otherwise, and only requests whose body can be replayed: a streamed upload is consumed by the first attempt, so it is never retried. Once a response head has arrived the request is past this point, so an error later in the body is not retried either. | the profile's: `2`; `1` off, `3` aggressive |
| `APERIO_RETRY_DELAY` |  | `retry.delay` | Wait before the second attempt, doubled before each further one (`100ms`, `1s`). | `100ms`, `50ms` when aggressive |
| `APERIO_SECURITY_HEADERS` |  | `security_headers` | Security response-header preset: `true` injects HSTS, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and `Referrer-Policy`; a mapping picks headers individually. |  |
| `APERIO_SECURITY_HEADERS_CSP` |  | `security_headers.csp` | `Content-Security-Policy` value to inject (no default, CSP is application-specific). |  |
| `APERIO_SECURITY_HEADERS_FRAME_OPTIONS` |  | `security_headers.frame_options` | `X-Frame-Options` value to inject. |  |
| `APERIO_SECURITY_HEADERS_HSTS` |  | `security_headers.hsts` | Inject `Strict-Transport-Security` (only meaningful behind HTTPS). |  |
| `APERIO_SECURITY_HEADERS_HSTS_MAX_AGE` |  | `security_headers.hsts_max_age` | HSTS `max-age`. Default: `730d` (two years). | `730d` |
| `APERIO_SECURITY_HEADERS_NOSNIFF` |  | `security_headers.nosniff` | Inject `X-Content-Type-Options: nosniff`. |  |
| `APERIO_SECURITY_HEADERS_REFERRER_POLICY` |  | `security_headers.referrer_policy` | `Referrer-Policy` value to inject. |  |
| `APERIO_SERVER_SIDE` |  | `server_side` | Ask the server to reach this service's `target` itself instead of relaying through this client, when the server's `server_side_targets:` permits the address and the token carries `allow_server_side`. Saves the two hops a relayed request makes. Written at the top level it is the default for every `services:` entry, and it also reaches the single service of a file-less client; an entry may opt out with `server_side: false`. Cannot be combined with `serve:`, whose files are on this machine. See [Serving From the Server](server-side-services.md). | `0` |
| `APERIO_STARTUP_DELAY` |  | `startup_delay` | How long a service waits before opening its tunnel, for a backend that starts alongside the client and is not ready the moment the process is. Only the first connection of a parallel pool waits; the rest are the same service, and making each wait would turn a stagger into a per-connection delay. | `0s` |
| `APERIO_TIMEOUT` |  | `timeout` | Per-request backend timeout. | `30s` |
| `APERIO_TRIM_BIND` |  | `trim_bind` | Strip the path bind prefix before forwarding. | `1` when a path bind is set |
| `APERIO_WEBHOOK_INBOX` |  | `webhook_inbox` | Persist every inbound **POST** to this service into the server's webhook inbox (dashboard *Webhook Inbox* page): browse the payloads and re-fire any event to the connected client. Restart-surviving, newest 500 entries kept. | `0` |

#### Sections written only in a file

A map of entries has no flat spelling, so these live in the file (or in `~/.aperio.yaml`):

| Key | What it holds |
| --- | --- |
| `services` | The backends this client exposes, keyed by name, see [Multiple services](#multiple-services). |
| `defaults` | What every service gets unless its own entry says otherwise: any key of the service tables above, apart from where its traffic comes from and goes to (and `depends_on`, `bandwidth`, which belong to one service). |
| `tunnels` | Private services a peer may bind, keyed by the name binders use: `target`, `protocol` (`tcp`, `udp`, `tcp/udp`), `encrypt`, `psk`, `proxy_protocol`, `idle_timeout`, `custom_name`. See [Tunnels](emergency-tunnels.md). |
| `binds` | Tunnels `aperio-client bind` listens for, keyed by tunnel name: a local port, or `{port, address, token, psk}`. |
| `subscribe` | Topic filters, each a bare filter or `{topic, run, env, timeout, max_concurrent}` to run a command per message. `APERIO_SUBSCRIBE` carries bare filters only: a command to run is written in the file. See [Messaging](messaging.md). |
| `include` | Other files to read first, see [Splitting the config across files](#splitting-the-config-across-files-include). |

### aperio.yaml & ~/.aperio.yaml

```yaml
# ~/.aperio.yaml, user-level defaults shared across projects
server:
  url: https://tunnel.example.com
  token: ${APERIO_SERVER_TOKEN}
```

```yaml
# ./aperio.yaml, per-project service description
services:
  web:
    target: http://localhost:3000
    hostname: app.example.com
    path: /api
    access: public
    limits:
      concurrent_requests: 8
    health:
      endpoint: /health   # probe the backend; report unhealthy without dropping the tunnel

defaults:                 # every service, unless its entry says otherwise
  timeout: 60s
  health:
    interval: 10s
```

**A file describes its backends under `services:`, keyed by name, even when there is one.** The key is the service's name, the identifier the dashboard and the logs use (`a-z`, `0-9`, `_`); `custom_name:` is where anything human goes. A service key written at the top level (`target:`, `hostname:`, ...) is refused with a message saying where it goes, and a key that every service shares is written once under `defaults:`.

A one-liner still runs a single service, on the command line or in the environment:

```bash
# the positional target is the one service
aperio-client http://localhost:3000 \
  --server-url https://tunnel.example.com --server-token apr_xxxxxxxxxxxxxxxx \
  --hostname app.example.com --path /api --access public
```

```bash
# the same thing, for a container or a unit file
export APERIO_SERVER_URL=https://tunnel.example.com
export APERIO_SERVER_TOKEN=apr_xxxxxxxxxxxxxxxx
export APERIO_TARGET=http://localhost:3000
export APERIO_HOSTNAME=app.example.com
export APERIO_PATH=/api
export APERIO_ACCESS=public
aperio-client
```

A `hostname` or `path` given that way with no target is refused rather than ignored: it describes a service, and there is none to put it on.

### Splitting the config across files (`include:`)

One `aperio.yaml` stops scaling when there are twenty services, or when different teams own different entries. `include:` reads other files first:

```yaml
# aperio.yaml
include:
  - shared/defaults.yaml   # relative to *this* file, not the working directory
  - services/prod.yaml
defaults:
  timeout: 30s             # this file wins over anything an include set
services:
  web:                     # added next to the services the includes named
    target: http://localhost:3000
```

The rule is the one every layer uses: **mappings merge key by key, and anything else is replaced.** A fragment adds a service by naming it, sets a default under `defaults:`, and a list (`allowed_ips:`, `subscribe:`) is the value the last file to write it gave. Includes are merged in the order written, so a later one wins over an earlier one, and the including file wins over all of them.

Paths resolve relative to the file that wrote them, so a fragment means the same thing whichever directory the client is started from. Chains may nest five deep; a cycle, a missing file, or a malformed `include:` is an error naming the file rather than a silently partial configuration. Every file is checked on its own, so a key nothing reads is reported against the fragment that wrote it. Hot-reload watches every file that contributed, not just the root, and re-reads the set on each change so adding or removing an include is picked up too.

### Multiple services

One client process can expose several targets: add an entry per backend to `services:`, and the client opens one tunnel connection per entry (or shares one, with `multiplex`), each with its own binds, health probe, and knobs:

```yaml
# ./aperio.yaml (client)
server:
  url: https://tunnel.example.com
  token: ${APERIO_SERVER_TOKEN}
defaults:
  access: public
services:
  web:
    target: http://localhost:3000
    hostname: app.example.com
    health:
      endpoint: /health
  api:
    target: http://localhost:4000
    hostname: api.example.com
    limits:
      concurrent_requests: 8
  docs:
    target: http://localhost:5000
    path: /docs
```

An entry takes every key of the service tables above. `target` (or `serve`, a local directory of static files served as the service, or `tcp_target`, a raw TCP backend) is what it exposes; `hostname`, `path`, `custom_name`, `depends_on` and `bandwidth` belong to the one service. Everything else may also be written under `defaults:`, and **an entry is laid over `defaults:` by the merge rule**: a key the entry writes wins, a block it writes (`health:`, `limits:`) is merged with the default block field by field, and a key it does not write is the default's. The environment and the command line write `defaults:`, so `APERIO_TIMEOUT=60s` reaches every service that does not write its own `timeout`. The one exception to "a default is copied into each service" is the top-level `bandwidth`, which is a budget the services share rather than a value each of them gets.

Services run in the order the file writes them, and each runs under the client's id suffixed with its position.

**Backend resilience (`retry:` and `circuit_breaker:`).** The server can fail a request over to another *client* and eject one that keeps misbehaving; neither helps the client whose own backend is refusing connections. These two blocks cover that hop, per service or under `defaults:`. They apply to every backend scheme: plain HTTP, `h2c://` / `h2://`, and `unix://`.

```yaml
services:
  api:
    target: http://localhost:3000
    retry:
      attempts: 3        # total, including the first; 1 = off (the default)
      delay: 100ms       # before the second attempt, doubled after that
      all_methods: false # POST/PATCH are not retried unless this is true
    circuit_breaker:
      failures: 5        # consecutive failures that open it; 0 = off (default)
      open_for: 30s      # before one request probes the backend again
```

Retrying only covers failures that happen *before* a response head arrives, and only requests that can be replayed: a streamed upload is consumed by its first attempt. The breaker counts any response head as a success regardless of status, because a `500` is a backend that is up and answering.

**One case is handled whatever `retry:` says.** The client keeps backend connections alive and reuses them, and the backend closes idle ones on its own schedule, so there is a window where a request is written onto a socket the backend has just finished with. Nothing is wrong with the backend, and no response head arrives; under load that window is hit constantly. The client re-dials once, silently, for a request the same fences already allow to be replayed (an idempotent method, a replayable body). This is not the retry policy, which is about a backend that failed, and it is why the same backend behind any mainstream proxy does not answer with a scattering of `502`s under load. A second such failure on the same request is a backend genuinely closing on the client, and it reaches the visitor.

`connections: N` (default 1) opens N parallel tunnel connections for a service. How many N may be is the server's decision, `limits.connections_per_service` (default 16), lowerable per token and announced on connect: a client asking for more opens what it is allowed and says so in its log. The server pools them like separate clients, its load-balancing strategy spreads requests across them, so a single service is no longer serialized behind one WebSocket under heavy parallel traffic. Each connection gets its own instance id (`<id>`, `<id>-c2`, `<id>-c3`, …), so the dashboard's shared-id warning is not triggered and failover lookups stay unambiguous; `limits.concurrent_requests` applies per connection. Config hot-reload re-resolves every service, so adding or removing one doesn't need a restart.

More connections is not automatically faster: each one costs a reader and a writer task on both ends, and its own sockets and queues. Raising N pays off when the bottleneck is the network, a long round-trip or a single TCP stream's throughput ceiling between client and server. When client and server share a machine's CPU (loopback, a dev box, co-located containers), small values win; in a loopback bulk-throughput measurement the curve peaked at 4 connections and fell past it, with 10 slower than 1. Start small, and let a measurement on your own deployment justify each increase.

### Template variables

`${NAME}` and `${NAME:-default}` in a client config file are expanded from the environment before the yaml is parsed, so one file can serve several environments instead of being copied per environment, which is how two files drift:

```yaml
server:
  token: ${APERIO_TOKEN}
services:
  web:
    target: http://localhost:3000
    hostname: ${ENV:-dev}.example.com
```

Only this spelling is expanded. A bare `$NAME` is left exactly as written, because `$` appears in generated passwords, regular expressions and the shell snippets inside `run:` commands, and a config loader that rewrote those would corrupt files that work today. A variable that is not set and has no default is an error naming the variable, not an empty string: substituting nothing produces a file that still parses and means something else (`hostname: .example.com`, or an empty token), which then fails somewhere unrelated. Expansion happens per file, so an included fragment reports its own name.

This is a **client** feature. The server's settings expand nothing, so a `${VAR}` written in a settings document is the value itself, not a reference to one: an `oidc.client_secret` spelled that way is a secret that reads as a placeholder and is neither. Set a server credential from the dashboard instead, where it is stored and never read back.

### Literal secrets

`aperio-client check` warns when a credential (`token`, `psk`, `client_secret`, `password`, `api_key`, `device_key`, or a generic `secret`) is written into the file literally rather than as `${VAR}`. A warning, not a failure: it is a working configuration, and where the file is a private deploy artifact it may be the deliberate one. But a secret typed into a file ends up in a repository, a backup and a support ticket, and the alternative costs one `${VAR}`.

### Serving a service from the server (`server_side:`)

A service whose target the *server* can already reach does not need the two
hops through this client. `server_side: true` on the entry asks the server to
send the request straight to `target` instead:

```yaml
services:
  internal_api:
    target: http://10.0.0.5:8080
    hostname: api.example.com
    server_side: true
```

It takes two permissions that have to agree: the server's
`security.server_side_targets` must name the address, and the token must carry
`allow_server_side`. Neither is enough alone, an unset list permits nothing,
and a target outside it is refused rather than relayed. `serve:` cannot be
combined with it.

See [Serving From the Server](server-side-services.md) for what it refuses and
why, the WebSocket half, and what the last hop gives up.

### Service start order

A service can name others it should follow:

```yaml
services:
  db:
    tcp_target: 127.0.0.1:5432
  api:
    target: http://localhost:3000
    depends_on: [db]
    startup_delay: 2s
```

`depends_on` waits for those services to have a live tunnel, then **proceeds anyway** after 60 seconds: a dependency that never arrives must not keep a service that could be serving traffic off the air forever. The wait is bounded and what it gave up on is logged. It is written on the service that waits, never under `defaults:`.

"Has a live tunnel" means right now, not "did at some point": a service that loses its connection stops counting as ready, and one with `connections: N` counts as ready while any of its connections is up. It orders startup and nothing more, though. Once a service is past its own gate it stays up whatever its dependency does afterwards, because taking a healthy service off the air over someone else's outage turns one failure into two.

A name that is not a service in this configuration, a service depending on itself, and a cycle are all refused at startup. All three would otherwise be invisible: at runtime every one of them ends with everybody waiting out the grace period and then starting anyway, which looks exactly like working.

### Elastic connections

A plain `connections: N` opens N connections at startup and keeps them, whether or not there is traffic. Written as a range, the pool sizes itself:

```yaml
services:
  api:
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

Environment: `APERIO_CONNECTIONS_MIN` / `APERIO_CONNECTIONS_MAX`. The server's `limits.connections_per_service` still wins over `max`, and the `bandwidth:` share is divided by `max` (never by the current size), so growing the pool cannot exceed the budget the file declares. The dashboard shows the connections a service actually has open, not its ceiling.

A range is opt-in for a reason: as the note above says, more connections is not automatically faster, and on a host where client and server share CPU the curve has a peak. `connections: N` keeps behaving exactly as it always did.

### One connection for several services

`connections:` is about giving one service several sockets. `multiplex: true` is the opposite question, and it is the one an operator with forty services asks: can several services share one socket?

```yaml
defaults:
  multiplex: true        # every service, unless it says otherwise
services:
  web:
    target: http://localhost:3000
    hostname: web.example.com
  api:
    target: http://localhost:4000
    hostname: api.example.com
  bulk:
    target: http://localhost:5000
    hostname: files.example.com
    multiplex: false     # large responses, keep a connection of its own
```

`web` and `api` are carried on one WebSocket; `bulk` opens its own. What that saves is per service and it is the whole cost of a connection: one socket, one TLS session, one reader task, one writer task and one heartbeat for the group, rather than that many of each on both ends. Forty services on one client is forty of everything today, and it is the same forty on the server.

What it costs is that the services share a link. They send through one writer, so a large response occupies it while the others wait their turn, and they share a fate: the connection dropping takes all of them out of routing together, and brings all of them back together. That is why it is opt-in and why `multiplex: false` on the one service whose responses are large is worth writing.

Each service is still its own service everywhere it matters. The server routes to it by its own binds, gates it with its own `access:` and `allowed_ips:`, ejects it on its own backend failures without touching its neighbours, and shows and controls it separately in the dashboard. On the client, each keeps its own backend, header rules, timeouts, circuit breaker, health probe and `limits.concurrent_requests`. The one service of the command line has nobody to share with and never multiplexes.

**`connections:` is not honored for a multiplexed service.** One connection is what multiplexing means. The client reports the difference in the dashboard's config view rather than dropping it silently, so a `connections: 4` that stopped applying is visible.

### Sharing the bandwidth budget

The server shapes each tunnel connection with a token bucket of its own, so a client that announced the same rate on every connection could be pushed at N times that rate. The client therefore divides what it announces instead of repeating it: a service's share is split across its `connections`, and the top-level `bandwidth` is a budget the services are settled against.

- **A service's own limit is divided by its connections.** `connections: 10` with `bandwidth: 10mbit` announces 1mbit per connection.
- **Services that named a rate keep it; the rest split what is left equally.** With a 10mbit budget, `x: 3mbit` and an unset `y` gives y 7mbit; add an unset `z` and y and z take 3.5mbit each.
- **With no top-level budget nothing is settled.** A service that named a rate gets it, a service that did not stays unlimited.
- **Requests that starve the others are dropped, with a warning.** If the named rates leave nothing of the budget for the services without one (2mbit of a 2mbit budget, or more), every named rate is ignored and the budget is split equally, since a service configured to run cannot be given zero.
- **Requests that overshoot are scaled, with a warning.** When every service named a rate and together they exceed the budget, the rates keep their relative weight and are scaled to fit: `3mbit` and `7mbit` under a 5mbit budget become 1.5mbit and 3.5mbit.

The budget is split per **service**, not per connection, so a service does not get a bigger share by opening more connections. Each service logs the rate a single connection of it announces at startup. Note the trade-off of per-connection shaping: with `connections: N` a single transfer runs over one connection and therefore sees `1/N` of the service's share, even while its siblings are idle.

### Header rules

A `headers:` block (on a service, or under `defaults:`) edits proxied HTTP traffic on the client: `request` rules apply to what the local backend receives, `response` rules to what the visitor receives. `add` sets a header, replacing any existing value of the same name; `remove` strips headers case-insensitively:

```yaml
# ./aperio.yaml (client)
defaults:
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

A service's own `headers:` is laid over the default one by the merge rule: an `add` map merges name by name, and a `remove` list it writes replaces the default's. Hop-by-hop and tunnel-critical headers (`Connection`, `Upgrade`, `Sec-WebSocket-*`, …) stay managed by Aperio regardless of these rules, and WebSocket upgrade traffic is not affected. From the environment: `APERIO_HEADERS_REQUEST_ADD=X-Env=staging`, `APERIO_HEADERS_RESPONSE_REMOVE=Server,X-Powered-By`.

### Security header preset (`security_headers:`)

`security_headers:` (on a service, or under `defaults:`) injects standard security response headers without hand-writing `headers:` rules. `security_headers: true` enables the standard set, `Strict-Transport-Security: max-age=63072000`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, and `false` opts a single service out of a default preset. A mapping picks headers individually:

```yaml
# ./aperio.yaml (client)
defaults:
  security_headers:
    hsts: true                  # Strict-Transport-Security (only meaningful behind HTTPS)
    hsts_max_age: 365d          # optional, default 730d (2 years)
    frame_options: SAMEORIGIN   # X-Frame-Options: SAMEORIGIN or DENY (anything
                                # else is sent as DENY, a browser acts on those
                                # two and ignores a typo, which would leave a
                                # header that only looks like protection)
    nosniff: true               # X-Content-Type-Options: nosniff
    referrer_policy: strict-origin-when-cross-origin
                                # an unrecognized value is sent as
                                # strict-origin-when-cross-origin, for the same
                                # reason frame_options falls back to DENY
    csp: "default-src 'self'"   # Content-Security-Policy (no default, app-specific)
```

Preset headers replace whatever the backend sent for the same name, but explicit `headers:` rules always win: a name you `add` or `remove` yourself is left to your rules. From the environment: `APERIO_SECURITY_HEADERS` (the whole preset on or off) and the granular `APERIO_SECURITY_HEADERS_HSTS`, `APERIO_SECURITY_HEADERS_HSTS_MAX_AGE`, `APERIO_SECURITY_HEADERS_FRAME_OPTIONS`, `APERIO_SECURITY_HEADERS_NOSNIFF`, `APERIO_SECURITY_HEADERS_REFERRER_POLICY` and `APERIO_SECURITY_HEADERS_CSP`.

### Visitor authentication

A service's `access:` says who may reach it. It is one grammar wherever a route's access is written: a client's service, a [site](#sites), and the server's own `access`. On the server it is what a route nothing else declares answers; on a client's service it speaks for the traffic routed there; on a site it is the operator's word on that hostname, and wins over what the clients serving it declare.

```yaml
access: public                                     # anyone, with no login
access: { method: basic, users: "admin:s3cret" }   # one method
access:                                            # several: any one admits
  - method: basic
    users: [admin:s3cret, ops:hunter2]
  - method: bearer
    secret: ${API_SECRET}
```

The list is **any-of**: a visitor is admitted by the first method that admits them, which is what lets one route say "a browser signs in, a script presents a key". Unset, the server's own `access` decides. Six methods exist, simplest first:

| `method:` | What it does | Where it may be written |
| --- | --- | --- |
| `none` | Deliberately open. The long spelling of `access: public`, and it needs the same token permission. | both |
| `basic` | A `user:password` login. `users:` takes one credential or a list, all alternatives on the same gate. | both |
| `bearer` | An opaque secret presented as `Authorization: Bearer <secret>`. `secret:` takes one or a list, so a key rotates by adding the new one before withdrawing the old. | both |
| `jwt` | A token the visitor already holds, verified against the keys its issuer publishes (`jwks_url:`) or a shared secret (`hmac_secret:`). No round trip per request. | both |
| `forward` | Ask an endpoint you run about each request: `2xx` admits it, anything else refuses it and the endpoint's own answer is what the visitor gets. `via: client` asks over the tunnel, so the endpoint can live on the client's network; a client-written `forward` must say so. | both |
| `sso` | A visitor's sign-in at an identity provider: `issuer`, `client_id`, `client_secret`, and `allowed_emails` (exact addresses, `*@domain`, or `*`), optional `scopes`. It mints a session for the hostname alone, on the visitor plane, so letting somebody see a site never gives them a role on Aperio; the email is checked again on every request. The provider lists `https://<hostname>/aperio/sso/callback` for each hostname the gate covers. | server only |
| `aperio` | This server's own sign-in: a dashboard session (the master token, a named user, a passkey or OIDC) reaching the hostname's organization. No credential to invent, which is what `auth: DUMMY:DUMMY` was standing in for. Alone, it admits the admin plane only, so the site's visitor password does not open it; beside `bearer`, a person signs in and a script presents a key. | both |

The set is closed on purpose rather than being a plugin interface, and `forward` is what lets it stay closed: anything deliberately left out (LDAP, SAML, a rule nobody anticipated) is thirty lines behind that URL, in a process that is not Aperio's, with a contract that is two HTTP messages rather than an ABI. A `method:` this build does not know **refuses the start** and names the ones it does, so a gate is never silently absent. `oidc` as a visitor method is still its own piece of work.

#### How a refusal is shaped

A browser navigation (a `GET` carrying `Accept: text/html`) is sent to the login page, the OIDC one where that is configured. Anything else, where the gate has a method a caller can satisfy on the request itself (`bearer` or `jwt`), is answered `401` with `WWW-Authenticate: Bearer`; where the gate is `aperio`, `401` without a challenge, since there is no header a script could answer with. Redirecting a script to an HTML login form answers a question it did not ask, and it is why a gated route could not be reached with `curl` at all. A `forward` refusal is the endpoint's own answer, whatever it chose.

So one route serves a person and a script each in the form they can act on:

```yaml
access:
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
access:
  - method: forward
    url: http://127.0.0.1:7070/_authcheck
    request_headers: [cookie, authorization]   # the default
    response_headers: [x-auth-user]            # empty by default
    timeout: 5                                 # seconds; a timeout refuses
    cache: 30                                  # seconds; 0 (default) asks every time
    via: server                                # or client: ask over the tunnel, see below
```

What nginx spells `auth_request` and Traefik spells ForwardAuth. Five decisions, each deliberate:

**What crosses, to the endpoint.** A `GET` describing the request rather than replaying it: `X-Forwarded-Method`, `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Uri`, `X-Forwarded-For`, plus the request headers you name. The default is `cookie` and `authorization`, the two that carry an identity, rather than everything the visitor sent: handing the endpoint the whole request makes every header it happens to read part of a contract nobody wrote down.

**What crosses back.** Only the response headers you name, and the list is empty by default. It is how the pattern delivers an identity onto the request that reaches your backend, and an open list is how it becomes a header injection.

A visitor's own copy of a name on that list is **dropped**, always, whether or not the endpoint answered with one. You named those headers so your backend could trust what is in them, and a request arriving with `X-Auth-User: admin` already on it would otherwise reach the backend alongside the endpoint's answer, where most frameworks read the first of the two. This is the same rule the `x-aperio-` namespace has, and for the same reason.

**A timeout refuses.** An auth gate that opens when its check is unreachable is not a gate. This means the endpoint's availability becomes the route's, which is the trade you are making, and it is stated rather than discovered.

**A refusal is the endpoint's own answer**, relayed with its status, `Location`, `WWW-Authenticate`, `Content-Type` and `Set-Cookie`, so it can send a browser to a login of its own. Redirects from the endpoint are deliberately not followed: a `302` is an answer for the visitor, not a request for Aperio to make.

**`via: client` asks over the tunnel.** The endpoint that decides can live on the *client's* network, deliberately not reachable from the internet: the server, holding a request it cannot yet admit, asks the connection that would serve it, the client calls its own internal endpoint (`localhost:7070` then means the client's localhost), and the verdict comes back the way a response does. A `forward` written on a client's service must say `via: client`, so the same word never means two networks in two files; the server refuses to dial a URL a client chose, whatever the entry says. What crosses is what an endpoint of the server's own would receive, composed on the server, less Aperio's own cookies (the dashboard session, share-link and affinity cookies), which are addressed to Aperio and never reach a client; what comes back is a status and the allow-listed headers, never a body. Three things are different over a tunnel, all deliberate: the check spends from the visitor's IP bucket at the price of a credential attempt, and a connection holds at most 32 open checks, so a flood of unauthenticated visitors cannot be a denial of service performed by the site's own gate; `cache:` defaults to `30` here, since the cold path is two round trips before the visitor's request starts; and "unreachable" has more shapes, disconnected, draining, saturated, too old to answer, every one of which refuses, so during a reload's handover the site cannot be logged into either. It replaces the workaround of publishing the auth endpoint as its own bearer-gated service, which cost a public hostname, a second gate to rotate and a full trip out through the proxy and back. A client too old to answer holds the service back at the handshake, as with any method it cannot carry; a server-written `via: client` asked of such a client refuses after the timeout. A client calls only the endpoints its own `access:` declares unless it sets `allow_server_forward: true` (`APERIO_ALLOW_SERVER_FORWARD=1`): a `via: client` the server writes would otherwise let the server send requests anywhere on every client's network, so each client that should answer one has to agree to, and one that has not refuses the visitor and says so in its log.

**`cache:` remembers a verdict** for an identical *request*, so a busy route does not pay a round trip per page load. Only admissions are remembered, never refusals: somebody who has just been given access must not keep being turned away for the rest of the window. The key is a hash of everything the subrequest carried, the endpoint, the hostname, the method, the path, the visitor's address and the credential headers, so no secret is held in it and a `yes` for one path, or for one address, is never a `yes` for another. Your endpoint is told the method and the path and may answer on them, which is the ordinary way this pattern is used, so a cache that keyed on the credential alone would quietly turn a per-request authorization into a per-session one.

The URL goes through the server's [outbound policy](threat-model.md), like every other destination the server is told to call.

#### The `jwt` method

```yaml
access:
  - method: jwt
    jwks_url: https://accounts.example.com/.well-known/jwks.json
    issuer: https://accounts.example.com
    audience: aperio                 # one value or a list
    claims: { groups: engineering }  # further claims, each an exact value
    cookie: CF_Authorization         # where the token is, if not `Authorization: Bearer`
```

One `jwt` method subsumes a shelf of provider integrations: Cloudflare Access, an ALB doing OIDC, and anyone running their own auth service all hand out the same thing. That is the argument for it over anything vendor-shaped.

Two ways of knowing who signed a token, and exactly one of them per method: `jwks_url:`, the issuer's public keys, fetched and cached for an hour and re-fetched when a token names a key id that is not in the cache (which is what a key rotation looks like from here, so rotation needs no restart). An unknown key id inside the 30 second minimum refetch window is refused rather than re-fetched, so a flood of tokens naming made-up ids cannot turn into a fetch per token; or `hmac_secret:` for `HS256`, when the issuer is your own service. The key-set URL goes through the server's [outbound policy](threat-model.md) like any other destination the server is told to call, and no redirect from it is followed: the policy vets the URL in the file, and a `Location` would mean the address it vetted is not the address that gets the request.

**`iss` and `aud` are only checked when the file asks, and asking means the claim must be present.** An unset `audience:` is not "accept any audience", and a configured one refuses a token that carries none, which is exactly the token the requirement was written to keep out. `exp` is required whatever the file says: a token with no expiry is one that never stops working.

A token in the `Authorization` header is Aperio's credential and is stripped before the request is forwarded; one in a `cookie:` is the visitor's own and travels, because stripping the cookie header would take the application's session with it.

`jsonwebtoken` is on its 10 line, built with the `rust_crypto` feature and no default features. That keeps the crate on `ring`, which rustls already puts in every Aperio binary, so the method adds no crypto implementation and no cross-compilation surface; a build that pulled the default `aws-lc-rs` backend would be a third crypto stack here. `aperio-server/Cargo.toml` carries the reason.

#### What a client may declare, and how that is agreed

A client's `access:` may write `public` (`none`), `basic`, `bearer`, `jwt`, `aperio`, and `forward` with `via: client`. A bare `forward` is refused on a client: its URL would otherwise be called by the *server*, from the server's network, so `localhost:7070` written in a client's file would mean the server's localhost and not its own. `via: client` is what makes the same word mean the client's network, and the server never dials a URL a client chose.

Which of those actually travel is **negotiated on the handshake rather than assumed**. The server announces the methods it accepts from a client on the upgrade response, and a client whose `access:` needs one that is missing **does not serve that service**: it says which side is too old and retries, instead of connecting under a gate the server was never told about. A server too old to send the announcement sends nothing, which reads as "only what the scalar `visitor_auth` can carry", since that is the only field such a server reads: `method: none`, or a `basic` naming a *single* `user:password`. A gate whose method is one of those two but whose shape is not, `basic` with two users, say, is refused there as well, and the message says the server is too old rather than naming the method: the method is not the problem, the shape is, and there is one credential's worth of room in the field it would have to travel in.

That is worth the machinery because of what the alternative does quietly: a server that ignored a policy it did not understand would read the client as declaring *no* gate, and the route would come up open. Only the one service stops; its siblings keep serving.

**The announcement is about the connection, not about the build.** Declaring a visitor gate needs the same token permission as `access: public`, so a token without it is answered with an empty list and holds the service back, rather than being told the gate was accepted and then having it dropped a message later. The server's log names the token and the reason; the client says the permission is the usual cause.

#### Who has the last word

Three places can say who reaches a route, and they are asked in this order:

1. **A site's `access`**, in the server's settings, covering the hostname (and the path, when the site names one). The operator's word on a hostname is the last one: whatever the clients serving it declare, this decides. A client whose own `access:` is overruled this way is told so once per connection, in its own log, so its author does not go on editing a line that has no effect.
2. **A client's `access:`** for its own service. Every client serving a route declares the same one, since a client that says something else is kept out of the route and told why ([One route, one policy](tokens-and-auth.md#one-route-one-policy)).
3. **The server's `access`**, for a route nothing above speaks for.

`public` is a keyword, never a password: a login is always a method block, so a value cannot be read as a credential when it was meant as a word.

#### Telling the backend who came in

The gate has always been a wall: it decided whether a request continued and told the backend nothing, so an application behind a tunnel could not greet anyone without building a second login next to Aperio's. `visitor_identity_headers` (`identity_headers.visitor`) turns on two headers on the forwarded request:

| Header | Value |
| --- | --- |
| `x-aperio-visitor-how` | How they were admitted: `session`, `bearer`, `jwt`, `forward` or `share`. |
| `x-aperio-visitor-id` | The email or username behind a session, or the `x-auth-user` a `forward` endpoint answered with. Absent for `bearer` and `jwt`, which identify a caller rather than a person, and for a share link. |

The secret itself does not travel: an `Authorization` header that opened Aperio's gate is stripped before the request is forwarded, on the same rule that already strips the `aperio_session` and `aperio_share` cookies while leaving every other cookie alone. A credential addressed to the gate is not addressed to what is behind it. An `Authorization` header that did *not* open the gate is the visitor's own and passes through untouched.

Off by default: it is the same new trust surface as `identity_headers` and should be adopted as deliberately. A route that is open or ungated identifies nobody and sends neither header, since a value meaning "anonymous" is noise a backend has to learn to ignore. The forgery half is already done, inbound `x-aperio-*` headers are stripped from every proxied request whatever this is set to.

#### Two planes, one cookie

A session is for one of two things: administering Aperio, or viewing a site behind it. They share a store and a cookie, and what tells them apart is which credential created the session:

| Credential | Plane |
| --- | --- |
| Master token, a named dashboard user, a passkey, OIDC | **admin**: the dashboard, its API, and (fenced by organization) the visitor gate |
| A `basic` login in the server's `access`, or a site's | **visitor**: the hostnames that gate covers, nothing administrative |
| A client's own `access:` | **visitor**, and only on that hostname |
| A single sign-on (`method: sso`) | **visitor**, and only on the hostname it was made on, and only for the gate whose provider vouched for it |

The visitor password is server-wide, so a visitor who signs in on one hostname is not asked again on the next; what it never opens is Aperio itself.

#### Closed by default

The server's `access` says what a route **nothing declares** answers:

| Value | Meaning |
| --- | --- |
| `deny` (the default) | A route is served because something said so, a client's or a site's `access` that admits the visitor, or an explicit `access: public`, and not because nothing said otherwise. A caller carrying a valid Aperio session (master token, dashboard session, passkey or OIDC) also reaches it, see below. |
| `public` | With nothing gating a route, it serves everyone: what the server did before 0.10.0. |
| a method block or a list | A route nothing else declares is behind this gate, the same grammar a client's `access:` has. |

Under `deny` an undeclared route is refused with the same answer an unclaimed hostname gives, so the existence of something there does not leak to a caller who was never going to be let in. Two exceptions, both deliberate: this server's own signed-in users (a master token, a dashboard session, a passkey or an OIDC login against the hostname's organization) reach it, because "not published" says nothing about whether the operator may look; and a service with `cache.offline` and a cached entry answers from that entry rather than the stealth refusal. A route that *is* gated is untouched: the default decides what an unstated route means, nothing else.

This is the setting that makes `auth: DUMMY:DUMMY` unnecessary. It was the only way to say "not public" while being public was an exemption from a gate that might not exist.

**The default is `deny`.** If this server publishes a public site, which is the commonest deployment there is, every client serving one says so with `access: public`, or a site for the hostname does; until one of them does, those routes answer as an unclaimed hostname does, and the client is told why in its own log. `public` here is a supported setting, not a migration escape hatch.

### Editor autocompletion (JSON Schema)

Building the client emits JSON Schemas to `schemas/` (git-ignored build artifacts, regenerated from the parser types so they never drift): `aperio-client.schema.json` for `aperio.yaml`, and `aperio-server.schema.json` for the server's settings document (what the dashboard's settings page and `aperio-server check` validate against). Point your editor's YAML extension at the client one for completion, hover docs, and validation:

```jsonc
// .vscode/settings.json (VS Code / Antigravity, with the YAML extension)
{
  "yaml.schemas": {
    "./schemas/aperio-client.schema.json": ["aperio.yaml", "**/aperio.yaml", "~/.aperio.yaml"]
  }
}
```

Run `cargo build -p aperio-client` once to generate them (or `cargo run -p aperio-config > schemas/aperio-client.schema.json` and `cargo run -p aperio-config -- --server > schemas/aperio-server.schema.json`). The server binary also emits its own, `aperio-server schema > aperio-server.schema.json`, so a deployment can regenerate it without the source tree. Tagged releases attach each schema twice: a versioned `aperio-{client,server}.<tag>.json` for pinning, and a stable-named `aperio-{client,server}.schema.json` so schema managers can point at a URL that always serves the latest release:

```
https://github.com/co3moz/aperio/releases/latest/download/aperio-client.schema.json
https://github.com/co3moz/aperio/releases/latest/download/aperio-server.schema.json
```

## Server

### Server settings

The server reads a handful of **boot settings** from its environment, the ones it needs before its database opens, and keeps every other setting in that database. A container is started with a token and a data volume; everything after that is changed from the dashboard or the API, and takes effect without a restart.

```bash
APERIO_SERVER_TOKEN=change-me-to-a-long-random-string aperio-server
```

There is no server configuration file. Since 0.13.0 a variable that used to configure the server (`APERIO_TRUSTED_PROXIES`, `APERIO_CACHE`, and the rest) is refused at startup with the path of the setting that replaced it, so a deployment cannot keep believing in a value nothing reads; see the [Upgrade Guide](upgrade-guide.md).

#### Boot environment

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_TOKEN` | **Required.** Master token: authenticates tunnel clients and signs in to the dashboard as the built-in admin `aperio`. |  |
| `HOST` | Bind address. | `0.0.0.0` |
| `PORT` | Listen port. | `8080` |
| `APERIO_DATA_DIR` | Directory for persisted state: the database (settings, tokens, users, statistics), the audit log, the metrics token. **Mount a volume here in Docker.** | `./data` |
| `LOG_LEVEL` | `error`, `warn`, `info`, `debug`, `trace`. | `info` |
| `APERIO_REUSEPORT` | `1` = bind the listener with `SO_REUSEPORT`, so a second process can bind the same `host:port` while the first is still running, for a zero-downtime rolling restart. | `0` |
| `APERIO_BACKUP_KEY` | Encryption key for backup snapshots, as 64 hex characters or base64 of 32 bytes. Unset writes them in the clear. In the environment rather than the database because it protects the database's copies. |  |
| `APERIO_BACKUP_KEY_FILE` | File holding that key, which is what a secret manager mounts. Refused when it is inside `backup.dir`, where it would travel with the snapshots it protects. |  |
| `APERIO_INIT_FILE` | A settings document applied on the **first** start, into an empty database. See [The first start](#the-first-start-aperio_init_file). |  |

#### Changing a setting

A setting has a dotted path, `section.field`: `proxy.trusted`, `cache.enabled`, `limits.request_body`. The same document is read and written three ways:

- the dashboard's **Settings** page;
- `GET` / `PUT /aperio/api/settings`, the whole document as JSON (a field left out takes its default);
- `POST /aperio/api/import` with a `settings` section, which is how a document kept in git is applied to a running server.

A change is checked before anything moves: a value the server cannot honour is refused with its path in the message (`proxy.trusted: invalid trusted proxy entry: '10.0.0.0/99'`), and an unknown key is refused rather than ignored. It is then stored, put in force for the next request, and audited as `settings_updated` with the paths it changed.

**Most settings apply at once.** A few are read when the server starts: `dashboard.enabled`, `dashboard.webauthn`, `oidc`, `metrics.enabled`, `otel`, `logging.access_log`, `outbound.proxy`, `outbound.no_proxy`, `edge.token` and `backup`. A change to one of those is stored, and reported as `restart_pending` until the next start puts it in force.

**Security-relevant settings take more.** The paths that decide who reaches what, or where the server may be steered (`access`, `proxy`, `security`, `outbound`, `oidc`, the dashboard's hostname and fence, `inspector.redact`, the metrics and edge tokens, `otel.endpoint`/`otel.headers`, the autoscaling fences, a site's `access`), need Admin in every organization (`*`) and, from a dashboard session, a sign-in from the last ten minutes. Such a change is audited a second time as `settings_security_changed`. `GET /aperio/api/settings` lists them as `security_paths`, and the restart-only ones as `restart_paths`.

**Credentials are never read back.** `oidc.client_secret`, `metrics.token`, `edge.token`, `otel.headers` and the secrets inside the server's `access` and each site's `access` read as `********`. Writing `********` back keeps the stored value, so a page that saves the whole document does not erase a secret it was never shown; a mask with nothing stored behind it is refused.

#### Values

Durations are written with a unit: `250ms`, `30s`, `5m`, `2h`, `1d`, `1w`. Sizes use binary units: `512KB`, `10MB`, `1GB`. A bare number still works and means seconds or bytes, so `30` and `"30s"` are the same timeout.

#### The first start (`APERIO_INIT_FILE`)

A server started into an empty data directory takes its first settings from the document `APERIO_INIT_FILE` names, yaml or json, in the shape an export writes: a `settings:` section and, optionally, any other section of an export (`tokens`, `users`, `organizations`, `webhooks`, `scaling`, `exposes`, …).

```yaml
# server-init.yaml
settings:
  proxy:
    trusted: [10.0.0.0/8, 173.245.48.0/20]
  security:
    secure_cookies: true
  cache:
    enabled: true
    size: 256MB
  sites:
    - hostname: app.example.com
      path: /login
      rate_limit: {rps: 5}
```

The document is checked like any change, and one the server refuses stops the start with the setting named. It is read **once**: from the second start on the database holds the settings, and the file is not consulted again, so editing it and restarting changes nothing. That is deliberate, the dashboard and the file would otherwise disagree about which of them is the truth.

#### Keeping settings in git

The settings are a document, so they can be reviewed and versioned like one:

```bash
# Take the current settings, only what differs from the defaults.
curl -fsS -b cookies.txt "https://tunnel.example.com/aperio/api/export?include=settings" > aperio-settings.json
# Check a document without applying it.
aperio-server check aperio-settings.json
# Apply it to a running server.
curl -fsS -b cookies.txt -H 'Content-Type: application/json' \
  --data @aperio-settings.json https://tunnel.example.com/aperio/api/import
```

`aperio-server apply <file>` stores a document's settings straight into the data directory of a **stopped** server, for the next start.

#### Dashboard sign-in

**Dashboard users and roles.** Beyond the master token (which signs in as a built-in admin named `aperio`), admins can create named dashboard users from the *Users* page, each with a role: **viewer** (read-only, statistics, traffic, audit), **operator** (day-to-day operations, clients, tokens, webhooks, maintenance, share links), or **admin** (everything, including server settings and user management). Passwords are stored as Argon2id hashes in `APERIO_DATA_DIR/aperio.db`. The role floor of every dashboard route is enforced server-side (a viewer gets `403` on any mutation, and on the admin-only settings/users routes); the UI additionally hides controls a role cannot use. An OIDC login is what its dashboard user record says: the identity provider vouches for the email, the record (matched by email, created at the first login) carries the grants, see [Organizations](organizations.md#oidc-identity-is-the-providers-authorization-is-aperios). Dashboard sessions last 24 hours and are persisted in `APERIO_DATA_DIR/aperio.db` (hashed cookie tokens only), so a server restart does not sign anyone out. Users (and tokens) can also be grouped into isolated **organizations**, in which case a named user's reach is the list of grants on their record, one organization for most, several with a role in each for an operator who administers more than one tenant, and the server-global settings/export routes take Admin in the master organization, which the built-in `aperio` account has, see [Organizations](organizations.md).

**Two-factor authentication (TOTP).** Any named dashboard user can enroll an authenticator app (Google Authenticator, Authy, 1Password, …) from the sidebar's *Two-factor auth* dialog: scan the QR code, confirm a 6-digit code, and store the eight single-use recovery codes shown once. From then on the login form asks for the code after the password (a recovery code works too, and is consumed). Wrong codes count towards the brute-force lockout. An admin can reset a locked-out user's TOTP from the *Users* page (`DELETE /aperio/api/users/:id/totp`); enrollment endpoints live under `/aperio/api/me/totp/*`. The built-in `aperio` admin (master token / dashboard password) has no user row and cannot enroll, create named users for anyone who needs 2FA. OIDC is not that account: an OIDC login is matched to a dashboard user record by email and takes the grants on it, so it can enroll like any named user.

**Passkeys (WebAuthn).** Set `dashboard.webauthn.origin` to the public URL the dashboard is reached at (e.g. `https://tunnel.example.com`, the RP ID is its domain, so an IP origin won't work) and named users can register passkeys (YubiKeys, Touch ID / Face ID, password managers) from the sidebar's *Passkeys* dialog, then sign in passwordless from the login page. Passkey sign-ins skip TOTP (the authenticator already verifies the user), failed ceremonies count towards the brute-force lockout, and registrations/deletions are audit-logged. Up to 10 passkeys per user, stored in `aperio.db` (public keys only, private keys never leave the authenticator). A passkey registered with **“allow signing in without a username”** becomes a discoverable credential: pressing the passkey button with an empty username brings up the authenticator's account picker directly. Passkeys without the opt-in keep working username-first only. By default a passkey is scoped to the exact host of `dashboard.webauthn.origin`, so it will **not** work when the dashboard is also reached through a sibling host (e.g. `test-aperio.robogon.com` next to `aperio.robogon.com`); set `dashboard.webauthn.rp_id` to the shared parent domain (`robogon.com`) to make one passkey cover every subdomain. WebAuthn has no wildcard RP ID; the parent-domain RP ID is the supported way to get the `*.robogon.com` effect.

#### Getting back in

The master token always signs in as `aperio`, whatever the settings say. When a setting locks the dashboard out (an `security.admin_allowed_ips` that does not include you, a `dashboard.hostname` nobody can reach), stop the server and put that setting back:

```bash
aperio-server settings reset security.admin_allowed_ips
```

`aperio-server settings reset` with no path puts every setting back to its default; it also repairs a stored document this build cannot read.

### The `aperio-server` command

| Command | What it does |
| --- | --- |
| `aperio-server` (or `run`) | Start the server. |
| `aperio-server settings show [--full] [--json]` | Print the stored settings, only what differs from the defaults unless `--full`. |
| `aperio-server settings reset [path]` | Put one setting or section back to its default, or every setting. |
| `aperio-server apply <file>` | Store a document's settings for the next start. |
| `aperio-server check <file>` | Check a document (yaml or json) without applying it. |
| `aperio-server schema` | Print the settings document's JSON Schema. |
| `aperio-server audit verify` | Verify the audit log's tamper-evident hash chain. |
| `aperio-server backup decrypt <input> [output]` | Decrypt a backup snapshot with `APERIO_BACKUP_KEY` / `APERIO_BACKUP_KEY_FILE`. |

The commands that touch the data directory (`settings`, `apply`, `audit verify`) read `APERIO_DATA_DIR` like the server does, and are meant for a stopped server.

### Settings reference

Every setting, by section, with its default. A blank default is unset. The `sites` list is described after the tables, in [Sites](#sites).

#### Access and the visitor gate (`access`)

| Setting | Description | Default |
| --- | --- | --- |
| `access` | What a route nothing else declares answers: `deny` refuses it, `public` serves it to anyone, and a method block or a list of them gates it. A client's `access:` speaks for its own service, and a site's `access` speaks for its hostname over both. See [Visitor authentication](#visitor-authentication). | `deny` |

#### Advanced (`advanced`)

What the defaults are chosen for and almost nobody needs to touch. Read past it unless a measurement points here: a slow visitor holding memory on the server, or a slow link carrying text.

| Setting | Description | Default |
| --- | --- | --- |
| `advanced.compression` | `true` = offer per-message zlib compression to clients (enabled per connection once acknowledged). Worth it on a slow or metered link carrying text; it costs CPU on both ends. | `false` |
| `advanced.stream.backlog_limit` | Hard per-stream backlog cap: a stream whose producer cannot be paused (one ignoring the pause) is dropped past this much buffered data. A value below twice the pause mark is raised to it, so there is always room to pause before the cap bites. | `16MB` |
| `advanced.stream.min_throughput` | Bytes per second a streamed response's consumer must take **while data is waiting for it**, or the stream is ended. The pump already ends a stream whose consumer cannot take a single chunk within the gateway timeout, so a reader taking *nothing* was covered; this closes the gap in between, a reader that accepts one chunk just inside the timeout forever, holding a client concurrency slot and megabytes of buffer for as long as it likes. Only the time the consumer kept data waiting counts, so a stream that is quiet because the **backend** has nothing to send (server-sent events, long polling) is never ended for it. Measured over 30-second windows. | `0` |
| `advanced.stream.pause_at` | Per-stream flow control: the server-side backlog of one streamed response / WebSocket / TCP relay at which the producing client is asked to pause that stream, so a visitor reading slower than the backend sends throttles the backend through ordinary TCP backpressure instead of piling data up on the server. Values under 64 KB are raised to 64 KB. Each stream snapshots the three watermarks when it starts, so a live edit applies to streams opened after it. | `2MB` |
| `advanced.stream.resume_at` | Backlog under which a paused producer is asked to resume. Kept well below the pause mark so the pair does not flap; a value at or above `advanced.stream.pause_at` is repaired to a quarter of it. | `512KB` |

#### Alerts (`alerts`)

| Setting | Description | Default |
| --- | --- | --- |
| `alerts.client_down` | How long a known service may stay down before an `alert_triggered` event (kind `client_down`); resolves when it comes back. Unset = off. |  |
| `alerts.error_rate` | Failed-request fraction (`0.0` to `1.0`) that fires an `error_rate` alert. Unset = off. |  |
| `alerts.min_requests` | Minimum requests inside the window before the error-rate rule may fire. | `20` |
| `alerts.rules` | Operator-defined alerts over what the server measures. |  |
| `alerts.window` | Sliding window the error rate is measured over. | `5m` |

#### Audit log (`audit`)

| Setting | Description | Default |
| --- | --- | --- |
| `audit.max_files` | Rotated audit generations to keep (`audit.jsonl.1` … `.N`; `0` = truncate instead of keeping history). | `3` |
| `audit.max_size` | Rotate `audit.jsonl` once it exceeds this size (`0` = never rotate). | `10MB` |

#### Backups (`backup`)

| Setting | Description | Default |
| --- | --- | --- |
| `backup.dir` | Destination directory for the automatic backups. Required to enable them. |  |
| `backup.interval` | Time between automatic physical snapshots of the SQLite store (a consistent online backup). Unset or `0s` = disabled; enabling also requires `backup.dir`. |  |
| `backup.keep` | How many timestamped backup snapshots to retain; older ones are pruned. | `7` |

#### Response cache (`cache`)

| Setting | Description | Default |
| --- | --- | --- |
| `cache.enabled` | `true` = enable the server-side GET response cache for services that opt in with the client's `cache.enabled`. Strictly `Cache-Control`-driven: only responses explicitly allowing shared caching (`max-age`/`s-maxage`, no `no-store`/`no-cache`/`private`, no `Vary`/`Set-Cookie`) are stored, for the advertised lifetime; only credential-less plain GETs are answered from it. Hits carry `x-aperio-cache: hit` and an `Age` header; entries without a backend validator get a synthesized `ETag`, and a matching `If-None-Match` is answered `304` at the edge without a tunnel round-trip. Misses are **single-flighted**: concurrent identical cacheable GETs collapse into one upstream fetch (followers wait for the leader and answer from the freshly stored entry), so cache expiry on a hot URL cannot stampede the backend. Responses advertising `stale-while-revalidate=N` (RFC 5861) keep serving for N seconds past expiry (marked `x-aperio-stale`) while one background revalidation refreshes the entry, visitors never wait on the refresh. `POST /aperio/api/cache/purge` (admin) drops entries by `hostname`, `path_prefix` and/or `surrogate_key`, for immediate invalidation after a deploy. Cached entries also satisfy single-range **`Range` requests** (video scrubbing, resumable downloads) at the edge: `206 Partial Content` sliced from the stored full body with `Accept-Ranges`/`Content-Range`, `416` when out of range, honoring `If-Range`, partial requests never re-traverse the tunnel while the entry lives. | `false` |
| `cache.max_stale` | Serve-stale window for services that set `cache.offline`: how long past its advertised lifetime a cached response may still answer visitors while the route has no healthy client. `0` disables serve-stale. | `1h` |
| `cache.negative_ttl` | How long to briefly cache error / negative responses (e.g. `404`) so a hot missing URL cannot hammer the backend with repeated misses. `0` = disabled. Needs `cache.enabled`. | `0s` |
| `cache.size` | Total in-memory budget of the response cache; inserting past it evicts the entries closest to expiry, and a single body larger than a quarter of the budget is never cached. | `64MB` |

#### Dashboard (`dashboard`)

| Setting | Description | Default |
| --- | --- | --- |
| `dashboard.enabled` | `false` = disable the admin dashboard entirely. | `true` |
| `dashboard.fenced_login` | `true` = fence the dashboard login to the hostname's organization. On a hostname inside an organization's allowlist, the login form, a passkey and an OIDC login admit that organization's people and a `*` grant, with the wrong-password answer for everyone else; a hostname no fence claims admits master's people and the users of organizations without a fence; a random subdomain follows the organization serving it; and a session is good only on the hostname it was minted on, so a cookie value lifted from one hostname opens nothing elsewhere. Off by default, since a deployment whose tenants sign in at the server's own name would be locked out. See [Organizations](organizations.md#fenced-login). | `false` |
| `dashboard.hostname` | A hostname whose root is the dashboard: `panel.example.com` opens the panel the way `panel.example.com/aperio` does, and `/aperio` stays on every hostname regardless. One exact name, no pattern. It serves the panel and nothing else, so no token, client or override may bind it. Needs a certificate like any other name, and a passkey registered on another host does not sign in here unless `dashboard.webauthn.rp_id` covers both. An organization picks its own the same way, inside its fence, see [Organizations](organizations.md#panel-hostname). |  |
| `dashboard.ui_language` | Default dashboard/login UI language (`en`, `de`, `es`, `fr`, `tr`, `ru`, `zh`, `ja`) used when the visitor's browser language is unsupported. | `en` |
| `dashboard.webauthn` | Passkey sign-in. |  |

#### Edge proxy integration (`edge`)

| Setting | Description | Default |
| --- | --- | --- |
| `edge.cert_resolver` | Traefik certificate resolver named on the generated routers. Unset = no `tls` block. |  |
| `edge.entrypoints` | A list of Traefik entry points set on the generated routers. Empty = leave it to Traefik's defaults. |  |
| `edge.include_offline` | `true` = also publish hostnames a token permits but no client is serving, so a certificate can exist before the first connect. Lets a tenant provoke an ACME request for any hostname its token covers. | `false` |
| `edge.service_url` | URL the edge should forward matched traffic to, i.e. this server as the edge sees it (e.g. `http://aperio:8080`). Required by the Traefik document; the Caddy `ask` endpoint works without it. |  |
| `edge.token` | Credential the edge proxy presents (`Authorization: Bearer` or `?token=`). **Unset = the `/aperio/api/edge/*` endpoints are not registered at all.** |  |

#### Resilience (`resilience`)

| Setting | Description | Default |
| --- | --- | --- |
| `resilience.profile` | How hard the server works to answer when a client fails. `off`: a failure is the visitor's answer, nothing is re-dispatched or ejected. `safe`: an idempotent request is re-dispatched to another healthy client, and a client that keeps failing is taken out of routing for a while, never emptying it. `aggressive`: more attempts, a waiting failover, and a retryable `5xx` re-dispatched too. Each setting below, where written, wins over the profile's value. | `safe` |
| `resilience.failover` | Re-dispatching a request whose client fails mid-flight; the fields below. |  |
| `resilience.failover.mode` | What to do when a client dies mid-request: `fail`, `retry`, `wait`, or `retry-wait`. See [In-Flight Failover](failover.md). | the profile's: `fail` (off), `retry` (safe), `retry-wait` (aggressive) |
| `resilience.failover.max_jumps` | Max re-dispatch attempts per request. | `2`, `3` when aggressive |
| `resilience.failover.window` | Total time the `wait`/`retry-wait` modes may spend waiting for a candidate, across all jumps. | `15s`, `30s` when aggressive |
| `resilience.failover.all_methods` | `true` = also fail over non-idempotent methods (POST/PATCH), which may then reach a backend twice. No profile turns it on: it is a decision about the backend. | `false` |
| `resilience.failover.retry_on_5xx` | `true` = when a fully-buffered response is a retryable server error, re-dispatch the request to a freshly picked client instead of returning it. Shares the jump budget and honors method idempotency; streamed responses are never retried. | `true` only when aggressive |
| `resilience.failover.retry_statuses` | The status codes that count for `retry_on_5xx`. Empty = every 5xx. | `[502, 503, 504]` when aggressive |
| `resilience.outlier` | Taking a client out of routing when it keeps failing under real traffic, while its health probe still reports green; the fields below. Per-route fail-open: a route whose every client is ejected keeps them all. See [Routing & Load Balancing](routing-and-load-balancing.md#passive-outlier-ejection). |  |
| `resilience.outlier.enabled` | Passive ejection on or off. | `true`, `false` when off |
| `resilience.outlier.max_failures` | Failures inside the window that eject a client. | `5`, `3` when aggressive |
| `resilience.outlier.window` | Window the failures are counted over. | `30s` |
| `resilience.outlier.eject_for` | How long an ejected client stays out before it is re-admitted. | `30s`, `1m` when aggressive |

#### Gateway (`gateway`)

| Setting | Description | Default |
| --- | --- | --- |
| `gateway.client_down_threshold` | Time without a heartbeat before a client is dropped from the routing pool (it rejoins on the next ping). A connection silent for **four times** this is closed and removed by the server itself, whatever the socket says: a client that dropped its socket without a Close frame behind a proxy chain used to hold its `max_tunnels` slot, its organization quota and its dashboard row for as long as the operating system kept the socket. | `15s` |
| `gateway.response_timeout` | How long to wait for a client to answer a dispatched request. | `30s` |
| `gateway.timeout` | How long to wait for a client to (re)connect before failing a request. | `10s` |

#### Identity headers (`identity_headers`)

| Setting | Description | Default |
| --- | --- | --- |
| `identity_headers.serving` | `true` = tell the backend which client, organization and token served the request, as `x-aperio-client-id`, `x-aperio-org` (absent for the master organization) and `x-aperio-token`. Off by default: they are new trust surface, and a backend that starts believing them should do so deliberately. Added per dispatch attempt, so after a failover they name the client that actually served it. **Inbound `x-aperio-*` headers are stripped from every proxied request whatever this is set to**, so a visitor can never forge one, and a backend that trusts them can do so without checking whether the announcement is enabled. | `false` |
| `identity_headers.visitor` | `true` = tell the backend who the visitor is: `x-aperio-visitor-how` (`session` / `bearer` / `share`) and `x-aperio-visitor-id` (the email or username behind a session, where there is one). Off by default, the same new trust surface as `identity_headers.serving` and adopted the same way. An open or ungated route identifies nobody and sends neither header. Inbound `x-aperio-*` is stripped from every proxied request regardless, so a visitor cannot forge one. | `false` |

#### Request inspector (`inspector`)

| Setting | Description | Default |
| --- | --- | --- |
| `inspector.enabled` | `false` = do not record transactions for the request inspector. On by default. Off gives back a mutex, two header clones and a capture entry per proxied request, at the cost of not being able to inspect or replay anything. A client may opt out for one service with `inspector: false`. | `true` |
| `inspector.redact` | `false` = disable secret masking in the request inspector. On (default), credential headers (`Authorization`, `Cookie`, `X-Api-Key`, …) and secret-looking body fields (`password`, `token`, `api_key`, …) show as `[REDACTED]` in the inspector, the cURL copy, and the HAR export; the raw capture kept in memory for replay is always intact. | `true` |

#### Limits (`limits`)

| Setting | Description | Default |
| --- | --- | --- |
| `limits.concurrent_requests` | Max in-flight proxied requests across all tunnels. | `100` |
| `limits.connections_per_service` | Parallel tunnel connections one client may open for a single service (its `connections:`). Announced to the client on connect, so it sizes its fan from this rather than guessing; a connection past the ceiling is closed. A token's own `max_connections` can lower it for its holder, never raise it. | `16` |
| `limits.ip_rate` | The per-visitor-IP token bucket every request spends from. | `{"burst": 100.0, "per_second": 5.0}` |
| `limits.request_body` | Max request body size. | `10MB` |
| `limits.streams_per_ip` | Streamed responses one visitor address may hold open at once. Saturating a service's concurrency budget otherwise takes one host holding many slow streams; this makes it take a botnet. The slot is taken when a response turns out to *be* a stream and released when the body ends or the visitor walks away, so it is a concurrency limit rather than a rate limit: opening and closing streams quickly never trips it, holding them open does. Refusals appear as `aperio_rate_limited_total{limit="streams-per-ip"}`. **Check `proxy.trust` before setting it**, or every visitor behind your CDN shares one address. | `0` |
| `limits.tunnels` | Max simultaneously connected tunnel connections. A client holds one per service unless it multiplexes, and a config reload briefly wants two per service (the replacement comes up before the old one closes). Was `10` before 0.12.0. | `64` |
| `limits.websockets` | Max concurrently-live proxied public WebSockets (they are long-lived, so they get their own ceiling separate from the request limit above); beyond it an upgrade gets `503`. `0` = no cap. | `10000` |

#### Logging (`logging`)

| Setting | Description | Default |
| --- | --- | --- |
| `logging.access_events` | `false` = do not emit the per-request structured access event for a **successful** request (`target: aperio_access`, level `info`). On by default. Distinct from `LOG_LEVEL`: it silences the one-per-request line and leaves warnings and errors where they are, so a refused or failed request still logs, at `warn`. The access log *file* (`logging.access_log`) is separate and unaffected. | `true` |
| `logging.access_log` | File path for the structured access log: one JSON line per proxied request (`request_id`, `method`, `uri`, `status`, `duration_ms`, `host`, `client_id`, `token`, `error`), directly ingestible by Loki/ClickHouse. The same data is emitted to stdout as structured `aperio_access` tracing events unless `logging.access_events: false`, and both sinks are thinned by `logging.sample_rate` for successful requests. |  |
| `logging.sample_rate` | Fraction of **successful** requests that produce an access line, `0.0` to `1.0`. Thins out both the `aperio_access` event and the `logging.access_log` file, for the case where the choice would otherwise be between a log bill and no request log at all. Sampling is deterministic (`0.1` = exactly one line in ten, not one in ten on average). A response of `500` or worse is **always** logged whatever the rate, and so is every refused or failed request. The dashboard's counters, the latency histogram and the rate charts are unaffected: they are fed before sampling and stay exact. | `1.0` |

#### Prometheus metrics (`metrics`)

| Setting | Description | Default |
| --- | --- | --- |
| `metrics.enabled` | `true` = enable the Prometheus endpoint at `/aperio/metrics`. | `false` |
| `metrics.token` | Token required to scrape metrics (`?token=` or `Authorization: Bearer`). Unset, with `metrics.enabled: true`, a random one is generated on first start and persisted in `APERIO_DATA_DIR/metrics_token`; with metrics off no file is written. |  |

#### OIDC / SSO (`oidc`)

| Setting | Description | Default |
| --- | --- | --- |
| `oidc.allowed_emails` | A list of allowlist (required with issuer). |  |
| `oidc.client_id` | OAuth client id registered at the issuer. |  |
| `oidc.client_secret` | OAuth client secret. |  |
| `oidc.default_grants` | What an email with no dashboard user record is granted at its first login, a list of `<org>:<role>` entries (`<org>` a child organization's handle or id, `master`, or `*`). Empty means the login is refused, with a message to ask an administrator, and a record is created so the grant has a name to land on; `master:admin` is what every allowed email got before 0.12.0. |  |
| `oidc.group_grants` | What each group means, a list of `<group>=<org>:<role>` entries. Applied at every login: a grant the map produces is written, one it produced before and no longer does is taken back, a grant an admin wrote by hand, or one an organization's own provider mapped, is left alone unless the map names the same organization. A person removed from a group loses the access at their next login, not before; disable the record for sooner. |  |
| `oidc.groups_claim` | The claim that carries the identity provider's groups, read from userinfo and then from the ID token. | `groups` |
| `oidc.issuer` | Issuer URL. Setting it enables SSO enforcement. |  |
| `oidc.redirect_url` | Fixed callback URL; otherwise derived from the request `Host` (and `X-Forwarded-Proto` when `proxy.trust: true`), and warned about at startup. **Set it.** Deriving means the `Host` of the request that starts a login decides where the provider returns the authorization code: a visitor lured to any hostname that resolves to this server has their code sent there. Redeeming it still needs the client secret, and your provider's registered-callback list is the other gate, but neither of those is Aperio's to enforce. |  |
| `oidc.scopes` | Requested scopes. | `openid email profile` |

#### OpenTelemetry (`otel`)

| Setting | Description | Default |
| --- | --- | --- |
| `otel.bridge` | `true` = accept OTLP exports from tunnel clients and forward them to the collector `otel.endpoint` names. Off by default: it is an outbound path a client can drive, so it is a decision rather than a consequence of having `otel` on. See [the OTel bridge](observability.md#the-otel-bridge-telemetry-from-the-edge). | `false` |
| `otel.enabled` | `true` = export one OTLP span per proxied request to an OpenTelemetry collector (adopts inbound W3C `traceparent`, propagates its own context to the backend). | `false` |
| `otel.endpoint` | OTLP collector base URL. Over HTTP the `/v1/traces` signal path is appended if absent; over gRPC it is stripped, since gRPC takes the bare base URL. The default follows `otel.protocol`. |  |
| `otel.headers` | Extra headers on every outgoing OTLP request, as `k=v,k=v`. Where a collector's credential goes, and it stays on the server: the point of the bridge is that an edge host does not hold one. |  |
| `otel.protocol` | OTLP transport: `http` (protobuf over HTTP, the spec spelling `http/protobuf` is also accepted) or `grpc`. Unset, the endpoint's port decides: 4317 is gRPC, anything else HTTP. A collector answering the other protocol drops every span in silence, so the startup probe checks the transport that was actually chosen and warns when the port contradicts it. |  |
| `otel.sample_rate` | Fraction of traces to record, `0.0` to `1.0`. Default `1.0`, every request builds a span tree and exports it, which is the setting that makes tracing visible in a benchmark. `0.01` samples one request in a hundred; the decision is made once per request and every span of that request follows it. An unparseable or out-of-range value traces everything rather than nothing. | `1.0` |
| `otel.service_name` | `service.name` reported on exported spans. Falls back to `OTEL_SERVICE_NAME`. | `aperio-server` |

#### Outbound calls (`outbound`)

| Setting | Description | Default |
| --- | --- | --- |
| `outbound.allowlist` | Optional policy over where the server may send *outbound* callbacks (webhook deliveries, autoscaling hooks): a list of host/CIDR patterns (`hooks.example.com`, `*.corp.example`, `10.1.0.0/16`). When set, a destination either matches an entry or is refused, both when a webhook is created and again at every delivery; a matching entry is trusted even if it is private, since the operator named it. Empty = no restriction. An invalid entry is refused rather than a partial allowlist applied. Through a proxy (`outbound.proxy`) the proxy resolves the destination's name on its own network, so the addresses this policy vetted are not the ones reached, and CIDR entries cannot admit a named destination. |  |
| `outbound.block_private` | With no allowlist configured: `true` = refuse outbound callback destinations that are, or resolve to, internal addresses (loopback, RFC 1918, link-local and the cloud metadata services, CGNAT, unique-local). Off by default because internal receivers are the normal webhook deployment; turn it on when tenants you do not fully trust may create webhooks, since the delivery log would otherwise let them probe the server's private network one port at a time. Through a proxy the check covers addresses written as addresses only: the proxy resolves names on its own network, and the startup log says so. | `false` |
| `outbound.no_proxy` | Destinations that skip the proxy and are called directly, a list of: exact names, or `.suffix` / `*.suffix` for a domain and everything under it. The usual case is a `forward` auth endpoint or an OIDC issuer inside your own network. Loopback is always direct, listed or not. These destinations keep the **whole** outbound policy, since the server chooses their addresses itself. `NO_PROXY` from the environment is not consulted (it was measured not to behave as a bypass switch even set to `*`). |  |
| `outbound.proxy` | HTTP proxy the server's outbound callbacks (webhooks, autoscaling hooks, JWKS, `forward` auth, OIDC, OTLP) go through, on a network with no direct route out: `host:port` or `http://host:port`, with an optional `user:password@`. **The ambient `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` are no longer read by anything**, so this is the only way to send them through a proxy; a server that still has those set is told at startup that they do nothing. Note what a proxy costs the two settings above: the proxy resolves the destination's name on its own network, so `block_private` covers addresses written as addresses only, and CIDR allowlist entries cannot admit a named destination. The startup log names which half is in force. |  |

#### Reverse proxy and client IPs (`proxy`)

| Setting | Description | Default |
| --- | --- | --- |
| `proxy.real_ip_header` | Header consulted **before** `X-Forwarded-For` for the real client IP (with `proxy.trust: true`). Needed behind CDN→proxy chains where the proxy resets XFF to the CDN edge, e.g. set `CF-Connecting-IP` behind Cloudflare, or configure the proxy's `forwardedHeaders.trustedIPs` instead. |  |
| `proxy.trust` | `true` = trust `X-Forwarded-For` / `X-Real-IP` for client IPs. Enable **only** behind a trusted reverse proxy. | `false` |
| `proxy.trusted` | A list of IPs/CIDRs of your reverse proxies and CDN egress ranges (e.g. `10.0.0.0/8,173.245.48.0/20`). When set, the client IP is resolved by walking `X-Forwarded-For` (plus the direct peer) from the nearest hop backwards past trusted addresses, the CDN-agnostic model that works for Cloudflare, Fastly, Akamai, LB chains, etc. Headers from an untrusted direct peer are ignored entirely. Implies `proxy.trust: true`. Prefer this over the header-based options. |  |

#### Request id (`request_id`)

| Setting | Description | Default |
| --- | --- | --- |
| `request_id.enabled` | `false` = do not manage a request-id header. On by default: the id the server already assigns every proxied request is sent to the backend and echoed on the response, so one identifier joins the visitor's report, the server's access log and the backend's own logs. | `true` |
| `request_id.header` | Header the id travels in. | `x-request-id` |
| `request_id.trust_inbound` | `true` = adopt the visitor's own value when the request already carries the header, instead of replacing it. Off by default because the header is attacker-supplied: trusting it lets any visitor choose what appears in your logs and your backend's, and repeat somebody else's id. Turn it on only behind a proxy that sets the header itself. An adopted value must be at most 128 characters of `A-Za-z0-9-_.:/+`; anything else is quietly ignored in favour of the server's own id. | `false` |

#### Retention (`retention`)

| Setting | Description | Default |
| --- | --- | --- |
| `retention.access_log` | How long to keep `logging.access_log` lines; expired lines are pruned in place. |  |
| `retention.audit` | How long to keep audit events: rotated generations whose newest event expired are deleted whole, and the active file loses only its leading expired prefix, the tamper-evidence hash chain stays verifiable. |  |
| `retention.captures` | How long to keep inspector captures and webhook inbox entries; a background pruner enforces the TTL at startup and hourly. unset = keep (bounded only by the entry caps). |  |
| `retention.db_max_size` | Disk-usage guard: cap on `aperio.db` (plus its WAL/SHM sidecars). At 90% a `disk_usage_warning` webhook/audit event fires (once per episode, resetting below 80%); past the cap the hourly guard auto-prunes the lowest-priority data, oldest webhook inbox entries, oldest webhook deliveries, oldest day-stat buckets, then vacuums so the file actually shrinks, and emits `disk_pruned`. |  |
| `retention.stats` | How long to keep day-granularity statistics buckets (week/month/year buckets keep their built-in caps). |  |

#### Routing & load balancing (`routing`)

| Setting | Description | Default |
| --- | --- | --- |
| `routing.alternate_servers` | A list of Aperio server URLs (`ws://`/`wss://`) a client of this one may fall back to, announced on every handshake. A planned migration or a regional failover otherwise means editing every client's config; announce the new server here and clients learn it on their next connection. Advice rather than instruction: a client appends these **after** the servers its own config names, so the operator's list still decides the order, and the rotation wraps, so an alternate is never a one-way door. A client that has never reached this server learns nothing, which is why this is for a migration announced in advance, not a rescue. At most 8. |  |
| `routing.lb_strategy` | Load-balancing strategy: `round-robin`, `primary-standby` (client priority tiers), or `sticky` (visitor affinity via cookie). See [Routing & Load Balancing](routing-and-load-balancing.md). | `round-robin` |
| `routing.preview_noindex` | `true` = services reached through their random subdomain answer with `X-Robots-Tag: noindex, nofollow` and a disallow-all `/robots.txt`, so preview environments never end up in search engines. | `false` |
| `routing.random_subdomain` | Pattern with a `*` placeholder in the leftmost label, every connecting client gets the pattern with `*` replaced by a random label, in addition to its other binds. `example.com` ≡ `*.example.com`; `*-test.example.com` yields `<random>-test.example.com` (stays on the same subdomain level, so one wildcard TLS cert covers it). |  |

#### Autoscaling (`scaling`)

| Setting | Description | Default |
| --- | --- | --- |
| `scaling.allow_http` | `true` = permit a plain-http autoscaling endpoint. The URL comes from a client, so https is the floor by default. | `false` |
| `scaling.allow_private` | `true` = permit an autoscaling endpoint that resolves to a private, loopback or link-local address. The URL comes from a client, so it is treated as untrusted input: every address the name resolves to is checked, since a hostname pointing at `127.0.0.1` or `169.254.169.254` is the classic SSRF bypass. Turn it on only when the scaler genuinely lives on the same private network. | `false` |
| `scaling.enabled` | `true` = honor the `scaling:` block clients announce. Off = the declaration is ignored entirely. | `false` |
| `scaling.record_ttl` | Time after which an autoscaling record nothing has re-announced is dropped. | `30d` |

#### Security (`security`)

| Setting | Description | Default |
| --- | --- | --- |
| `security.admin_allowed_ips` | A list of IPs/CIDRs allowed to reach the authenticated admin surface (the `/aperio` dashboard and its session-protected API); other sources get a network-level block. A few routes live deliberately outside that fence because they authenticate themselves and must work from CI: `/aperio/api/tokens/refresh`, `/aperio/api/tunnels` and `/aperio/api/edge/*`. The login page and its auth endpoints stay reachable from anywhere so password-gated services keep working. Empty = no restriction. An invalid entry is refused rather than a partial allowlist applied. |  |
| `security.denied_ips` | A list of IPs/CIDRs refused everything, checked before every other rule: proxied traffic, the dashboard and its API, and the tunnel endpoints alike. The inverse of the allowlists, for blocking an abusive source without turning on an `allowed_ips` that would lock out everyone unnamed. Blocked requests get `403` and never reach a handler, so they cannot spend a rate-limit bucket or occupy a request slot. The client IP is resolved with the same proxy-trust rules as everything else, so a deployment behind a trusted proxy blocks the visitor rather than the proxy. It applies when saved: an address can be blocked without a restart. An invalid entry is refused rather than a partial deny list applied. Quote IPv6 entries in yaml (`["::1"]`): a bare `::1` inside a flow sequence is a yaml syntax error. |  |
| `security.jwks` | How far a client-declared `jwt` gate's key-set URL may reach. | `{"allow_http": false, "allow_private": false}` |
| `security.login_lockout` | Locking out an address after failed sign-ins. | `{"duration": "1m", "threshold": 5}` |
| `security.secure_cookies` | `true` = set the `Secure` flag on session cookies, which also lets the session cookie carry the `__Host-` prefix (`__Host-aperio_session`). The prefix matters here because the same server also serves other people's sites: without it, a tenant on a sibling hostname can set a cookie for the parent domain that the dashboard would read as its own. With this on, the unprefixed name is not read at all: a session is only ever the `__Host-` cookie, so a neighbour can neither displace one nor plant one for an operator who is signed out. Defaults to the `proxy.trust` value. |  |
| `security.server_side_targets` | A list of host/CIDR patterns a client may ask the server to reach itself, with `server_side: true` on a service, instead of relaying the request through the client. Same spellings as `outbound.allowlist`: an exact host, `*.suffix`, an IP, or a CIDR. **Unset permits nothing.** That default is the opposite of `outbound.allowlist` on purpose: that list governs callbacks the server makes on its own initiative, while this one opens a request and response channel a tenant steers, so an unset list has to mean deny. A service naming a target outside the list is refused at declaration, naming the target, rather than quietly relayed; the client's token must also carry `allow_server_side`. |  |
| `security.token_expiry_warning` | How long before a dynamic token's expiry at which a `token_expiring` webhook/audit event fires (once per token per expiry window; `0` = off). The dashboard tokens table shows an "expiring soon" badge in the last 24 h regardless. | `1d` |
| `security.token_pinning` | `true` = trust-on-first-use device pinning: the first client device key seen for a dynamic token is pinned, and a later connection presenting a different (or missing) key for that token is rejected, so a leaked token replayed from another machine cannot serve. See [Tokens & Authentication](tokens-and-auth.md). | `false` |

#### Shutdown (`shutdown`)

| Setting | Description | Default |
| --- | --- | --- |
| `shutdown.drain` | How long to let in-flight proxied requests finish (or `auto`) before shutdown ends the connections carrying them. Behind a load balancer this is the number that decides whether a deploy is invisible or shows up as a handful of 502s: the balancer needs long enough to take the instance out of rotation, and the requests it already sent need long enough to answer. `auto` sizes it from the drain budgets connected clients announce (the longest of them, since the drain is over when the slowest client has finished), capped at 30s, so a client cannot hold the process past a typical orchestrator grace period. | `0s` |
| `shutdown.timeout` | Time after which shutdown stops waiting for anything still holding a connection open (a proxied WebSocket, a TCP relay, a stalled peer) and exits. | `10s` |

#### Uptime history (`uptime`)

| Setting | Description | Default |
| --- | --- | --- |
| `uptime.tick` | Time between availability-history ticks for the dashboard Uptime panel (`GET /aperio/api/uptime`). Minimum `1s`. | `10s` |

#### Webhooks (`webhooks`)

| Setting | Description | Default |
| --- | --- | --- |
| `webhooks.retry_schedule` | Waits between webhook redelivery attempts after a transport error / 5xx / 429 (attempt count = schedule length + 1). Empty = no retries. See [Observability](observability.md#delivery-reliability--the-delivery-log). | `["1s", "5s", "25s", "1m"]` |

### Sites

The `sites` setting is everything the server does for a hostname, or a path under one, by itself: an answer given without a client, the policy on traffic a client serves, and what the hostname says when nothing serves it. Each entry names a `hostname` and, optionally, a `path`, and carries any of the settings below.

```yaml
# settings
sites:
  - hostname: "*"                     # every hostname
    headers:
      response:
        remove: [Server, X-Powered-By]
    waf:
      - path: "^/\\.git"             # probes for an exposed repo, 403
    pages:
      unavailable: "<h1>Nothing is serving this right now</h1>"
  - hostname: old.example.com
    redirect: {url: https://new.example.com, permanent: true, preserve_path: true}
  - hostname: app.example.com
    fallback: {url: https://status.example.com}
    maintenance:
      - {from: "02:00", to: "04:00", days: [sun], tz: Europe/Istanbul}
  - hostname: app.example.com
    path: /login
    rate_limit: {rps: 5, burst: 10}
```

**Which site a request falls under.** `hostname` is exact (`app.example.com`), a pattern (`*.example.com` for any subdomain, `*-pi.example.com` for one level of a naming convention), or `*` for every hostname, including a request that carries no `Host`. `path` is a prefix on a segment boundary: `/api` covers `/api/v1` but not `/apix`. A request takes **each setting from the most specific site that writes it**: an exact hostname before a pattern before `*`, then the longest `path`, then the order the sites were written in. So `/login` above is limited to 5 a second while every other site keeps whatever broader limit it has.

Three settings add up instead, because for them "most specific wins" would be the wrong answer. **`headers`** layer from the broadest site to the narrowest, so `*` sets what every hostname gets and a narrower site has the last word. **`waf`** rules of every matching site apply: a hostname that adds a rule of its own must not escape the scanner blocks `*` put up for everybody. **`maintenance`** windows of every matching site apply, for the same reason.

| Setting | Effect |
| --- | --- |
| `access` | Who may reach the site: `public`, a method block, or a list of them, the grammar of a client's `access:`. It wins over what any client serving the site declares, and a client overruled this way is told so in its own log. |
| `redirect` | Answer every request here with a redirect, without a client: `url`, `permanent` (`301` rather than `302`), `preserve_path` (append the request path and query). |
| `respond` | Answer with a fixed response: `status` (default `200`), `content_type` (default `text/html; charset=utf-8`), `body`. Typical uses: a "coming soon" page for a hostname whose client is not deployed yet, or a fixed `/robots.txt`. |
| `timeout` | How long to wait for the serving client's answer (`2m`), over `gateway.response_timeout` and any `timeout` the client declared. |
| `headers` | `request:` / `response:` edits, each an `add` map (replacing a header of the same name) and a `remove` list (case-insensitive), the same shape as a client service's `headers:`. Client rules run too, on the backend's side of the tunnel. Response edits happen before the response cache and the inspector see the response, so a `cache-control` added here is what all three see. |
| `rate_limit` | A ceiling on requests per second from every visitor together: `rps`, `burst` (default one second's worth), `methods` (only those verbs count, so a write path can be limited without throttling reads). One shared bucket per site, beside the per-visitor `limits.ip_rate`. A refused request answers `429` and names the limit in its `x-aperio-limit` header. |
| `canary` | Split the traffic between two versions of a service: `service` (the name the new version's client announces), `weight` (percent of visitors), optional `header`/`value` for opting in by hand. Decided per visitor, not per request. See [Canary releases](routing-and-load-balancing.md#canary-releases-and-weighted-routing). |
| `waf` | A small firewall evaluated before dispatch. Each rule ANDs the conditions it sets, a `path` regex (matched against the whole request path), `methods`, a `header` with `name` and `regex`, and a match answers `403`. A rule with `max_body` (`1MB`) is a size limit instead and answers `413` when a body exceeds it. A coarse first line of defense, meant to complement the rate limits, not replace a real WAF. |
| `fallback` | Where to send visitors when no client serves the hostname, instead of a `504`: `url`, `permanent`, `preserve_path`, as for `redirect`. A visitor refused by an allowlist never gets it, so the route's existence does not leak. |
| `pages` | The hostname's own `unavailable` (`504`) and `maintenance` (`503`) pages, the HTML itself, at most 512KB each. The `*` site's are the server's. A maintenance page fills in `{reason}` and `{until}` where it writes them. |
| `maintenance` | Recurring windows during which the hostname answers the maintenance page: `from` and `to` (`HH:MM`, a `to` earlier than `from` wraps midnight), optional `days` (`mon`..`sun`, the day a window *starts*), `tz` (an IANA zone, default `UTC`) and `reason`. See [Dashboard](dashboard.md#maintenance-mode) for the runtime flag beside it. |

A site that answers (`redirect` or `respond`) comes before client routing; maintenance still wins over it, and the visitor gate does not apply, since it serves operator-authored content. Because such a site never reaches a client, it cannot also carry `access`, `timeout`, `headers`, `rate_limit`, `canary`, `waf` or `fallback`: write those on a second site with the same hostname and path. `fallback`, `pages` and `maintenance` are about the whole hostname, so a site with a `path` cannot carry them.

A site that cannot work is refused when the settings are saved, naming it (`sites[2] (app.example.com/api): waf[0].path: not a regular expression`), and so is a site that sets nothing. `aperio-server check` reports the same before a deploy.

### Names

Everything Aperio addresses by name, an organization, a service, a tunnel, carries a **handle**: `a-z`, `0-9` and `_`, at most 64 characters. Nothing else, and deliberately so. A handle is written in one file and read in another, typed into a command line and joined with other handles to form an address (`payments@postgres`), so every character outside that set is a way for two people to write down what they think is the same name and be wrong: `Postgres` and `postgres`, `pg-main` and `pg_main`, an `i` that is actually `ı`.

What is left out stays available as *syntax around* a name rather than inside one: `@` already separates an organization from a tunnel, and `-`, `.` and `*` are reserved for whatever an address needs to say next.

Anything a person should read goes in `custom_name:` instead, free text, any language, any punctuation, changeable at any time, because nothing addresses it. Services and tunnels take it in `aperio.yaml`; an organization takes it when it is created and can be renamed from the dashboard afterwards. The handle never changes, since an expose listener and a binder's config on another machine point at it.

```yaml
services:
  web:
    custom_name: "Public Web"    # what the dashboard shows
    target: http://localhost:3000

tunnels:
  pg_main:
    custom_name: "Primary Postgres"
    target: 127.0.0.1:5432
```

## HTTP endpoints

Every endpoint the server answers, grouped by what it is for. The same list, with request and response schemas, is the OpenAPI document at `GET /aperio/api/openapi.json` (a dashboard session or admin key), which the dashboard's *API Explorer* renders and `aperio-client api openapi` prints; `aperio-client api ...` wraps most of these as commands, see [Admin API from the CLI](cli-api.md). A test in `aperio-server` checks that every annotated endpoint appears here, so a row missing from this table fails the build rather than the reader.

Three things hold for the whole `/aperio/api/` surface. The credential is a dashboard session cookie or a programmatic admin key (`Authorization: Bearer`), and the role floor is the one the dashboard enforces: reads need `viewer`, mutations `operator`, and users, sessions, settings, organizations and admin keys `admin`, with a few endpoints reserved for **the server admin** (`*: admin`), the server-global ones. A caller's organization fence applies before anything else, so a child organization's session only ever sees its own rows. And `security.admin_allowed_ips` fences the dashboard and the API to operator addresses, while the login, the probes, the OIDC flow and the visitor-auth endpoints stay reachable from anywhere.

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `/*` (fallback) | Proxied to tunnel clients, through the visitor gate. | the route's `auth:` policy, a share link, or a dashboard session on a hostname the caller's organization serves |
| `GET /aperio` | The dashboard, and its assets under `/aperio/assets/`. | a dashboard session; a browser without one is sent to the login |
| `GET /aperio/auth` | The login page. | none |
| `GET /aperio/oidc/login`, `GET /aperio/oidc/callback` | The OIDC flow; `?org=<id>` starts a child organization's own. | none |
| `GET /aperio/sso/login`, `GET /aperio/sso/callback` | A visitor's single sign-on, for a gate with `method: sso`, on the hostname the gate covers. | none |
| `GET /aperio/ws` | The tunnel endpoint clients connect to. | a tunnel token, as `Authorization: Bearer` or `x-auth-token` |
| `GET /aperio/tcp`, `GET /aperio/udp` | The relay endpoints a binder dials for a declared tunnel. | a tunnel token that may bind it |
| `GET /aperio/tunnels/:client_id` | Per-client tunnel discovery, the older spelling of `GET /aperio/tunnels` below. | master, the client's own token, or one in its organization with `allow_bind` |
| `GET /aperio/api/openapi.json` | The OpenAPI 3.1 document describing everything below. | a dashboard session or admin key |

#### Probes and the tunnel

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/health` | Liveness probe. Without a credential: status, the default UI language, and `panel`/`login_org` when the request hostname is a dashboard panel or a fenced login hostname. | none for liveness; the master token, a tunnel token, a dashboard session or an admin key for the numbers |
| `GET /aperio/healthz` | Liveness probe: 200 with an empty body, no locks taken. For container HEALTHCHECKs and Kubernetes livenessProbe. | none |
| `GET /aperio/metrics` | Prometheus text-format metrics. Requires the metrics token as `?token=` or `Authorization: Bearer`. | the metrics token (`metrics.token`), as `?token=` or `Authorization: Bearer` |
| `GET /aperio/readyz` | Readiness probe: 200 while the server should receive traffic, 503 once it is shutting down. | none |
| `GET /aperio/tunnels` | Lists the tunnels the presented token may bind. | a tunnel token (master, the declaring client's own, or one in its organization with `allow_bind`) |

#### Signing in

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/session` | The presented dashboard session: remaining lifetime (`expires_in_seconds`), `username`, `role`, `totp`, `server_admin`, `selected_org` and the `orgs` the session may act in. | a dashboard session or admin key |
| `POST /aperio/auth` | Login form submission (form-encoded username/password). On success sets the aperio_session cookie and redirects. | none (this is how a session is made) |
| `POST /aperio/auth/logout` | Drops the server-side session and expires the session cookie. | the session being ended |
| `GET /aperio/auth/passkey` | True when passkey sign-in is configured (dashboard.webauthn.origin set). | none |
| `POST /aperio/auth/passkey/discoverable/finish` | Completes a usernameless passkey sign-in; the credential's user handle identifies the account. | none (this is how a session is made) |
| `POST /aperio/auth/passkey/discoverable/start` | Starts a usernameless passkey sign-in; the returned challenge lets the authenticator pick from its resident credentials. | none (this is how a session is made) |
| `POST /aperio/auth/passkey/finish` | Completes a passkey sign-in; on success the aperio_session cookie is set (TOTP is not required for passkey sign-ins). | none (this is how a session is made) |
| `POST /aperio/auth/passkey/start` | Starts a passkey sign-in for a username: returns the WebAuthn request challenge and a ceremony id. | none (this is how a session is made) |

#### Live view, reports and diagnostics

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/activity` | Request volume per bucket (total and failed) for the activity chart: `range=15m` (5-second slices, the default), `2h` (2-minute) or `1d` (15-minute). | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/bandwidth` | Bytes in/out per token and hostname, bucketed per day or month (unit=day|month, count). | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/clients/{id}/config` | Effective configuration of one connection as a YAML document, plus every setting whose effective value differs from what was configured (a bandwidth budget divided across parallel connections, a cache opt-in the server ignores, an active overrule). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/clients/{id}/enabled` | Kill switch: enable/disable routing to one client without dropping its tunnel. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/clients/{id}/override` | Temporarily overrule a client's hostname/path bind server-side (empty values clear the override). | a dashboard session or admin key (operator+) |
| `GET /aperio/api/config/schema/{kind}` | JSON Schema of a configuration file, for editors and the dashboard's config builder. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/explain` | Dry run: which rule would answer a request to this hostname and path, and what every other stage saw. | a dashboard session or admin key (operator+) |
| `GET /aperio/api/export/traffic.csv` | Per-period traffic history as CSV (requests, bytes, latency) for the caller's org. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/logs` | Recent proxied requests (bounded ring buffer), optionally filtered by status, method and path. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/requests/{id}` | Full captured transaction (headers and possibly-truncated bodies) for the request inspector. | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/requests/{id}/replay` | Re-dispatches a captured request through the tunnel and returns the fresh response. | a dashboard session or admin key (operator+) |
| `GET /aperio/api/route-trends` | Per-route status-code trend: one-minute buckets (2xx/3xx/4xx/5xx counts) over the last 30 minutes. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/self-health` | Server process/memory/store/cache self-health snapshot. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/slow-endpoints` | Slowest endpoints by recent-window p95 latency (host|path, avg/p50/p95/max, request and 5xx counts). | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/stage-stats` | Rolling per-stage latency statistics (mean/stddev/last, µs) per route, with anomaly verdicts. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/stats` | Live statistics snapshot: counters, persistent stats, and the active client connections. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/stats/history` | Chronological traffic buckets (requests, success/failed, bytes, latency) for a rolling window (unit=day|week|month|year + count) or a custom day range (from/to, YYYY-MM-DD). | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/stream` | Server-Sent Events stream: named `traffic` events (one per proxied request), periodic `stats` events, `notification` events (server events, as webhooks receive them), and, for every topic named in `?topics=a,b` (or `?a&b`), that endpoint's document as an event of the same name, on connect and whenever it changes: `tokens`, `users`, `sessions`, `admin_keys`, `webhooks`, `deliveries`, `inbox`, `maintenance`, `scaling`, `orgs`, `subscribers`, `topology`, `stage_stats`, `uptime`, `route_trends`, `slow_endpoints`, `tunnels`, `audit`, `cache_stats`, `self_health`, `session`. | a dashboard session or admin key (viewer+); each topic takes the role its endpoint takes, refused by name otherwise |
| `GET /aperio/api/topology` | The routing map: routes, tunnel clients and backends, plus the routing the server owns with no client behind it (the sites that answer by themselves, public expose ports, offline token-granted binds) and the client-to-client dependencies of bound tunnels. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/uptime` | Uptime/SLA summary per service entity: current status, uptime percentages for today / 7 days / 30 days of observed time, and the last 30 daily buckets (seconds up/degraded/down). | a dashboard session or admin key (viewer+) |

#### Tokens

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/tokens` | Lists dynamic API tokens (hashes stripped; only the display prefix is exposed). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/tokens` | Creates a dynamic token; the plaintext secret is returned exactly once. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/tokens/refresh` | Slides a TTL-token's expiry forward by its creation TTL. Authenticates with the token secret itself (Bearer); no dashboard session needed. | the token's own secret, as `Authorization: Bearer` |
| `PUT /aperio/api/tokens/{id}` | Edits a token's scope/limits/expiry in place without changing the secret. | a dashboard session or admin key (operator+) |
| `DELETE /aperio/api/tokens/{id}` | Revokes a token and immediately drops any tunnel connections using it. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/tokens/{id}/rotate` | Rotates a token's secret; the old secret stays valid for grace_seconds. | a dashboard session or admin key (operator+) |

#### Ephemeral tunnels

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/tunnels` | Lists the tunnels declared by this organization's connected clients. | the master token as `Authorization: Bearer`, or a dashboard session / admin key (viewer+) |
| `POST /aperio/api/tunnels` | Programmatically provisions an ephemeral tunnel (scoped short-lived token + hostname). | the master token as `Authorization: Bearer`, or a dashboard session / admin key (operator+) |
| `DELETE /aperio/api/tunnels/{id}` | Deletes an ephemeral tunnel: revokes its token and drops its live connection. | the master token as `Authorization: Bearer`, or a dashboard session / admin key (operator+) |

#### Managed public TCP/UDP listeners

See [public expose configuration and deployment](public-exposes.md). What a caller may do with a listener comes with its role in the listener's organization, and the organization's port allocation bounds everyone but the server admin.

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/exposes` | List scoped listeners. | a dashboard session or admin key, any role in the listener's organization |
| `POST /aperio/api/exposes` | Create a listener. | a dashboard session or admin key, Admin in the organization, within its port allocation |
| `GET /aperio/api/exposes/export` | Export scoped configuration. | a dashboard session or admin key, any role in the listener's organization |
| `POST /aperio/api/exposes/import` | Preview or commit configuration import. | a dashboard session or admin key, Admin in the organization, within its port allocation; port allocations take the server admin |
| `GET /aperio/api/exposes/policies` | List visible port allocations. | a dashboard session or admin key, any role in the organization |
| `PUT /aperio/api/exposes/policies/{org}` | Set an organization's port allocation. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/exposes/{id}` | Inspect a listener. | a dashboard session or admin key, any role in the listener's organization |
| `PUT /aperio/api/exposes/{id}` | Update with revision check. | a dashboard session or admin key, Admin in the organization, within its port allocation |
| `DELETE /aperio/api/exposes/{id}` | Remove listener and sessions. | a dashboard session or admin key, Admin in the organization |
| `POST /aperio/api/exposes/{id}/actions` | Enable, disable, retry, drain or disconnect. | a dashboard session or admin key, Operator in the organization (enabling stays within its port allocation) |
| `GET /aperio/api/exposes/{id}/events` | Read scoped listener audit history. | a dashboard session or admin key, any role in the listener's organization |
| `GET /aperio/api/exposes/{id}/sessions` | Read active or retained session metadata. | a dashboard session or admin key, any role in the listener's organization |

#### Maintenance, share links and the cache

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `POST /aperio/api/cache/purge` | Drops response-cache entries matching a `hostname`, a URI `path_prefix`, and/or a `surrogate_key`; an empty object clears the whole cache (admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/cache/stats` | Response-cache entry count, byte size, and hit/miss rate. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/maintenance` | Hostnames and patterns currently in maintenance mode (`*` = every hostname, `*.example.com` = every subdomain of it, `*-pi.example.com` = one label's shape). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/maintenance` | Turns maintenance mode on/off for a hostname, a `*.example.com` subdomain wildcard, a partial label like `*-pi.example.com`, or `*` (503 page while on). The `*` wildcard is reserved for the master organization, and a named hostname has to be one the caller's organization serves. | a dashboard session or admin key (operator+) |

**Recurring maintenance windows (a site's `maintenance`).** A runtime flag is a switch somebody has to remember to flip; a window is written down, travels in an export, applies when saved and is still there after a restart. A site's `hostname` says where it applies (`*`, an exact name, or a pattern matched like a runtime flag's), and each window has `from:`/`to:` as `HH:MM`, an optional `days:` list (`mon`..`sun`, empty = every day), an optional IANA `tz:` (default UTC, because 02:00 means 02:00 where the operator lives, in March as well as in July) and an optional `reason:` that reaches the 503 page:

```yaml
# settings
sites:
  - hostname: "*.example.com"
    maintenance:
      - from: "02:00"
        to: "04:00"
        days: [sat]
        tz: Europe/Istanbul
        reason: "weekly database backup"
```

A window whose `to:` is earlier than its `from:` wraps midnight, and then `days:` names the day it starts. The windows of every site matching a hostname apply, the most specific site's first.
| `POST /aperio/api/purge` | Right-to-erasure: deletes traffic records (logs, inspector captures, stats aggregates, cache, access-log lines) matching a hostname, token label, or visitor IP (admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `POST /aperio/api/share` | Mints a signed, expiring share link that grants visitors gate-free access to a host/path scope. | a dashboard session or admin key (operator+) |

#### Webhooks and the inbox

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/inbox` | Inbound webhooks persisted for services with webhook_inbox: true (newest first, payloads omitted). | a dashboard session or admin key (viewer+) |
| `DELETE /aperio/api/inbox` | Deletes every webhook inbox entry of the caller's organization. | a dashboard session or admin key (operator+) |
| `GET /aperio/api/inbox/{id}` | One webhook inbox entry with redacted headers and payload. | a dashboard session or admin key (viewer+) |
| `DELETE /aperio/api/inbox/{id}` | Deletes one webhook inbox entry. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/inbox/{id}/refire` | Re-dispatches a stored webhook to the currently connected client for its route. | a dashboard session or admin key (operator+) |
| `GET /aperio/api/webhooks` | Lists webhook definitions (signing secrets are never exposed, only a signed flag). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/webhooks` | Creates a webhook; an optional HMAC signing secret (16-128 chars) enables signed deliveries. | a dashboard session or admin key (operator+) |
| `GET /aperio/api/webhooks/deliveries` | Recent webhook delivery outcomes (attempts, status, payload), newest first. | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/webhooks/deliveries/{id}/redeliver` | Queues a redelivery of the logged payload to the webhook's current URL (fresh signature, normal retry policy); the outcome lands in the delivery log as a new row. | a dashboard session or admin key (operator+) |
| `DELETE /aperio/api/webhooks/{id}` | Deletes a webhook definition. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/webhooks/{id}/test` | Sends one synthetic `webhook_test` event through the real delivery path (outbound policy, signature, timeout) and returns what the receiver answered. | a dashboard session or admin key (operator+) |

#### Messages between clients

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `POST /aperio/api/publish` | Publishes a message to the subscribers of this organization (operator). | a dashboard session or admin key (operator+) |
| `GET /aperio/api/subscribers` | Lists the client processes subscribed to messages in this organization, and their topic filters. | a dashboard session or admin key (viewer+) |

#### Autoscaling

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/scaling` | Autoscaling records armed for this organization, with live pool capacity and utilization. | a dashboard session or admin key (viewer+) |
| `DELETE /aperio/api/scaling/{id}` | Disarms an autoscaling record. A running client that still declares the block re-arms it on its next heartbeat. | a dashboard session or admin key (operator+) |

#### Users, sessions and second factors

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/me/passkeys` | Lists the signed-in user's registered passkeys (id, name, created_at only). | the signed-in user's own dashboard session (any role) |
| `POST /aperio/api/me/passkeys/register/finish` | Completes passkey registration with the browser's credential response and stores the passkey on the user. | the signed-in user's own dashboard session (any role) |
| `POST /aperio/api/me/passkeys/register/start` | Starts passkey registration for the signed-in dashboard user: returns the WebAuthn creation challenge and a ceremony id. | the signed-in user's own dashboard session (any role) |
| `DELETE /aperio/api/me/passkeys/{id}` | Deletes one of the signed-in user's passkeys. | the signed-in user's own dashboard session (any role) |
| `DELETE /aperio/api/me/totp` | Disables TOTP for the signed-in user. Requires a currently valid authenticator code (or an unused recovery code). | the signed-in user's own dashboard session (any role) |
| `POST /aperio/api/me/totp/enable` | Completes TOTP enrollment by verifying a code against the pending secret. | the signed-in user's own dashboard session (any role) |
| `POST /aperio/api/me/totp/setup` | Begins TOTP enrollment for the signed-in dashboard user: returns a fresh secret and otpauth:// URL. | the signed-in user's own dashboard session (any role) |
| `GET /aperio/api/sessions` | Live sessions with identity, IP, User-Agent and age; the caller's own session is marked. | a dashboard session or admin key (admin+) |
| `DELETE /aperio/api/sessions` | Ends every live session except the caller's own; everyone else must sign in again. | a dashboard session or admin key (admin+) |
| `DELETE /aperio/api/sessions/{id}` | Ends one session immediately; its cookie stops working on the next request. | a dashboard session or admin key (admin+) |
| `GET /aperio/api/users` | Lists dashboard users (admin only; password hashes are never exposed). | a dashboard session or admin key (admin+) |
| `POST /aperio/api/users` | Creates a dashboard user with a role, or a list of per-organization grants (admin only). | a dashboard session or admin key (admin+) |
| `PUT /aperio/api/users/{id}` | Updates a user's role or grants, enabled state, or password (admin only). | a dashboard session or admin key (admin+) |
| `DELETE /aperio/api/users/{id}` | Deletes a dashboard user (admin only). Live sessions of that user are dropped. | a dashboard session or admin key (admin+) |
| `DELETE /aperio/api/users/{id}/totp` | Clears TOTP for a user (admin only), the escape hatch when someone loses their authenticator and recovery codes. | a dashboard session or admin key (admin+) |

#### Organizations

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/orgs` | Lists organizations (server admin only): the implicit master org plus child orgs, with user/token counts. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `POST /aperio/api/orgs` | Creates a child organization (server admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `POST /aperio/api/orgs/select` | Switches the session's active organization (stored on the session). | a dashboard session or admin key whose grants reach the target (viewer+) |
| `DELETE /aperio/api/orgs/{id}` | Deletes an empty child organization (server admin only); rejected while it still has users or tokens. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `PUT /aperio/api/orgs/{id}/custom-name` | Sets an organization's display name; the handle it is addressed by never changes (server admin). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `PUT /aperio/api/orgs/{id}/hostnames` | Replaces a child org's hostname allowlist (empty list = unrestricted). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `PUT /aperio/api/orgs/{id}/oidc` | Sets or clears a child org's OIDC override (empty issuer clears). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `PUT /aperio/api/orgs/{id}/panel` | Sets or clears an organization's panel hostname, a name inside its allowlist whose root is its dashboard (Admin of that organization). | a dashboard session or admin key, Admin of that organization |
| `PUT /aperio/api/orgs/{id}/quota` | Sets a child org's quotas (max clients/tokens/users, monthly bytes). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/orgs/{id}/usage` | Current-month usage vs quota for an organization; also emits an org_usage webhook. | a dashboard session or admin key, **the server admin** (`*: admin`) |

#### Admin keys

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/admin-keys` | Lists programmatic admin keys (hashes stripped). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `POST /aperio/api/admin-keys` | Creates a scoped admin key; the secret is returned once. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `DELETE /aperio/api/admin-keys/{id}` | Revokes an admin key. | a dashboard session or admin key, **the server admin** (`*: admin`) |

#### Audit log

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/audit` | Audit events for the caller's organization. Unfiltered: the recent ring. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/audit/verify` | Verifies the audit log hash chain across all files; reports any broken line (server admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/export/audit.csv` | Audit events matching the same filters as /aperio/api/audit, as CSV, from the durable log. | a dashboard session or admin key (viewer+) |

#### Server settings, backup and import

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/export` | Downloads a logical dump. ?include= names the sections (tokens, webhooks, users, organizations, scaling, settings_overrides, statistics, uptime, activity, inbox, admin_keys); omitted, the six configuration sections. | a dashboard session or admin key, **`*` Admin** (every organization) |
| `POST /aperio/api/import` | Applies a dump created by /aperio/api/export; every section present in the document replaces its store, a missing one leaves it untouched (admin only). | a dashboard session or admin key, **`*` Admin** (every organization) |
| `GET /aperio/api/settings` | Effective server settings plus which keys are overridden from the dashboard. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `PUT /aperio/api/settings` | Applies dashboard settings overrides live and persists them (missing keys keep env defaults). | a dashboard session or admin key, **the server admin** (`*: admin`) |

#### Edge proxy and telemetry

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/edge/ask` | Answers 200 when the hostname is currently served, 404 otherwise. | `edge.token`, as `Authorization: Bearer` or `?token=` |
| `GET /aperio/api/edge/traefik` | Traefik dynamic configuration: one router per served hostname, pointing at this server. | `edge.token`, as `Authorization: Bearer` or `?token=` |
| `POST /aperio/otlp/v1/{signal}` | Forwards an OTLP protobuf export from a tunnel client to the server's configured collector. | a tunnel token carrying `allow_otel`, with `otel_bridge` on |

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`simple`](examples/simple/): minimal one-target pair
- [`multiple_services`](examples/multiple_services/): several backends from one client
- [`headers`](examples/headers/): header rules on both sides, and per service
- [`static_site`](examples/static_site/): serve local directories, one or several
