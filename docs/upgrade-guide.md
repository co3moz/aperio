# Upgrade Guide & Compatibility

How to upgrade Aperio safely, and what to expect from version skew between the
server and its clients.

## Client ↔ server compatibility

Client and server negotiate a tunnel `PROTOCOL_VERSION` on every connection and
log a warning when they differ. The protocol is designed to tolerate skew:

- **New optional Ping fields degrade gracefully.** A field a peer does not know
  is simply absent (serde defaults fill it in), so an **older client keeps
  working against a newer server** and vice-versa — it just does not benefit
  from the newer feature. Every per-service flag (cache, resilience, response
  timeout, device key, …) was added this way.
- **A protocol-version bump signals a breaking frame change.** When the major
  tunnel behavior changes (e.g. the v1→v2 streamed-body frames), both sides log
  the mismatch. Traffic still flows for the shared subset, but you should update
  the older side to avoid subtle incompatibilities.

Rule of thumb: **upgrade the server first, then the clients.** The server stays
backward-compatible with older clients, so a fleet can be rolled forward
gradually with no coordinated cutover.

| Situation | Behavior |
| --- | --- |
| Newer server, older client | Works; client misses newer per-service features. |
| Older server, newer client | Works; newer client's new flags are ignored by the server. |
| Protocol-version mismatch | Logged on both sides + shown on the dashboard; shared subset works. |

The versioned config JSON Schemas (`aperio-client.<tag>.json`,
`aperio-server.<tag>.json`) are attached to each GitHub Release, so an editor
validates the exact keys a given version accepts.

## Recommended upgrade procedure

1. **Read the [CHANGELOG](../CHANGELOG.md).** Breaking changes are called out
   under the release's `Changed` section.
2. **Validate the config against the new binary.** `aperio-server --check-config`
   flags anything the new version would reject or silently default — run it
   before restarting.
3. **Back up the store.** Take a snapshot (`APERIO_BACKUP_*` or a logical
   `/aperio/api/export`) so a rollback has a known-good state. The SQLite schema
   is created idempotently; new columns are additive with serde defaults, so an
   older store loads cleanly into a newer server.
4. **Roll the server forward.** With `APERIO_REUSEPORT=1` you can start the new
   process alongside the old one and drain the old one for a
   [zero-downtime restart](development.md#zero-downtime-restarts); otherwise a
   normal restart broadcasts a graceful shutdown so clients reconnect promptly.
5. **Verify.** `aperio-server --verify-audit` confirms the audit chain survived,
   and the dashboard shows every client reconnected and healthy.
6. **Roll the clients forward** at your own pace.

## Downgrade

Downgrading the server is generally safe because store changes are additive: a
newer store opened by an older server ignores columns it does not know. The
exception is a protocol-version bump — pair the server downgrade with clients
that speak the matching version. Always keep the pre-upgrade backup until the
new version has run cleanly for a while.
