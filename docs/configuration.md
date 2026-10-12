# Configuration

The two sides are configured differently. A **client** is a file next to the service it exposes, `aperio.yaml`, with an environment variable for every setting and a flag for the common ones. A **server** reads a few boot settings from its environment and keeps every other setting in its database, where the dashboard and the API change it without a restart.

Every setting of both sides is listed in the [Reference](reference.md). This page is how the two models work.

## Client

### One name, three spellings

A client setting has one name, its yaml key. The environment variable and the flag are derived from it:

| Spelling | Form | Example |
| --- | --- | --- |
| yaml key | `snake_case`, dotted inside a block | `server.token`, `health.interval` |
| Environment | `APERIO_` + the key, dots as underscores | `APERIO_SERVER_TOKEN`, `APERIO_HEALTH_INTERVAL` |
| Flag | `--` + the key, dots and underscores as dashes | `--server-token` |

A key of `defaults:` drops the prefix (`APERIO_TIMEOUT` is `defaults.timeout`), and `log_level` is the bare `LOG_LEVEL`. A value that is not one scalar is still reachable: a list is comma-separated (`APERIO_ALLOWED_IPS=10.0.0.0/8,203.0.113.7`), a map of strings is `k=v,k=v`, and anything can be written as yaml (`APERIO_ACCESS='{method: basic, users: "admin:secret"}'`). Only maps of entries (`services:`, `tunnels:`, `binds:`) and `include:` have no environment spelling.

Durations carry a unit (`250ms`, `30s`, `5m`, `2h`, `1d`) and sizes carry one (`512KB`, `10MB`, `1GB`). A bare number is read as seconds or bytes.

### Layers

The client merges four layers, lowest first:

**`~/.aperio.yaml` < `./aperio.yaml` < environment < command line**

Mappings merge key by key and anything else is replaced, so `~/.aperio.yaml` can hold `server.url` and `server.token` for every project while each project's file adds its services. `aperio-client check` reports which layer supplied each value.

A key nothing reads is an error that names the key it was probably meant to be, or what replaced it when an earlier release read it. Top-level keys starting with `x-` are left alone, for yaml anchors.

A config file is **hot-reloaded**: a change is picked up within about five seconds, the new connections come up before the old ones drain, and a file that no longer parses is ignored with a warning while the previous configuration keeps serving. A few keys apply only at start (how the client dials the server, its identity, `otel_bridge`, `pid_file`, logging); a reload that changes one says so by name.

### A file

```yaml
# ~/.aperio.yaml, shared by every project on this machine
server:
  url: https://tunnel.example.com
  token: ${APERIO_SERVER_TOKEN}
```

```yaml
# ./aperio.yaml
defaults:                 # every service, unless its entry says otherwise
  access: public
  timeout: 60s
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
```

A file names its backends under `services:`, keyed by name, even when there is one. The key is the service's [name](#names); `custom_name:` is where anything human goes. An entry is laid over `defaults:` by the merge rule: a key the entry writes wins, a block it writes (`health:`, `limits:`) is merged with the default block field by field.

