# Reference

Every setting of both sides, and every HTTP endpoint. The two settings sections are generated from the doc comments in `aperio-config`, the same text an editor shows from the JSON Schema and the dashboard shows beside a field; change the comment, then run `APERIO_BLESS=1 cargo test -p aperio-config reference`. How the settings are written and changed is in [Configuration](configuration.md).

## Client settings

A client setting is a key of `aperio.yaml`, and its variable is derived from the key (see [One name, three spellings](configuration.md#one-name-three-spellings)).

<!-- BEGIN GENERATED client-settings -->

### The client

What the client as a whole is, and how it reaches its server.

| Variable | Key | Description |
| --- | --- | --- |
| `APERIO_ALLOW_SERVER_FORWARD` | `allow_server_forward` | Answer a `forward` check (`via: client`) whose endpoint the *server* wrote, in its own visitor gate, rather than one of this client's services. Such a check makes this client call a URL the server chose, from this client's network, so it is refused unless this is set; the checks this client's own `access:` declares are always answered. Default: `false`. |
| `APERIO_BANDWIDTH` | `bandwidth` | Total link capacity of this client: a budget divided across every service and every parallel connection, so the sum the server is allowed to push never exceeds it. Bit suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte suffixes (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000. |
| `APERIO_CLIENT_ID` | `client_id` | This client's instance id, a UUID kept across restarts so failover can recognize the process when it comes back. Unset, one is generated on the first run and kept in `client_id_file`. |
| `APERIO_CLIENT_ID_FILE` | `client_id_file` | Where a generated `client_id` is kept. Default: a file under `~/.aperio/clients/` named after the config file this client reads, so two projects on one machine never share an id and the file never lands in a repository. A client with no config file, or one that cannot write here, runs with a fresh id each time. |
| `APERIO_DEVICE_KEY` | `device_key` | Trust-on-first-use device key announced with the tunnel token, so a server with token pinning on can bind that token to this machine. |
| `APERIO_DEVICE_KEY_FILE` | `device_key_file` | File holding the device key; a random one is generated and kept there on the first run. Ignored when `device_key` is set directly. Default: beside the kept `client_id`, under `~/.aperio/clients/`, for a client that reads a config file; a client with none announces no key unless one is given. |
| `APERIO_IDLE_TIMEOUT` | `idle_timeout` | Shut this client down after it has served no request for this long (unset = never). The scale-in half of `scaling`: the server never stops anything, an idle client retires itself. The shutdown is graceful (the server stops routing to it first, in-flight requests finish), and the timer only starts once the client has served its first request, so a slow cold start cannot make it exit before it is ever used. |
| `APERIO_LOG_FORMAT` | `log_format` | Log output format: `json`, or `pretty`/`text` for the human-readable form. Unset auto-detects, a TTY gets `pretty` and a pipe gets `json`. |
| `LOG_LEVEL` | `log_level` | Log verbosity of this client (`RUST_LOG` overrides it). Default: `info`. |
| `APERIO_MAX_MESSAGE_SIZE` | `max_message_size` | Largest single tunnel frame this client accepts. Default: `32MB`. |
| `APERIO_MESSAGES_LISTEN` | `messages.listen` | Local address the HTTP face listens on: subscribe with SSE, publish with POST. Unset = no local listener. |
| `APERIO_MESSAGES_MQTT_LISTEN` | `messages.mqtt_listen` | Local address an MQTT listener answers on, for an application that would rather use the MQTT client library it already has. MQTT 3.1.1, QoS 0; the protocol never leaves this machine. Unset = no MQTT listener. |
| `APERIO_NAME` | `name` | What to call this client on screen and in the server's logs, e.g. `eu_server_1`. Lowercase letters, digits and `_`, like a service name. A label, not an address: `client_id` stays the identity that failover recognizes this process by, because that needs something unique and a name is not. Unset shows the id. |
| `APERIO_OTEL_BRIDGE_LISTEN` | `otel_bridge.listen` | Address for the OTLP/HTTP receiver, the one every SDK can reach with `OTEL_EXPORTER_OTLP_ENDPOINT`. Default: `127.0.0.1:4318`. |
| `APERIO_OTEL_BRIDGE_LISTEN_GRPC` | `otel_bridge.listen_grpc` | Address for the OTLP/gRPC receiver, for SDKs pinned to that transport. Unset = no gRPC listener. Conventionally port 4317. |
| `APERIO_OTEL_BRIDGE_QUEUE` | `otel_bridge.queue` | Exports to hold when the far end is not keeping up. Past this, the newest is dropped and counted, never waited on: an exporter that cannot hand off its batch blocks the application it is instrumenting, so telemetry must never be the reason a tunnel stalls. Default: `256`. |
| `APERIO_OTEL_BRIDGE_TRANSPORT` | `otel_bridge.transport` | How exports reach the server: `tunnel` sends them as frames on the WebSocket this client already holds, `https` posts them to the server's endpoint as ordinary requests. Default: `tunnel`. `tunnel` is the one that keeps the "exactly one outbound connection" property that makes this worth having; `https` exists because a client whose telemetry is bursty may prefer it to stay off the tunnel entirely, where it would share flow control with proxied traffic. |
| `APERIO_PID_FILE` | `pid_file` | Path to write this process's pid to at startup, removed on a clean exit. For an init system that wants one; a process supervisor usually knows the pid without being told. |
| `APERIO_RELOAD_DRAIN` | `reload_drain` | How long in-flight requests may finish when a configuration reload stops a service, before its tunnel connection is dropped. The client announces `Draining` first, so the server sends it nothing new while it finishes. `0s` drops the connection immediately. Default: `10s`. |
| `APERIO_SCALING_COLD_START` | `scaling.cold_start` | How long a visitor request may be held while a cold start completes (`0s` = do not hold, answer immediately). Default: `45s`. |
| `APERIO_SCALING_COOLDOWN` | `scaling.cooldown` | Minimum gap between two calls for this bind. A new instance needs time to appear; without this the server would ask again while it is still starting. Default: `60s`. |
| `APERIO_SCALING_MAX` | `scaling.max` | Ceiling the server will never ask to exceed (0 = only cold starts, no scale-out). Default: `0`. |
| `APERIO_SCALING_MIN` | `scaling.min` | Instances that should always be running. `0` opts into scale-to-zero: a request for an unserved hostname triggers a cold start instead of a 504. Default: `0`. |
| `APERIO_SCALING_SECRET` | `scaling.secret` | Sent as `Authorization: Bearer` on the outgoing call. Write-only: the server never echoes it back and never logs it. |
| `APERIO_SCALING_TARGET_UTILIZATION` | `scaling.target_utilization` | Pool utilization above which the server asks for one more instance, between 0 and 1. Default: `0.8`. |
| `APERIO_SCALING_URL` | `scaling.url` | Endpoint the server POSTs to when it wants more capacity. HTTPS only unless the server allows plain HTTP; private and loopback addresses are refused, since the caller is a lower-trust credential than an operator. |
| `APERIO_SCALING_WINDOW` | `scaling.window` | How long utilization must stay above the target before scaling out. Guards against reacting to a single spike. Default: `15s`. |
| `APERIO_SERVE_404` | `serve_404` | Static-file services: HTML file served with status 404 for misses the SPA fallback does not cover. Applies to every `serve:` directory of this client. |
| `APERIO_SERVE_SPA` | `serve_spa` | Static-file services: answer a navigation (`Accept: text/html`) that matches no file with the root `index.html` and status 200, so a client-side router owns its routes. A missing asset still 404s. Applies to every `serve:` directory of this client. Default: `false`. |
| `APERIO_SERVER_API_KEY` | `server.api_key` | Admin API key used by `aperio-client api ...` calls, never by the tunnel itself. |
| `APERIO_SERVER_IP_FAMILY` | `server.ip_family` | IP family used to dial the server: `auto` (tries both), `ipv4`, or `ipv6`. Set `ipv4` when the server hostname resolves to an IPv6 address this host cannot reach. Default: `auto`. |
| `APERIO_SERVER_MIN_TLS_VERSION` | `server.min_tls_version` | Lowest TLS version offered when dialing the server over `wss://`: `1.2` or `1.3`. Unset leaves rustls' own set (1.2 and 1.3) in place, which is the right default; pin it when a policy has to name the floor rather than inherit it. A value this client cannot offer is refused at startup. |
| `APERIO_SERVER_PROXY` | `server.proxy` | HTTP proxy to dial the server through, for a network that allows no direct outbound connection: `host:port`, or `http://host:port`, with an optional `user:password@` in front. The client sends `CONNECT` and then runs TLS inside the tunnel the proxy opens, so the proxy sees the server's hostname and nothing else. Your own backends are never reached through it. Unset: dial the server directly. |
| `APERIO_SERVER_TLS_CIPHER_SUITES` | `server.tls_cipher_suites` | Exact cipher suites offered when dialing the server, by their IANA names, comma-separated. Unset leaves rustls' preference order alone, which is almost always better. An unknown name is refused at startup. |
| `APERIO_SERVER_TOKEN` | `server.token` | Tunnel token (master or a scoped dynamic token) that authorizes this client. |
| `APERIO_SERVER_URL` | `server.url` | URL of the Aperio server this client dials out to. |
| `APERIO_SERVER_URLS` | `server.urls` | Further server URLs to fail over to, tried in order when `url` is unreachable, for a redundant control plane. |
| `APERIO_SUBSCRIBE` | `subscribe` | Topic filters this client subscribes to, for messages from the other clients of its organization. MQTT filter syntax: `+` is one level, `#` is the rest. `$aperio/...` carries the server's own events. An entry is a bare filter, or an object that also names a command to run when a message arrives. |

### A service: where its traffic goes

The one service of the command line and the environment, or a key of a `services:` entry.

| Variable | Key | Description |
| --- | --- | --- |
| `APERIO_TARGET` | `target` | Local backend to expose: a port (`3000`), a host, or a URL. `h2c://` / `h2://` targets are dialed over HTTP/2 (gRPC backends, trailers relayed); `unix://` targets forward over a Unix domain socket. |
| `APERIO_SERVE` | `serve` | Serve a local directory of static files instead of forwarding to a backend (instead of `target`); directories serve their `index.html`. |
| `APERIO_TCP_TARGET` | `tcp_target` | Raw TCP backend instead of HTTP (instead of `target`). |
| `APERIO_HOSTNAME` | `hostname` | Public hostname(s) that route to the service: one, or a list. Each must be permitted by the client's token. `*` serves every hostname no other service claims, and needs a token that is not limited to particular hostnames. A service naming none is reached only by a hostname its token grants, or a random subdomain the server hands out. |
| `APERIO_PATH` | `path` | Public path prefix that routes to the service. |
| `APERIO_CUSTOM_NAME` | `custom_name` | What to call the service on screen. Free text: any language, any punctuation, spaces. Nothing addresses it, so nothing breaks when it changes. |

### A service: what it does with its traffic

Written under `defaults:` for every service, or on one `services:` entry, which wins.

| Variable | Key | Description |
| --- | --- | --- |
| `APERIO_ACCESS` | `access` | Who may reach the service: `public`, one method block, or a list of them (any one admits). Unset, the server's own `access` decides, and a site's `access` on the server wins over this either way. |
| `APERIO_ALLOWED_IPS` | `allowed_ips` | Visitor IPs/CIDRs allowed to reach the service; unset = everyone. Enforced by the server before dispatch. |
| `APERIO_CACHE_ENABLED` | `cache.enabled` | Let the server cache this service's GET responses, per their `Cache-Control`. Default: `false`. |
| `APERIO_CACHE_OFFLINE` | `cache.offline` | Keep serving this service's cached responses (marked, even past their lifetime) while no healthy client is connected, instead of failing with 504. Needs `enabled`. Default: `false`. |
| `APERIO_CIRCUIT_BREAKER_FAILURES` | `circuit_breaker.failures` | Consecutive failures that open the breaker. `0` disables it. Default: the profile's, off unless `resilience` is `aggressive`. |
| `APERIO_CIRCUIT_BREAKER_OPEN_FOR` | `circuit_breaker.open_for` | How long the breaker stays open before one request is let through to test the backend again. Default: `30s`. |
| `APERIO_CONNECT_TIMEOUT` | `connect_timeout` | How long to wait for the TCP connection to the backend, separate from `timeout`, which covers the whole request. A backend across a VPN needs longer than one on loopback. Unset: only `timeout` applies. |
| `APERIO_CONNECTIONS` | `connections` | Parallel tunnel connections; the server's `limits.connections_per_service` is the ceiling, announced on connect, and a token may lower it further. The server load-balances across them like separate clients, so a single dropped connection leaves no visitor-facing gap. A number opens exactly that many at startup; `{min: 1, max: 8}` opens the floor and grows towards the ceiling while requests queue up, then shrinks back when they stop. Default: `{min: 1, max: 4}` for a service on a connection of its own, and `1` for one that shares a connection or whose bandwidth is paced, since the rate is divided across the pool's ceiling. A connection the pool grew that the server refuses as full stands down rather than waiting for room. |
| `APERIO_CONNECTIONS_MAX` | `connections.max` | Most connections the pool may grow to under load. Default: `min`. |
| `APERIO_CONNECTIONS_MIN` | `connections.min` | Connections opened at startup and never dropped. Default: `1`. |
| `APERIO_DENIED` | `denied` | Redirect URL for visitors refused by `allowed_ips` when no other client of the route admits them. Unset = stealth: the same answer as an unclaimed route. |
| `APERIO_HEADERS_REQUEST_ADD` | `headers.request.add` | Headers to set, name → value; replaces an existing header of the same name. |
| `APERIO_HEADERS_REQUEST_REMOVE` | `headers.request.remove` | Header names to strip (case-insensitive). |
| `APERIO_HEADERS_RESPONSE_ADD` | `headers.response.add` | Headers to set, name → value; replaces an existing header of the same name. |
| `APERIO_HEADERS_RESPONSE_REMOVE` | `headers.response.remove` | Header names to strip (case-insensitive). |
| `APERIO_HEALTH_ENDPOINT` | `health.endpoint` | Endpoint to probe: a path like `/health` or a full URL. Unset = no probe, and the service is routable as soon as the tunnel is up. Against an `h2c://`/`h2://` target this names the **gRPC service** to health-check instead: the probe calls `grpc.health.v1.Health/Check`, because a plain GET cannot reach a server that speaks HTTP/2 with prior knowledge and routes by method name. `/` asks about the server as a whole, which is the usual answer. An absolute `http(s)://` URL still means an ordinary HTTP probe, for a backend exposing health on a separate port. |
| `APERIO_HEALTH_INTERVAL` | `health.interval` | Time between probes. Default: `10s`. |
| `APERIO_HEALTH_THRESHOLD` | `health.threshold` | Failed probes in a row before the backend is reported unhealthy. Default: `2`. |
| `APERIO_HEALTH_TIMEOUT` | `health.timeout` | How long to wait for each probe before counting it as failed. Default: `5s`. |
| `APERIO_HEALTH_WAIT_FOR_BACKEND` | `health.wait_for_backend` | Hold the service out of routing until the backend first accepts a connection, avoiding connection-refused errors while it boots (superseded by `endpoint` when that is set). Default: `false`. |
| `APERIO_INSPECTOR` | `inspector` | Record this service's transactions for the server's request inspector. Turning it off for a service that carries heavy traffic buys back a mutex, two header clones and a capture entry per request, at the cost of not being able to inspect or replay its requests afterwards. Default: `true`. |
| `APERIO_LIMITS_ADAPTIVE_CONCURRENCY` | `limits.adaptive_concurrency` | Lower the announced `concurrent_requests` while requests queue up waiting for a local permit, and climb back when they stop. The server already queues rather than dispatching past the announced number, so this is how a client that has become slow stops being sent work it cannot do; the server then holds the request, picks another client, or asks for capacity through autoscaling, all of which beat a refusal. Needs `concurrent_requests`, since that is the number being moved. Default: `false`. |
| `APERIO_LIMITS_CONCURRENT_REQUESTS` | `limits.concurrent_requests` | Most requests this service handles at once before the server queues the rest. Unset: no limit of its own. |
| `APERIO_LIMITS_REDIRECTS` | `limits.redirects` | How many backend redirects to follow transparently before passing one through. Default: `5`. |
| `APERIO_LIMITS_REQUEST_BODY` | `limits.request_body` | Largest request body visitors may upload to this service; the server rejects bigger uploads with 413 before they enter the tunnel. Unset: only the server's own `limits.request_body` applies. |
| `APERIO_LIMITS_RESPONSE_BODY` | `limits.response_body` | Largest response body this client relays to a visitor. Default: `50MB`. |
| `APERIO_METRICS_LABELS` | `metrics_labels` | Static Prometheus labels on the service's metric series, e.g. `{env: prod, region: eu-west}`, so one Prometheus can serve several environments without relabelling rules. At most 8 labels; names must be `[a-zA-Z_][a-zA-Z0-9_]*` and the server drops anything else, including the names it writes itself (`client_id`, `job`, `instance`, …). |
| `APERIO_MIN_TLS_VERSION` | `min_tls_version` | Lowest TLS version accepted from an `https://` backend: `1.2` or `1.3`. Default: rustls' own floor. |
| `APERIO_MULTIPLEX` | `multiplex` | Carry the service on one WebSocket shared with the client's other services, instead of a connection of its own. What it saves is the whole cost of a connection per service: one socket, one TLS session, one reader, one writer and one heartbeat. What it costs is that the services then share a link, so a large response occupies the writer the others send through, and a dropped connection takes all of them down together. Two services are needed for it to do anything, and `connections:` is not honored for a service that asks for it, one connection is what multiplexing means. Default: shared, for one of several services that writes nothing a shared socket would change (a `connections:` above one, a `bandwidth:` of its own, `startup_delay:`, `depends_on:` or `tcp_target:`); `false` keeps a connection of its own. |
| `APERIO_PASS_HOSTNAME` | `pass_hostname` | Forward the visitor's original Host header to the backend instead of the target's. Default: `false`. |
| `APERIO_PRIORITY` | `priority` | Failover tier (0 = primary, higher numbers are standbys). Default: `0`. |
| `APERIO_RESILIENCE` | `resilience` | How hard this service works to answer when its backend fails: `off`, `safe` (the default: an idempotent request that fails before any response is tried once more) or `aggressive` (three attempts, and a circuit breaker). `retry` and `circuit_breaker`, where written, win over the profile's values. |
| `APERIO_RESPONSE_TIMEOUT` | `response_timeout` | How long the server waits for this service to answer a dispatched request before failing it: the service's own `gateway.response_timeout`, for slow report and upload endpoints. Unset: the server's applies. |
| `APERIO_RETRY_ALL_METHODS` | `retry.all_methods` | Retry non-idempotent methods (POST, PATCH) as well. Off by default, because a retried write may reach the backend twice; the same reasoning as the server's `resilience.failover.all_methods`. Default: `false`. |
| `APERIO_RETRY_ATTEMPTS` | `retry.attempts` | Total attempts, including the first. `1` disables retrying. Default: the profile's, `2` unless `resilience` says otherwise. |
| `APERIO_RETRY_DELAY` | `retry.delay` | Wait before the second attempt, doubled before each further one. Default: the profile's, `100ms` unless `resilience` is `aggressive`. |
| `APERIO_SECURITY_HEADERS` | `security_headers` | Security response-header preset: `true` injects HSTS, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and `Referrer-Policy`; a mapping picks headers individually. |
| `APERIO_SECURITY_HEADERS_CSP` | `security_headers.csp` | `Content-Security-Policy` value to inject (no default, CSP is application-specific). |
| `APERIO_SECURITY_HEADERS_FRAME_OPTIONS` | `security_headers.frame_options` | `X-Frame-Options` value to inject. |
| `APERIO_SECURITY_HEADERS_HSTS` | `security_headers.hsts` | Inject `Strict-Transport-Security` (only meaningful behind HTTPS). |
| `APERIO_SECURITY_HEADERS_HSTS_MAX_AGE` | `security_headers.hsts_max_age` | HSTS `max-age`. Default: `730d` (two years). |
| `APERIO_SECURITY_HEADERS_NOSNIFF` | `security_headers.nosniff` | Inject `X-Content-Type-Options: nosniff`. |
| `APERIO_SECURITY_HEADERS_REFERRER_POLICY` | `security_headers.referrer_policy` | `Referrer-Policy` value to inject. |
| `APERIO_SERVER_SIDE` | `server_side` | Let the server reach `target` itself instead of relaying through this client, when the server's `security.server_side_targets` permits that address (needs a token that allows it). Saves the two hops a relayed request makes, and is only useful when the target is somewhere the server can already reach. Refused rather than ignored when the target is not permitted, and cannot be combined with `serve:`, whose files are on this machine. Default: `false`. |
| `APERIO_STARTUP_DELAY` | `startup_delay` | Wait this long before the service opens its tunnel, for a backend starting alongside the client that is not ready the moment the process is. Default: `0s`. |
| `APERIO_TIMEOUT` | `timeout` | How long to wait for the backend to answer before failing the request. Default: `30s`. |
| `APERIO_TRIM_BIND` | `trim_bind` | Strip the `path` prefix before forwarding, so the backend sees `/` and not the bind. Default: `true` when `path` is set. |
| `APERIO_WEBHOOK_INBOX` | `webhook_inbox` | Persist inbound POST requests (third-party webhooks) into the server's webhook inbox, for browsing and re-firing. Default: `false`. |

### Written only in a file

Maps of entries and the keys inside them, which have no flat spelling.

| Key | Description |
| --- | --- |
| `defaults` | What every service gets unless its own entry says otherwise: the same keys a `services:` entry takes, apart from where its traffic comes from and goes to. The environment and the command line write here too, so `APERIO_TIMEOUT=60s` is `defaults.timeout`. |
| `services` | The backends this client exposes, keyed by name. Each opens its own tunnel connection (or shares one, with `multiplex`). |
| `include` | Other config files to read before this one, each path relative to the file that names it. Mappings merge key by key and anything else is replaced, the same rule as every other layer: a fragment can add a service, and this file wins over all of its includes. Later includes win over earlier ones. Chains may nest up to five deep; a cycle is an error. |
| `services.<name>.bandwidth` | The service's share of the link: the server never pushes it faster than this, and the share is split across its `connections`. Settled against the top-level `bandwidth` budget when there is one. Bit suffixes (`kbit`/`mbit`/`gbit`) count as /8, byte suffixes (`kb`/`mb`/`gb`, or bare `k`/`m`/`g`) as x1000. |
| `services.<name>.depends_on` | Services in this file that must have a live tunnel before this one opens its own. Waits for them, then proceeds regardless after a bounded grace period, because a dependency that never arrives must not keep a service that could serve traffic off the air forever. |
| `tunnels` | Private local services a peer client may reach with `aperio-client bind`, keyed by the name binders use. Never exposed to the public web. |
| `tunnels.<name>.custom_name` | What to call it on screen. Free text: any language, any punctuation, spaces. Nothing addresses it, so nothing breaks when it changes. |
| `tunnels.<name>.encrypt` | End-to-end encrypt this tunnel between the two clients (X25519 + ChaCha20-Poly1305); the server only relays ciphertext. TCP only. Default: `false`. |
| `tunnels.<name>.idle_timeout` | UDP only: how long a relay may sit with no datagrams in either direction before it expires; binders learn it via tunnel discovery. Default: `60s`. |
| `tunnels.<name>.protocol` | Transport of the tunnel: `tcp`, `udp` (best-effort datagram relay), or `tcp/udp` for a service that is genuinely both (DNS, for instance). A combined tunnel is one tunnel with one name and one local port on the binder, answering on both transports. Default: `tcp`. |
| `tunnels.<name>.proxy_protocol` | Write a PROXY protocol v2 header to this backend before any payload byte, announcing the visitor's real address. TCP only. Without it the backend sees a connection from the client process and the visitor's address is lost at the last hop. Turn it on only when the backend is configured to expect the header (nginx `listen ... proxy_protocol`, HAProxy `accept-proxy`, MySQL `proxy-protocol-networks`): a backend that is not will read it as protocol garbage and drop the connection. Default: `false`. |
| `tunnels.<name>.psk` | Pre-shared key mixed into the key derivation of an encrypted tunnel, protecting against an actively hostile server. Never sent anywhere; the binder writes the same value in its `binds:` entry. |
| `tunnels.<name>.target` | Local address this client dials when a peer binds the tunnel. |
| `binds` | Tunnels `aperio-client bind` listens for on this machine, keyed by the tunnel's name: a local port, or `{port, address, token, psk}`. |
| `binds.<name>.address` | Local address the listener binds; anything but the default puts a deliberately unexposed service on the network and is warned about. Default: `127.0.0.1`. |
| `binds.<name>.port` | Local port for this tunnel. Unset reuses the declared target's port when it is free and unprivileged, and otherwise picks a stable port derived from the tunnel's name (logged at startup). |
| `binds.<name>.psk` | Pre-shared key of an end-to-end encrypted tunnel; must match the `psk` the declaring client wrote. Never sent to the server. |
| `binds.<name>.token` | Token to bind with, when the tunnel is reached with a different credential than `server.token`. |
| `subscribe[].env` | Extra environment variables for `run:`, on top of what the client sets. The command inherits the client's own environment as well; this is for the values that belong to *this* subscription. `APERIO_MESSAGE_TOPIC` and `APERIO_MESSAGE_ID` are set after these and cannot be overridden, since a command reading them must be able to trust what it finds. |
| `subscribe[].max_concurrent` | Runs allowed at once for this subscription. Messages that arrive while the cap is reached are dropped with a warning rather than queued: a queue for a command that cannot keep up is the same problem one step later. Default: `1`. |
| `subscribe[].run` | Command to run for each message, through the shell. The message body arrives on **stdin**, never as an argument; `APERIO_MESSAGE_TOPIC` and `APERIO_MESSAGE_ID` are set in the environment. Unset = deliver only. |
| `subscribe[].timeout` | How long a run may take before it is killed. A command that hangs must not hold the subscription's one slot forever. Default: `60s`. |
| `subscribe[].topic` | The topic filter to subscribe to. |

<!-- END GENERATED client-settings -->

## Server settings

A server setting is a path of the settings document, changed from the dashboard, `PUT /aperio/api/settings` or an import (see [Changing a setting](configuration.md#changing-a-setting)). The environment holds only the [boot settings](configuration.md#boot-environment). `sites` is written up in [Sites](configuration.md#sites), and `alerts.rules` in [Observability](observability.md#your-own-rules).

<!-- BEGIN GENERATED server-settings -->

### `access`

Who may reach a proxied route when nothing more specific says: `deny` (the default) refuses a route nothing declares, `public` serves it to anyone, and a method block or a list of them gates it. A client's `access:` speaks for its own service, and a site's `access` speaks for its hostname over both.

### `advanced`

What the defaults are chosen for and almost nobody needs to touch: per-stream flow control, and compressing tunnel frames.

| Setting | Description | Default |
| --- | --- | --- |
| `advanced.compression` | Offer per-message zlib compression to clients. Worth it on a slow or metered link carrying text; it costs CPU on both ends. | `false` |
| `advanced.stream` | Per-stream flow control for streamed responses, WebSockets and TCP. |  |
| `advanced.stream.backlog_limit` | Backlog past which a stream whose client ignores the pause is dropped. | `16MB` |
| `advanced.stream.min_throughput` | Bytes per second a stream's reader must take while data waits for it, or the stream is ended. `0` = no floor. | `0` |
| `advanced.stream.pause_at` | Backlog of one stream at which its client is asked to pause. | `2MB` |
| `advanced.stream.resume_at` | Backlog under which a paused client is asked to resume. | `512KB` |

### `alerts`

Built-in and operator-defined alerts.

| Setting | Description | Default |
| --- | --- | --- |
| `alerts.client_down` | How long a known service may stay down before a `client_down` alert. Unset = off. |  |
| `alerts.error_rate` | Failed-request fraction (`0.0` to `1.0`) that fires an `error_rate` alert. Unset = off. |  |
| `alerts.min_requests` | Requests the window must hold before the error rate may fire. | `20` |
| `alerts.rules` | Operator-defined alerts over what the server measures. | `[]` |
| `alerts.window` | Window the error rate is measured over. | `5m` |

### `audit`

Rotation of the tamper-evident audit log.

| Setting | Description | Default |
| --- | --- | --- |
| `audit.max_files` | Rotated generations kept. | `3` |
| `audit.max_size` | Rotate the audit log past this size; `0` = never. | `10MB` |

### `backup`

Scheduled snapshots of the database.

| Setting | Description | Default |
| --- | --- | --- |
| `backup.dir` | Directory the snapshots are written to. Required with `interval`. |  |
| `backup.interval` | Time between snapshots. Unset = no backups. |  |
| `backup.keep` | Snapshots kept. | `7` |

### `cache`

The server-side GET response cache for services that opt in.

| Setting | Description | Default |
| --- | --- | --- |
| `cache.enabled` | Turn the cache on for services that set `cache.enabled`. | `false` |
| `cache.max_stale` | How long past its lifetime a cached response may still answer while a service with `cache.offline` has no healthy client. | `1h` |
| `cache.negative_ttl` | How long an error or negative response (a `404`) is cached. `0s` = never. | `0s` |
| `cache.size` | Memory budget of the cache. | `64MB` |

### `dashboard`

The built-in dashboard and its sign-in.

| Setting | Description | Default |
| --- | --- | --- |
| `dashboard.enabled` | Serve the dashboard. The API stays up either way. | `true` |
| `dashboard.fenced_login` | Fence the sign-in to the hostname's organization. | `false` |
| `dashboard.hostname` | A hostname whose root is the dashboard. One exact name. |  |
| `dashboard.ui_language` | Language the dashboard and the login page open in when the browser's is not supported: en, de, es, fr, tr, ru, zh or ja. | `en` |
| `dashboard.webauthn` | Passkey sign-in. |  |
| `dashboard.webauthn.origin` | The public URL the dashboard is reached at; setting it turns passkeys on. Its domain becomes the relying-party id. |  |
| `dashboard.webauthn.rp_id` | A parent domain of the origin's host, so one passkey covers its subdomains. |  |

### `edge`

Publishing the served hostnames to a dynamic edge proxy.

| Setting | Description | Default |
| --- | --- | --- |
| `edge.cert_resolver` | Traefik certificate resolver on the generated routers. |  |
| `edge.entrypoints` | Traefik entry points on the generated routers. | `[]` |
| `edge.include_offline` | Also publish hostnames a token permits but no client serves. | `false` |
| `edge.service_url` | This server as the edge reaches it, e.g. `http://aperio:8080`. |  |
| `edge.token` | Credential the edge proxy presents; setting it turns the `/aperio/api/edge/*` endpoints on. |  |

### `gateway`

How long the server waits on a client.

| Setting | Description | Default |
| --- | --- | --- |
| `gateway.client_down_threshold` | Silence after which a client is dropped from routing (it rejoins on the next heartbeat); four times this closes its connection. | `15s` |
| `gateway.response_timeout` | How long a dispatched request waits for the client's answer. | `30s` |
| `gateway.timeout` | How long a request waits for a client to (re)connect before failing. | `10s` |

### `identity_headers`

Headers telling the backend who served and who visited.

| Setting | Description | Default |
| --- | --- | --- |
| `identity_headers.serving` | Tell the backend which client, organization and token served the request (`x-aperio-client-id`, `x-aperio-org`, `x-aperio-token`). | `false` |
| `identity_headers.visitor` | Tell the backend who the visitor is (`x-aperio-visitor-how`, `x-aperio-visitor-id`). | `false` |

### `inspector`

The dashboard's request inspector.

| Setting | Description | Default |
| --- | --- | --- |
| `inspector.enabled` | Record proxied transactions for the request inspector. | `true` |
| `inspector.redact` | Mask credential headers and secret-looking body fields in the inspector and its exports. | `true` |

### `limits`

Ceilings that protect the server: body size, concurrency, tunnels.

| Setting | Description | Default |
| --- | --- | --- |
| `limits.concurrent_requests` | Proxied requests in flight across every tunnel; beyond it a request is answered `429`. | `100` |
| `limits.connections_per_service` | Parallel connections one client may open for one service; a token may lower it, never raise it. | `16` |
| `limits.ip_rate` | The per-visitor-IP token bucket every request spends from. |  |
| `limits.ip_rate.burst` | Burst capacity. | `100.0` |
| `limits.ip_rate.per_second` | Requests per second the bucket refills by. | `5.0` |
| `limits.request_body` | Largest request body a visitor may send. | `10MB` |
| `limits.streams_per_ip` | Streamed responses one visitor address may hold open at once. `0` = no limit. | `0` |
| `limits.tunnels` | Tunnel connections at once. A client holds one per connection it runs: one for several services sharing it, which is their default, one per service that does not, and a few more for a pool under load while there is room. A reload briefly wants two. | `64` |
| `limits.websockets` | Proxied WebSockets open at once; beyond it an upgrade is answered `503`. `0` = no cap. | `10000` |

### `logging`

The per-request access log.

| Setting | Description | Default |
| --- | --- | --- |
| `logging.access_events` | Emit the per-request access event for successful requests (failures always log). | `true` |
| `logging.access_log` | File the structured access log is appended to, one JSON line per request. |  |
| `logging.sample_rate` | Fraction of successful requests that produce an access line, `0.0` to `1.0`. | `1.0` |

### `metrics`

The Prometheus endpoint at `/aperio/metrics`.

| Setting | Description | Default |
| --- | --- | --- |
| `metrics.enabled` | Serve `/aperio/metrics`. | `false` |
| `metrics.token` | Token required to scrape. Unset = a random one, generated once and kept in the data directory. |  |

### `oidc`

Signing in to the dashboard with an identity provider. It is the operators' sign-in and gates no visitor by being configured; a route that should ask for it says `access: {method: aperio}`, and visitors sign in at a provider of their own with `{method: sso}`.

| Setting | Description | Default |
| --- | --- | --- |
| `oidc.allowed_emails` | Who may sign in: exact addresses, `*@domain`, or `*`. | `[]` |
| `oidc.client_id` | OAuth client id registered at the issuer. |  |
| `oidc.client_secret` | OAuth client secret. |  |
| `oidc.default_grants` | What an email with no user record is granted at its first sign-in, `<org>:<role>` entries. Empty = refused until granted. | `[]` |
| `oidc.group_grants` | What each group means, `<group>=<org>:<role>` entries. | `[]` |
| `oidc.groups_claim` | The claim carrying the provider's groups. | `groups` |
| `oidc.issuer` | Issuer URL; setting it turns the dashboard's sign-on on. |  |
| `oidc.redirect_url` | Fixed callback URL (`https://<host>/aperio/oidc/callback`). Set it: derived from the request otherwise. |  |
| `oidc.scopes` | Requested scopes. | `openid email profile` |

### `otel`

OpenTelemetry trace export, and the bridge for clients' exports.

| Setting | Description | Default |
| --- | --- | --- |
| `otel.bridge` | Accept OTLP exports from tunnel clients and forward them to `endpoint`. | `false` |
| `otel.enabled` | Export a span per proxied request. | `false` |
| `otel.endpoint` | Collector base URL. |  |
| `otel.headers` | Extra headers on every OTLP request, where a collector's credential goes. | `{}` |
| `otel.protocol` | `http` or `grpc`. Unset: port 4317 is grpc, anything else http. |  |
| `otel.sample_rate` | Fraction of traces recorded, `0.0` to `1.0`. | `1.0` |
| `otel.service_name` | `service.name` on exported spans. | `aperio-server` |

### `outbound`

Where the server may send callbacks it makes on its own initiative (webhooks, autoscaling hooks, JWKS, OIDC, OTLP). Security-relevant.

| Setting | Description | Default |
| --- | --- | --- |
| `outbound.allowlist` | Destinations (hosts, `*.suffix`, IPs or CIDRs) callbacks may reach; empty = no restriction. | `[]` |
| `outbound.block_private` | With no allowlist, refuse destinations that are or resolve to internal addresses. | `false` |
| `outbound.no_proxy` | Destinations called directly rather than through `proxy`: names, or `.suffix` / `*.suffix`. | `[]` |
| `outbound.proxy` | HTTP proxy callbacks go through: `host:port` or `http://host:port`. |  |

### `proxy`

The reverse proxy or CDN in front of this server, and whose `X-Forwarded-For` to believe. Security-relevant.

| Setting | Description | Default |
| --- | --- | --- |
| `proxy.real_ip_header` | A header read before `X-Forwarded-For` for the visitor's address, such as `CF-Connecting-IP` behind Cloudflare. Only with a trusted proxy. |  |
| `proxy.trust` | Believe `X-Forwarded-For` from the direct peer. Turn on only behind a proxy that overwrites it. | `false` |
| `proxy.trusted` | The proxies and CDN egress ranges in front of this server (IPs or CIDRs). The visitor's address is found by walking `X-Forwarded-For` back past these. | `[]` |

### `request_id`

The request id sent to the backend and echoed to the visitor.

| Setting | Description | Default |
| --- | --- | --- |
| `request_id.enabled` | Send the request id to the backend and echo it to the visitor. | `true` |
| `request_id.header` | Header it travels in. | `x-request-id` |
| `request_id.trust_inbound` | Adopt the visitor's own value when the request carries one. | `false` |

### `resilience`

How hard the server works to answer when a client fails: a profile, `off`, `safe` (the default) or `aggressive`, and the failover and outlier settings it stands for, each of which can be written to differ.

| Setting | Description | Default |
| --- | --- | --- |
| `resilience.failover` | Re-dispatching a request whose client fails mid-flight. A field left out is the profile's. |  |
| `resilience.failover.all_methods` | Also fail over non-idempotent methods, which may then reach a backend twice. `false` in every profile: it is a decision about the backend, which no profile can make. |  |
| `resilience.failover.max_jumps` | Re-dispatch attempts per request. `2`, or `3` when aggressive. |  |
| `resilience.failover.mode` | What to do when a client dies mid-request: `fail`, `retry`, `wait` or `retry-wait`. `off`: `fail`; `safe`: `retry`; `aggressive`: `retry-wait`. |  |
| `resilience.failover.retry_on_5xx` | Re-dispatch a buffered response whose status is a retryable server error to another client, sharing the jump budget. `true` only when aggressive. |  |
| `resilience.failover.retry_statuses` | The statuses that count for `retry_on_5xx`; empty = every 5xx. `[502, 503, 504]` when aggressive. |  |
| `resilience.failover.window` | Total time the waiting modes may spend across all jumps. `15s`, or `30s` when aggressive. |  |
| `resilience.outlier` | Taking a client out of routing when it keeps failing, while its health probe still reports green. A field left out is the profile's. |  |
| `resilience.outlier.eject_for` | How long an ejected client stays out. `30s`, or `1m` when aggressive. |  |
| `resilience.outlier.enabled` | Turn passive ejection on. `false` only when the profile is `off`. |  |
| `resilience.outlier.max_failures` | Failures inside `window` that eject a client. `5`, or `3` when aggressive. |  |
| `resilience.outlier.window` | Window the failures are counted over. `30s`. |  |
| `resilience.profile` | `off` (a failure is the answer, nothing is ejected), `safe` (the default: an idempotent request is re-dispatched once to another client, and a client that keeps failing is taken out of routing for a while) or `aggressive` (more attempts, a retryable `5xx` re-dispatched too, a waiting failover). | `safe` |

### `retention`

How long recorded data is kept.

| Setting | Description | Default |
| --- | --- | --- |
| `retention.access_log` | How long access-log lines are kept. |  |
| `retention.audit` | How long audit events are kept; the hash chain stays verifiable. |  |
| `retention.captures` | How long inspector captures and webhook inbox entries are kept. Unset = kept, bounded only by their caps. |  |
| `retention.db_max_size` | Cap on the database file; past it the oldest low-priority data is pruned. Unset = unbounded. |  |
| `retention.stats` | How long day-granularity statistics are kept. |  |

### `routing`

How a request picks among the clients serving its route.

| Setting | Description | Default |
| --- | --- | --- |
| `routing.alternate_servers` | Other Aperio servers (`ws://` / `wss://`) a client may fall back to, announced on every handshake. At most 8. | `[]` |
| `routing.lb_strategy` | How a client is picked from the routed pool. | `round-robin` |
| `routing.preview_noindex` | Mark services reached through their random subdomain as not to be indexed (`X-Robots-Tag` and a disallow-all `robots.txt`). | `false` |
| `routing.random_subdomain` | Give every client a random hostname from this pattern, `*` being the random label: `*.example.com` or `*-test.example.com`. |  |

### `scaling`

Autoscaling hooks clients may declare.

| Setting | Description | Default |
| --- | --- | --- |
| `scaling.allow_http` | Allow a plain-http autoscaling endpoint. | `false` |
| `scaling.allow_private` | Allow an autoscaling endpoint on a private or loopback address. | `false` |
| `scaling.enabled` | Honor the `scaling:` blocks clients announce. | `false` |
| `scaling.record_ttl` | How long a record nothing re-announces is kept. | `30d` |

### `security`

Fences around the server itself. Security-relevant.

| Setting | Description | Default |
| --- | --- | --- |
| `security.admin_allowed_ips` | Addresses (IPs or CIDRs) allowed to reach the dashboard and its API. Empty = no restriction. | `[]` |
| `security.denied_ips` | Addresses refused everything, checked before any other rule. | `[]` |
| `security.jwks` | How far a client-declared `jwt` gate's key-set URL may reach. |  |
| `security.jwks.allow_http` | Allow a plain-http key-set URL in a client-declared gate. | `false` |
| `security.jwks.allow_private` | Allow a key-set URL resolving to a private or loopback address in a client-declared gate. | `false` |
| `security.login_lockout` | Locking out an address after failed sign-ins. |  |
| `security.login_lockout.duration` | The first lockout's length; it doubles with each repeat, up to an hour. | `1m` |
| `security.login_lockout.threshold` | Consecutive failed sign-ins from one address before it is locked out. | `5` |
| `security.secure_cookies` | Mark session cookies `Secure`. Unset, they are when `proxy.trust` or `tls.enabled` is on. |  |
| `security.server_side_targets` | Destinations (hosts, `*.suffix`, IPs or CIDRs) a client may ask this server to reach itself with `server_side: true`. Empty permits nothing. | `[]` |
| `security.token_expiry_warning` | How long before a token expires the `token_expiring` event fires. `0s` = never. | `1d` |
| `security.token_pinning` | Pin a dynamic token to the first device key that presents it. | `false` |

### `shutdown`

What a shutdown waits for.

| Setting | Description | Default |
| --- | --- | --- |
| `shutdown.drain` | How long in-flight requests may finish before their connections are ended, or `auto`. | `0s` |
| `shutdown.timeout` | When shutdown stops waiting for anything still open and exits. | `10s` |

### `sites`

What the server does for a hostname, or a path under one, by itself: answers without a client, policy on proxied traffic, and what a hostname says when nothing serves it. Each setting comes from the most specific site that writes it; `hostname: "*"` is every hostname.

### `tls`

Serving HTTPS directly, with no proxy in front: certificates from files, from an ACME authority, or both. Security-relevant, and read at start.

| Setting | Description | Default |
| --- | --- | --- |
| `tls.acme` | Certificates obtained and renewed from an ACME authority, Let's Encrypt unless another is named, proven with TLS-ALPN-01 on the HTTPS port. |  |
| `tls.acme.directory` | The authority's ACME directory. | `https://acme-v02.api.letsencrypt.org/directory` |
| `tls.acme.email` | Where the authority sends notices about the certificates, such as one about to expire that could not be renewed. |  |
| `tls.acme.enabled` | Obtain and renew certificates automatically. Turning it on agrees to the authority's subscriber agreement. | `false` |
| `tls.acme.hostnames` | Hostnames to hold a certificate for from the start, such as the dashboard's own. A wildcard needs a DNS-01 proof, which this server does not make. | `[]` |
| `tls.acme.on_demand` | Also obtain a certificate the first time a hostname a client serves is reached over HTTPS, holding that handshake until it is issued. Random subdomains are left out: each would be a certificate of its own, and an authority limits how many one domain may have. | `true` |
| `tls.certificates` | Certificates you manage yourself, as PEM files. Each serves the names written in it, a wildcard included, and is read again when its files change, so a renewal by another tool needs no restart. | `[]` |
| `tls.enabled` | Serve HTTPS on `port`, beside the plain listener on the boot `PORT`. Needs certificates: `certificates`, `acme`, or both. | `false` |
| `tls.port` | Port of the HTTPS listener, on the boot `HOST`. An ACME authority proves a hostname by connecting to it on 443, so with `acme` this is 443, or whatever forwards 443 to it. | `443` |
| `tls.redirect_http` | Answer plain HTTP with a redirect to HTTPS, except the health probes. Tunnel clients then have to dial `wss://`. | `false` |

### `uptime`

The availability history behind the dashboard's Uptime panel.

| Setting | Description | Default |
| --- | --- | --- |
| `uptime.tick` | Time between availability-history ticks. | `10s` |

### `webhooks`

Webhook delivery.

| Setting | Description | Default |
| --- | --- | --- |
| `webhooks.retry_schedule` | Waits between redelivery attempts after a failed delivery; empty = no retries. | `["1s","5s","25s","1m"]` |

<!-- END GENERATED server-settings -->

## HTTP endpoints

Every endpoint the server answers, grouped by what it is for. The OpenAPI document at `GET /aperio/api/openapi.json` has the same list with request and response schemas; the dashboard's *API Explorer* renders it, and `aperio-client api` wraps most of these as commands (see [Operations](operations.md#the-admin-api-from-the-command-line)). A test in `aperio-server` fails when an endpoint is missing here.

On the whole `/aperio/api/` surface the credential is a dashboard session or an admin key (`Authorization: Bearer`). Reads need `viewer`, changes `operator`, and users, sessions, settings, organizations and admin keys `admin`; a few server-global endpoints need **the server admin** (`*: admin`). The caller's organization fence applies first, so a child organization sees only its own rows. `security.admin_allowed_ips` fences the dashboard and the API, while the login, the probes, OIDC and the visitor sign-in stay reachable from anywhere.

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `/*` (fallback) | Proxied to tunnel clients, through the visitor gate. | the route's `access`, a share link, or a dashboard session on a hostname the caller's organization serves |
| `GET /aperio` | The dashboard, and its assets under `/aperio/assets/`. | a dashboard session; a browser without one is sent to the login |
| `GET /aperio/auth` | The login page. | none |
| `GET /aperio/oidc/login`, `GET /aperio/oidc/callback` | The OIDC flow; `?org=<id>` starts a child organization's own. | none |
| `GET /aperio/sso/login`, `GET /aperio/sso/callback` | A visitor's single sign-on, for a gate with `method: sso`, on the hostname the gate covers. | none |
| `GET /aperio/ws` | The tunnel endpoint clients connect to. | a tunnel token, as `Authorization: Bearer` or `x-auth-token` |
| `GET /aperio/tcp`, `GET /aperio/udp` | The relay endpoints a binder dials for a declared tunnel. | a tunnel token that may bind it |
| `GET /aperio/tunnels/:client_id` | Per-client tunnel discovery, the older spelling of `GET /aperio/tunnels` below. | master, the client's own token, or one in its organization with `allow_bind` |
| `GET /aperio/api/openapi.json` | The OpenAPI 3.1 document describing everything below. | a dashboard session or admin key |

### Probes and the tunnel

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/health` | Liveness probe. Without a credential: status, the default UI language, and `panel`/`login_org` when the request hostname is a dashboard panel or a fenced login hostname. | none for liveness; the master token, a tunnel token, a dashboard session or an admin key for the numbers |
| `GET /aperio/healthz` | Liveness probe: 200 with an empty body, no locks taken. For container HEALTHCHECKs and Kubernetes livenessProbe. | none |
| `GET /aperio/metrics` | Prometheus text-format metrics. Requires the metrics token as `?token=` or `Authorization: Bearer`. | the metrics token (`metrics.token`), as `?token=` or `Authorization: Bearer` |
| `GET /aperio/readyz` | Readiness probe: 200 while the server should receive traffic, 503 once it is shutting down. | none |
| `GET /aperio/tunnels` | Lists the tunnels the presented token may bind. | a tunnel token (master, the declaring client's own, or one in its organization with `allow_bind`) |

### Signing in

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

### Live view, reports and diagnostics

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/activity` | Request volume per bucket (total and failed) for the activity chart: `range=15m` (5-second slices, the default), `2h` (2-minute) or `1d` (15-minute). | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/bandwidth` | Bytes in/out per token and hostname, bucketed per day or month (unit=day|month, count). | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/clients/{id}/config` | Effective configuration of one connection as a YAML document, plus every setting whose effective value differs from what was configured (a bandwidth budget divided across parallel connections, a cache opt-in the server ignores, a setting the server did not apply). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/clients/{id}/enabled` | Kill switch: enable/disable routing to one client without dropping its tunnel. | a dashboard session or admin key (operator+) |
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

### Tokens

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/tokens` | Lists dynamic API tokens (hashes stripped; only the display prefix is exposed). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/tokens` | Creates a dynamic token; the plaintext secret is returned exactly once. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/tokens/refresh` | Slides a TTL-token's expiry forward by its creation TTL. Authenticates with the token secret itself (Bearer); no dashboard session needed. | the token's own secret, as `Authorization: Bearer` |
| `PUT /aperio/api/tokens/{id}` | Edits a token's scope/limits/expiry in place without changing the secret. | a dashboard session or admin key (operator+) |
| `DELETE /aperio/api/tokens/{id}` | Revokes a token and immediately drops any tunnel connections using it. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/tokens/{id}/rotate` | Rotates a token's secret; the old secret stays valid for grace_seconds. | a dashboard session or admin key (operator+) |

### Ephemeral tunnels

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/tunnels` | Lists the tunnels declared by this organization's connected clients. | the master token as `Authorization: Bearer`, or a dashboard session / admin key (viewer+) |
| `POST /aperio/api/tunnels` | Programmatically provisions an ephemeral tunnel (scoped short-lived token + hostname). | the master token as `Authorization: Bearer`, or a dashboard session / admin key (operator+) |
| `DELETE /aperio/api/tunnels/{id}` | Deletes an ephemeral tunnel: revokes its token and drops its live connection. | the master token as `Authorization: Bearer`, or a dashboard session / admin key (operator+) |

### Managed public TCP/UDP listeners

See [Public listeners](tunnels.md#public-listeners). What a caller may do with a listener comes with its role in the listener's organization, and the organization's port allocation bounds everyone but the server admin.

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

### Maintenance, share links and the cache

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `POST /aperio/api/cache/purge` | Drops response-cache entries matching a `hostname`, a URI `path_prefix`, and/or a `surrogate_key`; an empty object clears the whole cache (admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/cache/stats` | Response-cache entry count, byte size, and hit/miss rate. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/maintenance` | Hostnames and patterns currently in maintenance mode (`*` = every hostname, `*.example.com` = every subdomain of it, `*-pi.example.com` = one label's shape). | a dashboard session or admin key (viewer+) |
| `POST /aperio/api/maintenance` | Turns maintenance mode on/off for a hostname, a `*.example.com` subdomain wildcard, a partial label like `*-pi.example.com`, or `*` (503 page while on). The `*` wildcard is reserved for the master organization, and a named hostname has to be one the caller's organization serves. | a dashboard session or admin key (operator+) |
| `POST /aperio/api/purge` | Right-to-erasure: deletes traffic records (logs, inspector captures, stats aggregates, cache, access-log lines) matching a hostname, token label, or visitor IP (admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `POST /aperio/api/share` | Mints a signed, expiring share link that grants visitors gate-free access to a host/path scope. | a dashboard session or admin key (operator+) |

### Webhooks and the inbox

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

### Messages between clients

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `POST /aperio/api/publish` | Publishes a message to the subscribers of this organization (operator). | a dashboard session or admin key (operator+) |
| `GET /aperio/api/subscribers` | Lists the client processes subscribed to messages in this organization, and their topic filters. | a dashboard session or admin key (viewer+) |

### Autoscaling

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/scaling` | Autoscaling records armed for this organization, with live pool capacity and utilization. | a dashboard session or admin key (viewer+) |
| `DELETE /aperio/api/scaling/{id}` | Disarms an autoscaling record. A running client that still declares the block re-arms it on its next heartbeat. | a dashboard session or admin key (operator+) |

### Users, sessions and second factors

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

### Organizations

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

### Admin keys

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/admin-keys` | Lists programmatic admin keys (hashes stripped). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `POST /aperio/api/admin-keys` | Creates a scoped admin key; the secret is returned once. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `DELETE /aperio/api/admin-keys/{id}` | Revokes an admin key. | a dashboard session or admin key, **the server admin** (`*: admin`) |

### Audit log

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/audit` | Audit events for the caller's organization. Unfiltered: the recent ring. | a dashboard session or admin key (viewer+) |
| `GET /aperio/api/audit/verify` | Verifies the audit log hash chain across all files; reports any broken line (server admin only). | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `GET /aperio/api/export/audit.csv` | Audit events matching the same filters as /aperio/api/audit, as CSV, from the durable log. | a dashboard session or admin key (viewer+) |

### Server settings, backup and import

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/export` | Downloads a logical dump. `?include=` names the sections (`settings`, `tokens`, `webhooks`, `users`, `organizations`, `scaling`, `statistics`, `uptime`, `activity`, `inbox`, `admin_keys`, `exposes`); omitted, the configuration sections. | a dashboard session or admin key, **`*` Admin** (every organization) |
| `POST /aperio/api/import` | Applies a dump created by /aperio/api/export; every section present in the document replaces its store, a missing one leaves it untouched (admin only). | a dashboard session or admin key, **`*` Admin** (every organization) |
| `GET /aperio/api/settings` | The settings document, with which paths are security-relevant and which need a restart. | a dashboard session or admin key, **the server admin** (`*: admin`) |
| `PUT /aperio/api/settings` | Replaces the settings document; omitted fields take their defaults. Security-relevant paths take `*` Admin and a recent sign-in. | a dashboard session or admin key, **the server admin** (`*: admin`) |

### Edge proxy and telemetry

| Endpoint | What it does | Credential |
| --- | --- | --- |
| `GET /aperio/api/edge/ask` | Answers 200 when the hostname is currently served, 404 otherwise. | `edge.token`, as `Authorization: Bearer` or `?token=` |
| `GET /aperio/api/edge/traefik` | Traefik dynamic configuration: one router per served hostname, pointing at this server. | `edge.token`, as `Authorization: Bearer` or `?token=` |
| `POST /aperio/otlp/v1/{signal}` | Forwards an OTLP protobuf export from a tunnel client to the server's configured collector. | a tunnel token carrying `allow_otel`, with `otel_bridge` on |
