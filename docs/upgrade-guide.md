# Upgrade Guide & Compatibility

How to upgrade Aperio safely, and what to expect from version skew between the
server and its clients.

## Configuration across an upgrade

A client file does not declare the release it was written for, and no layer
translates an old spelling into a new one. A key that was removed or renamed
is reported by name when the client reads the file, with what replaced it,
instead of the generic "not a setting" a typo gets. On the server, a variable
that moved into the settings refuses the start and names the setting, and a
stored settings document this build cannot read stops the start with the
setting named; `aperio-server settings reset` puts it back. The
[CHANGELOG](../CHANGELOG.md) lists every such change under the release that
made it, marked **Breaking**.

## Client ↔ server compatibility

**A client and a server must share a minor release.** `0.13.0` and `0.13.4`
talk to each other; `0.13.x` and `0.12.x` do not, in either direction. In a 0.x
version the minor number is where the tunnel protocol, the handshake and the
configuration are allowed to change, so neither side carries code for a peer
of another minor: there is no compatibility window and no negotiation of what
an older peer can do.

The rule is enforced **at the WebSocket upgrade**, before a socket exists, on
every tunnel endpoint (`/aperio/ws`, `/aperio/tcp`, `/aperio/udp`):

- The client announces its release (`x-aperio-release`). A server on another
  minor answers `426 Upgrade Required` with a sentence naming both releases and
  the side to upgrade. A client that announces nothing predates the rule and is
  refused the same way.
- The server announces its own release on the response, and a client refuses
  a server on another minor, or one that announces nothing, with the same
  sentence.
- Neither side gives up. A refused client keeps retrying on its normal backoff,
  so it connects by itself as soon as the other side is upgraded.

`aperio-client check` reports the pairing as its own line (`release`), so a
mismatch is visible before anything is started.

| Situation | Behavior |
| --- | --- |
| Same minor, any patch | Works. |
| Server on a newer minor | Refused at the upgrade: *Upgrade the client.* |
| Server on an older minor | Refused at the upgrade: *Upgrade the server.* |

**A patch release never forces anything.** Fixes ship as patches and pair with
every other patch of the same minor, so only a minor release asks for the
server and its clients to move together. Plan those as a short window: upgrade
the server, then the clients; services come back as each client is upgraded,
since a refused client reconnects on its own.

The versioned JSON Schemas (`aperio-client.<tag>.json`) are attached to each
GitHub Release, so an editor validates the exact keys a given version accepts.

## Upgrading to 0.13.0: the server's settings move into its database

