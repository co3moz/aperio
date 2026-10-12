# Admin API from the CLI

Everything the dashboard does is an HTTP call against the server's admin API, and `aperio-client api ...` is that API on the command line. Mint a share link from a deploy script, rotate a token from cron, flip maintenance mode from a Makefile, all without a browser session or a hand-written `curl`.

```bash
aperio-client api share --hostname app.example.com --path /test --expire 1d
```

Each command performs exactly one call, prints the server's JSON answer (pretty-printed) on stdout, and exits: `0` on success, `1` on any transport error or non-2xx status, with the message on stderr. Logs go to stderr too, so piping into `jq` is safe.

## Authentication

`admin` is another name for `api`: `aperio-client admin token list` is `aperio-client api token list`.

### Signing in

The quickest way to a working command line is to sign in once, the way `gh auth login` does:

```bash
# the built-in account, whose password is the master token
printf '%s' "$APERIO_SERVER_TOKEN" | aperio-client login --server-url https://tunnel.example.com --password -

aperio-client admin token list          # no key on the command line, none in a file
aperio-client profile list              # the servers signed in to, the one in use marked *
aperio-client profile use staging       # switch
aperio-client logout                    # forget it here, and revoke its key on the server
```

`login` signs in, mints an admin key for this machine (`--role admin --org '*'` by default, the server admin; narrow it with `--role` and `--org`, give it a lifetime with `--expire 90d`), closes the session again and keeps the key in `~/.aperio/profiles.yaml`, a file only you may read. `--user alice --password -` signs in as a named user and `--totp <code>` adds a second factor. Only the server admin mints keys, so anyone else is handed one and keeps it with `aperio-client login --server-url <url> --key -` instead. Signing in again to the same profile replaces its key and revokes the old one.

A profile is the fallback, not an override: an admin key given with `--server-api-key`, `APERIO_SERVER_API_KEY` or `server.api_key` wins, and the profile in use is sent only to its own server. A project whose `aperio.yaml` names another server never sends it that profile's key.

### Admin keys

The admin API is authenticated with a **programmatic admin key**, a credential that carries a role (`viewer` / `operator` / `admin`) and an organization, presented as `Authorization: Bearer`. The first one takes a single step on the server's host, no session or cookie involved:

```bash
# on the aperio-server host (or `docker exec` into it); the secret alone goes to stdout
KEY=$(aperio-server admin create-key --name ci)
```

That is the server admin, `--role admin --org '*'`; narrow it with `--role` and `--org`. A running server takes it up within a second. After that, create keys in the dashboard (*Settings*, then the *Admin Keys* section of the *Users* pane), or over the API itself:

```bash
# from anywhere, against the server's admin API
curl -s -X POST -u "aperio:$APERIO_SERVER_TOKEN" https://tunnel.example.com/aperio/auth -c jar.txt
curl -s -X POST -b jar.txt https://tunnel.example.com/aperio/api/admin-keys \
  -H 'Content-Type: application/json' -d '{"name":"ci","role":"operator"}'
```

A key acts in one organization with one role: `"org_id": "acme"` for Acme, none for master, `"*"` for every organization. Server-wide commands (`org`, `admin-key`, `settings`, `export`, `import`, port allocations) take the **server admin**, a `*` key with the `admin` role; an Admin key of master administers master and nothing about the server. Only the server admin mints keys.

The secret is returned exactly once. Hand it to the client the same way as any other setting:

| Surface | Name |
| --- | --- |
| CLI | `--server-api-key <KEY>` |
| Environment | `APERIO_SERVER_API_KEY` |
| `aperio.yaml` | `server.api_key` |

```yaml
# ./aperio.yaml (client)
server:
  url: https://tunnel.example.com
  token: apr_...      # tunnel token, for exposing services
  api_key: apk_...    # admin key, for `aperio-client api ...`
```

The server URL comes from the usual `--server-url` / `APERIO_SERVER_URL` / `server.url` layers.

When no admin key is configured and no profile is in use for that server, the tunnel token is sent instead. The server accepts it only where the master token is a valid credential (`api tunnel create` / `delete`), so CI jobs that only provision ephemeral tunnels need no admin key at all. Everything else answers with an authentication error.

