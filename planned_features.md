# Planned Features

Feature ideas and what became of them. Items carry stable `#N` ids, never
renumbered and never reused, so a commit message or a comment naming `#28`
points at the same thing forever.

The file has three sections, and an entry moves between them exactly once:

- **Future ideas**, what is open. An entry that turns out to be bigger than
  one piece of work is split, the original id keeping the part it best
  describes and the rest taking the next free number.
- **Withdrawn**, dropped with the reason, so the decision is on record rather
  than re-argued from memory.
- **Completed**, shipped, ticked `[x]` and moved here with a `shipped:` note
  saying what was actually built and where that differed from the plan.

Keeping the sections apart is the point: what is left to do should be
readable without scrolling past what is already done.

## Future ideas

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

- [ ] **#40 Mutual TLS on the tunnel connection.** (triage 45) A client
  authenticates with a bearer token, optionally pinned and IP-fenced. Some
  deployments want a client certificate as well, so a leaked token alone is not
  enough. Rustls supports it on both sides; the work is less in the handshake
  than in the operational surface: where certificates come from, how they are
  rotated, and what the server does with the identity once verified (map it to
  a token, or to an organization).

- [ ] **#43 Shadow traffic to a second backend.** (triage 45) Send a copy of a
  route's requests to a staging service and discard its answer, so a new
  version sees production traffic before it serves any. `shadow: {target}`
  under a route. What makes it more than a fan-out: the shadow must never
  affect the visitor (no waiting on it, no error from it), must be bounded so
  it cannot double the client's concurrency budget, and needs an explicit
  answer for request bodies, which have to be buffered to be sent twice.

- [ ] **#45 A notification centre in the dashboard.** (triage 40) Token expiry,
  a client that dropped, maintenance left on, an alert that fired: all of these
  are already events on the `$aperio/` bus, and all of them are currently found
  by noticing something in a table. A bell with unread state, fed from the
  existing SSE stream, org-scoped like everything else.

- [ ] **#46 One WebSocket for several services of the same client process,
  as an opt-in mode.** (triage 40) Each service opens its own tunnel
  connection, so a client with five services pays five readers, five writers,
  five sockets and five heartbeats. At five services nobody notices. At forty
  it is forty of everything, and the ping/pong traffic alone is forty
  round-trips per interval for one process, which is the case this exists for.
  The 2026-08 connection sweep found that on a machine where client and server
  share CPU fewer connections is measurably faster, which is the same argument
  from the other end.

  **What the code says today.** A connection *is* the unit of identity. The
  Ping describes exactly one service: a single `path_bind`, one
  `hostname_binds` list, one `target`, one `max_concurrent`, one `connections`.
  On the server, `ClientHandle` is keyed by connection id and carries that
  service's binds, health, permissions, announced limits and per-client
  statistics. Routing, load balancing, failover tiers, the dashboard's client
  table, per-service bandwidth and the `--bind-tunnels` registry all key on
  that handle. So the architecture does **not** support this today, and saying
  so is the point of this entry.

  **What it would take.** A protocol version bump (v8), and in it: a service
  *list* in the handshake/Ping instead of the current singular fields; a
  service selector on every server-to-client frame; and `ClientHandle` split
  so its identity is `(connection, service)` rather than `connection`.
  Everything keyed on the handle follows from that split.

  **Its relationship with what has already shipped, which is the part not to
  forget.** Three completed items assume one connection = one service, and
  this contradicts all three:
  - **#48 (`connections: {min, max}`)** scales *one service's* connection pool
    with load. #46 goes the other way, many services per connection. They are
    not opposites, they compose: together they mean "sockets proportional to
    load, not to service count", which is the right target. But the elastic
    pool's growth signal is per-service in-flight, and under multiplexing
    in-flight would have to be attributed per service on a shared socket.
  - **#24 (client-side chunk coalescing)** and **#38 (server writer batching
    and pacing)** both assume one writer serves one service. Multiplexed, a
    service streaming a large response would queue its frames ahead of a small
    API's on the same writer, which is head-of-line blocking between unrelated
    services. Splitting flow control and the pacer per service on a shared
    socket is part of this work, not a follow-up to it.
  - **#37 (client health telemetry)** reports RTT, jitter and reconnects per
    connection. Multiplexed, those become properties of the process rather
    than of a service, which is arguably more useful but is a reporting change
    that has to be made deliberately.

  **Shape.** An opt-in mode (`multiplex: true` or similar), not a default. The
  per-connection model is simpler, is what every deployment runs today, and is
  faster for the small-service-count case that most deployments are. The mode
  is the open door for the operator with forty services, who is the only one
  who pays for the current design.
- [ ] **#54 Encrypted backups.** (triage 35) Scheduled snapshots of the SQLite
  store are written in the clear, and that store holds hashed credentials,
  sessions and organization data. AES-256-GCM with the key from an environment
  reference. Worth doing only with a real answer for key handling, since a key
  sitting next to the backup is decoration.

- [ ] **#67 Chunked transfer fidelity.** (triage 30) `Transfer-Encoding` is
  stripped and the body is re-framed by our own streaming, which is correct for
  every case we know of but means a backend's chunk boundaries are not the
  visitor's chunk boundaries. It matters for protocols where a chunk is a
  message. Worth a written answer even if the answer stays "we re-frame".

- [ ] **#71 TLS termination on an `expose:` port.** (triage 25) Exposed ports
  carry raw TCP or an end-to-end encrypted stream, so a service that wants TLS
  in front of it has to terminate it itself. Per-port `tls_cert` / `tls_key`.
  Modest value: the fronting proxy that most deployments already have does this.

- [ ] **#72 Configurable TLS floor and cipher list for the tunnel.** (triage 25)
  Rustls defaults are used as they come, which is the right default and an
  awkward answer to a compliance questionnaire that wants the floor pinned to
  1.3 in writing. Cheap, and mostly a documentation feature.

- [ ] **#73 `permessage-deflate` instead of application-level compression.**
  (triage 25) Tunnel compression is negotiated and applied by us, one message at
  a time. The RFC 7692 extension does it in the transport with context takeover
  across messages, which compresses better on streams of similar frames. Worth
  it only if a measurement shows the ratio difference matters, and note the v7
  finding that compression is a loss for payloads that are already compressed.

- [ ] **#74 Forwarding the client's own logs.** (triage 20) Streaming the
  client's stderr to the server, or to syslog, so a fleet's logs land in one
  place. Low because every container runtime and init system already does this,
  and because client logs can contain backend URLs and header values, which
  makes shipping them somewhere new a decision rather than a convenience.

- [ ] **#86 A `TokenSpec` for the token store's `create`/`update`.** (triage 25)
  `TokenStore::create` now takes fourteen positional arguments and `update`
  the same in `Option` form. The store's own comment records why they are
  appended rather than filed where they belong semantically: the compiler
  names every call site for an *added* argument and cannot see a *shifted*
  one, which is how `canary` once ended up in `allow_bind`. That trade has
  held, but appending is not free either, `allow_otel` moved forty-odd test
  call sites to add one flag, and the next capability will move more. A
  `TokenSpec { .. }` struct with `Default` makes both calls read as what they
  set and makes a new field cost nothing at the sites that do not care. Purely
  internal, no config or API surface moves, which is what keeps it low: it
  buys clarity and future edits, not behavior.

