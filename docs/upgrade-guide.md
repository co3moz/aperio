# Upgrade Guide & Compatibility

How to upgrade Aperio safely, and what to expect from version skew between the
server and its clients.

## Declaring the version your config was written for

Both configuration files take a `version:` key naming the Aperio release they were written against:

```yaml
# aperio.yaml (or aperio-server.yaml)
version: 0.5.0
```

On startup the binary compares that against its own build and looks up every recorded change to the *configuration format* that landed in between. The behaviour is deliberately quiet:

- **Nothing changed in that range**, the upgrade cannot affect this file, and nothing is printed. Silence is the signal.
- **Something changed**, a warning names each change, the keys it touched, and what to do about it, then asks you to set `version:` to the new release once you have looked.
- **A change with security consequences**, the binary **refuses to start**. A default that opens something previously closed, or an enforcement that silently stopped applying, is exactly the case where continuing quietly is worse than an outage you can see.
- **No `version:` at all**, the check is off, and one informational line says so. Existing deployments keep working unchanged; adding the key is what buys the warning.

This is the safety net for `docker pull` on a Friday: an upgrade either behaves exactly as your file says, or tells you precisely which keys to look at, or stops. A rollback is covered too, a file declaring a version *newer* than the binary is called out, since it may use settings that binary has never heard of.

The history deliberately starts at the release that introduced the mechanism: entries for older versions could only be guesses. From that point on, every change able to alter how an existing file behaves is recorded as part of the change itself.

## Client ↔ server compatibility

Client and server negotiate a tunnel `PROTOCOL_VERSION` on every connection and
log a warning when they differ. The protocol is designed to tolerate skew:

- **New optional Ping fields degrade gracefully.** A field a peer does not know
  is simply absent (serde defaults fill it in), so an **older client keeps
  working against a newer server** and vice-versa, it just does not benefit
  from the newer feature. Every per-service flag (cache, resilience, response
  timeout, device key, …) was added this way.
- **Except where being ignored would be worse than being refused**, which is
  the one case that is negotiated instead. A client's `auth:` gate is the
  example: an older server that ignored a method it does not understand would
  read the client as declaring *no* gate, and the route would come up open. So
  the server announces on the handshake which methods it accepts from a
  client, and a client whose gate needs one that is missing **does not serve
  that service**, logging which side is too old. Only that service stops; the
  client's others keep running. What is assumed of a server that announces
  nothing is exactly what the old scalar field can carry: `method: none`, or a
  `basic` naming one `user:password`. A gate of any other shape, including a
  `basic` naming two users, is held back there too, since the field that would
  carry it is the one such a server ignores.
- **A protocol-version bump signals a breaking frame change.** When the major
  tunnel behavior changes (the v1→v2 streamed-body frames, the v2→v3 per-stream
  flow control), both sides log the mismatch. Traffic still flows for the shared
  subset, but you should update the older side to avoid subtle
  incompatibilities. The fallbacks are per feature: a pre-v2 peer gets buffered
  bodies and base64 frames, a pre-v5 server gets a buffered response body
  base64-encoded inside the JSON instead of as bytes in one frame, a pre-v6
  client gets its request bodies the same way, and a pre-v3 client is
  never asked to pause, so the
  server lets its streams buffer up to `APERIO_STREAM_BACKLOG_LIMIT` (16 MB)
  before dropping them. Running an old client therefore costs worse behavior
  under load, not a broken connection.

Rule of thumb: **upgrade the server first, then the clients.** The server stays
backward-compatible with older clients, so a fleet can be rolled forward
gradually with no coordinated cutover. It is also the order that avoids the one
exception above: a client upgraded first, whose file uses a newer gate, will
hold that service back until its server catches up.

| Situation | Behavior |
| --- | --- |
| Newer server, older client | Works; client misses newer per-service features. A pre-v3 client cannot be flow-controlled, so a download to a visitor slower than the backend is cut at `APERIO_STREAM_BACKLOG_LIMIT` instead of the producer being paused. |
| Older server, newer client | Works; newer client's new flags are ignored by the server. The exception is a client-declared `auth:` gate beyond `basic`: rather than be ignored, it is negotiated, and the client holds that one service back until the server understands it. |
| Protocol-version mismatch | Logged on both sides + shown on the dashboard; shared subset works. |

