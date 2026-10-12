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

- `src/file.rs` and `src/client.rs`, the client file: `FileConfig` (the top
  level), `ServicePolicy` (what `defaults:` and every service share),
  `ServiceEntry` (one service), `TunnelEntry`, `BindValue`, the header-rule
  types, `schema_json()`.
- `src/layers.rs`, merging the client's layers (files, environment, command
  line) by one rule, deriving every environment variable from the schema,
  and refusing a key nothing reads with the one it was meant to be.
- `src/server_settings.rs` (with `server_settings/sections.rs`, one struct per
  section), the server's settings document, its boot
  environment, which paths are security-relevant, hold a credential or wait
  for a restart; `server_schema_json()` describes it.
- `src/units.rs`, durations (`30s`) and sizes (`10MB`) as the settings write
  them.
- `src/removed.rs`, every removed client key and variable, and every removed
  server variable, with what replaced it, so a refusal can name the new
  spelling.
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

1. For a client field, follow the [one-name rule](../docs/configuration.md#one-name-three-spellings):
   its environment variable is derived from the schema, so there is no env
   plumbing to write. A flag, when the setting earns one, writes the same key
   (`CliArgs::overlay` in `aperio-client/src/config/cli.rs`).
2. A renamed or removed key gets a hint in `src/removed.rs`, and a renamed
   variable an entry in `REMOVED_CLIENT_ENV`. A test fails on a hint that
   points at a setting the schemas do not have.
3. The doc comment is the documentation: it becomes the schema's
   description and the setting's row in [docs/reference.md](../docs/reference.md).
   Regenerate that page (`APERIO_BLESS=1 cargo test -p aperio-config reference`)
   and the dashboard's schema snapshot (`node scripts/dump-schemas.mjs` in
   `aperio-dashboard/`), and add a `CHANGELOG.md` entry.
