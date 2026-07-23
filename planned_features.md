# Planned Features

Future feature ideas. Backlog items carry stable `#N` ids (never renumbered or
reused); a shipped item keeps its id and flips to `[x]` in place with a short
"shipped: ..." note.

## Future ideas

- [ ] **#1 Auto-tune resource limits from the environment.** Derive sensible
  defaults for some capacity settings (e.g. `APERIO_MAX_CONCURRENT_REQUESTS`,
  `APERIO_MAX_WS_CONNECTIONS`, cache budget) from the container/host it runs in
  — cgroup CPU/memory limits, Docker deploy constraints, available file
  descriptors — instead of fixed constants. Needs care: an operator must always
  be able to tell what value is in effect and why (surface it via
  `--print-config`), and an explicit env/yaml/dashboard value must always win
  over an auto-derived one, so behaviour is never surprising. Discuss scope
  before implementing.

- [ ] **#2 Speed up the Windows release build without vendoring OpenSSL from
  source.** The `x86_64-pc-windows-msvc` release job spends several minutes
  compiling OpenSSL from source via `aperio-server/vendored-openssl` (needed
  because webauthn-rs pulls in openssl). Dropping vendored on Windows and
  linking the runner's system OpenSSL would cut that, but naively it breaks the
  self-contained `.exe`: dynamic linking makes the binary depend on
  `libssl`/`libcrypto` DLLs at runtime, and MSVC static linking hits the classic
  CRT (MT vs MD) mismatch. Explore a reliably-static, ABI-compatible prebuilt
  OpenSSL (or a webauthn crypto path that needs no openssl at all) so the
  released binary stays download-and-run. Until then the cost is mitigated by
  the default-branch release cache (ci.yml `warm-release-cache`) and the Windows
  Defender exclusion in `release.yml`. Discuss before implementing.

- [ ] **#3 Re-validate the dashboard SSE live stream while it is open.** The
  dashboard live stream (`live_stream_handler` in `aperio-server/src/api/clients.rs`)
  only resolves the caller's org/role at connection time; an already-open stream
  keeps emitting live traffic + stats even after the session is revoked, expires,
  or is cleared via "sign out everywhere". Re-check `validate_session` /
  `dashboard_role` periodically inside the stream loop (e.g. on each stats tick)
  and close the stream when the session is no longer valid; an explicit
  "auth revoked" signal on the `state.shutdown` path could also drive it. Low
  severity: requires an already-authenticated session and leaks at most one
  ~2s tick of data after revocation. (From the 2026-07 static security review.)

