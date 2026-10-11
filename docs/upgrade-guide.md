# Upgrade Guide & Compatibility

How to upgrade Aperio safely, and what to expect from version skew between the
server and its clients.

## Declaring the version your config was written for

Both configuration files take a `version:` key naming the Aperio release they were written against:

```yaml
# aperio.yaml (or aperio-server.yaml)
version: 0.12.0
```

On startup the binary compares that against its own build and looks up every recorded change to the *configuration format* that landed in between. The behaviour is deliberately quiet:

- **Nothing changed in that range**, the upgrade cannot affect this file, and nothing is printed. Silence is the signal.
- **Something changed**, a warning names each change, the keys it touched, and what to do about it, then asks you to set `version:` to the new release once you have looked.
- **A change with security consequences**, the binary **refuses to start**. A default that opens something previously closed, or an enforcement that silently stopped applying, is exactly the case where continuing quietly is worse than an outage you can see.
- **No `version:` at all**, the check is off, and one informational line says so. Existing deployments keep working unchanged; adding the key is what buys the warning.

This check flags configuration changes during upgrades. It also warns when a file declares a version newer than the binary, as can happen after a rollback.

The history deliberately starts at the release that introduced the mechanism: entries for older versions could only be guesses. From that point on, every change able to alter how an existing file behaves is recorded as part of the change itself.

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

## Upgrading to 0.12.0: what an OIDC login is, and a wider default

Two recorded configuration changes, both reported by the `version:` check
above. Neither needs a coordinated cutover.

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
full. Write `max_tunnels: 10` if ten was meant as a fence.

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

**To postpone it**, set `default_access: allow` (`APERIO_DEFAULT_ACCESS`). That
restores the previous behaviour exactly. It is a supported posture rather than
a migration flag, so there is no deadline attached to it.

## Recommended upgrade procedure

1. **Read the [CHANGELOG](../CHANGELOG.md).** Breaking changes are called out
   under the release's `Changed` section.
2. **Validate the config against the new binary.** `aperio-server --check-config`
   flags anything the new version would reject or silently default. Point it at
   the file the service reads (`APERIO_SERVER_CONFIG=/etc/aperio/aperio-server.yaml
   aperio-server --check-config`), and export the token it requires first, or it
   fails on the missing credential rather than on the config.
3. **Back up the store.** Take a snapshot (`APERIO_BACKUP_*` (yaml `backup_*`) or a logical
   `/aperio/api/export`) so a rollback has a known-good state. The logical dump
   carries the configuration by default; add `?include=` to name the sections,
   `tokens,webhooks,users,organizations,scaling,settings_overrides,statistics,uptime,activity,inbox,admin_keys`
   is everything the store holds. The SQLite schema
   is created idempotently; new columns are additive with serde defaults, so an
   older store loads cleanly into a newer server.
4. **Roll the server forward.** With `APERIO_REUSEPORT=1` (yaml `reuseport`) you can start the new
   process alongside the old one and drain the old one for a
   [zero-downtime restart](development.md#zero-downtime-restarts); otherwise a
   normal restart broadcasts a graceful shutdown so clients reconnect promptly.
5. **Roll the clients forward.** On a patch release this can happen at any
   pace. On a minor release the old clients are refused until they are
   upgraded, so do it right after the server; each one reconnects by itself.
6. **Verify.** `aperio-server --verify-audit` confirms the audit chain survived,
   and the dashboard shows every client reconnected and healthy.

## Downgrade

Downgrading the server is generally safe because store changes are additive: a
newer store opened by an older server ignores columns it does not know. Across
a minor release the clients have to be downgraded with it, since a client pairs
only with a server of its own minor. Always keep the pre-upgrade backup until the
new version has run cleanly for a while.
