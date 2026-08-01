# Planned Features

Future feature ideas. Backlog items carry stable `#N` ids (never renumbered or
reused); a shipped item keeps its id and flips to `[x]` in place with a short
"shipped: ..." note. An idea that is dropped moves to **Withdrawn** at the
bottom with the reason, so the decision is on record and its id stays retired.
An entry that turns out to be bigger than one piece of work is split, the
original id keeping the part it best describes and the rest taking the next
free number.

## Future ideas

- [ ] **#1 Warn when a capacity setting does not fit the machine, rather than
  deriving it.** Originally "auto-tune resource limits from the environment":
  derive `max_concurrent_requests`, `max_ws_connections`, the cache budget and
  friends from cgroup CPU/memory limits and the file-descriptor ceiling.
  Rescoped after the 0.7 configuration work, which spent its effort on making
  "which value is in effect, and where does it come from" answerable (the file
  layer winning over stored overrides, the yaml key each setting is written as,
  the explain endpoint). A number that silently changes because the host
  changed is the opposite of that, and it changes under an operator who moved
  the same file to a bigger box.

  So: keep the detection, drop the derivation. At startup (and in
  `--check-config`) compare the effective values against what the environment
  can actually support, and say so once: `max_concurrent_requests` far above
  `RLIMIT_NOFILE`, a cache budget above the cgroup memory limit,
  `max_ws_connections` that cannot fit alongside it. One line naming the
  setting, the value, and the limit it exceeds. No behaviour changes, nothing
  to be surprised by, and the operator keeps the decision. If a later release
  still wants real derivation, this is the layer it would build on.

- [ ] **#2 Speed up the Windows release build without vendoring OpenSSL from
  source.** *Parked, not refused.* The `x86_64-pc-windows-msvc` release job
  spends several minutes compiling OpenSSL from source via
  `aperio-server/vendored-openssl` (needed because webauthn-rs pulls in
  openssl). Dropping vendored on Windows and linking the runner's system
  OpenSSL would cut that, but naively it breaks the self-contained `.exe`:
  dynamic linking makes the binary depend on `libssl`/`libcrypto` DLLs at
  runtime, and MSVC static linking hits the classic CRT (MT vs MD) mismatch.
  Hunting a reliably-static, ABI-compatible prebuilt OpenSSL is a known dead
  end; the version worth doing is the other one in the original note, **a
  webauthn crypto path that needs no openssl at all**, which removes the
  dependency instead of packaging it. That is a dependency swap with its own
  risk, and the cost it saves is CI minutes on a job the default-branch release
  cache (ci.yml `warm-release-cache`) already warms. Left open so the option is
  recorded; not worth starting while the cache holds.

- [x] **#3 Re-validate the dashboard SSE live stream while it is open.** shipped:
  `live_stream_handler` keeps the caller's headers and re-runs `dashboard_role`
  on every stats tick, the same check the session middleware makes when the
  stream is opened, and ends the stream the moment it comes back empty. A sign
  out, a "sign out everywhere", an expiry or a disabled user therefore closes
  the stream within one ~2 s tick instead of leaving it emitting traffic and
  statistics for as long as the tab stays open. The org stays fixed for the
  life of the connection, as before. A test seeds a session, reads the first
  snapshot, removes the session and asserts the stream ends; it fails without
  the check. (From the 2026-07 static security review.)

- [x] **#4 Stream static-serve responses instead of reading whole files into
  memory.** shipped in two parts. The body: `serve.rs` answers from an open
  `tokio::fs::File` through a `BoxBody` stream instead of `tokio::fs::read`
  into a `Full<Bytes>`, so peak memory no longer scales with file size times
  concurrent requests, and a `HEAD` reports the length from metadata without
  reading anything (`7f92cbb`, which also added single-range `Range` support
  built on the same stream). The syscalls: `resolve` was doing a blocking
  `canonicalize` and up to two `stat` calls on the Tokio worker thread polling
  the request, so a slow filesystem stalled every other task on that worker,
  not just this response; it is async now, through `tokio::fs`. The SPA
  fallback had both problems at once (a blocking `is_file()` and reading the
  whole index per navigation) and takes the same path as any other file. A
  per-serve maximum file size was considered and deliberately not added:
  streaming removed the reason for it, and a cap would only surprise an
  operator who meant to publish a large file. (From the 2026-07 static
  security review + a 2026-07 client review.)