- [ ] **#87 Activity chart: 1m / 15m / 2h / 1d, with the bucket width scaled
  to each.** The chart offers 60 s, 5 min and 15 min. Drop the 5-minute view
  and add two long ones, so the ranges become 1 minute, 15 minutes, 2 hours
  and 1 day. The 5-minute view was cut from the same 15-minute ring in the
  browser (#60) and is close enough to its neighbour to be worth the slot.

  The point is that the **resolution scales with the range**, which is what
  keeps the chart readable and the payload small: roughly sixty cells whatever
  the span. One minute is already 1-second cells; fifteen minutes is 5-second
  cells; two hours wants 2-minute cells and a day wants 15-minute ones. A day
  at 5-second resolution would be seventeen thousand points to draw a line
  nobody can read.

  Unlike #60 this is **not** a browser-side cut. The server keeps one ring of
  15 minutes, so two hours and a day are data it does not have: the activity
  store needs coarser rings alongside the fine one, fed by the same recording
  path, or a rollup that folds finished fine buckets into coarse ones. That
  is the real work here, and it decides the shape of everything else,
  including whether the ranges survive a restart (the fine ring does not, and
  a day-long view that resets on deploy is a worse answer than no day-long
  view).

  Worth settling while designing it: retention and memory. A day of 15-minute
  cells is 96 numbers per organization and costs nothing; the temptation is to
  keep going to a week and a month, which is where it stops being a live
  activity chart and starts being the traffic history that already exists
  (`/api/stats/history`, persisted, with its own date-range picker). The line
  between the two belongs in the entry before the code.

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

- **#25 `aperio-lib`: the client as an embeddable library, with the host
  framework dispatching requests in-process.** Withdrawn 2026-08-02, from a
  design discussion, nothing was built. The idea: ship the client as a library
  with native bindings (Node first, then Java / .NET / Rust), so an
  express/fastify app hands requests straight to its own router (fastify's
  `inject()`) and the loopback HTTP hop between client and backend disappears.
  Four reasons it was dropped, in the order that decided it:

  1. **The performance case is small and setup-specific.** The loopback hop
     costs roughly 50-150 us per small request (two syscall pairs, an HTTP/1
     serialize and a parse) and about two memcpy passes over a large body.
     Against a real deployment's 5-50 ms tunnel round trip that is under one
     percent of latency. It only looks large in a loopback benchmark where the
     machine is CPU-bound, which is exactly the measurement that motivated the
     question. The same class of saving was already collected by #23 and #24
     without changing the architecture.
  2. **`inject()` gives up streaming and, worse, fidelity.** light-my-request
     buffers the whole response, so the 256 KB streaming path, SSE and long
     polling regress. And it bypasses the real socket, so connection-level
     middleware, timeouts and anything reading `req.socket` behave differently:
     tunnelled traffic would take a *different code path* than a real request.
     For a preview and debugging tool that fidelity is the product, not an
     implementation detail worth trading.
  3. **Most of the transport win is already available, language-agnostically.**
     A `unix://` target skips the TCP stack today with no new code (it still
     dials fresh per request, see #8). Whatever UDS does not recover is roughly
     the ceiling of what embedding could add.
  4. **The cost is a permanent one, not a one-off.** The only sane shape is one
     Rust core plus thin bindings (reimplementing the protocol per language is a
     combinatorial interop matrix against a protocol that reached v7 in a single
     cycle). Even that shape needs a `Backend` trait, a decomposed supervisor
     loop, programmatic config, its own runtime, panic isolation at the FFI
     boundary and a logging bridge; then roughly seven prebuilt artifacts per
     release per language, plus issue triage in each ecosystem. It also gives up
     the separate-process supervisor: today a client crash cannot take the app
     down, and either side restarts alone.

  **What would justify revisiting is ergonomics, not speed:** environments where
  a sidecar cannot run at all (some PaaS and serverless), or wanting
  "`npm install` plus three lines" as a product feature. Then the scope is Node
  only, HTTP only (no relays, no `serve:`, no pub/sub, no bind-tunnels), with
  the real socket as the default dispatch and `inject()` as an opt-in fast path
  the user knowingly accepts the fidelity trade for. Before any of it, measure
  the hop: `scripts/profile-request.sh` against the same backend over
  `http://localhost:...` and over `unix://...` prices it in an afternoon.

  Two prerequisites from that list are worth doing on their own merits whether
  or not this is ever revisited: extracting a `Backend` trait from the if-chain
  in `handle_incoming_request` (`aperio-client/src/proxy/http.rs`), which would
  put the reqwest, h2, unix and serve paths under one shape, and decomposing the
  client's supervisor loop (`service.rs`), which #21 already leaves open.

- **#75 An `Expect-CT` response header.** Withdrawn 2026-08-02. The header is
  obsolete: Certificate Transparency is enforced by the browsers themselves and
  Chrome removed `Expect-CT` support, so shipping it would add a setting that
  does nothing and implies a protection nobody is getting.

- **#76 Client binary self-update.** Withdrawn 2026-08-02. Having the client ask
  the server for a new binary, download it and restart into it puts a
  code-execution channel inside the tunnel: whoever controls the server, or
  anyone who can impersonate it for one response, controls every client host.
  Package managers, container images and the existing release artifacts already
  solve this with signatures and an audit trail we would have to reinvent
  badly. The version *mismatch* problem is real and is already handled by
  reporting it, which is the part worth keeping.

- **#77 A QUIC / HTTP-3 tunnel transport.** Withdrawn 2026-08-02. Large,
  invasive, and aimed at a problem no measurement has shown: the 2026-08 sweep
  found the ceiling in byte movement and per-connection CPU, not in the
  WebSocket transport. It also points the wrong way for the deployments that
  need a tunnel most, since the restrictive networks that make tunnelling
  necessary are the ones that block or throttle UDP. Revisit only with a
  measurement from a real deployment showing the transport itself is the limit.

- **#78 Response body rewriting (`sub_filter`, JSON path edits).** Withdrawn
  2026-08-02. Rewriting a body means buffering it, which undoes the streaming
  path deliberately built and then made zero-copy in #23 and #24, and it
  interacts badly with compression, `Content-Length` and range requests. Header
  transforms stay because a header is small and bounded; a body is neither.

- **#79 SLO tracking, anomaly detection and a geographic traffic map.**
  Withdrawn 2026-08-02. All three are the job of the observability stack the
  metrics endpoint already feeds, and each carries a cost we would own forever:
  error budgets need a definition of "good" per service that only the operator
  has, anomaly detection needs tuning nobody will do, and a map needs a GeoIP
  database with its own licensing and update schedule. Exporting good metrics is
  the contribution; drawing conclusions from them is somebody else's product.

- **#80 cgroup limits for `run:`, cloud upload for backups, per-organization
  data directories.** Withdrawn 2026-08-02. Each re-implements a layer that
  already exists underneath us: container runtimes and systemd enforce resource
  limits, a cron job with the vendor's own CLI uploads a file better than three
  embedded cloud SDKs would, and data residency is a deployment topology
  question, not a directory layout inside one SQLite store.

- **#81 Unmeasured micro-optimizations: parallel health probes, client-side
  request coalescing, backend TLS session resumption.** Withdrawn 2026-08-02.
  The probe runs once every ten seconds, so multiplexing it optimises nothing;
  the server already single-flights concurrent identical cacheable GETs, so
  doing it again at the client adds a lock to the hot path for a case that is
  handled; and reqwest already pools connections, which is what makes session
  resumption moot. Each was justified by a claim about the code that did not
  hold.

- **#82 Binding a dashboard session to the IP it was created from.** Withdrawn
  2026-08-02. It signs out anyone whose address changes, which on mobile
  networks, VPNs and CGNAT is normal traffic rather than an attack, and it does
  not stop the theft it targets, since a stolen cookie is usually replayed from
  a path the attacker can shape. Session lifetime, rotation and the existing
  lockout are the defences worth having.

- **#83 Directory listing for `serve:`, port ranges for `expose:`, reserved
  bandwidth per service.** Withdrawn 2026-08-02. Sugar with either a footgun or
  no demand behind it: an autoindex exposes files nobody meant to publish and is
  a common source of accidental disclosure, a port range is ten `expose:`
  entries written differently, and genuine bandwidth reservation needs
  admission control across all clients rather than the per-stream pacing we have.

- **#84 Ten proposals describing features that already exist.** Withdrawn
  2026-08-02, recorded so they are not proposed a third time. Each arrived with
  a claim that the code had no such thing; each was checked and the claim was
  wrong. Dashboard brute-force lockout, escalating per IP, is
  `aperio-server/src/auth.rs` with `login_lockout.threshold` / `.secs`.
  Per-organization bandwidth limits are the `max_bytes_month` quota in
  `store/orgs.rs`. Per-token statistics are `by_token` and `by_token_periods` in
  `store/stats.rs`, exposed through the metrics API. The `config_reloaded`
  audit entry already carries the diff (`lib.rs`, built from
  `settings::config_reload_diff`). Token rotation already has an overlap
  window, `rotate(id, grace_seconds)` in `store/tokens.rs`, exercised by the e2e
  suite. The client already pools backend connections, since reqwest does so by
  default and the client is built once per service. Programmatic API access
  already exists as admin API keys carrying a role and an organization
  (`auth::admin_key_identity`, `/aperio/api/admin-keys`). Statistics retention
  already exists as `APERIO_RETENTION_STATS`. The self-health dashboard panel
  and the config builder panel are both fully implemented, not placeholders.
  The lesson worth keeping: "grep found nothing" is evidence about a spelling,
  not about a feature.

- **#34 `restart_policy` for a process started with `run:`.** Withdrawn
  2026-08-02, on a false premise, found when the implementation started. There
  is no long-lived process to restart: `run:` is not "start my backend
  alongside the tunnel", it is a command inside a `subscribe:` entry that runs
  **once per received message**, with the payload on stdin, a timeout and a
  concurrency cap (`aperio-client/src/messages_run.rs`). A restart policy for
  a command that exits after every message is not a small feature, it is a
  category error. Retrying a *failed* message handler is a coherent idea, but
  that is message-delivery semantics, next to QoS 1 and redelivery, not
  process supervision, and would be its own entry.

  The related proposal to pass environment variables to `run:` is also partly
  answered already: `APERIO_MESSAGE_TOPIC` and `APERIO_MESSAGE_ID` are set for
  every run. What is genuinely missing there is *operator-defined* env, which
  stays open as a small idea.

  The lesson is the same one #84 records: the triage scored this from the
  proposal's description of `run:` without opening the file. Reading what a
  name refers to is part of scoring it.

## Completed

- [x] **#88 The i18n check catches prose that never reaches `t()`.** The
  checker verified that everything routed through `t()` was translated, and
  could not see the other question: whether everything *visible* is routed
  through `t()`. That is the gap that shipped "5m ago" to a Turkish dashboard,
  because the time helpers live in `lib/format.ts`, which is not a component
  and so never had a translator to call, and nothing failed since from the
  checker's side those were literals like any other. shipped: a third rule
  over the TypeScript AST, looking at JSX text, the JSX attributes a browser
  renders or reads aloud, and strings returned from a function. The predicate
  is deliberately narrow, whitespace plus a word of three letters plus four
  letters in total plus no identifier or path punctuation, because most
  English literals here are `aperio.yaml`, `5xx`, `ms` and `app.example.com`,
  which the project keeps in English on purpose; a rule that flagged those
  would fire constantly and be turned off. Three exemptions carry their
  reason. It found four real gaps on the first run: two sidebar strings (one
  read by a screen reader) and two name-validation messages. Its limit is
  written down rather than hidden: a single English word is not flagged,
  because a rule that did would flag every identifier in the source.

- [x] **#51 Weighted routing and header-based canaries.** (triage 35) The load
  balancer picks between clients by priority tier and round robin, so "send 20
  percent to the new version" or "send my requests to the new version if I set
  this header" cannot be expressed. Both are the same mechanism seen from two
  angles, which is why they are one entry. Depended on #26, which had landed.
  shipped as `canary:` on a route policy, membership by service name. The
  decision worth recording: the split is per **visitor**, by hashing the
  address, not per request. A per-request coin flip would send one page load's
  twenty assets to both versions, which is a mixture rather than a canary. The
  hash is FNV rather than `DefaultHasher`, which is seeded per process and
  would have two servers disagreeing about who is in the canary. It never
  empties the pool, a failover keeps the visitor on their side, and proxied
  WebSockets are left out because a socket is one connection rather than a
  stream of requests.

- [x] **#1 Warn when a capacity setting does not fit the machine, rather than
  deriving it.** Originally "auto-tune resource limits from the environment".
  Rescoped after the 0.7 configuration work, which spent its effort on making
  "which value is in effect, and where does it come from" answerable: a number
  that silently changes because the host changed is the opposite of that, and
  it changes under an operator who moved the same file to a bigger box.
  shipped: two checks at startup, connections against the file-descriptor
  ceiling (`/proc/self/limits`) and the cache budget against the cgroup memory
  limit, each naming both numbers and changing nothing. Only two, chosen by
  asking what an operator can act on: anything derived from `max_body_size`
  times a concurrency limit was left out on purpose, since that product is a
  worst case reached by roughly no deployment and a warning that fires
  constantly is one nobody reads. A machine with no limits to read gets
  silence rather than a guess.

- [x] **#69 Per-organization inspector retention.** (triage 25) Capture
  retention is a global entry cap and a global TTL, so a noisy organization can
  evict a quiet one's captures. A per-org ceiling is the multi-tenant hygiene
  that the byte quotas and client quotas already have. shipped, but as **fair
  share rather than a ceiling**, which is the design decision worth recording.
  A ceiling has to be chosen and interacts badly with the total: five orgs
  capped at twenty each is a hundred entries in a buffer of fifty, so the
  total cap evicts across tenants again and the ceiling bought nothing.
  Eviction instead drops the oldest capture of whichever org holds the most,
  which needs no number, lets an org alone on the server use the whole buffer,
  and converges on an even split by itself. Only the entry cap was addressed:
  the TTL half has no cross-tenant problem, since one org's volume cannot age
  out another's captures.

- [x] **#64 Differentiated rate budgets for the admin API.** (triage 30, cut
  from a proposed 85 whose premise was wrong.) The admin surface *is* rate
  limited: `check_rate_limit` runs on login, token creation, the tunnels API,
  the WebAuthn ceremonies, expose and the TCP endpoints, all against the same
  per-IP bucket. What is missing is a budget per endpoint class, so that a
  login attempt, a token creation and a full export are not charged the same.
  shipped: a `RateCost` on the same bucket, `Cheap` (1), `Guessable` (2) for
  anything that authenticates a credential, `Expensive` (5) for anything that
  provisions or reads the whole store. The first cut priced these at 5 and 10
  and the e2e suite refused it: one address making a couple of hundred calls,
  fourteen of them logins, went from comfortable to throttled. The bucket was
  sized when everything cost one, so a steep multiple does not make a class
  cost more, it tightens the limit on it against a ceiling nobody re-chose, and
  an office behind one NAT is the same shape as that test. Deliberately **one** bucket at
  different prices rather than a bucket per class: separate buckets would let
  an attacker spend a full allowance on each, and the capacity being protected
  is shared anyway. The prices are ratios, not measurements; sizing stays
  `ip_limit_max`. Export and import had no rate limit at all before this.

- [x] **#17 An opt-in minimum-throughput guard for streamed responses.** Part
  (1) of the original entry shipped (`stream.pause_bytes` /
  `stream.resume_bytes` / `stream.backlog_limit`), and part (3) is now #20.
  What was left is the slow-read defense: a deliberately slow reader can hold
  a streamed response, and the client-side `max_concurrent` slot it occupies,
  alive indefinitely at roughly 2 MiB of server-side buffer each. shipped as
  `stream.min_throughput`. One correction to the premise found while building:
  a reader taking *nothing* was already covered, the pump's per-chunk stall
  timeout ends it. The real hole is a reader that accepts one chunk just
  inside that timeout, forever. What makes the floor safe to switch on is the
  denominator, only time the consumer kept data **waiting** counts, so a
  stream quiet because the backend has nothing to send (SSE, long polling) is
  never ended for it. Together with #20 this closes the slowloris pair: #20
  caps how many streams one address holds, this ends the ones going nowhere.

- [x] **#70 Shell completion for the CLI.** (triage 25) `clap_complete` turns
  this into a subcommand and a build step, and the client has enough
  subcommands and flags to make it worth having. shipped: `completions
  <shell>` for the five shells clap names, generated from the same definition
  the CLI is parsed from so it cannot describe a flag that no longer exists.
  No build step was needed. One thing the first cut got wrong and the check
  caught: the script goes to stdout and so do this client's logs, so the gate
  has to sit before logging is initialized, or a startup line lands in the
  middle of a shell function and breaks it for whoever sourced it.

- [x] **#52 The server hands out alternate addresses.** (triage 35) A client
  knows exactly one server URL, so a planned migration or a regional failover
  means editing every client's config. The server could include a list of
  alternates in its handshake, to be tried in order when the primary refuses.
  Small protocol addition, but it needs a story for how a client decides the
  primary is really gone rather than briefly restarting. shipped as
  `alternate_servers`, announced in a handshake header, no protocol version
  needed. The story the entry wanted was **already answered** by the existing
  rotation: it is round-robin and wraps, so a client never abandons the
  primary, it keeps coming back to try it, and the reconnect backoff spaces
  the attempts. Learned addresses are appended after the configured ones so
  the operator's list still decides the order, and both ends cap and filter
  the list. It cannot rescue a client that never connected, which is the
  honest limit of the idea.

- [x] **#65 Client-side load shedding.** (triage 30) When the client's own host
  is saturated it keeps accepting whatever `max_concurrent` allows, so the
  queue grows and every visitor waits instead of some failing fast. Lowering
  the effective concurrency under load pressure needs the process metrics from
  #37 first, and needs care not to oscillate. shipped as
  `adaptive_concurrency`, **reframed twice**. The signal is not #37's CPU: that
  measures this process, and the interesting case is a client at 3% CPU in
  front of a fallen-over backend. What measures it is the wait for a local
  `max_concurrent` permit. And the action is not shedding: a client refusing a
  request the server already dispatched turns a slow success into a fast
  failure, and the client is the place with the least context. Instead the
  announced `max_concurrent` moves, and the server, which already queues past
  it, holds the request, picks another client, or scales out. AIMD for the
  reason TCP uses it, an idle window is not recovery, and the local limiter is
  resized in step so a server ignoring the number cannot push past it.

- [x] **#20 A per-IP ceiling on concurrently open streamed responses.** Split
  out of #17, where it was part (3). Saturating a service's concurrency budget
  currently takes one host holding many slow streams; a per-IP cap makes it
  take a botnet. The pattern exists already: `try_acquire_ws_slot`
  (`aperio-server/src/state.rs`) holds a slot for the life of a proxied
  WebSocket under `max_ws_connections`, and the per-IP rate limiter's map shows
  how the keying and its eviction are done here. shipped:
  `max_streams_per_ip`, following that pattern, with the slot moved into the
  streamed body's own state so it lives exactly as long as the response does.
  Claimed only once a response turns out to *be* a stream, so the limit never
  fires on traffic it was not about. Off by default **and with no suggested
  number**, which is the honest position: a CGNAT puts many real people behind
  one address, so a default would be a guess with a queue of users behind it.
  The map entry is removed at zero rather than left at zero, or it would grow
  one entry per stranger for the life of the process.

- [x] **#11 Restart the background tickers when one panics; escalate the rest.**
  Under the default `unwind` strategy a panic only unwinds its own task, so the
  process survives, but a bare `tokio::spawn`ed background loop that panics
  just *stops*, silently, and its function is lost for the life of the process.
  The global panic hook makes such a panic visible in the log; it does not
  bring the loop back. shipped: `supervise::spawn_supervised` /
  `spawn_ticker` wrap all eleven loops. Restarting is bounded (growing delay,
  five consecutive panics and it is left down, budget restored after five
  minutes of health), because a supervisor that restarts forever turns a loud
  bug into a quiet one. `spawn_critical` is the "escalate the rest" half, for
  the three tasks that own a channel receiver or a bound socket and so cannot
  be called again; they fail visibly rather than silently, which is why they
  get a naming log line instead of a restart. One correction to the entry: the
  panic hook is in `lib.rs`, not `main.rs`.

- [x] **#49 User-defined alert rules, including disk and memory.** (triage 40)
  Two threshold rules exist and are hard-coded: error rate and client-down
  (`alerts.rs`). Everything else an operator might want to be told about,
  starting with the disk filling up and the server's own RSS climbing (both
  already measured for the self-health panel), needs a rule engine rather than
  another pair of environment variables. Merged from three proposals for that
  reason: the general shape is the feature, the two specific alerts are its
  first users. shipped: `alert_rules:` with one metric, one bound and a `for` window that applies to firing and resolving alike. Four metrics rather than an expression language, because the value is in being able to write the rule at all. An unreadable metric (rss on non-Linux) is reported at startup instead of firing on a zero.

