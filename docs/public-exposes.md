# Managed public TCP and UDP exposes

A public expose binds a socket on the Aperio **server** and forwards accepted
TCP streams or UDP peer sessions to a declared client tunnel. It needs no
second binder client and can be managed without restarting the server.
`--bind-tunnels` remains the way to bind a socket on another client's machine.

Public visitors do not authenticate with an Aperio token. Keep the target's
own authentication, restrict `allowed_ips` where appropriate, and configure
firewall, NAT and container port publishing separately. A successful listener
bind proves that the server owns its socket; it does not prove Internet
reachability. IPv6 binds are IPv6-only. TCP and UDP have separate port spaces
and may use the same port number. An encrypted tunnel cannot be exposed.

## Authority and ownership

Expose actions are explicit organization grants: `read`, `create`, `update`,
`enable`, `disable`, `delete`, `disconnect` and `delegate`. Every action implies
read access. Existing tenant roles do not acquire these rights automatically.
Master administrators retain control. A user's account, session or key is an
actor, not the resource owner: revoking its control rights does not delete the
organization's rules.

Server administrators allocate exact listener addresses, protocol/port ranges,
reserved sockets and aggregate rule/session/bandwidth ceilings. An allocation
permits creating rules; it opens no ports itself. Child organizations require
an allocation. Broad wildcard binding is a different allocation from binding
a particular interface. Invalidated policies suspend affected rules. Only
server administrators change these top-level allocations.

The dashboard's Tunnels page combines declarations and server exposures.
Actions are derived from the current caller's capabilities and checked again
on each API request. Grant editing is available in Users and Admin keys.
A user who can delegate may give only the expose actions they hold.

## Resource shape

An API-owned resource has a UUID, monotonically increasing revision, `source:
"api"`, and a `spec`. The organization is its stable id (`master` for the
built-in organization), not its mutable display name. A TCP specification:

```json
{
  "org_id": "master",
  "tunnel": "database",
  "listener": {"address": "0.0.0.0", "port": 25432, "protocol": "tcp"},
  "enabled": false,
  "allowed_ips": ["192.0.2.0/24"],
  "limits": {
    "protocol": "tcp",
    "max_connections": 256,
    "open_timeout_secs": 10,
    "drain_timeout_secs": 30,
    "ingress_bytes_per_second": 16777216,
    "egress_bytes_per_second": 16777216
  }
}
```

For UDP, use `protocol: "udp"` in both listener and limits. Its limit fields
are `max_sessions`, `max_sessions_per_ip`, `idle_timeout_secs`,
`max_datagram_bytes`, `queue_packets`, `queue_bytes`,
`new_sessions_per_second`, `ingress_bytes_per_second`, and
`egress_bytes_per_second`. The defaults are 1024, 32, 60, 65507, 64, 262144,
64, 16777216 and 16777216 respectively. Packet and byte queue limits apply
per direction per peer. Queue admission is conservatively sized using the
maximum datagram size. UDP boundaries, including empty datagrams, are retained;
oversize datagrams are dropped, never truncated and forwarded. UDP source
addresses can be spoofed: per-IP quotas do not authenticate senders. Restrict
sources and explicitly budget egress when exposing an amplification-prone
service.

Desired `enabled` is separate from observed `state` and `target_state`.
A listener can be listening while its declared target is offline. The API
reports bind failures and policy suspensions separately from target readiness.
Existing streams stay pinned when a target is edited; new streams resolve
against the new rule. Editing source restrictions or limits disconnects old
sessions so their old budgets do not outlive the new restrictions.

`disable` and `delete` immediately close the listener and its sessions. `drain`
stops new admission and allows existing sessions until a 1–3600 second deadline.
UDP draining keeps the shared socket only for established peers until that
deadline. `disconnect` ends one selected session. Unexpected listener task
exits get at most three automatic recovery attempts; an exhausted listener
requires an explicit retry. Intentional stops are not recovered.

File-owned `expose:` entries remain controlled by the server YAML. They appear
read-only in the UI, do not travel in API backups and cannot be overwritten by
API imports. Legacy token/key matching stays internal; management responses
never contain those secrets. Configuration reload stages the complete expose
change and retains working rules on validation or bind failure.

## CLI and API

