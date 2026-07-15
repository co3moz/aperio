# aperio-config

Shared crate defining the Aperio configuration schemas — the exact serde
types `aperio-client` deserializes its `aperio.yaml` into, a documented
schema for the server's `aperio-server.yaml`, and a JSON Schema generator
built on `schemars`.

The types live in their own crate so the client's build script can emit the
editor-facing JSON Schema straight from them: the schema and the parser can
never drift apart. Doc comments on fields become `description`s in the
generated schema, so they double as the `aperio.yaml` reference — keep them to
a single purposeful sentence and add `examples` where a value has a specific
format.

## Layout

- `src/lib.rs` — the schema types: `FileConfig`, `ServiceEntry` (public
  services), `TunnelDecl` (private TCP/UDP tunnels), `BindTunnelEntry`,
  header-rule types, `schema_json()`; and `ServerFileConfig` (the
  `aperio-server.yaml` keys) with `server_schema_json()`.
- `src/main.rs` — prints a JSON Schema to stdout:

```bash
cargo run -p aperio-config            > aperio-client.schema.json
cargo run -p aperio-config -- --server > aperio-server.schema.json
```

The release workflow attaches both versioned schemas
(`aperio-client.<tag>.json`, `aperio-server.<tag>.json`) to every GitHub
Release. The server is env-driven, so `ServerFileConfig` is a documented
mirror of the `APERIO_*` settings (kept in sync by hand) rather than the
literal parser type.

## When you change a field

1. Follow the [one-name-three-surfaces standard](../docs/configuration.md#the-standard-one-name-three-surfaces)
   (CLI ↔ yaml ↔ env names must match; keep legacy spellings as aliases).
2. Update the corresponding CLI/env plumbing in
   [`aperio-client/src/config.rs`](../aperio-client/src/config.rs).
3. Document the option in [docs/configuration.md](../docs/configuration.md)
   and add a `CHANGELOG.md` entry.