- [x] **#50 `ETag` and `304` for `serve:`.** (triage 35) The static file server
  already does single-range `206` responses, so the hard part of HTTP file
  serving is done, but it has no validator: every reload of an unchanged file
  ships the whole body again. An ETag from size and mtime, plus
  `If-None-Match`, is a small amount of code next to what is already there. shipped: a strong validator from size and mtime (nginx's shape), `If-None-Match` compared weakly per the RFC, and `304` on GET and HEAD. Being strong is what also let `If-Range` start working: it used to be declined because there was no validator to compare against, so a resumed download always restarted.

- [x] **#85 An OpenTelemetry bridge for edge hosts.** Asked for directly
  rather than coming from triage. An edge host usually has exactly one
  outbound connection it may make, the tunnel, so its own telemetry has
  nowhere to go without a new firewall rule and a collector credential on a
  machine that should hold as few of those as possible. shipped: the client
  runs an OTLP receiver on loopback (HTTP always, gRPC when a port is given,
  written against hyper rather than pulling tonic into the client for one
  unary method) and carries exports to the server, which forwards them to the
  collector it already uses. The client picks the transport, `tunnel` (frames
  on the socket it already holds, which is the property that makes this worth
  having) or `https`. Three rules make it safe: identity is stamped by the
  server and never taken from the payload; a full queue drops rather than
  waits, because an exporter that cannot hand off blocks the application it
  instruments; and the payload is walked only at its outermost level, so a
  field from a newer OTLP is copied through rather than dropped. A token
  carries `allow_otel` and the server carries `otel_bridge`; both have to line
  up, because "does this deployment forward telemetry" and "may this tenant
  ask it to" are different questions and one setting cannot answer both.

- [x] **#66 An access log for the relays.** (triage 30) HTTP requests get a
  structured line each; TCP and UDP relay connections get nothing, so a
  database tunnel leaves no record of who connected, for how long, or how much
  moved. Connection-level lines (open, close, bytes each way) rather than
  per-packet, which would be unusable. shipped: one `relay_closed` line per
  connection, covering both `expose:` ports and peer clients dialling a
  tunnel, in the same shape and the same two destinations as an HTTP access
  line so one pipeline ingests both. Honours `access_log_sample_rate` with its
  own accumulator, so a busy HTTP surface cannot starve the relay log of its
  share. Pairs with #56: the topology graph says who depends on a tunnel, this
  says when and how much, which is the half an audit actually needs.

- [x] **#44 The visitor's real address reaches a tunnelled backend
  (`proxy_protocol:`).** shipped, after the entry was reframed. It began as
  "accept PROXY protocol on `expose:` ports", which turned out to be worth
  little on its own: it only helps where an L4 load balancer sits in front of
  Aperio, and it does nothing for the driver connecting directly, which is the
  case people picture. The valuable direction was the opposite one. A TCP
  tunnel hands bytes to the backend over a fresh local connection, so the
  backend saw `127.0.0.1` and the visitor's address died at the last hop.
  Now the server carries the observed address in the stream-open frame and the
  client writes a PROXY v2 header before any payload byte, per tunnel and off
  by default, because a backend that is not expecting the header drops the
  connection. It never fabricates an address: unknown, unparseable or a mixed
  address family all mean no header rather than a wrong one, since a receiver
  acts on what the header says. The *accept* half (Aperio behind an L4
  balancer) was deliberately not built; if it ever has a real deployment
  behind it, it takes a new id.

- [x] **#68 Autoscaling refinements.** (triage 30) Cooldown is global while
  every other scaling parameter is per bind, and scale-in is left to the client
  noticing it is idle, so the server can ask for more capacity but never for
  less. Grouped, and both only matter to deployments that use autoscaling at
  all. shipped: the scale-in half. The cooldown half was a **wrong premise**,
  `cooldown` is already a `scaling:` field, carried per record as
  `cooldown_secs` and used per record at both call sites; nothing was global.
  Scale-in emits a `scale_in` reason with a lower desired capacity to the same
  endpoint, so the server still kills nothing. Two asymmetries with scale-out
  are deliberate: below **half** the target rather than merely below it, which
  is the hysteresis that keeps a pool at its target from oscillating, and for
  **four** windows rather than one, because an instance short costs latency on
  live traffic while an instance over costs money. It never asks for the last
  one to go: 1 to 0 stays the client's decision via `idle_timeout`, which knows
  about in-flight requests the server cannot see.

- [x] **#63 Config authoring help.** (triage 30) Template variables
  (`${HOSTNAME}`, `${ENV}`) so one file serves several environments, and a
  warning from `check` when a literal secret appears in the file rather than an
  environment reference. Grouped as two sides of "the config file is written by
  hand and we can help". shipped: both, plus a third that belongs to the same
  idea and was asked for while building it, **typo tolerance**: an unknown key
  used to be ignored in silence, and now warns naming the key it was probably
  meant to be, checked against the generated schema rather than a hand-kept
  list so it cannot go stale. Only `${NAME}` is expanded, never a bare
  `$NAME`, because `$` appears in passwords, regexes and `run:` snippets and
  rewriting those would corrupt working files; an unset variable is an error
  rather than an empty string, since substituting nothing yields a file that
  parses and means something else.

- [x] **#62 Client process lifecycle knobs.** (triage 30) `pid_file` for init
  systems that want one, `startup_delay` before a service registers, and
  `depends_on` so one service waits for another's tunnel. Grouped because they
  are the same category of small operational sugar, and worth being sceptical
  about: a process supervisor does all three better, which is why none of them
  scores higher. shipped: all three, with the scepticism kept in the docs
  rather than dropped. `depends_on` waits and then proceeds anyway after 60s,
  since a dependency that never arrives must not keep a service off the air;
  what makes that bound safe is that an unknown name, a self-dependency and a
  cycle are all refused at startup, because each of them otherwise ends as
  "everybody waited, then started", which is indistinguishable from working.
  `pid_file` is removed on a clean exit only, never after a crash, where a
  stale pid would have an init system signalling an unrelated process.

- [x] **#61 Probe endpoints for container orchestrators.** (triage 30)
  `/aperio/health` returns a JSON body with counters and takes two locks to
  build it, which is more than a `HEALTHCHECK` every five seconds needs, and
  there is no separate readiness signal for the window where the server is up
  but the store has not finished opening. A bodiless `/aperio/healthz`, plus
  `ready` and `live` split the way Kubernetes expects. An aggregate
  `/aperio/api/health/tunnels` was proposed alongside and is folded in here,
  though `/api/stats` and `/api/clients` already answer that question.
  shipped: `/aperio/healthz` (bodiless, no locks) and `/aperio/readyz`. The
  readiness signal turned out to be worth more than the entry expected, not for
  the store-opening window but for **shutdown**: `readyz` answers 503 from the
  moment a signal arrives while the process is still serving, which is the
  other half of #58's `shutdown_drain`, the load balancer stops routing here
  and the drain finishes what is in flight. The aggregate tunnels endpoint was
  not built, for the reason the entry already gives.

- [x] **#59 Per-service backend tuning knobs.** (triage 35) Several settings
  that matter per backend are only available globally: connect timeout, idle
  timeout for pooled connections, the buffered/streamed threshold, the minimum
  TLS version for an `https://` backend, and the heartbeat interval. Merged
  because they are one pattern (a global default with a per-entry override) and
  the value of any single one is small; the value of the pattern is that a
  slow backend and a fast one stop having to share a number. shipped: **two of
  the five**, `connect_timeout` and `min_tls_version`, which are the two with a
  nameable victim today (a loopback backend next to one across a VPN; a fleet
  with one legacy `https://` backend). The other three stay global on purpose:
  no deployment we can describe is hurt by sharing one number for the pooled
  idle timeout, the buffered/streamed threshold or the heartbeat interval, and
  five keys across four config surfaces for two real cases is the wrong trade.
  If one of them acquires a concrete case, it is a one-line addition to the
  same pattern.

- [x] **#60 Two dashboard readability wins.** (triage 30) Syntax highlighting
  for captured JSON, XML and HTML bodies in the inspector (currently raw text),
  and a calendar range picker for the activity and traffic charts, which today
  offer a live window and a fixed long window and nothing in between. Grouped
  because each is a contained frontend change with no backend surface.
  shipped: highlighting is a hand-written tokenizer in `lib/highlight.ts`, not
  a library, since the inspector shows three shapes and a general-purpose
  highlighter is hundreds of kilobytes of grammars for languages that never
  appear here; minified JSON is re-indented too, and anything that does not
  parse or is too large to tokenize falls back to exactly what was rendered
  before. The second half turned out to be half-built: the **traffic** chart
  already had a custom `from`/`to` range picker. What was missing was the
  **activity** chart's middle ground, and its data is a fixed 15-minute ring
  on the server, so the answer is a 5-minute view cut from the same ring in
  the browser rather than a new endpoint.

- [x] **#56 Client-to-client edges in the topology view.** (triage 35)
  `--bind-tunnels` lets one client dial another client's exposed tunnel, so
  those dependencies are real, but the topology graph only draws client to
  route. A dependency the graph does not show is a dependency nobody remembers
  at the moment it breaks. shipped: `/api/topology` returns `consumers`, and
  the graph draws them. The identity problem was the real work: a consumer is
  not a registered client and opens a fresh WebSocket per connection, so
  counted naively every invocation and every reconnect would be a new node.
  An edge is keyed by peer address plus token, and the node is the address, so
  several processes behind one NAT collapse into one node, which is the right
  grain for "who depends on this tunnel" and is honest, an address is observed
  rather than claimed. An edge outlives its connections by 15 minutes, because
  a consumer that has just finished a query is idle rather than gone, and an
  idle edge is drawn dashed.

- [x] **#57 Command-line parity with the dashboard.** (triage 35)
  `aperio-client api` covers most admin operations but not all of them, so
  automation occasionally has to fall back to raw curl. An ongoing chore rather
  than a project: the rule worth adopting is that a new dashboard action ships
  with its subcommand. shipped: eleven gaps closed (`client config`,
  `webhook test`, `org custom-name`, `org oidc`, `publish`, `subscribers`,
  `explain`, `schema`, `activity`, `audit-csv`, `edge-traefik`/`edge-ask`), and
  the rule is now a test rather than a habit: it scans the server's own route
  declarations and fails when one has no subcommand. Four routes are exempt
  with the reason next to them (the SSE stream, and the browser-bound WebAuthn
  and TOTP enrolment ceremonies, which a one-shot call cannot perform). The
  gaps the entry named from triage, token update and org hostnames, turned out
  to already exist.

- [x] **#58 A configurable shutdown drain.** (triage 35) Shutdown already
  broadcasts `ServerShutdown`, waits 200 ms for those frames to flush, and then
  ends long-lived connections so axum's graceful shutdown can complete. What is
  not configurable is how long to wait for in-flight requests before that,
  which is the number an operator behind a load balancer actually wants to set.
  Merged from two proposals describing the same knob from opposite ends.
  shipped: `shutdown_drain` in seconds, plus `auto`, which takes the longest
  drain budget connected clients announce in their Ping (the longest, not the
  average, since the drain is over when the slowest client has finished) and
  caps it at 30s, because a client is not the operator and cannot be allowed
  to hold the process past the platform's SIGKILL timer. Default stays `0`, so
  no deploy starts waiting because of a version bump. The forced-exit fallback
  is now `shutdown_timeout` instead of a hard-coded ten seconds.

- [x] **#53 Static Prometheus labels from the client.** (triage 35)
  `metrics_labels: {env: prod, region: eu-west}` announced on connect and
  attached to that client's series, so one Prometheus can serve several
  environments without relabelling rules. Needs a cap on label count and
  cardinality, since labels come from clients and cardinality is how a metrics
  backend dies. shipped: announced in the Ping, attached to
  `aperio_client_requests_total`, and sanitized on arrival rather than on the
  way out, because a series once scraped is in the backend whatever the server
  does afterwards. At most 8 labels, Prometheus-legal names only, reserved
  names refused, values bounded and escaped, and an invalid label is dropped
  rather than costing the client its metrics. `APERIO_METRICS_LABELS` takes the
  flat `k=v,k=v` spelling a container platform can inject.

- [x] **#55 Sampling for the access log.** (triage 35) The per-request access
  line is all or nothing. At high volume operators want a fraction, the way OTel
  export already takes `sample_rate`. Note the trap the OTel implementation
  already avoided: sampling must be per request and consistent, and failures
  should always be logged regardless of the sample decision, since a sampled-out
  error is the one line anybody needed. shipped: `access_log_sample_rate`
  (`APERIO_ACCESS_LOG_SAMPLE_RATE`) thins out both the `aperio_access` event
  and the access-log file. Sampling is deterministic rather than random, an
  accumulator so `0.1` is exactly one line in ten and not one in ten on
  average, since the point of turning the volume down is knowing what the
  volume now is. A 5xx response is never sampled out, and neither is a refused
  or failed request. The decision is made after the telemetry submission, so
  the dashboard's counters, the latency histogram and the rate charts stay
  exact.

- [x] **#48 `connections:` as a `{min, max}` range, an elastic pool.**
  (triage 40) Reframed from `lazy_connect`: not dialing until the first
  request trades away the property people like most about a tunnel, that the
  URL works the instant the client starts, whereas sizing the *pool* with load
  keeps that and still stops a service holding its busy-hour connection count
  around the clock. shipped: `connections: {min: 1, max: 8}` opens the floor
  and grows towards the ceiling, one connection at a time, on requests in
  flight per connection; it shrinks on a much longer cooldown than it grows.
  A plain `connections: N` is untouched, elasticity is opt-in, because our own
  measurement has the throughput curve peaking at four connections on a
  shared-CPU host. Bandwidth is still divided by `max` so a growing pool
  cannot exceed the declared budget, and each connection announces the pool's
  real size rather than its ceiling. `APERIO_CONNECTIONS_MIN` / `_MAX`.

Shipped ideas, newest id last. They keep their id forever: it is never
renumbered and never reused, so a commit message or a comment naming `#28`
still points at the same thing. Each entry keeps the reasoning it was
written with, followed by a `shipped:` note saying what was actually built
and where that differed from the plan.

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

- [x] **#23 Streamed-response receive path on the server: drop the per-chunk
  lock and the per-chunk copy.** shipped: `ConnCtx` caches each stream's pump
  sender after one ownership-checked registry lookup, chunk payloads travel
  as `Bytes` slices of the arriving WebSocket message end to end
  (`BodyFrame::Data` and `TcpConsumerMsg::Data` are `Bytes` now, covering the
  TCP/UDP/WS relay arms too), and the per-chunk byte accounting became 1 MiB
  batches settled at stream end, disconnect, or error, so the shared stats
  and quota locks are taken per megabyte rather than per chunk. Found by
  reading the code against a 2 MB-body
  wrk run (the 2026-08 tunnel bandwidth look; numbers unmeasured beyond that
  run). Every streamed chunk arriving over the tunnel goes through
  `deliver_response_chunk` (`aperio-server/src/tunnel/ws.rs`), which (a) locks
  the global `state.response_streams` tokio `Mutex<HashMap>` to find the
  stream's sender, and (b) is handed `payload.to_vec()`, a full copy, even
  though the axum 0.8 ws `Message` already owns the bytes as `Bytes`, so a
  `slice()` would be a refcount bump. The buffered path already got exactly
  this fix (commit 2818322); the streamed path (bodies over 256 KB) did not.
  At bulk throughput that is one global lock acquisition and one memcpy per
  16-128 KB chunk across all connections. Plan: cache the `chunk_tx` per
  stream in the connection's read loop after first lookup (the ownership
  check only needs doing once per stream), or shard/replace the map
  (`DashMap` or per-connection registry), and thread `Bytes` end to end like
  the buffered path does. Same story for the TCP/UDP/WS relay arms next to
  it, which also `to_vec()` each frame.