Use a scoped admin API key (`--api-key`, `APERIO_API_KEY`, or
`server.api_key`) for automation. A client tunnel token is not an expose
management credential. Examples assume normal client server/key configuration:

```sh
aperio-client api expose list --protocol udp
aperio-client api expose create --file spec.json --id 03ce7b2e-9b07-4d55-9787-5d0867e627d7
aperio-client api expose show RESOURCE_ID
aperio-client api expose update RESOURCE_ID --revision 1 --file spec.json
aperio-client api expose enable RESOURCE_ID --revision 2
aperio-client api expose sessions RESOURCE_ID --history
aperio-client api expose drain RESOURCE_ID --revision 3 --seconds 30
aperio-client api expose disconnect RESOURCE_ID --revision 4 --session SESSION_ID
aperio-client api expose disable RESOURCE_ID --revision 4
aperio-client api expose delete RESOURCE_ID --revision 5
aperio-client api expose policies
aperio-client api expose set-policy --file policy.json
```

Revisions above are illustrative: use the latest response for every mutation.
The optional create UUID is an idempotency key: an identical retry returns the
existing resource; reusing it for a different spec is a conflict. List commands
accept `--offset` and `--limit` (maximum 500). Grant syntax also accepts
`--grant 'ORG:viewer+expose.create+expose.update+expose.enable'`.
Admin-key creation accepts repeated/comma-separated `--expose` actions.

API endpoints are under `/aperio/api/exposes`: collection GET/POST, `/{id}`
GET/PUT/DELETE, `/{id}/actions` POST, `/{id}/sessions` GET, `/policies` GET and
`/policies/{org}` PUT. Errors carry `code` and `message`, or `errors` with
field names for structural validation. Mutations use revisions and return a
conflict rather than silently overwriting another editor. The UI preserves
a conflicting edit as a draft for explicit review.

## Backup, preview and recovery

The full server dump includes an `exposes` configuration section with API rules
and port policies. Existing user and admin-key sections retain capability
grants. File rules, active sockets and session history are not imported.
Without the `organizations` section, only master-owned configurations travel.
Full server restore preflights exposed sockets before changing any section;
its existing section-by-section transaction model still applies, and a later
failure reports both the failed section and those already imported.

For a scoped transfer, use the expose-specific backup:

```sh
aperio-client api expose export > exposes.json
aperio-client api expose import --file exposes.json --org-map SOURCE_ID=DESTINATION_ID --omit-policies
# Review the preview response, then use its destination revision:
aperio-client api expose import --file exposes.json --org-map SOURCE_ID=DESTINATION_ID --omit-policies --apply --revision 7
```

The destination organizations must already exist. Preview is the default and
bind-probes changed sockets without starting relays or writing configuration.
Preview does not reserve ports: apply repeats the checks under the mutation
lock. Identical existing identities are skipped; differing specs are reported
as conflicts and never silently overwritten. Policy imports require a server
administrator. Delegated users use `--omit-policies` and the destination's
existing allocation. Import requires create permission in every destination
organization and cannot bypass quotas or file ownership.

`GET /exposes/export` produces `{version, resources, policies}`. Import accepts
`{backup, org_map, preview, revision}` at `POST /exposes/import`. A successful
preview returns the destination revision required by apply. A concurrent
configuration change requires a fresh preview.

Dynamic rules are persisted before a successful mutation response. Corrupt
stored configuration is preserved and blocks further mutation until recovery;
it is not replaced by an empty document. A volatile database is explicitly
reported in the API and UI. Bind failure on startup is reported per resource,
allowing unrelated saved listeners to start. Invalid file exposes do not hide
API-owned saved resources.

Recent completed session metadata is retained for seven days, at most 256
records per organization and 2048 globally. It includes peer, target, byte and
packet totals, timestamps and a termination reason, not traffic payloads or
credentials. Current-session queries remain permission checked. Session
history persistence failures are surfaced independently of listener operation.

An optional `advertised_host` in the rule's spec (or YAML entry) supplies the
visitor-facing DNS name or IP without a scheme or port. It changes the copied
endpoint only, not the socket bind or external DNS/NAT. The dashboard does not
copy wildcard bind addresses as visitor destinations. IPv6 visitor literals
are bracketed in endpoint URLs.

