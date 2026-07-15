//! Emits the `aperio.yaml` JSON Schema to `<workspace>/schemas/` on build, so
//! editors (VS Code, Antigravity, ...) can point `yaml.schemas` at it. The
//! schema is derived from the shared `aperio-config` types, so it always tracks
//! the parser. The output directory is git-ignored (a build artifact).

use std::path::PathBuf;

fn main() {
  // Only regenerate when this script or the schema model changes — never on
  // our own writes into schemas/, so there is no rebuild loop.
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-changed=../aperio-config/src/lib.rs");

  let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
  let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);
  let out_dir = workspace_root.join("schemas");
  if let Err(e) = std::fs::create_dir_all(&out_dir) {
    println!(
      "cargo:warning=aperio: could not create {}: {e}",
      out_dir.display()
    );
    return;
  }

  // Two schemas: the client aperio.yaml and the server aperio-server.yaml.
  for (name, schema) in [
    ("aperio-client.schema.json", aperio_config::schema_json()),
    (
      "aperio-server.schema.json",
      aperio_config::server_schema_json(),
    ),
  ] {
    let out_file = out_dir.join(name);
    // Skip the write when unchanged to keep file mtimes stable.
    let changed = std::fs::read_to_string(&out_file)
      .map(|existing| existing != schema)
      .unwrap_or(true);
    if changed && let Err(e) = std::fs::write(&out_file, &schema) {
      println!(
        "cargo:warning=aperio: could not write {}: {e}",
        out_file.display()
      );
    }
  }
}
