//! Prints a JSON Schema for the Aperio config files to stdout.
//!
//! - `cargo run -p aperio-config`            → the `aperio.yaml` client schema
//! - `cargo run -p aperio-config -- --server` → the `aperio-server.yaml` schema
//!
//! Handy for CI (the release workflow versions both) and for regenerating a
//! schema by hand.

fn main() {
  let kind = std::env::args().nth(1);
  let schema = match kind.as_deref() {
    Some("--server") => aperio_config::server_schema_json(),
    Some("--expose") => aperio_config::expose_schema_json(),
    Some("--expose-policy") => aperio_config::expose_policy_schema_json(),
    _ => aperio_config::schema_json(),
  };
  println!("{schema}");
}