- [x] **#24 Client send path for streamed bodies: coalesce backend chunks
  into full frames and batch the WebSocket flush.** shipped: `ChunkCoalescer`
  (`aperio-client/src/proxy/http.rs`) accumulates backend chunks into full
  `STREAM_CHUNK_SIZE` frames on all three backend paths (http, h2, unix),
  flushing the remainder the moment the backend has nothing more ready
  (`now_or_never` poll) so trickles are never held, and the tunnel writer
  drains its queue with `feed()` and flushes once per batch. The
  tokio-tungstenite upgrade to a `Bytes`-based release was deliberately left
  out, it is a dependency-wide change with its own interop surface; still
  worth its own look if the final copy into the write buffer ever shows up
  in a profile. Sibling of #23, same origin. Once a response switches to streaming, the client forwards each
  chunk exactly as reqwest yields it (`aperio-client/src/proxy/http.rs`,
  `handle_incoming_request`), typically 16-64 KB per read, so a 2 MB body
  becomes several dozen frames where 16 would do at `STREAM_CHUNK_SIZE`
  (128 KB). Each frame costs an `encode_binary_frame` allocation and copy, an
  mpsc hop, a client-side WebSocket mask pass over every byte, and a
  `ws_sender.send` which is feed+flush, one syscall per frame. Plan:
  accumulate backend chunks up to `STREAM_CHUNK_SIZE` (flushing on quiet, so
  latency-sensitive trickles are not held), and in the writer task drain the
  channel with `feed()` and flush once per drained batch instead of per
  message. Upgrading tokio-tungstenite (0.23 on the client) to a
  `Bytes`-based release would also let the frame be built once without the
  final copy into the write buffer.

