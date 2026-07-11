# aperio-config

Shared crate defining the `aperio.yaml` client configuration schema — the
exact serde types `aperio-client` deserializes its config file into, plus a
JSON Schema generator built on `schemars`.

The types live in their own crate so the client's build script can emit the
editor-facing JSON Schema straight from them: the schema and the parser can
never drift apart. Doc comments on fields become `description`s in the
generated schema, so they double as the `aperio.yaml` reference — keep them to
a single purposeful sentence and add `examples` where a value has a specific
format.

## Layout

- `src/lib.rs` — the schema types: `FileConfig`, `ServiceEntry` (public
  services), `TunnelDecl` (private TCP/UDP tunnels), `BindTunnelEntry`,
  header-rule types, and `schema_json()`.
- `src/main.rs` — prints the JSON Schema to stdout:

```bash
cargo run -p aperio-config > aperio-client.schema.json
```

The release workflow attaches the versioned schema
(`aperio-client.<tag>.json`) to every GitHub Release.

## When you change a field

1. Follow the [one-name-three-surfaces standard](../docs/configuration.md#the-standard-one-name-three-surfaces)
   (CLI ↔ yaml ↔ env names must match; keep legacy spellings as aliases).
2. Update the corresponding CLI/env plumbing in
   [`aperio-client/src/config.rs`](../aperio-client/src/config.rs).
3. Document the option in [docs/configuration.md](../docs/configuration.md)
   and add a `CHANGELOG.md` entry.