Prometheus exports `aperio_expose_sessions_opened_total`,
`aperio_expose_bytes_total`, `aperio_expose_frames_total`,
`aperio_expose_drops_total`, `aperio_expose_sessions`, and
`aperio_expose_listeners`. Counters are process-lifetime totals including
retired generations. Labels contain only protocol, direction, observed state
and fixed rejection reasons, never visitor addresses or resource UUIDs.


## TCP write EOF and older clients

Protocol v10 adds the server-to-client `TcpEof {stream_id}` message. It closes
only the backend socket's write direction, after queued visitor bytes have
been delivered; backend responses continue until backend EOF, administrative
close or the configured public listener drain deadline. The server sends it
only when the client's existing protocol announcement is at least 10.
`TcpClose` keeps its immediate-close meaning. With older clients the server
uses the existing full-close behavior, so applications that send a request
and then require a response after write EOF should upgrade their client.
UDP uses the existing UdpOpen/UdpDatagram/UdpClose wire messages without a
protocol change. Binary and legacy JSON payload encodings remain supported.


## YAML and deployment examples

A DNS tunnel can serve both transports on port 1053. Declare the target on the
client (use the actual DNS server address):

```yaml
tunnels:
  - name: dns
    protocol: tcp/udp
    target: 127.0.0.1:53
```

For operator-owned server listeners:

```yaml
expose:
  - tunnel: master@dns
    protocol: tcp
    address: 0.0.0.0
    port: 1053
    advertised_host: dns.example.com
    allowed_ips: [192.0.2.0/24]
  - tunnel: master@dns
    protocol: udp
    address: 0.0.0.0
    port: 1053
    advertised_host: dns.example.com
    allowed_ips: [192.0.2.0/24]
    limits:
      protocol: udp
      max_sessions: 128
      max_sessions_per_ip: 8
      idle_timeout_secs: 30
      max_datagram_bytes: 4096
      queue_packets: 16
      queue_bytes: 65536
      new_sessions_per_second: 16
      ingress_bytes_per_second: 1048576
      egress_bytes_per_second: 1048576
```

Use a real authorized source range instead of the documentation range above.
For dynamic listeners, create the equivalent two rules in Tunnels; omit them
from YAML so the dashboard owns their configuration. Publish **both** container
transports, for example with Docker Compose:

```yaml
services:
  aperio:
    ports:
      - "1053:1053/tcp"
      - "1053:1053/udp"
```

Add these mappings to the existing Aperio service. Kubernetes Services likewise
need two named entries with `port: 1053`, `targetPort: 1053`, and `protocol: TCP`
or `protocol: UDP`. A load balancer must support the selected transport.
Provision the matching host/cloud firewall and NAT rules. Dynamically allocating
a port in Aperio never changes these infrastructure settings; provision the
intended allocation range in advance when using a container network.

From an authorized external machine, `dig @dns.example.com -p 1053 example.com`
checks UDP and `dig +tcp @dns.example.com -p 1053 example.com` checks TCP. These
checks prove connectivity from that machine only. Use a controlled DNS service;
do not publish an unrestricted recursive resolver.

## Diagnostics and schemas

* `waiting` target: confirm the named tunnel is declared in the owning org.
* `incompatible` target: check protocol and encryption; encrypted declarations
  cannot terminate at a public plaintext socket.
* `failed` listener: inspect the error, release the occupied port or correct the
  interface/OS permission, then Retry. Listening with no serving target is not
  the same as bind failure.
* `suspended`: restore the organization's allocation/quotas, then enable or retry
  as appropriate. Tightening a policy can stop previously permitted listeners.
* Increasing `ingress_queue`, `client_queue` or rate drops: examine the configured
  budget and backend throughput before increasing it. UDP has no delivery retry.
* A persisted configuration error requires recovery of the database from backup;
  it is never silently cleared by creating a new resource.

The configuration endpoint also serves `expose` and `expose-policy` JSON Schemas
at `/aperio/api/config/schema/{kind}`. Generate those documents with
`cargo run -p aperio-config -- --expose` or `--expose-policy`. OpenAPI includes
structured create/update/action/import payloads and protocol-specific limits.
Expose mutations record safe before/after metadata; rejected authenticated
mutations record method, status and readable resource identity, without request
bodies. Each resource's `/{id}/events` endpoint shows its scoped audit history.
