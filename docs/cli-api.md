# Admin API from the CLI

Everything the dashboard does is an HTTP call against the server's admin API, and `aperio-client api ...` is that API on the command line. Mint a share link from a deploy script, rotate a token from cron, flip maintenance mode from a Makefile, all without a browser session or a hand-written `curl`.

```bash
aperio-client api share --hostname app.example.com --path /test --expire 1d
```

Each command performs exactly one call, prints the server's JSON answer (pretty-printed) on stdout, and exits: `0` on success, `1` on any transport error or non-2xx status, with the message on stderr. Logs go to stderr too, so piping into `jq` is safe.

## Authentication

The admin API is authenticated with a **programmatic admin key**, a credential that carries a role (`viewer` / `operator` / `admin`) and an organization, presented as `Authorization: Bearer`. Create one in the dashboard (*Settings, Admin Keys*), or over the API itself:

```bash
curl -s -X POST -u "aperio:$APERIO_SERVER_TOKEN" https://tunnel.example.com/aperio/auth -c jar.txt
curl -s -X POST -b jar.txt https://tunnel.example.com/aperio/api/admin-keys \
  -H 'Content-Type: application/json' -d '{"name":"ci","role":"operator"}'
```

The secret is returned exactly once. Hand it to the client the same way as any other setting:

| Surface | Name |
| --- | --- |
| CLI | `--api-key <KEY>` |
| Environment | `APERIO_API_KEY` |
| `aperio.yaml` | `server.api_key` |

```yaml
server:
  url: https://tunnel.example.com
  token: apr_...      # tunnel token, for exposing services
  api_key: apk_...    # admin key, for `aperio-client api ...`
```

The server URL comes from the usual `--server-url` / `APERIO_SERVER_URL` / `server.url` layers.

When no admin key is configured the tunnel token is sent instead. The server accepts it only where the master token is a valid credential (`api tunnel create` / `delete`), so CI jobs that only provision ephemeral tunnels need no admin key at all. Everything else answers with an authentication error.

A key's role gates what it can do, exactly as it does for a dashboard user: reads need `viewer`, mutations need `operator`, and users, settings, organizations, and admin keys themselves need `admin`. Organization scoping applies too, a key bound to an org only ever sees that org's clients, tokens, and traffic.

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

`never` means what the endpoint means by "no expiry": a share link with no expiry, a token that never expires.

## Commands

Run `aperio-client api --help`, or `aperio-client api <group> --help`, for the full list with every flag.

### Share links

```bash
aperio-client api share --hostname app.example.com [--path /docs] [--expire 1d|never]
```

Mints a signed link that lets a visitor past the site's password or OIDC gate. See [Share Links](share-links.md).

### Tokens

```bash
aperio-client api token list
aperio-client api token create --name ci --hostname app.example.com --expire 30d \
  [--allowed-ip 10.0.0.0/8] [--max-rps 50] [--daily-max-bytes 1000000000] [--allow-public] [--canary]
aperio-client api token update <id> [--name new] [--hostname ...] [--expire never] [--no-canary]
aperio-client api token rotate <id> [--grace 1h]
aperio-client api token revoke <id>
aperio-client api token refresh [--secret apr_...]
```

`create` and `rotate` print the secret once. `refresh` authenticates with the token secret itself (defaulting to `--server-token`), so a long-running job can slide its own expiry forward without holding an admin key. See [Tokens & Authentication](tokens-and-auth.md).

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
aperio-client api maintenance off app.example.com
```

While a host is flagged, visitors get the maintenance page instead of the backend. Flags live in memory and a server restart clears them.

### Connected clients

```bash
aperio-client api client disable <client-id>      # kill switch: out of the routing pool
aperio-client api client enable  <client-id>
aperio-client api client override <client-id> --hostname other.example.com
aperio-client api client override <client-id> --clear
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
aperio-client api user update <id> [--role viewer] [--disable] [--password -]
aperio-client api user delete <id>
aperio-client api user sessions
aperio-client api user revoke <session-id> | --all
aperio-client api user reset-totp <user-id>

aperio-client api org list
aperio-client api org create --name acme [--hostname acme.com,*.acme.example.com]
aperio-client api org hostnames <id> [--hostname "*.acme.example.com"]
aperio-client api org quota <id> [--max-clients 10] [--max-tokens 20] [--max-users 5] [--max-bytes-month 0]
aperio-client api org usage <id>
aperio-client api org delete <id>
aperio-client api org select [<id>]

aperio-client api admin-key list
aperio-client api admin-key create --name ci --role operator [--org <id>] [--expire 90d]
aperio-client api admin-key revoke <id>
```

Passing `-` as a password reads it from stdin, so a secret never lands in the shell history:

```bash
printf '%s' "$NEW_PASSWORD" | aperio-client api user create --username alice --password - --role viewer
```

A quota of `0` clears that limit. `org hostnames` replaces the organization's hostname allowlist, which fences every bind made inside it; passing no `--hostname` clears the fence. See [Organizations](organizations.md).

### Reports and diagnostics

```bash
aperio-client api stats            # live snapshot: clients, requests, latency
aperio-client api history --unit week --count 8
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
```

### Settings, backup, and purging

```bash
aperio-client api settings get
aperio-client api settings set --file overrides.json

aperio-client api export > backup.json
aperio-client api import --file backup.json

aperio-client api purge --hostname app.example.com   # or --token-name ci, --ip 203.0.113.7
aperio-client api openapi                            # the OpenAPI document for this server
```

`settings set` and `import` both replace what they touch, so read the current state first (`settings get`, `export`). Both accept `-` as the file to read stdin.

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
aperio-client api self-health | jq -e '.status == "ok"' >/dev/null
```

Because a failed call exits non-zero, `set -e` scripts stop on an API error rather than continuing with an empty result.

## Related

- [Configuration Reference](configuration.md), every setting and the full HTTP endpoint list.
- [Tokens & Authentication](tokens-and-auth.md), what tokens and roles can do.
- [The Dashboard](dashboard.md), the same operations with a UI.