<!--
Everything from #26 down came out of the 2026-08 proposal triage: a batch of
suggestions was checked against the code one by one, scored, merged where
several described one feature, and dropped where the premise turned out to be
false. The parenthesised number is that triage score (100 = must have, 0 =
noise), kept so the next prioritisation starts from the reasoning rather than
from scratch. Ten proposals described things that already exist and are
recorded in Withdrawn under #84 so they are not proposed again.
-->

- [x] **#26 `routes:` becomes a first-class policy block, not just a
  destination.** (triage 70) Today a `routes:` entry only says where a
  hostname/path goes; every knob that should be per-route lives somewhere
  coarser: the gateway timeout is server-global, header rules are server-wide
  or per-service, rate limits are a separate `rate_limits:` list that repeats
  the same hostname and path, and there is no per-route body ceiling or
  cache-control. Merged from six proposals that each asked for one field, and
  worth doing as one change because they all land on the same struct
  (`RouteRule`, `aperio-config/src/lib.rs`) and the same lookup in
  `routing.rs`: `timeout`, `headers: {request, response}` reusing the existing
  `HeaderDirectives`, an inline `rate_limit: {burst, per_second}` (the
  standalone `rate_limits:` list stays, the inline one wins for that route),
  `max_response_body`, `cache_control`, and a `methods:` filter on rate limit
  rules. The care it needs: precedence has to be stated once and tested (route
  beats service beats server), and every added field must degrade to today's
  behaviour when unset. shipped (8cc4b62): an entry with neither `redirect` nor `respond` is a policy rule carrying `timeout`, `headers` and `rate_limit`; the two kinds are matched independently so neither can hide the other, and mixing them on one entry is refused at startup. `rate_limits:` gained the `methods:` filter from the same batch. `max_response_body` and `cache_control` did not need their own fields: a per-route `headers.response.add` sets cache-control, and a server-side response ceiling is a different mechanism, left out rather than half-built.