- [x] **#7 Run the backend health probe once per service, not per parallel
  connection.** shipped: `BackendHealth::for_spec` builds one shared verdict per
  service; `spawn_services` creates it once and passes it to every connection's
  `run_service`, with only the first connection (`run_probe`) driving the
  probe/`wait_for_backend` gate (`notify_waiters` wakes all connections on a
  flip). Original note below.

  Each parallel connection of a service (`connections: N`) runs its
  own `run_service`, which builds its own `backend_healthy`/`backend_probed`
  flags and spawns its own `probe_task` hitting the backend's `target_health`
  endpoint independently (`aperio-client/src/service.rs`). So `connections: N`
  makes N independent probes and reports `backend_healthy` per connection, N×
  the health-check load on the backend, and connections can disagree during a
  blip. Now that `connections` defaults to 2 this doubles the probe load by
  default. Move the probe out of `run_service` into `spawn_services`
  (`aperio-client/src/main.rs`): one probe per service writing a shared
  `Arc<AtomicBool>` that every connection's `run_service` reads for its Ping.
  Touches `run_service`'s signature (13 call sites, mostly tests). Low-moderate
  severity. (From a 2026-07 client review.)

- [x] **#10 Turn Topology into the full routing map (config + live), not a
  second Clients table.** shipped: a dedicated `GET /api/topology`
  (`aperio-server/src/api/topology.rs`) returns `{ clients, routes, exposes,
  offline }`; `TopologySection.tsx` self-fetches it and renders, A: client-less
  static `routes:` and public `expose:` ports (master-only); B: dashed
  "declared but offline" nodes from token-granted binds no client serves
  (per-org); C: passive outlier ejection (`ejected` now on every client detail)
  coloured/labelled in the map. Deferred the route-limits overlay and
  per-connection bytes/geo edge weights (server tracks bytes only in aggregate).
  Original note below.

  Today `TopologySection.tsx` derives its graph purely
  from `stats.active_clients`, the same snapshot the Clients table renders, so
  it only shows *connected* clients and adds nothing but live req/s edge labels.
  It should become the one view that shows *how a request is routed*, including
  routing that has no live tunnel client. Clean split: **Clients = who is
  connected now (table); Topology = the routing map (config + live)**. Needs a
  dedicated `GET /api/topology` handler returning a typed
  `TopologyGraph { nodes, edges }` (org-scoped like the others), unioning the
  in-memory client registry with the config-side route registries. Three parts:
  - **A, client-less route nodes.** Fold in the server-side route definitions
    that exist whether or not a client is connected: static routes
    (`static_routes.rs` `RouteRule`: redirect/respond), expose rules
    (`expose.rs` `ExposeRule`: public TCP port → tunnel key), and route rate
    limits (`route_limits.rs`) as an overlay. These have no `ClientDetail` today
    so Topology can't see them; they render as route nodes that terminate in a
    redirect/respond/expose sink instead of a backend.
  - **B, "declared but offline" gap.** From each token's granted binds
    (`store/tokens.rs`: `hostnames`/`paths` a token *may* claim), emit a dim /
    dashed route node when no active client currently claims that bind, the one
    thing a table structurally cannot show (there is no row for an absent
    client). This surfaces "the service that should be up but isn't" at a
    glance. Decision needed: derive expected binds from granted token scopes
    (broad) vs. an explicit "expected services" declaration (precise).
  - **C, routing-health overlay.** Surface per-client routing state the graph
    is the natural home for and that no screen shows today: `ejected_until`
    (passive outlier ejection, a client silently pulled from rotation right
    now), `draining`, and load-balance fan-out (N clients on one hostname shown
    as a one-to-many group, not N unrelated rows). Colour nodes/edges by state.
  Non-goal for now: per-connection **bytes**/**geo** edge weights, the server
  tracks bytes only in aggregate (`PersistentStats`), not per connection, so
  that needs new server-side counters and is out of scope. Ship A/B/C behind the
  new endpoint; keep Clients as-is. (From a 2026-07 dashboard review.)

- [x] **#9 Give the WS and TCP relay arms the bounded hand-off the upload path
  already has.** shipped: `deliver_to_relay` in `aperio-client/src/service.rs`,
  the generic form of `feed_request_chunk`, is what the `WsData` and `TcpData`
  arms use now. The map is released before the send, the frame goes over
  without waiting when there is room, and a full channel is waited on for a
  bounded two seconds (`STREAM_STALL_BUDGET`) before that one stream is
  dropped with a log line naming it. So a lossless stream whose consumer is
  merely slower than a burst survives, where `try_send` alone killed it, and a
  consumer that has genuinely stopped still cannot stall the shared read loop
  or the heartbeat on it. `UdpDatagram` keeps dropping on a full channel, now
  with a comment saying that is its contract rather than an oversight.

  The per-stream credit/window protocol the original entry proposed was
  deliberately not built: the server already pauses producers in the other
  direction (protocol v3), and a symmetric scheme means a protocol bump and
  skew handling for a case a bounded wait covers. Revisit only if the wait
  proves too blunt in practice. (From the 2026-07 unpushed-commits review.)

- [x] **#5 Client-side IP-family control + Happy Eyeballs when dialing the
  server.** shipped: the client now owns the dial (`aperio-client/src/dial.rs`):
  it resolves every address, applies an `ip_family` (auto/ipv4/ipv6; CLI
  `--ip-family`, env `APERIO_IP_FAMILY`, yaml `ip_family`) preference, and tries
  each in turn (IPv4-first interleaved) with a per-address connect timeout. Wired
  into all three dial sites (service/check/tcp). Delivered the config knob + the
  address-fallback tier; kept it as sequential-with-timeout rather than full
  RFC 8305 concurrent racing. Original design below.

  tokio-tungstenite 0.23 dials with a single
  `TcpStream::connect("domain:port")` (`connect.rs:73`), so address selection and
  IPv4/IPv6 fallback are left entirely to the OS resolver. On the musl/Alpine
  client image this is unreliable: when a Cloudflare-fronted server hostname
  publishes AAAA but the host has no working internet IPv6, the client tries the
  IPv6 address and fails (`ENETUNREACH`), and, unlike a glibc `curl` on the same
  host, does not fall back to the reachable IPv4. musl does not honor
  `AI_ADDRCONFIG` the way glibc does, so even disabling IPv6 in the container
  (`net.ipv6.conf.all.disable_ipv6=1`) does not help: getaddrinfo still returns
  the AAAA and the client still tries it first (fails with `EADDRNOTAVAIL`). This
  caused a real outage (2026-07); the only reliable workarounds are DNS-side
  (drop AAAA / pin an IPv4 via `extra_hosts`), which is a footgun.
  Proposed fix (client-only):
  - **Tier 1, config escape hatch:** an `ip_family: auto | ipv4 | ipv6` field
    (CLI `--ip-family`, env `APERIO_IP_FAMILY`). `ipv4` connects only to A
    records, deterministically dodging unreachable AAAA. ~small change.
  - **Tier 2, robust default (`auto`):** replace the single `TcpStream::connect`
    with a shared connect helper that `lookup_host`s all addresses, applies the
    `ip_family` filter, and does Happy Eyeballs (RFC 8305: race IPv4/IPv6 with a
    small head-start, first to connect wins), with a per-address connect timeout.
    Feed the connected socket to `client_async_tls_with_config` so TLS
    (rustls/webpki-roots) is unchanged.
  Apply the shared helper at all three dial sites so they behave consistently:
  `service.rs:411` (main tunnel), `check.rs:190` (preflight check), `tcp.rs:304`
  (TCP tunnel). Tests: unit for the family filter/ordering; an e2e phase dialing a
  dual-stack loopback with `ip_family: ipv4`. Ship both tiers (auto default + the
  knob). (From a 2026-07 field debugging session.)

- [x] **#6 Probe the OTLP endpoint at startup when OTel export is enabled.**
  shipped: `telemetry::init` now spawns a detached thread that TCP-connects to
  the resolved endpoint host:port (`endpoint_host_port` parses host/port incl.
  IPv6 literals + scheme-default ports) and logs INFO on success / WARN on
  failure ("… unreachable, trace spans will be dropped"). Blocking IO on a
  thread so it needs no Tokio runtime and never blocks startup. Original note
  below.

  With `APERIO_OTEL` on, the batch span exporter silently POSTs to
  `otel_endpoint`; any failure (wrong host/port, DNS, collector down, wrong
  protocol/path) is invisible, spans just never arrive, and the only visible log
  is the harmless `BatchSpanProcessor.ExportingDueToTimer` heartbeat. In a 2026-07
  session this made a misconfig indistinguishable from "no traffic to trace":
  Jaeger stayed empty with no error. After building the provider in
  `telemetry::build_provider` / `init` (`aperio-server/src/telemetry.rs`), do a
  lightweight reachability probe to the resolved endpoint host:port (a short-
  timeout TCP connect, or an HTTP request to the `/v1/traces` path) and log a
  clear line: INFO on success ("OTLP endpoint <ep> reachable"), WARN on failure
  ("OTLP endpoint <ep> unreachable: <err>, spans will be dropped"). Must NOT fail
  startup, tracing is non-critical, so a bad collector must never take the server
  down; run the probe non-blocking (spawn it, or a single short-timeout connect
  before serving). Consider also surfacing the batch exporter's own runtime export
  errors (currently swallowed) and/or a periodic re-check. Note the probe only
  confirms the collector is listening, not that spans parse end-to-end, but it
  catches the common wrong-endpoint / not-running / DNS cases immediately.
  (From a 2026-07 field debugging session.)

- [ ] **#11 Restart the background tickers when one panics; escalate the rest.**
  Under the default `unwind` strategy a panic only unwinds its own task, so the
  process survives, but a bare `tokio::spawn`ed background loop that panics
  just *stops*, silently, and its function is lost for the life of the process.
  The global panic hook (`main.rs`) makes such a panic visible in the log; it
  does not bring the loop back.

  Split by whether a restart is safe, which the original note asked for and is
  the whole difficulty:

  - **Restart:** the idempotent tickers, stats and uptime flush, the
    token-expiry sweep, retention pruning. These are the ones where "silently
    stopped" is invisible for days: nothing errors, the numbers simply stop
    moving. A small `spawn_supervised(name, factory)` that awaits the
    `JoinHandle`, logs a panic with the loop's name, and respawns with a short
    backoff covers all of them.
  - **Escalate, do not respawn:** accept loops and expose listeners. A
    listener that panicked is in an unknown state, and a respawned one that
    silently accepts nothing is worse than an outage that is visible. These
    should log and drive a graceful shutdown instead.

  Request-scoped work and one-shot tasks stay exactly as they are.
  (From a 2026-07 panic-resilience review, scoped 2026-07-31.)

- [x] **#12 Capacity-aware autoscaling: the server signals desired capacity,
  the client declares the actuator.** shipped: `scaling:` in `aperio.yaml`
  announced via Ping and persisted per bind (`store/scaling.rs`), the
  single-flight state machine with cooldown/backoff/breaker and the
  SSRF-fenced actuator (`scaling.rs`), cold start on the empty-pool path,
  the scale-out sampler over the per-client concurrency limiters, client
  `idle_timeout` with graceful drain, `GET/DELETE /api/scaling`,
  `aperio-client api scaling`, and the dashboard's Autoscaling section
  (`ScalingSection.tsx`: live instances/utilization per record, breaker
  state, disarm). One refinement was deliberately left out: a per-token
  `allow_scaling` permission (today any token may arm a record for its own
  bind, which the org fence and bind validation already constrain, and
  `APERIO_SCALING` is the operator switch).

  Original plan below. Two halves of one feature, planned
  together because they share the same machinery: **0 to 1 (cold start)**, a
  request arrives for a bind no client serves and the server calls a
  client-declared URL to wake the service instead of answering 504; and **N to
  N+1 (scale out)**, the connected pool is saturated and the server asks for
  one more instance. Scale *in* stays client-driven (an idle client shuts
  itself down), so the server never kills anything. Aperio is the sensor and
  the policy, never the orchestrator: it emits a desired-capacity signal to a
  URL the operator controls and the provider decides what to do with it.
  - **Declaration.** One `scaling:` block in `aperio.yaml`, announced via Ping
    and persisted server-side (it must outlive the client process):
    `url`, `secret` (write-only), `min` (0 enables cold start), `max`,
    `cold_start` budget, `target_utilization`, `window`, `cooldown`. Honored
    only when the token carries a new `allow_scaling` permission (same trust
    model as `public` / `visitor_auth`), with an `APERIO_IGNORE_CLIENT_SCALING`
    server-side escape hatch. Also settable from the admin API so a client
    never has to know about it.
  - **Record identity.** Keyed by `(org_id, hostname_bind, path_bind)`: that is
    all a request carries, so the lookup is O(1) on the miss path. Ownership is
    a *set* of token ids (8 identical replicas may hold 8 different tokens);
    the record disarms when the last owner token is revoked or expires.
    Duplicate registration is deduped by a content hash of the config, so N
    identical replicas are an idempotent no-op refresh (no audit spam, no
    flapping). A differing config is last-writer-wins plus an audit entry, and
    the dashboard flags a conflict when two live clients disagree.
  - **One state machine per bind** (in memory, not persisted): Idle to Waking
    to Idle, with single flight (a burst of requests produces exactly one
    actuator call), cooldown with exponential backoff, and a circuit breaker
    that disarms after K consecutive failures. The same machine serves both
    triggers, only the reason differs.
  - **Where it hooks in.** Cold start belongs on the empty-pool path
    (`PickOutcome::NoRoute` in `proxy.rs`), *not* to `FailoverMode`: failover
    only governs in-flight failures and never covered the empty pool, so wake
    changes no existing guarantee. The record carries its own `on_empty: hold |
    fail` policy for that path. Because the request was never dispatched,
    holding it is safe for every method, so the `failover_all_methods` /
    idempotency rules must NOT be reused here.
  - **Scale-out signal.** Every client already carries an `inflight_limiter`
    semaphore sized by its announced `max_concurrent`, so pool utilization is
    `1 - (sum available_permits / sum max_concurrent)` over routable clients,
    and requests over capacity already wait on that semaphore. Trigger on
    sustained utilization above `target_utilization` for `window` *plus* real
    semaphore wait time (p95), never on raw request counts, which are far too
    noisy. Exclude standby-tier clients (priority > 0) under
    `primary-standby`, and note that sticky routing cannot be relieved by
    adding instances.
  - **Known traps to design for.** (a) `try_acquire_request_slot` runs *before*
    client selection, so a hold on the empty-pool path would pin a global
    concurrency slot for the whole cold-start budget and starve healthy
    services; the wait must release the permit or the route must resolve
    earlier. (b) With an empty pool there are no candidates to evaluate
    `allowed_ips` against, so a visitor who would have been denied can trigger
    a paid cold start and learn the route exists; check the token's IP scope
    before firing. (c) Bots, crawlers and uptime checks will keep a
    scale-to-zero service awake forever unless the trigger is filtered by
    method/path. (d) Wait for a *routable* candidate, not merely a connected
    one (`backend_healthy` / `wait_for_backend` gate). (e) An `idle_timeout`
    shorter than the cold start produces a death spiral; the client must not
    start its idle timer before serving a request, and "woken but died without
    serving" must count against the breaker. (f) Never fire while the bind is
    in maintenance mode or its client was disabled from the dashboard: both are
    explicit operator intent. (g) An owner token that expired while the service
    slept would burn the budget on every request; check validity before firing.
    (h) A server restart drops the in-memory state, so bound the blast radius
    with a global concurrent-actuator semaphore. (i) Several aperio-servers in
    HA each hold their own view, so the actuator must be idempotent; document
    it. (j) With `resilience: true`, serve the stale cached answer immediately
    and fire the actuator in the background rather than holding the visitor.
    (k) The outbound call is SSRF from a lower-trust credential: https only, no
    private or loopback targets by default, no redirects, short timeout,
    response body ignored, optional host allowlist, secret never logged, every
    firing audited.
  - **Delivery order.** 1) Capacity telemetry only (utilization, semaphore wait,
    saturation in `/api/stats`, Prometheus and the dashboard) so the signal can
    be validated with zero risk. 2) The actuator plus the desired-capacity
    state machine with `min: 0`, i.e. cold start. 3) Scale-out on the same
    machine. 4) Policy refinements: hysteresis, cost guards (max scale events
    per hour), per-org caps. Client-side `idle_timeout` with a graceful drain
    ships with step 2, otherwise every cold-start cycle ends in a 502.

- [x] **#13 Optional per-organization hostname allowlist.** shipped: `hostnames`
  on the org record, enforced in `ClientPerms` (org fence + token fence) and at
  token create/edit, ephemeral tunnel provisioning, and the dashboard bind
  override; random subdomains exempt; `PUT /api/orgs/{id}/hostnames`,
  `aperio-client api org create|hostnames`, dashboard Organizations page. An organization can
  be given a list of hostname patterns (e.g. `acme.com`, `*.acme.example.com`)
  that fences every bind created inside it, so a tenant cannot claim a hostname
  it does not own. Today the only fence is the token's own `hostnames` list,
  and `hostname_allowed` treats an empty list or `*` as "any hostname on this
  server" (`state.rs`), so an org admin minting a wildcard token for their own
  org can bind another tenant's hostname. Enforce the org fence in three
  places: token create/update rejects permissions outside it, the Ping bind
  validation re-checks it (tokens can predate the allowlist, so this is the
  defence in depth that actually holds), and random-subdomain assignment stays
  within it. Inside a fenced org, `*` then means "any hostname within the org's
  patterns" instead of "anything". The master organization has no allowlist
  (None = unrestricted, current behavior), and the field is optional so
  existing deployments are unchanged. Surfaces as `hostnames` on the org record
  (dashboard org form, `PUT /api/orgs/{id}`, `aperio-client api org`), next to
  the existing quotas. Related: [[#12]] records are org-keyed, so the same
  fence bounds which hostnames a scaling record may be armed for.

- [x] **#14 Publish live hostnames to a dynamic edge proxy (Traefik, Caddy,
  nginx).** shipped: `GET /aperio/api/edge/ask` (Caddy on-demand TLS) and
  `GET /aperio/api/edge/traefik` (Traefik HTTP provider), both gated by
  `APERIO_EDGE_TOKEN`, plus the wildcard-label setup documented in
  `docs/edge-proxy.md`. Left out: writing the same document to a file for
  Traefik's file provider (`APERIO_TRAEFIK_FILE`), which would remove the
  network coupling on a single host. Original plan below. With Aperio behind a dynamic reverse proxy, every new tunnel
  hostname needs a router and a certificate at the edge, which today means
  hand-written config or a wildcard. The server already knows the full live
  inventory (`/api/topology`: connected clients, their binds, token-granted
  but offline binds, static routes, exposes), so it can serve that inventory in
  the format each proxy consumes. Two endpoints, sharing one hostname
  inventory: (a) a **Traefik HTTP provider** document
  (`GET /aperio/api/edge/traefik`) returning `http.routers` / `http.services`
  with a `Host(...)` rule per live hostname pointing back at this server, plus
  the cert resolver, so Traefik picks up a new tunnel within its poll interval;
  (b) a proxy-agnostic **hostname check** (`GET /aperio/api/edge/ask?domain=`)
  answering 200 or 404 by whether the hostname is currently served, which is
  exactly Caddy's on-demand TLS `ask` contract and doubles as a generic probe
  for scripts and nginx templating. Both must be authenticated (admin key or a
  dedicated read-only edge token), org-scoped, cacheable, and must never expose
  the expose shared key or any secret. Decide before implementing: whether the
  inventory should include declared-but-offline binds (needed for a cert to
  exist before the first client connects, but it lets a tenant provoke an ACME
  request for any hostname their token permits, so it should probably be
  opt-in), and which proxy is the primary target for the first cut.

- [x] **#15 Restrict where webhook and autoscaling callbacks may be sent.**
  shipped: optional `outbound:` block / `APERIO_OUTBOUND_ALLOWLIST` +
  `APERIO_OUTBOUND_BLOCK_PRIVATE`, enforced at webhook creation and at every
  delivery, and layered on top of the scaling hook's own policy; defaults
  keep the permissive behaviour. Whether the delivery log should hide the
  raw status code from tenants was left unchanged (open question).
  `POST /api/webhooks` and the `scaling.url` field accept any URL after a
  schema check, and delivery attempts record the response status in the
  delivery log. An Operator in a child organization can therefore use the
  server as a blind SSRF probe: point a webhook at an internal address, fire an
  event, and read back from the delivery log whether the port answered, which
  maps the server's private network one port at a time. The reason this is not
  simply fixed by blocking private addresses is that internal receivers are the
  normal case: most deployments point webhooks at a service on the same
  network, so blocking them by default would break working installations. Needs
  a policy an operator chooses: an outbound allowlist (host/CIDR patterns the
  server may call), and/or a `block_private_targets` switch, defaulting to
  today's permissive behaviour with a clear note in the docs, plus consideration
  of whether the delivery log should show a tenant the raw status code at all.
  Decide the shape before implementing. (From the 2026-07 four-agent review.)

- [x] **#16 Stream static files instead of reading each one fully into memory.**
  shipped, and the same work as **#4**: two reviews found the one problem and
  filed it twice. #16 delivered the streaming body and range support; #4
  carried the rest of the request path (the blocking `canonicalize`/`stat`
  calls, the SPA fallback) and finished it. Kept as its own id because ids are
  never reused; read #4 for what shipped.

- [x] **#18 Fire a `CONFIG_CHANGES` entry only when the file actually uses the
  field.** shipped: entries carry `applies: WhenSet | Always`, `check_upgrade`
  takes the file's `ConfigKeys`, and a test refuses a `Security` entry that is
  not `WhenSet`. `dashboard_auth` is now `Security` as it should always have
  been. The `REMOVED_SETTINGS` check stays for the two cases the version
  mechanism cannot see: a file with no `version:`, and an env-only deployment.
  Original note below.

  Original: Entries currently apply on the version range alone, so a config that
  never set the changed key still gets the report on upgrade. That is tolerable
  for a warning and wrong for a refusal: a `Security` entry would stop a server
  whose file is entirely unaffected, which is the outage-generator failure mode
  CLAUDE.md rule 18 warns about. The removal of `dashboard_auth` worked around
  it with a dedicated presence check in `main.rs` (`REMOVED_SETTINGS`) plus a
  `Breaking` entry, which is precise but does not generalize. Fix: pass the
  parsed document into `check_upgrade` and filter entries to those whose
  `fields` appear in it, keeping the range check as the outer gate. Then
  `Security` becomes safe to use, the per-change presence checks can go, and
  the report stops mentioning keys the operator does not have.
  (From the 2026-07 dashboard_auth removal.)

- [ ] **#17 An opt-in minimum-throughput guard for streamed responses.** Part
  (1) of the original entry shipped (`stream.pause_bytes` /
  `stream.resume_bytes` / `stream.backlog_limit`, with `StreamLimits::sanitized`
  repairing an inconsistent trio), and part (3) is now #20. What is left is the
  slow-read defense: a deliberately slow reader can hold a streamed response,
  and the client-side `max_concurrent` slot it occupies, alive indefinitely at
  roughly 2 MiB of server-side buffer each. The old backlog cut used to kill it
  by accident; flow control made the server well-behaved and therefore patient.

  Drop a consumer that averages below N bytes/s over an M-second window, off by
  default, both numbers settings. It must **not** apply to WS or TCP relays,
  which are legitimately quiet for long stretches, that exclusion is the part
  worth getting right rather than the accounting. Best done alongside #9, which
  touches the same delivery paths. (From the 2026-07 flow-control discussion.)

- [ ] **#20 A per-IP ceiling on concurrently open streamed responses.** Split
  out of #17, where it was part (3). Saturating a service's concurrency budget
  currently takes one host holding many slow streams; a per-IP cap makes it
  take a botnet. The pattern exists already: `try_acquire_ws_slot`
  (`aperio-server/src/state.rs`) holds a slot for the life of a proxied
  WebSocket under `max_ws_connections`, and the per-IP rate limiter's map shows
  how the keying and its eviction are done here. Small next to #17 and worth
  doing in the same pass. (From the 2026-07 flow-control discussion.)

- [x] **#19 Pub/sub between the clients of an organization, over the tunnel
  that already exists.** shipped: four `TunnelMessage`
  variants, per-organization routing keyed on `instance_group`,
  `POST /aperio/api/publish`, `subscribe:`/`messages_listen:`/
  `messages_mqtt_listen:` on the client, a token `topics` capability, and the
  server's events mirrored onto `$aperio/`, and QoS 1 with a bounded send
  window plus client-side duplicate suppression, and a `run:` sink with the
  constraints the entry below names. Offline delivery stays out of scope, as
  it argues. Original note follows.

  Original: Clients can be reached from outside and can reach a
  private service through a peer, but they have no way to *signal each other*.
  The workaround today is an MQTT broker exposed as a `tunnels:` entry and
  bound by every consumer: three moving parts, and a message crosses the tunnel
  once to the broker and once more per subscriber, so a wide fan-out pays for
  it. See [`docs/examples/mqtt`](docs/examples/mqtt/) for that shape, which
  stays valid for anyone who wants their own broker's semantics.

  **On the wire it is not MQTT.** Three `TunnelMessage` variants, `Subscribe`,
  `Unsubscribe`, `Publish`, on the WebSocket connection the client already
  holds, plus a per-organization topic → subscriber map on the server. No
  second listener, no second connection, no second authentication path; the
  connection arrives already identified, org-scoped and heartbeated. Embedding
  a broker (rumqttd) was considered and rejected for that reason: with both
  ends ours, MQTT on the wire is a dependency and a second connection lifecycle
  for something no user would ever see.

  **Subscriptions key on `instance_group`, not on the connection.** A client
  with a `services:` list holds one connection per service, so a
  connection-keyed subscription delivers N copies to one process. Keying on the
  process identity the server already tracks means the duplicate never exists,
  which is strictly better than a client-side seen-id cache with a time window
  nobody can size correctly. A small seen-set stays justified for QoS 1 only,
  where a lost ack makes a redelivery legitimate.

  **What the server keeps is a send window, not a store**: per subscriber
  process, un-acked messages bounded by count and by age (seconds, at most a
  minute), oldest dropped on overflow with a metric, gone on restart. Offline
  delivery ("give me what I missed while I was down") is explicitly out of
  scope for v1. It is a different feature with retention, disk and backpressure
  semantics, and for the case that motivates this one (a client reacting to an
  event) replaying an hour-old message is a bug, not a service.

  **The application boundary is where a well-known protocol earns its keep.**
  Push is easy: a `POST /aperio/api/publish` endpoint, reachable through the
  existing `aperio-client api` wrapper, no tunnel needed. Subscribe is the hard
  half: something has to hand the message to the user's process. Two faces over
  one subscription machine, in this order:

  1. **A local HTTP port on the client**: SSE for subscribe, POST for publish.
     No codec, no dependency, works from `curl -N` and from every language's
     standard library, and it proves the whole path (server routing,
     per-process delivery, fan-out, send window) before any protocol work.
     **Agreed as the first face to build.**
  2. **A local MQTT listener on the client**, so an app connects with the MQTT
     library it already has and subscribes as usual, while the client
     translates that into a subscription over the tunnel. A packet codec
     (`mqttbytes`), never an embedded broker: a broker means local fan-out plus
     an upward bridge, and a bridge means loop prevention. The compatibility
     answer is written up front rather than discovered: granted QoS 0 or 1, no
     retained, clean session always, no will.

  **Reserve `$aperio/` for the server's own events**, the way MQTT reserves
  `$SYS`, and publish the existing event bus on it (`client.connected`,
  `request.failed`, `tunnel.bound`). The events already exist and already feed
  webhooks; putting them on topics lets a client react to infrastructure events
  without running a webhook receiver, and turns this from a new subsystem into
  the thing that makes an existing one reachable.

  **Authorization is a token capability with a topic prefix**, alongside
  `allow_bind`, never crossing an organization. Any sink that runs something
  locally on receipt is a remote-execution primitive by design and needs the
  payload off the command line (stdin or an env var), a concurrency cap, a
  timeout, and an audit line naming the publisher.

  Freeze before writing code: `instance_group` keying, the send window's
  numbers, the `$aperio/` split, the capability shape, and the sentence saying
  v1 has no offline delivery. (From the 2026-07 client-to-client messaging
  discussion.)

- [x] **#21 Split `aperio-server` into a library and a thin binary, and break
  the `ws.rs` read loop into per-message handlers.** The server is a binary
  crate, so its top 2,100 lines (`main.rs`: env resolution, router assembly,
  background task spawning, shutdown) and the 1,450-line `handle_socket` loop
  in `tunnel/ws.rs` are reachable only by running the whole process, which is
  why they sit at the bottom of every coverage report (486 and 324 missed
  regions as of 0.8.0) and why any test of startup wiring has to be an e2e
  phase. Plan, in order, each step shippable alone: (1) `src/lib.rs` takes
  every existing `mod`, `main.rs` shrinks to a call into the lib, CLI
  subcommands included; (2) `async_main` decomposes into
  `Settings::from_env()`, `build_state()`, `build_router()`,
  `spawn_background()` and `serve()`, so router-level tests can drive the
  full middleware stack in-process with `tower::ServiceExt::oneshot`; (3) a
  `tests/` integration crate boots the composed app without a subprocess;
  (4) `handle_socket`'s match arms become named handlers over a small
  `ConnCtx`, the loop keeps only decode-and-dispatch, and the writer task's
  compression transform becomes a free function, each testable with the
  channel-mock pattern the pubsub and expose tests already use. No behavior
  change anywhere; e2e green after every step. The client binary got the same
  lib/bin split in the same pass. shipped: 045f8a8 (lib/bin), 89ffe09
  (async_main stages + router tests), d59837b (integration crate), 9d94505
  (ConnCtx handlers + writer_transform/SendPacer tests), plus the client
  split. Decomposing the client's supervisor loop the same way remains open
  as its own idea if it ever earns it. (From the 2026-08 coverage push
  toward 95%.)

- [x] **#22 Protocol v7: TCP/UDP/WS relay payloads as binary frames instead of
  base64+JSON.** shipped: The HTTP body path went binary in v5/v6, but the passthrough
  relays did not: `TcpData`, `UdpDatagram` and binary `WsData` still carry
  their payload base64-encoded inside a JSON envelope, a third more bytes on
  the wire plus an encode/parse/decode on every 16 KB chunk, both directions.
  For anyone moving bulk over `expose:` or an emergency TCP tunnel this is the
  throughput ceiling and a large slice of the CPU. Plan: three new frame tags
  (`FRAME_TCP_DATA`, `FRAME_UDP_DATAGRAM`, `FRAME_WS_DATA` for binary WS only,
  text WS is already un-encoded), `PROTOCOL_VERSION` to 7, negotiated so an
  older peer on either side keeps getting base64+JSON. The care this needs and
  the reason it is its own item: the payload is sent from four sites with
  three different ways of knowing the peer's version, the server relays
  (`tunnel/tcp.rs`, `expose.rs`) and the client relays (the binder `tcp.rs`,
  the serving `service.rs`), and it is received in three read loops; every one
  needs old<->new interop reasoning, which is why it is not folded into the
  2026-08 perf pass that did the other five bottlenecks. Do it as one relay
  type fully (TCP, both directions, with an e2e assertion that a v6 peer still
  works), then UDP and WS follow the proven shape. shipped as planned: one `relay_frame` helper per
  side owns the negotiation (so none of the four senders can get it wrong
  independently), the receivers gained tag arms next to their JSON ones, and
  the ownership fence is asserted on the binary path too. (From the 2026-08
  bottleneck analysis; the base64 relays were finding #5 there.)

## Withdrawn

Ideas taken off the backlog. Their ids stay retired: nothing is renumbered and
nothing reuses them.

- **#8 Pool Unix-domain-socket backend connections.** Withdrawn 2026-07-31. It
  would break a documented behaviour to buy an unmeasured saving:
  `docs/configuration.md` promises that a `unix://` target "dials the socket
  fresh" per request, which is what makes socket-activated backends work, and a
  pool is exactly the opposite. The entry itself had already conceded that the
  FD-exhaustion framing behind it was overstated, since Unix sockets have no
  `TIME_WAIT`, which left efficiency as the only argument and no number behind
  it. Revisit only with a profile from a real deployment.