Several services share one tunnel connection unless one of them says otherwise, and a service on a connection of its own grows a small pool under load. See [Connections](traffic.md#connections).

A one-liner runs a single service from the command line or the environment, in place of the file's `services:`:

```bash
aperio-client http://localhost:3000 \
  --server-url https://tunnel.example.com --server-token apr_xxxxxxxxxxxxxxxx \
  --hostname app.example.com --access public
```

```bash
APERIO_SERVER_URL=https://tunnel.example.com APERIO_SERVER_TOKEN=apr_xxxxxxxxxxxxxxxx \
APERIO_TARGET=http://localhost:3000 APERIO_HOSTNAME=app.example.com APERIO_ACCESS=public \
aperio-client
```

A bare port expands to `http://localhost:<port>` and a bare host to `http://<host>`, on the command line and in `target:` alike.

### Splitting a file and keeping secrets out of it

`include:` reads other files first, relative to the file that names them. Includes merge in order, later ones win, and the including file wins over all of them. Chains nest five deep; a cycle or a missing file is an error naming the file. Hot-reload watches every file that contributed.

```yaml
include: [shared/defaults.yaml, services/prod.yaml]
services:
  web:
    target: http://localhost:3000
```

`${NAME}` and `${NAME:-default}` are expanded from the environment before the yaml is parsed. A variable that is not set and has no default is an error, not an empty string. A bare `$NAME` is left as written, since `$` appears in passwords and shell snippets.

`aperio-client check` warns when a credential (`token`, `psk`, `client_secret`, `password`, `api_key`, `device_key`, `secret`) is written literally rather than as `${VAR}`.

### Kinds of service

A service exposes one of:

- **`target:`**, an HTTP backend: `http://`, `https://`, `h2c://` or `h2://` (HTTP/2, for gRPC), or `unix://` (a Unix socket).
- **`serve:`**, a directory of static files, served by the client itself with no backend.
- **`tcp_target:`**, a raw TCP backend.

#### Static files (`serve:`)

`aperio-client --serve ./dist` publishes a folder. The client runs a file server on a random loopback port and exposes it through the tunnel, so every feature of an HTTP service applies.

- It answers `GET` and `HEAD`. A directory resolves to its `index.html`. Files are streamed from disk.
- Single `Range` requests are answered with `206`, `If-Range` is honoured, and every response carries a strong `ETag` from the file's size and modification time.
- `serve_spa: true` answers a browser navigation that matches no file with the root `index.html`. A request for a missing asset (`Accept: */*`) still gets `404`.
- `serve_404: ./dist/404.html` is the page for every other miss.
- `serve_spa` and `serve_404` apply to every `serve:` of the process.

The root is canonicalised at start, and a request path is rejected if it contains `..`, a backslash or a colon, or if it resolves (through a symlink) outside the root. Everything under the root is published, a `.env` or `.git/` included, so serve a build output directory rather than a working tree.

#### Reaching the target from the server (`server_side:`)

When the server can already reach a service's target, `server_side: true` sends requests straight there instead of through the client:

```yaml
services:
  internal_api:
    target: http://10.0.0.5:8080
    hostname: api.example.com
    server_side: true
```

It takes two permissions: the server's `security.server_side_targets` must name the address (unset permits nothing), and the token must carry `allow_server_side`. A target outside the list is refused and the service is left out of routing; it never falls back to the tunnel. Routing, gates, metrics and the inspector are unchanged; failover, the client's health probe and its bandwidth pacing do not apply to the last hop. WebSockets are spliced by the server. `serve:` cannot be combined with it. See [Architecture](architecture.md#server-to-a-clients-target) for the trust boundary.

#### Start order

```yaml
services:
  db:
    tcp_target: 127.0.0.1:5432
  api:
    target: http://localhost:3000
    depends_on: [db]
    startup_delay: 2s
```

`depends_on` waits for those services to have a live tunnel, then starts anyway after 60 seconds. It orders startup only: a service that is up stays up whatever its dependency does later. An unknown name, a self-dependency and a cycle are refused at start.

#### Editing headers

`headers:` edits traffic on the client: `request` rules change what the backend receives, `response` rules what the visitor receives. `add` sets a header, replacing one of the same name; `remove` strips headers by name.

```yaml
defaults:
  headers:
    request:
      add: {X-Forwarded-Env: staging}
    response:
      remove: [Server, X-Powered-By]
  security_headers: true
```

`security_headers: true` adds `Strict-Transport-Security`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and `Referrer-Policy: strict-origin-when-cross-origin`; a mapping picks them one by one and adds `csp`. Rules written in `headers:` win over the preset. Hop-by-hop and WebSocket headers are always Aperio's.

### Commands

```
aperio-client                          Run from the files and the environment
aperio-client 3000                     Expose http://localhost:3000
aperio-client bind [NAME...]           Listen locally for tunnels a peer declares
aperio-client init 3000 --hostname app.example.com
                                       Write the aperio.yaml that command line means
aperio-client check                    Check configuration and connectivity
aperio-client api <group> <action>     Call the admin API (also: admin)
aperio-client login --server-url URL   Sign in and keep a key for `api`
aperio-client logout [profile]         Forget a profile and revoke its key
aperio-client profile list|use NAME    The servers signed in to
aperio-client completions SHELL        Print a completion script
```

| Flag | Key |
| --- | --- |
| `--server-url`, `--server-token`, `--server-api-key` | `server.url`, `server.token`, `server.api_key` |
| `--server-ip-family`, `--server-proxy` | `server.ip_family`, `server.proxy` |
| `--name`, `--client-id` | `name`, `client_id` |
| positional, `--serve`, `--hostname`, `--path` | the one service's `target`, `serve`, `hostname`, `path` |
| `--access`, `--allowed-ips`, `--priority`, `--pass-hostname` | `defaults.access`, `defaults.allowed_ips`, `defaults.priority`, `defaults.pass_hostname` |
| `--config FILE` | the file to read instead of `./aperio.yaml` |

`aperio-client init` writes the `aperio.yaml` a one-liner means, with the token as `${APERIO_SERVER_TOKEN}`. It refuses to replace a file without `--force`; `--stdout` prints instead.

`aperio-client check` resolves the configuration, reports the layer behind each value, and checks every hop: the server's health and release, the token (a real handshake), every target and its health endpoint, every `serve:` directory and every declared tunnel. It exits non-zero on any failure, so it works as a deploy gate.

### When the server does not apply a setting

The client asks and the server, with the client's token, decides. When the server does not apply something a service asked for, the client's own log says so once per connection, as `The server says, about web: ...`. That covers:
- an `access:` the token does not permit, or a site's `access` deciding instead;
- a hostname or path the token does not grant;
- `cache:` while the server's cache is off;
- a refused `server_side`;
- invalid `allowed_ips` entries;
- a `scaling:` block the server does not honour;
- OTel exports the server drops.

### Editor support

Releases attach JSON Schemas for both sides, at a stable URL:

```
https://github.com/co3moz/aperio/releases/latest/download/aperio-client.schema.json
https://github.com/co3moz/aperio/releases/latest/download/aperio-server.schema.json
```

```jsonc
// .vscode/settings.json, with the YAML extension
{ "yaml.schemas": { "https://github.com/co3moz/aperio/releases/latest/download/aperio-client.schema.json": ["aperio.yaml"] } }
```

`aperio-server schema` prints the server's, and building the client writes both to `schemas/`.

## Server

### Boot environment

The server reads only these variables, the ones it needs before its database opens. Any variable an earlier release read is refused at start with the setting that replaced it.

| Variable | Description | Default |
| --- | --- | --- |
| `APERIO_SERVER_TOKEN` | **Required.** The master token: authenticates clients and signs in to the dashboard as the built-in admin `aperio`. |  |
| `HOST` | Bind address. | `0.0.0.0` |
| `PORT` | Listen port. | `8080` |
| `APERIO_DATA_DIR` | The database, the audit log and the metrics token. Mount a volume here. | `./data` |
| `LOG_LEVEL` | `error`, `warn`, `info`, `debug` or `trace`. | `info` |
| `APERIO_REUSEPORT` | `1` binds with `SO_REUSEPORT`, so a new process can start before the old one stops. | `0` |
| `APERIO_BACKUP_KEY` | Key that encrypts backup snapshots: 64 hex characters or base64 of 32 bytes. Unset writes them in the clear. |  |
| `APERIO_BACKUP_KEY_FILE` | File holding that key. Refused inside `backup.dir`. |  |
| `APERIO_INIT_FILE` | A settings document applied on the first start, into an empty database. See [The first start](#the-first-start). |  |

### Changing a setting

A setting has a dotted path, `section.field`: `proxy.trusted`, `cache.enabled`, `limits.request_body`. The same document is read and written by the dashboard's **Settings** page, by `GET`/`PUT /aperio/api/settings`, and by `POST /aperio/api/import` with a `settings` section.

A change is checked first: a value the server cannot honour, or an unknown key, is refused with its path. It is then stored, in force for the next request, and audited as `settings_updated`.

- **Most settings apply at once.** Those read at start (`dashboard.enabled`, `dashboard.webauthn`, `oidc`, `metrics.enabled`, `otel`, `logging.access_log`, `outbound.proxy`, `outbound.no_proxy`, `edge.token`, `backup`, `tls`) are stored and reported as `restart_pending` until the next start.
- **Security settings need more.** The paths that decide who reaches what, or where the server may be steered, need Admin in every organization (`*`) and, from a dashboard session, a sign-in from the last ten minutes. They are audited again as `settings_security_changed`. `GET /aperio/api/settings` lists them as `security_paths`.
- **Credentials are never read back.** Secrets read as `********`, and writing `********` back keeps the stored value.

### The first start

A server started into an empty data directory takes its first settings from the document `APERIO_INIT_FILE` names, yaml or json, in the shape an export writes: a `settings:` section and, optionally, `tokens`, `users`, `organizations`, `webhooks`, `scaling` or `exposes`.

```yaml
# server-init.yaml
include: [sso.yaml]
settings:
  proxy:
    trusted: [10.0.0.0/8]
  cache:
    enabled: true
  sites:
    - hostname: app.example.com
      path: /login
      rate_limit: {rps: 5}
```

It is read once. From the second start the database holds the settings and the file is not consulted, so the dashboard and the file never disagree about which is the truth. It is read with the client's loader, so `include:` and `${VAR}` work the same way, and a document the server refuses stops the start with the setting named.

### Keeping settings in git

```bash
# The current settings, only what differs from the defaults.
curl -fsS -b cookies.txt "https://tunnel.example.com/aperio/api/export?include=settings" > aperio-settings.json
# Check a document without applying it.
aperio-server check aperio-settings.json
# Apply it to a running server.
curl -fsS -b cookies.txt -H 'Content-Type: application/json' \
  --data @aperio-settings.json https://tunnel.example.com/aperio/api/import
```

### The `aperio-server` command

| Command | What it does |
| --- | --- |
| `aperio-server` (or `run`) | Start the server. |
| `aperio-server settings show [--full] [--json]` | Print the stored settings, only what differs from the defaults unless `--full`. |
| `aperio-server settings reset [path]` | Put one setting, one section, or every setting back to its default. |
| `aperio-server apply FILE` | Store a document's settings for the next start of a stopped server. |
| `aperio-server check FILE` | Check a document without applying it. |
| `aperio-server schema` | Print the settings JSON Schema. |
| `aperio-server audit verify` | Verify the audit log's hash chain. |
| `aperio-server backup decrypt IN [OUT]` | Decrypt a backup snapshot. |
| `aperio-server admin create-key --name NAME [--role admin] [--org '*'] [--expire 90d]` | Mint an admin key and print its secret once. |
| `aperio-server admin list-keys`, `admin revoke-key ID` | List or revoke admin keys. |

The commands read `APERIO_DATA_DIR` like the server does. `settings` and `apply` are for a stopped server; `admin` works against a running one, which picks up a new or revoked key within a second.

**Getting back in.** The master token always signs in as `aperio`. If a setting locks the dashboard out (an `security.admin_allowed_ips` that excludes you, a `dashboard.hostname` nobody can reach), stop the server and run `aperio-server settings reset security.admin_allowed_ips`.

### Sites

`sites` is everything the server does for a hostname, or a path under one, by itself: an answer without a client, policy on the traffic a client serves, and what the hostname says when nothing serves it.

```yaml
sites:
  - hostname: "*"                       # every hostname
    headers:
      response: {remove: [Server]}
    waf:
      - path: "^/\\.git"               # 403
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

`hostname` is exact, a pattern (`*.example.com`, `*-pi.example.com`), or `*` for every hostname. `path` is a prefix on a segment boundary: `/api` covers `/api/v1`, not `/apix`. A request takes **each setting from the most specific site that writes it**: exact hostname before pattern before `*`, then the longest path, then the order written. `headers`, `waf` and `maintenance` add up across every matching site instead, so a narrower site cannot escape what `*` sets.

| Setting | Effect |
| --- | --- |
| `access` | Who may reach the site, in the [access grammar](access.md). It wins over what the clients serving the site declare. |
| `redirect` | Answer with a redirect, without a client: `url`, `permanent`, `preserve_path`. |
| `respond` | Answer with a fixed response: `status`, `content_type`, `body`. |
| `timeout` | How long to wait for the client's answer, over `gateway.response_timeout`. |
| `headers` | `request:` and `response:` edits, the shape of a client's `headers:`. |
| `rate_limit` | Requests per second from every visitor together: `rps`, `burst`, `methods`. A refusal is `429` naming the limit in `x-aperio-limit`. |
| `canary` | Send a share of visitors to another version: `service`, `weight`, optional `header`/`value`. See [Canary releases](traffic.md#canary-releases). |
| `waf` | Rules evaluated before dispatch: a `path` regex, `methods`, a `header` with `name` and `regex`, all ANDed, answered `403`; or `max_body`, answered `413`. |
| `fallback` | Where to send visitors when no client serves the hostname, instead of `504`. |
| `pages` | The hostname's own `unavailable` (`504`) and `maintenance` (`503`) HTML. The `*` site's are the server's. |
| `maintenance` | Recurring windows answering the maintenance page: `from`, `to` (`HH:MM`, wrapping midnight when `to` is earlier), `days`, `tz`, `reason`. |

A site that answers (`redirect`, `respond`) never reaches a client, so it cannot also carry `access`, `timeout`, `headers`, `rate_limit`, `canary`, `waf` or `fallback`; write those on a second site. `fallback`, `pages` and `maintenance` are about the whole hostname and take no `path`. A site that cannot work is refused when saved, naming it.

### Names

An organization, a service and a tunnel are addressed by a **handle**: `a-z`, `0-9` and `_`, at most 64 characters. A handle is written in one file and read in another, so anything that could be spelled two ways is left out. A handle never changes; what a person reads goes in `custom_name:`, which is free text and can change at any time.

## Examples

Runnable pairs in [`docs/examples`](examples/README.md): [`simple`](examples/simple/), [`multiple_services`](examples/multiple_services/), [`headers`](examples/headers/), [`static_site`](examples/static_site/).