- [ ] **#4 Stream static-serve responses instead of reading whole files into
  memory.** In `--serve` mode, `handle` in `aperio-client/src/serve.rs` reads the
  entire resolved file into memory with `tokio::fs::read` on every request (even
  HEAD), and `max_response_body_size` only bounds what the *tunnel* forwards —
  the file is already fully buffered in the serve process. Concurrent requests to
  a large served file can OOM the client. Stream the file (e.g. `ReaderStream` /
  chunked body, which means moving the response body off `Full<Bytes>` to a boxed
  body) and/or add a per-serve max file size; on HEAD return metadata without
  reading the body. Low severity: opt-in feature, client-process-only DoS bounded
  by the size of files the operator chose to publish (a `dist/` of web assets is a
  non-issue). Also in scope: `serve.rs::resolve` uses blocking `std::fs::canonicalize`/
  `is_file`/`is_dir` in the async `handle` path — those synchronous syscalls run on
  a Tokio worker thread; move them to `tokio::fs` or `spawn_blocking` as part of the
  same rework. (From the 2026-07 static security review + a 2026-07 client review.)

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
  makes N independent probes and reports `backend_healthy` per connection — N×
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
  offline }`; `TopologySection.tsx` self-fetches it and renders — A: client-less
  static `routes:` and public `expose:` ports (master-only); B: dashed
  "declared but offline" nodes from token-granted binds no client serves
  (per-org); C: passive outlier ejection (`ejected` now on every client detail)
  coloured/labelled in the map. Deferred the route-limits overlay and
  per-connection bytes/geo edge weights (server tracks bytes only in aggregate).
  Original note below.

  Today `TopologySection.tsx` derives its graph purely
  from `stats.active_clients` — the same snapshot the Clients table renders — so
  it only shows *connected* clients and adds nothing but live req/s edge labels.
  It should become the one view that shows *how a request is routed*, including
  routing that has no live tunnel client. Clean split: **Clients = who is
  connected now (table); Topology = the routing map (config + live)**. Needs a
  dedicated `GET /api/topology` handler returning a typed
  `TopologyGraph { nodes, edges }` (org-scoped like the others), unioning the
  in-memory client registry with the config-side route registries. Three parts:
  - **A — client-less route nodes.** Fold in the server-side route definitions
    that exist whether or not a client is connected: static routes
    (`static_routes.rs` `RouteRule`: redirect/respond), expose rules
    (`expose.rs` `ExposeRule`: public TCP port → tunnel key), and route rate
    limits (`route_limits.rs`) as an overlay. These have no `ClientDetail` today
    so Topology can't see them; they render as route nodes that terminate in a
    redirect/respond/expose sink instead of a backend.
  - **B — "declared but offline" gap.** From each token's granted binds
    (`store/tokens.rs`: `hostnames`/`paths` a token *may* claim), emit a dim /
    dashed route node when no active client currently claims that bind — the one
    thing a table structurally cannot show (there is no row for an absent
    client). This surfaces "the service that should be up but isn't" at a
    glance. Decision needed: derive expected binds from granted token scopes
    (broad) vs. an explicit "expected services" declaration (precise).
  - **C — routing-health overlay.** Surface per-client routing state the graph
    is the natural home for and that no screen shows today: `ejected_until`
    (passive outlier ejection — a client silently pulled from rotation right
    now), `draining`, and load-balance fan-out (N clients on one hostname shown
    as a one-to-many group, not N unrelated rows). Colour nodes/edges by state.
  Non-goal for now: per-connection **bytes**/**geo** edge weights — the server
  tracks bytes only in aggregate (`PersistentStats`), not per connection, so
  that needs new server-side counters and is out of scope. Ship A/B/C behind the
  new endpoint; keep Clients as-is. (From a 2026-07 dashboard review.)

- [ ] **#8 Pool Unix-domain-socket backend connections.** `dial_and_send` in
  `aperio-client/src/proxy/unix.rs` opens a fresh `UnixStream::connect` +
  `http1::handshake` for every request with no reuse. Under very high request
  rates that is per-request connect/handshake overhead (a keep-alive pool via
  `hyper-util`'s legacy client would amortize it). Low priority: Unix sockets
  have no TCP `TIME_WAIT`, so FDs are released promptly — the reviewer's "EMFILE
  / FD exhaustion" framing is overstated; this is an efficiency win, not a leak
  fix. (From a 2026-07 client review.)

- [ ] **#9 Per-stream backpressure for tunneled WS/TCP delivery instead of
  drop-on-full.** The `try_send` fix (commit `2e5273b`) protects the tunnel read
  loop — one stalled backend can no longer starve `Pong` and trip the liveness
  watchdog — but it converts *transient* backpressure into stream death: when a
  stream's 64-slot channel fills, the stream is dropped on the spot. WS/TCP are
  lossless protocols (the UDP analogy the fix borrowed is weak), so a healthy
  but legitimately slow consumer — e.g. a large file piped over a tunneled TCP
  stream whose backend socket applies flow control — can be killed by a burst
  of server→client frames that momentarily outpaces the backend. Fix properly:
  per-stream backpressure that pauses reading *that stream's* frames without
  blocking the shared loop (e.g. buffer-and-park the stream with a bounded
  spill, or a per-stream credit/window echoed to the server), or at minimum a
  substantially larger per-stream buffer plus a grace timeout before dropping.
  (From the 2026-07 unpushed-commits review.)

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
  IPv6 address and fails (`ENETUNREACH`), and — unlike a glibc `curl` on the same
  host — does not fall back to the reachable IPv4. musl does not honor
  `AI_ADDRCONFIG` the way glibc does, so even disabling IPv6 in the container
  (`net.ipv6.conf.all.disable_ipv6=1`) does not help: getaddrinfo still returns
  the AAAA and the client still tries it first (fails with `EADDRNOTAVAIL`). This
  caused a real outage (2026-07); the only reliable workarounds are DNS-side
  (drop AAAA / pin an IPv4 via `extra_hosts`), which is a footgun.
  Proposed fix (client-only):
  - **Tier 1 — config escape hatch:** an `ip_family: auto | ipv4 | ipv6` field
    (CLI `--ip-family`, env `APERIO_IP_FAMILY`). `ipv4` connects only to A
    records, deterministically dodging unreachable AAAA. ~small change.
  - **Tier 2 — robust default (`auto`):** replace the single `TcpStream::connect`
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
  failure ("… unreachable — trace spans will be dropped"). Blocking IO on a
  thread so it needs no Tokio runtime and never blocks startup. Original note
  below.

  With `APERIO_OTEL` on, the batch span exporter silently POSTs to
  `otel_endpoint`; any failure (wrong host/port, DNS, collector down, wrong
  protocol/path) is invisible — spans just never arrive, and the only visible log
  is the harmless `BatchSpanProcessor.ExportingDueToTimer` heartbeat. In a 2026-07
  session this made a misconfig indistinguishable from "no traffic to trace":
  Jaeger stayed empty with no error. After building the provider in
  `telemetry::build_provider` / `init` (`aperio-server/src/telemetry.rs`), do a
  lightweight reachability probe to the resolved endpoint host:port (a short-
  timeout TCP connect, or an HTTP request to the `/v1/traces` path) and log a
  clear line: INFO on success ("OTLP endpoint <ep> reachable"), WARN on failure
  ("OTLP endpoint <ep> unreachable: <err> — spans will be dropped"). Must NOT fail
  startup — tracing is non-critical, so a bad collector must never take the server
  down; run the probe non-blocking (spawn it, or a single short-timeout connect
  before serving). Consider also surfacing the batch exporter's own runtime export
  errors (currently swallowed) and/or a periodic re-check. Note the probe only
  confirms the collector is listening, not that spans parse end-to-end, but it
  catches the common wrong-endpoint / not-running / DNS cases immediately.
  (From a 2026-07 field debugging session.)

- [ ] **#11 Supervise long-lived background loops so a panic restarts (or at
  least surfaces) instead of silently killing the loop.** Under the default
  `unwind` strategy a panic only unwinds its own task, so the process survives —
  but a bare `tokio::spawn`ed background loop (the stats/uptime tickers, the
  token-expiry sweep, accept loops, expose listeners in `main.rs`, and the
  per-connection writer tasks) that panics just *stops*, silently, with no
  restart. Its function is lost for the life of the process (e.g. uptime stops
  ticking) even though nothing crashed. The global panic hook added for #1/#2
  now makes such a panic *visible* in the log, but does not bring the loop back.
  Wrap the critical long-lived loops in a supervisor: a small helper that
  `tokio::spawn`s the loop, awaits its `JoinHandle`, and on a panic/early-exit
  logs it and respawns with a short backoff (a `JoinSet`-based supervisor, or a
  `spawn_supervised(name, factory)` wrapper). Scope carefully — only genuinely
  restartable, idempotent loops (tickers, accept loops); one-shot tasks and
  request-scoped work must stay as they are. Decide per loop whether a restart
  is safe or whether a panic there should instead be escalated to a graceful
  shutdown. (From a 2026-07 panic-resilience review.)