The versioned config JSON Schemas (`aperio-client.<tag>.json`,
`aperio-server.<tag>.json`) are attached to each GitHub Release, so an editor
validates the exact keys a given version accepts.

### What is enforced at connect time

The window above is also checked when a client connects, not only in CI. The
client announces its release on the upgrade request and the server answers
with its own release and the oldest client it accepts. A pairing outside the
window is refused **at the upgrade**, with a sentence naming both versions and
which side to upgrade, rather than being allowed to establish and then
misbehave somewhere the cause is no longer visible.

Two things it deliberately does not do. A client that announces nothing is
admitted: a release old enough to predate the header is inside the promise
anyway, and reading silence as age would take a fleet down on the very upgrade
that introduced the check. And a client *newer* than the server is not
refused, because that is not an unsupported pairing, it is the ordinary shape
of a fleet mid-upgrade seen from the other end.

Today the floors are set exactly where this document's promise is, every
released client from v0.1.0 onward, so nothing that works is refused. The
mechanism exists so that when a release does break something, narrowing the
window is one line in the same change, and the operator gets an answer instead
of a mystery.

### What is actually checked

The paragraphs above are a promise, and a promise nothing checks is a wish. CI
runs a slice of the end-to-end suite against the **previous release's real
binaries, in both directions**: that release's client against this server, and
this client against that server. A change that breaks either pairing fails the
build rather than reaching a fleet.

The slice is proxying a request end to end plus the admin API, because those
are what the promise is *about*: whatever else changed between two versions, a
request still reaches the backend and an operator can still see it. It is
deliberately not the whole suite, which asserts features that did not exist in
every past release, and would report the absence of a feature as an
incompatibility.

After each release is published, a second workflow pairs the **newly released
server with the client binary of every previous release** and prints the result
as a table, so the wider claim is measured too rather than inferred from the
narrow one. It is deliberately not a gate: an old client failing against a new
server is information, not a regression. The pairing it runs is old client
against new server, because that is the one a deployment actually produces,
the server being upgraded first and then running ahead of its fleet for a
while.

At the time of writing, every released client from **v0.1.0 onward** passes
that slice against the v0.9.0 server.

The supported window is therefore **one release of skew, gated**, and
everything older, measured after each release and reported rather than
enforced. Anyone can measure a specific pairing themselves, since the suite
takes the binaries as inputs:

```bash
APERIO_CLIENT_BIN=/path/to/old/aperio-client npm --prefix tests/e2e run test:compat
APERIO_SERVER_BIN=/path/to/old/aperio-server npm --prefix tests/e2e run test:compat
```

## Upgrading to 0.10.0: routes are closed by default

A proxied route is now refused unless something declares it reachable. Before
0.10.0 a server with no `auth:` anywhere served every route to everyone, and
`public: true` was an exemption from a gate that might not exist.

**Who has to do something.** A deployment publishing a public site, which is
the commonest one there is. Every client serving such a route must now say so:

```yaml
public: true          # or, the same statement in the policy grammar:
auth: { method: none }
```

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
   flags anything the new version would reject or silently default, run it
   before restarting.
3. **Back up the store.** Take a snapshot (`APERIO_BACKUP_*` (yaml `backup_*`) or a logical
   `/aperio/api/export`) so a rollback has a known-good state. The logical dump
   carries the configuration by default; add `?include=` to name the sections,
   `tokens,webhooks,users,organizations,scaling,settings_overrides,statistics,uptime,inbox,admin_keys`
   is everything the store holds. The SQLite schema
   is created idempotently; new columns are additive with serde defaults, so an
   older store loads cleanly into a newer server.
4. **Roll the server forward.** With `APERIO_REUSEPORT=1` (yaml `reuseport`) you can start the new
   process alongside the old one and drain the old one for a
   [zero-downtime restart](development.md#zero-downtime-restarts); otherwise a
   normal restart broadcasts a graceful shutdown so clients reconnect promptly.
5. **Verify.** `aperio-server --verify-audit` confirms the audit chain survived,
   and the dashboard shows every client reconnected and healthy.
6. **Roll the clients forward** at your own pace.

## Downgrade

Downgrading the server is generally safe because store changes are additive: a
newer store opened by an older server ignores columns it does not know. The
exception is a protocol-version bump, pair the server downgrade with clients
that speak the matching version. Always keep the pre-upgrade backup until the
new version has run cleanly for a while.
