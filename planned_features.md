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
  non-issue). (From the 2026-07 static security review.)
