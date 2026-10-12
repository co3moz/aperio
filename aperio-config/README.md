# aperio-config

Shared crate defining the Aperio configuration schemas: the exact serde
types `aperio-client` deserializes its `aperio.yaml` into, the server's
settings document (`ServerSettings`, the type the server stores and reads),
and a JSON Schema generator built on `schemars`.

The types live in their own crate so the client's build script can emit the
editor-facing JSON Schema straight from them: the schema and the parser can
never drift apart. Doc comments on fields become `description`s in the
generated schema, so they double as the `aperio.yaml` reference, keep them to
a single purposeful sentence and add `examples` where a value has a specific
format.

## Layout

- `src/lib.rs`, the schema types: `FileConfig`, `ServiceEntry` (public
  services), `TunnelDecl` (private TCP/UDP tunnels), `BindTunnelEntry`,
  header-rule types, `schema_json()`.
- `src/server_settings.rs` (with `server_settings/sections.rs`, one struct per
  section), the server's settings document, its boot
  environment, which paths are security-relevant, hold a credential or wait
  for a restart; `server_schema_json()` describes it.
- `src/units.rs`, durations (`30s`) and sizes (`10MB`) as the settings write
  them.
- `src/removed.rs`, every removed client key and server variable with what
  replaced it, so a refusal can name the new spelling.
- `src/main.rs`, prints a JSON Schema to stdout:

```bash
cargo run -p aperio-config            > aperio-client.schema.json
cargo run -p aperio-config -- --server > aperio-server.schema.json
```

The release workflow attaches both versioned schemas
(`aperio-client.<tag>.json`, `aperio-server.<tag>.json`) to every GitHub
Release. Both are the literal parser types, so neither can drift from what
the binaries accept.

## When you change a field

1. For a client field, follow the [one-name-three-surfaces standard](../docs/configuration.md#the-clients-standard-one-name-three-surfaces)
   (CLI ↔ yaml ↔ env names must match; keep legacy spellings as aliases).
2. Update the corresponding CLI/env plumbing in
   [`aperio-client/src/config.rs`](../aperio-client/src/config.rs).
3. Document the option in [docs/configuration.md](../docs/configuration.md)
   and add a `CHANGELOG.md` entry.