The server no longer reads `aperio-server.yaml`, and reads only its boot
settings from the environment: `APERIO_SERVER_TOKEN`, `HOST`, `PORT`,
`APERIO_DATA_DIR`, `LOG_LEVEL`, `APERIO_REUSEPORT`, `APERIO_BACKUP_KEY` /
`APERIO_BACKUP_KEY_FILE`, and the new `APERIO_INIT_FILE`. Every other setting
lives in the database and is changed from the dashboard (**Settings**) or with
`PUT /aperio/api/settings`, without a restart. See
[Configuration](configuration.md#server-settings).

**Nothing is carried over silently, and nothing is lost silently either.** A
0.13 server that still finds one of the old variables in its environment
refuses to start and prints each one with the setting that replaced it
(`APERIO_TRUSTED_PROXIES is set, but it was moved into the settings as
proxy.trusted`). Moving a deployment over is four steps:

1. **Write the settings down as a document.** Take what `aperio-server.yaml`,
   the environment and the dashboard's overrides (`settings.json` in the data
   directory) say, and write it under `settings:` in the new spellings. The
   [reference](configuration.md#settings-reference) has every path; the common
   ones:

   | 0.12 | 0.13 |
   | --- | --- |
   | `trusted_proxies`, `trust_proxy`, `real_ip_header` | `proxy.trusted`, `proxy.trust`, `proxy.real_ip_header` |
   | `server.auth` / `server_auth` | `access.gate` |
   | `default_access`, `ignore_client_auth` | `access.default`, `access.ignore_client_auth` |
   | `admin_allowed_ips`, `denied_ips`, `secure_cookies` | `security.admin_allowed_ips`, `security.denied_ips`, `security.secure_cookies` |
   | `max_body_size`, `max_concurrent_requests`, `max_tunnels` | `limits.request_body`, `limits.concurrent_requests`, `limits.tunnels` |
   | `gateway_timeout`, `gateway_response_timeout` | `gateway.timeout`, `gateway.response_timeout` |
   | `lb_strategy`, `random_subdomain` | `routing.lb_strategy`, `routing.random_subdomain` |
   | `cache: true`, `cache.max_bytes` | `cache.enabled: true`, `cache.size` |
   | `oidc_issuer`, `oidc_*` | `oidc.issuer`, `oidc.*` |
   | `alert_error_rate: 5` (percent) | `alerts.error_rate: 0.05` (a fraction) |
   | `alert_rules:` | `alerts.rules:` |
   | `504_page`, `503_page` (a file path) | `pages.unavailable`, `pages.maintenance` (the HTML itself) |
   | `expose:` | expose listeners, from the dashboard's Tunnels page or the API ([Public exposes](public-exposes.md)) |

   Durations and sizes may keep their bare numbers (seconds and bytes), or
   take a unit: `30s`, `5m`, `10MB`. The lists (`routes`, `rate_limits`,
   `waf`, `fallbacks`, `error_pages`, `maintenance_windows`, `headers`) keep
   their shape.

2. **Check it** with the new binary: `aperio-server check server-init.yaml`.
   It runs the checks a change gets and names the first setting it refuses.

3. **Start 0.13 with `APERIO_INIT_FILE=server-init.yaml`, and without the old
   variables.** An upgraded data directory has tokens and users but no
   settings yet, so the document is read on this first start and stored; the
   tokens, users, statistics and the rest of the store are untouched. Put only
   `settings:` in this document: another section (`tokens:`, say) would
   replace that store.

4. **Retire the file.** From the second start the database holds the settings
   and the document is not read again. Keep it in git as the record of what
   the server started with, and use `GET /aperio/api/export?include=settings`
   for what it runs now.

`aperio-server --check-config`, `--print-config`, `--print-schema`,
`--verify-audit` and `--decrypt-backup` are subcommands now: `aperio-server
check <file>`, `settings show`, `schema`, `audit verify` and
`backup decrypt`. The flags are refused rather than ignored.

## Upgrading to 0.12.0: what an OIDC login is, and a wider default

Two configuration changes. Neither needs a coordinated cutover.

**A global OIDC login is no longer the super-admin.** Every address on the
allowed-emails list used to sign in as the built-in `aperio` account. Now an
OIDC login is matched to a dashboard user record by email and holds that
record's grants; an email with no record is granted `oidc.default_grants`,
which is empty by default, and is refused a session until something reaches
it, with a message to ask an administrator. Before upgrading a server that
uses OIDC, decide what a signed-in email should be: create a record per person
ahead of their login (`aperio-client api user create --username
alice@example.com --grant <org>:<role>`), map the provider's groups with
`oidc.group_grants`, or set `oidc.default_grants: [master:admin]`, which
restores exactly what every allowed email got before. Per-organization OIDC is
unchanged by default. See [Organizations](organizations.md#oidc-identity-is-the-providers-authorization-is-aperios).

**A named Admin of master used to be the super-admin under another name.**
Such a user, and an admin key scoped to master, is read as `*: admin` at the
first start so nothing that worked stops working, and each one is written to
the audit log as `grants_widened_on_upgrade`. The Users page shows a notice
while any user still carries `*`; narrow them by hand.

**`max_tunnels` defaults to 64, up from 10.** A client holds one connection
per service and a reload briefly wants two, so ten refused ordinary fleets as
full. Set it to 10 (`limits.tunnels` since 0.13.0) if ten was meant as a
fence.

## Upgrading to 0.10.0: routes are closed by default

A proxied route is now refused unless something declares it reachable. Before
0.10.0 a server with no `auth:` anywhere served every route to everyone, and
`public: true` was an exemption from a gate that might not exist.

**Who has to do something.** A deployment publishing a public site, which is
the commonest one there is. Every client serving such a route must now say so:

```yaml
# aperio.yaml (inside services:)
services:
  - name: site
    target: http://127.0.0.1:3000
    hostname: site.example.com
    public: true
```

`auth: { method: none }` is the equivalent per-service policy. For a CLI-only client, use `--public` (or `APERIO_PUBLIC=1`). See [Configuration](configuration.md#multiple-services).

Until it does, those routes answer the way an unclaimed hostname does, so the
symptom is a `504` rather than an error naming the posture. The server warns
about exactly this, once per connection, naming the line to write.

**Who does not.** A route already behind an `auth:` policy, the server's own
password, or OIDC. Those were always reachable after a login and are untouched:
the posture decides what an *unstated* route means, nothing else.

**To postpone it**, set `access.default: allow`. That restores the previous
behaviour exactly. It is a supported posture rather than
a migration flag, so there is no deadline attached to it.

## Recommended upgrade procedure

1. **Read the [CHANGELOG](../CHANGELOG.md).** Breaking changes are called out
   under the release's `Changed` section.
2. **Check the settings against the new binary.** Export them
   (`GET /aperio/api/export?include=settings`) and run
   `aperio-server check` on the export with the new binary: it names anything
   the new version would refuse.
3. **Back up the store.** Take a snapshot (the `backup` settings, or a logical
   `/aperio/api/export`) so a rollback has a known-good state. The logical dump
   carries the configuration by default; add `?include=` to name the sections,
   `settings,tokens,webhooks,users,organizations,scaling,exposes,statistics,uptime,activity,inbox,admin_keys`
   is everything the store holds. The SQLite schema
   is created idempotently; new columns are additive with serde defaults, so an
   older store loads cleanly into a newer server.
4. **Roll the server forward.** With `APERIO_REUSEPORT=1` you can start the new
   process alongside the old one and drain the old one for a
   [zero-downtime restart](development.md#zero-downtime-restarts); otherwise a
   normal restart broadcasts a graceful shutdown so clients reconnect promptly.
5. **Roll the clients forward.** On a patch release this can happen at any
   pace. On a minor release the old clients are refused until they are
   upgraded, so do it right after the server; each one reconnects by itself.
6. **Verify.** `aperio-server audit verify` confirms the audit chain survived,
   and the dashboard shows every client reconnected and healthy.

## Downgrade

Downgrading the server is generally safe because store changes are additive: a
newer store opened by an older server ignores columns it does not know. Across
0.13.0 the settings are the exception: a 0.12 server does not read them from
the database, so give it back its `aperio-server.yaml` and environment. Across
a minor release the clients have to be downgraded with it, since a client pairs
only with a server of its own minor. Always keep the pre-upgrade backup until the
new version has run cleanly for a while.