- [x] **#27 A deny list for visitor IPs.** (triage 65) `allowed_ips` is a
  whitelist on a token or a service, `admin_allowed_ips` fences the admin
  surface, and the WAF matches on path, method, header and body size, but
  there is no way to say "this address never gets in" server-wide or per
  organization. Blocking one scanner today means either an allowlist (which
  breaks everyone else) or a fronting proxy. The pieces already exist:
  `parse_trusted_proxies` parses IP/CIDR lists, and the per-IP rate limiter
  shows where the check belongs in the request path. Wants to be checked
  early, before routing and before the rate-limit bucket is charged, and to
  answer with the same stealth response an unclaimed route gives rather than
  confirming that the address was recognised. shipped (6987ab2): `denied_ips:`, checked at the outermost layer so it covers proxied traffic, the dashboard, the API and the tunnel endpoints, and a blocked request cannot spend a rate-limit bucket. Answers 403 rather than the stealth response, because this is an operator's explicit server-wide block and locking yourself out has to be visible. Hot-reloadable.

- [x] **#28 The audit log becomes searchable and exportable.** (triage 65)
  `audit_handler` (`aperio-server/src/api/webhooks.rs:23`) takes no parameters
  at all: it returns the recent events for the caller's org and nothing else.
  For a log that is deliberately tamper-evident, hash-chained, retained by
  policy and org-isolated, not being able to answer "what did this user do last
  Tuesday" is the gap that makes it ceremonial rather than useful. Wants query
  parameters for event kind, actor, organization and a time range, then CSV and
  JSON export of exactly the filtered set (the traffic export at
  `/aperio/api/export/traffic.csv` is the shape to copy). Filtering must happen
  after the org fence, never instead of it, and the export must go through the
  same redaction the inspector uses. shipped (b1b9387): `event`, `actor`, `q`, `from`/`to` and `limit` on `/aperio/api/audit`, which switch it from the recent ring to a search of the durable log, plus `/aperio/api/export/audit.csv` and the matching dashboard filters. The organization fence is applied around the search, never as one of its predicates.