A key's role gates what it can do, exactly as it does for a dashboard user: reads need `viewer`, mutations need `operator`, and users, settings, organizations, and admin keys themselves need `admin`. The export, import and session routes are `admin` reads rather than `viewer` ones, and `api explain` is refused below `operator`. Organization scoping applies too, a key bound to an org only ever sees that org's clients, tokens, and traffic.

## Scope flags: `--hostname` and `--path`

The commands that act on a hostname or a path bind reuse the client's own global flags rather than inventing new ones, so a hostname means the same thing everywhere:

```bash
aperio-client api share --hostname app.example.com --path /docs --expire 12h
aperio-client api token create --name ci --hostname a.example.com,b.example.com --path /api
aperio-client api cache purge --hostname app.example.com --path-prefix /assets/
```

Where the endpoint accepts several values (token permissions), the flag takes a comma-separated list. `--host` is an accepted alias for `--hostname`.

## Durations

Every lifetime flag (`--expire`, `--grace`) takes a human duration: `45s`, `30m`, `2h`, `1d`, `2w`, a bare number of seconds, or `never`. Invalid values are rejected before any request is sent.

`never` means what the endpoint means by "no expiry": a share link with no expiry, a token that never expires. On `--grace` it means `0`, an immediate cutover of the old secret, not a grace period that lasts forever.

## Commands

Run `aperio-client api --help`, or `aperio-client api <group> --help`, for the full list with every flag.

> **Reading the command listings:** Text in `<angle brackets>` must be replaced. Items in `[square brackets]` are optional alternatives; remove the brackets and choose the flags you need. These listings show syntax, while the complete commands in the examples above and below can be pasted after replacing their URLs and credentials.

### Share links

```bash
aperio-client api share --hostname app.example.com [--path /docs] [--expire 1d|never]
```

Mints a signed link that lets a visitor past the site's gate, whatever it asks for. See [Share Links](share-links.md).

### Tokens

```bash
aperio-client api token list
aperio-client api token create --name ci --hostname app.example.com --expire 30d \
  [--allowed-ip 10.0.0.0/8] [--max-rps 50] [--daily-max-bytes 1000000000] \
  [--allow-public] [--allow-otel] [--allow-bind] [--allow-server-side] [--topic deploy/#] [--canary]
aperio-client api token update <id> [--name new] [--hostname ...] [--expire never] \
  [--allowed-ip 10.0.0.0/8] [--max-rps 50] [--daily-max-bytes 1000000000] \
  [--allow-public|--no-allow-public] [--canary|--no-canary] \
  [--allow-otel|--no-allow-otel] [--allow-bind|--no-allow-bind] [--allow-server-side|--no-allow-server-side] \
  [--topic deploy/# ... | --clear-topics]
aperio-client api token rotate <id> [--grace 1h]
aperio-client api token revoke <id>
aperio-client api token refresh [--secret apr_...]
```

`create` and `rotate` print the secret once. `refresh` authenticates with the token secret itself (defaulting to `--server-token`), so a long-running job can slide its own expiry forward without holding an admin key. `--topic` is repeatable and names a filter the token may publish and subscribe on (`*` is accepted and stored as `#`); on `update` a `--topic` list replaces the old one and `--clear-topics` withdraws messaging altogether, and either takes effect on the clients already connected under the token. `--allow-bind` lets the token bind the [tunnels](emergency-tunnels.md) other clients of its organization declare; `--allow-server-side` lets its services ask to be [served from the server](server-side-services.md). See [Tokens & Authentication](tokens-and-auth.md).

### Ephemeral tunnels

```bash
aperio-client api tunnel create --name pr-42 [--hostname pr-42.example.com] [--expire 2h]
aperio-client api tunnel delete <id>
```

Provisions a scoped, short-lived token plus its hostname, the building block behind per-PR previews. Works with the master token when no admin key is set. See [Ephemeral Tunnels](ephemeral-tunnels.md).

### Maintenance mode

```bash
aperio-client api maintenance list
aperio-client api maintenance on app.example.com     # or * for every hostname
aperio-client api maintenance on '*.example.com' --reason "database migration" --ttl 1800
aperio-client api maintenance off app.example.com
```

While a host is flagged, visitors get the maintenance page instead of the backend. `*.example.com` flags every subdomain of a domain at once (not the apex, list it too if you want both). `--reason` is shown on the 503 page and in the dashboard; `--ttl` makes the flag lift by itself and makes `Retry-After` truthful, which is worth setting because the flag that causes an outage is the one switched on for twenty minutes of work and left up. Flags live in memory and a server restart clears them.

