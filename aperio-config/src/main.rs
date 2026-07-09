//! Prints the `aperio.yaml` JSON Schema to stdout.
//!
//! Handy for CI (the release workflow versions the output) and for regenerating
//! the schema by hand: `cargo run -p aperio-config > aperio-client.schema.json`.

fn main() {
  println!("{}", aperio_config::schema_json());
}