- [x] **#29 Backend resilience: retry with backoff, and a circuit breaker per
  backend.** (triage 60) The server can fail a request over to another client
  (`failover`, `retry_on_5xx`) and can eject a client that misbehaves
  (`outlier_ejection`), but the client's own hop to its backend has nothing:
  one refused connection or one 502 is the visitor's answer. Merged from two
  proposals because they are two halves of one policy: `retry: {attempts,
  backoff}` for the transient case and `circuit_breaker: {failures, window,
  open_for}` for the backend that is simply down, so retries stop hammering it.
  `scaling.rs` already has a breaker for autoscaling callbacks and is the shape
  to reuse. The part that needs care is method idempotency (the same reasoning
  `failover_all_methods` already encodes) and not retrying a response whose
  body has started streaming. shipped (9c35aab): `retry: {attempts, backoff, all_methods}` and `circuit_breaker: {failures, open_for}`, per service or top level, both off by default. Only failures before a response head are retried, only replayable requests (a streamed upload is consumed by its first attempt), and only idempotent methods unless opted in. Any response head counts as a success for the breaker, since a 500 is a backend that is up.

- [x] **#30 Honour and forward `X-Request-Id`.** (triage 55) The server mints a
  UUID per request (`proxy.rs:1138`) and uses it everywhere internally, but it
  neither reads an inbound `X-Request-Id` nor passes any id to the backend. So
  a visitor's trace id dies at the edge, and a backend log line cannot be
  joined to the server's own access log for the same request. Take the inbound
  header when present (validated and length-capped, it is attacker-controlled),
  mint one otherwise, send it to the backend and echo it on the response. Cheap
  and standard, and it makes the profiling script and the inspector line up
  with whatever the operator already runs. shipped: the id travels to the backend and is echoed to the visitor under `request_id.header`; adopting a visitor-supplied one is opt-in (`request_id.trust_inbound`) and bounded, and the internal id that keys in-flight requests stays server-minted so it can never be chosen by a visitor.

- [x] **#31 Filters on the request inspector.** (triage 55) The inspector keeps
  recent transactions with a microsecond timeline and is one of the reasons to
  run the dashboard, but the only ways in are "the most recent N" and "this
  exact id". Merged from two proposals: field filters (`status`, `method`,
  `path`) and a time range (`before`/`after`, where only `before` plus a limit
  exists today). The interesting case is the one an operator actually has,
  "show me the 500s from the last ten minutes", which needs both. shipped, smaller than scored: the dashboard already filtered the traffic view by text, method and status in the browser, so what was missing was the same at the API. `/aperio/api/logs` now takes `status` (exact or class), `method`, `path` and `limit`. Searching the durable access log was deliberately left out: its lines carry no organization field, so a search over the file could not be scoped to a tenant.

