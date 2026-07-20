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