### Connected clients

```bash
aperio-client api client disable <client-id>      # kill switch: out of the routing pool
aperio-client api client enable  <client-id>
aperio-client api client config <client-id>       # the config it resolved, as it is running it
```

Client ids come from `api stats` or `api topology`.

### Response cache

```bash
aperio-client api cache stats
aperio-client api cache purge [--hostname app.example.com] [--path-prefix /assets/] [--surrogate-key build-7]
```

No filter clears the whole cache. See [Response Caching](caching.md).

### Webhooks and the inbox

```bash
aperio-client api webhook list
aperio-client api webhook create --name ops --url https://hooks.example.com/x \
  [--event client_connected] [--secret ...] [--format slack|discord|teams]
aperio-client api webhook delete <id>
aperio-client api webhook deliveries [--webhook-id <id>] [--limit 50]
aperio-client api webhook redeliver <delivery-id>
aperio-client api webhook test <id>               # one synthetic event down the real path
```

Where the server has an [outbound policy](threat-model.md) configured, `webhook create` refuses a URL the policy does not permit and reports why.

```bash
aperio-client api inbox list
aperio-client api inbox show <id>
aperio-client api inbox refire <id>
aperio-client api inbox delete <id>
aperio-client api inbox clear
```

### Users, sessions, organizations, and admin keys

```bash
aperio-client api user list
aperio-client api user create --username alice --password - --role operator
aperio-client api user create --username carol --password - --grant <acme-id>:admin --grant <beta-id>:viewer   # from master: one user, several organizations
aperio-client api user create --username alice@example.com --grant <acme-id>:admin              # no --password: signs in through the identity provider only
aperio-client api user update <id> [--role viewer] [--grant <org>:<role> ...] [--disable] [--password -]
aperio-client api user delete <id>
aperio-client api user sessions
aperio-client api user revoke <session-id> | --all
aperio-client api user reset-totp <user-id>

aperio-client api org list
aperio-client api org create --name acme [--hostname acme.com,*.acme.example.com] [--panel-hostname aperio.acme.example.com]
aperio-client api org hostnames <id> [--hostname "*.acme.example.com"]
aperio-client api org panel <id> [--hostname aperio.acme.example.com]   # a name inside the fence whose root is the org's dashboard; omit to clear
aperio-client api org quota <id> [--max-clients 10] [--max-tokens 20] [--max-users 5] [--max-bytes-month 0]
aperio-client api org custom-name <id> [--name "Acme Inc."]   # omit --name to show the handle again
aperio-client api org oidc <id> [--issuer https://idp.example.com --client-id ... --client-secret ... --allowed-email a@acme.com] [--default-role none] [--group-grant acme-ops=operator]
aperio-client api org usage <id>
aperio-client api org delete <id>
aperio-client api org select [<id>]

aperio-client api admin-key list
aperio-client api admin-key create --name ci --role operator [--org <id> | --org '*'] [--expire 90d]
aperio-client api admin-key revoke <id>
```

Passing `-` as a password reads it from stdin, so a secret never lands in the shell history:

```bash
printf '%s' "$NEW_PASSWORD" | aperio-client api user create --username alice --password - --role viewer
```

A quota of `0` clears that limit. `org hostnames` replaces the organization's hostname allowlist, which fences every bind made inside it; passing no `--hostname` clears the fence. See [Organizations](organizations.md).

### Autoscaling

```bash
# from anywhere, with an admin key
aperio-client api scaling list
aperio-client api scaling disarm <id>
```

Shows the records clients have armed with their live pool capacity and utilization, and disarms one. See [Autoscaling](autoscaling.md).

### Reports and diagnostics

```bash
aperio-client api stats            # live snapshot: clients, requests, latency
aperio-client api history --unit week --count 8         # or --from 2026-01-01 --to 2026-03-31
aperio-client api uptime
aperio-client api logs             # recent proxied requests
aperio-client api topology
aperio-client api slow-endpoints
aperio-client api bandwidth --unit month --count 6
aperio-client api route-trends
aperio-client api stage-stats
aperio-client api self-health
aperio-client api health           # liveness probe, needs no credential
aperio-client api traffic-csv --unit day --count 30   # raw CSV, not JSON
aperio-client api audit list
aperio-client api audit verify
aperio-client api request show <request-id>
aperio-client api request replay <request-id>
aperio-client api activity                        # five-second buckets, last 15 minutes
aperio-client api audit-csv                       # the audit trail as CSV
aperio-client api explain app.example.com --path /api --method POST
aperio-client api schema client                   # the JSON Schema for aperio.yaml
aperio-client api schema server
```