- [x] **#32 Scheduled maintenance windows.** (triage 55) Maintenance mode is a
  manual toggle with an optional TTL, so a window at 02:00 on a Sunday means
  somebody sets an alarm. Wants `from`/`to`, a `days` list and an explicit
  `tz`, evaluated server-side. The cost to be honest about is correctness
  around time zones and daylight saving, which is why the timezone has to be
  explicit rather than inferred from the host. shipped as `maintenance_windows:` in `aperio-server.yaml` rather than as a schedule on the runtime flag, because those flags are in-memory and a recurring window has to survive a restart. IANA time zones (hence chrono-tz), midnight-wrapping windows belong to the day they start, and a malformed entry refuses startup.

- [x] **#33 Draining a service that a config reload removed.** (triage 55) When
  a service disappears from a reloaded config the client stops serving it
  immediately, so requests already in flight through that service are dropped
  and the visitor sees a failure caused by an edit that was meant to be
  invisible. The protocol already has `Draining` and the server already knows
  how to stop dispatching to a draining client; this is about using both on the
  reload path with a bounded wait. Closer to a correctness fix than a feature,
  which is why it scores above several flashier ideas. shipped: the reload path now announces `Draining` and waits for in-flight requests, bounded by `reload_drain` (default 10s, 0 = the previous immediate drop).

- [x] **#35 gRPC health probing.** (triage 50) `h2c://` and `h2://` targets are
  a supported, documented shape for gRPC backends, but the health probe is a
  plain HTTP GET, so the documentation ends up advising an explicit URL
  instead. Speak `grpc.health.v1.Health/Check` when the target is an h2 one,
  falling back to the current GET when a probe path is set explicitly. shipped: against an `h2c://`/`h2://` target the probe calls `grpc.health.v1.Health/Check` and `health.endpoint` names the gRPC service (`/` = the whole server); healthy needs a 200, `grpc-status: 0` from headers or trailers, and `SERVING`. The two one-field protobuf messages are encoded by hand, so no prost/tonic dependency was added. An absolute URL still means a plain HTTP probe, and a target with no probe configured is untouched.

- [x] **#36 Environment variables for the `run:` process.** (triage 50) `run:`
  takes a command line and nothing else, so anything the child needs
  (`DATABASE_URL`, a port, a profile) has to come from the client's own
  environment, which also means it cannot differ per service. `env: {KEY:
  value}` per entry, with the usual rule that a value looking like `${VAR}` is
  expanded from the client's environment so secrets are not written into the
  file. shipped: an `env:` map on the `subscribe:` entry, applied before `APERIO_MESSAGE_TOPIC`/`APERIO_MESSAGE_ID` so a declaration cannot shadow them. `${VAR}` expansion was left out; it belongs with the config templating idea (#63) rather than to this one key.

- [x] **#37 The client reports its own health, not just its backend's.**
  (triage 50) Everything the server knows about a client is "is it pinging" and
  "does its backend answer". Merged from two proposals: process figures (CPU
  percent, RSS) and link figures (tunnel round-trip time from the existing
  ping/pong, jitter, reconnect count), all as additive `Ping` fields and new
  `ClientHandle` columns, surfaced in the dashboard's clients table. Two
  cautions: the numbers are per client process, not per service, and inside a
  container the naive readings mislead unless the cgroup files are preferred. shipped: `rtt_ms`, `jitter_ms` and `reconnects` measured from the client's own ping/pong (no protocol change beyond reporting them), plus `cpu_percent` and `rss_bytes` from `/proc`, Linux only and absent elsewhere rather than approximated. Stored on `ClientHandle`, carried by `/aperio/api/stats`, shown on the clients table. An absence overwrites the previous value so a figure cannot age while looking live.

- [x] **#38 Batch the server's writes to a client, as the client already does
  to the server.** (triage 45) #24 taught the client's tunnel writer to drain
  its queue with `feed()` and flush once per batch instead of paying a syscall
  per frame. The server's writer (`tunnel/ws.rs`) still sends one message at a
  time, and it is the busier side under fan-out. `SendPacer` sits in that loop
  already for bandwidth pacing, so the batching has to spend the pacer's budget
  for the whole batch rather than per frame. The technique is proven on the
  other side, which is what makes this cheap. shipped: the writer feeds what is already queued and flushes once per batch. The pacer is still spent per frame, and a paced connection flushes before sleeping its debt, so batching cannot turn shaping into bursts or leave finished frames sitting in the buffer.

- [x] **#39 A "test fire" button when creating a webhook.** (triage 45) A
  webhook that was configured wrong is discovered the next time something
  actually happens, which is exactly the wrong moment. Send a synthetic event
  through the real delivery path (including the outbound policy check and the
  signature) and show the response. The delivery log and the refire endpoint
  already do most of this. shipped: `POST /aperio/api/webhooks/{id}/test` and a Test button. One attempt rather than the retry schedule, since the caller is waiting and a success on the fourth try reported as success would hide the failure being tested for, and its own event name so a receiver can ignore it.

- [x] **#41 `include:` for splitting a config across files.** (triage 45) One
  `aperio.yaml` per deployment stops scaling when there are twenty services or
  when different teams own different entries. `include: [services/prod.yaml]`
  with a documented merge order, path resolution relative to the including
  file, and a depth cap so a cycle cannot hang startup. The hot reload watcher
  has to watch every included file, not just the root one. shipped: merged at the yaml level (keys replace, sequences of mappings concatenate), paths relative to the including file, five-deep cap, cycles reported. The hot-reload watcher tracks every contributing file and re-reads the set on each change, so adding an include is noticed.

- [x] **#42 Zero-copy chunk delivery on the client's receive path.** (triage 45)
  #23 did this on the server: chunk payloads travel as refcounted slices of the
  WebSocket message instead of copies. The client's read loop still calls
  `payload.to_vec()` five times (`service.rs:1175-1205`, request chunks, TCP,
  UDP, WS frames and the full-response body). The blocker is that
  tokio-tungstenite 0.23 hands out `Vec<u8>` rather than `Bytes`, so this needs
  the dependency upgrade first, which is why it is not simply the mirror of a
  change already made. shipped: tokio-tungstenite 0.23 to 0.29, which also removed the second copy of the WebSocket stack the workspace was compiling (axum already pulled 0.29). Relay, datagram, proxied-WS and request-body chunks are now slices of the arriving frame; the binder's own local-socket channels stay `Vec<u8>`, since those datagrams are built rather than received.

- [x] **#47 Identity headers to the backend.** (triage 40) The only
  `x-aperio-*` headers a backend can see are the cache markers. A multi-tenant
  backend that wants to know which organization or which token served a request
  has to infer it. Add opt-in `X-Aperio-Org` / `X-Aperio-Client-Id` /
  `X-Aperio-Token-Name`, opt-in because they are new trust surface: they must
  be stripped from the inbound request unconditionally so a visitor can never
  forge them. shipped: `identity_headers` (off by default) adds `x-aperio-client-id`, `x-aperio-org` and `x-aperio-token` per dispatch attempt, so a failover names the client that actually served. The inbound strip is unconditional rather than tied to the setting: a header only stripped while a feature is on is a header forgeable by turning it off.
