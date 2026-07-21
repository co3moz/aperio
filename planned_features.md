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