`api explain` is a dry run: it reports which rule would answer a request and what every other stage saw, consuming no rate limit and waking nothing.

### Message bus

```bash
aperio-client api publish orders/new --payload '{"id": 7}' [--qos 1]
aperio-client api publish sensors/raw --payload-base64 AAEC
aperio-client api subscribers                     # who is subscribed, and to what
```

### Edge integration

```bash
aperio-client api edge-traefik                    # Traefik dynamic configuration
aperio-client api edge-ask app.example.com        # does this server serve it?
```

Both need `edge.token` on the server, and present it as the ordinary API credential: `--server-api-key` (or `APERIO_SERVER_API_KEY` / `server.api_key`), sent as `Authorization: Bearer`. See [Edge proxy](edge-proxy.md).

### Settings, backup, and purging

```bash
aperio-client api settings get
aperio-client api settings set --file overrides.json

aperio-client api export > backup.json
aperio-client api export --include tokens,users,statistics > partial.json
aperio-client api import --file backup.json

aperio-client api purge --hostname app.example.com   # or --token-name ci, --ip 203.0.113.7
aperio-client api openapi                            # the OpenAPI document for this server
```

`settings set` and `import` both replace what they touch, so read the current state first (`settings get`, `export`). Both accept `-` as the file to read stdin.

`export` writes the configuration that rebuilds a deployment (`tokens`, `webhooks`, `users`, `organizations`, `scaling`, `settings_overrides`). `--include` names the sections instead, and adds the history the store also holds: `statistics`, `uptime`, `activity`, `inbox`, `admin_keys`. A misspelled name is an error rather than a silently missing section. Leave `organizations` out and only the master organization's rows travel, its statistics included, because a row whose organization does not exist on the target server is an orphan. `import` applies whatever sections the file holds, so the export is where the decision is made.

## Shell completion

```bash
aperio-client completions zsh > ~/.zfunc/_aperio-client   # then: autoload -U compinit && compinit
aperio-client completions bash > /etc/bash_completion.d/aperio-client
aperio-client completions fish > ~/.config/fish/completions/aperio-client.fish
```

Also `elvish` and `powershell`. The script is generated from the same definition the CLI is parsed from, so it cannot describe a flag that no longer exists, and it is printed before the client reads a config file or initializes logging: a completion run in a directory with a broken `aperio.yaml` still works, and nothing is pasted into the middle of the script.

## Scripting

The JSON output is the contract, so pipe it wherever it needs to go:

```bash
# Mint a preview tunnel in CI and export the credentials.
TUNNEL="$(aperio-client api tunnel create --name "pr-$PR" --expire 2h)"
export APERIO_SERVER_TOKEN="$(printf '%s' "$TUNNEL" | jq -r .token)"
echo "Preview: $(printf '%s' "$TUNNEL" | jq -r .url)"

# Post-deploy: drop the cached assets and put the site back.
aperio-client api cache purge --hostname app.example.com --path-prefix /assets/
aperio-client api maintenance off app.example.com

# Fail the job if the server is unhappy.
aperio-client api health | jq -e '.status == "healthy"' >/dev/null
```

Because a failed call exits non-zero, `set -e` scripts stop on an API error rather than continuing with an empty result.

## Related

- [Configuration Reference](configuration.md), every setting and the full HTTP endpoint list.
- [Tokens & Authentication](tokens-and-auth.md), what tokens and roles can do.
- [The Dashboard](dashboard.md), the same operations with a UI.


## Public TCP/UDP listeners

`aperio-client api expose` manages server-owned public listeners with the
same permissions as the dashboard: the key's role in the listener's
organization, within the port allocation the server admin gave it. It supports list/show, create and
update from a specification file, enable/disable/retry/drain/delete, session
inspection/disconnect, policy configuration, and scoped backup/restore with a
preview and revision check. See [public expose commands and examples](public-exposes.md#cli-and-api).
A tenant key needs explicit expose actions and a server port allocation; a
role such as operator alone does not authorize publishing a socket.
